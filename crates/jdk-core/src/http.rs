//! Shared HTTP client: rustls with corporate-CA support (`JDK_CAFILE` >
//! `JDK_CAPATH` > Windows certificate store), proxy from the environment,
//! HTTPS-only policy enforced on the initial URL AND on every redirect hop,
//! and manual redirect following — automatic redirects would drop the custom
//! headers some vendors require.
//!
//! Two body budgets: [`Http::get`] bounds the whole call (small JSON), while
//! [`Http::get_streaming`] bounds only DNS and connect so an archive body can
//! take as long as the link needs.

use crate::error::{Error, Result};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use ureq::Agent;
use ureq::http::Response;
use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig, parse_pem};

/// Budget for one request: the whole call on the strict path ([`Http::get`]);
/// DNS and connect only on the streaming path ([`Http::get_streaming`]).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: u32 = 10;
/// Ceiling for honoring a server's Retry-After.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(10);

/// User-Agent per component: `jdk/<component>/<version>`.
pub fn user_agent(component: &str) -> String {
    format!("jdk/{component}/{}", env!("CARGO_PKG_VERSION"))
}

/// Which URLs a client may touch, checked on the initial URL and on every
/// redirect hop. Production uses [`UrlPolicy::Strict`]; the loopback variant
/// exists for hermetic test servers and is the ONLY route to plain http —
/// there is no test-only compilation switch anywhere in the production path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlPolicy {
    /// https only; loopback hosts and `..` in the URL are rejected too.
    Strict,
    /// Like Strict, but loopback hosts are allowed, including over http.
    AllowInsecureLoopback,
}

impl UrlPolicy {
    pub fn check(self, url: &str) -> Result<()> {
        if url.contains("..") {
            return Err(Error::Security(format!("suspicious URL: {url}")));
        }
        if url.strip_prefix("https://").is_some() {
            if self == UrlPolicy::Strict && is_loopback(url_host(url)) {
                return Err(Error::Security(format!("loopback URL rejected: {url}")));
            }
            return Ok(());
        }
        if url.strip_prefix("http://").is_some() {
            if self == UrlPolicy::AllowInsecureLoopback && is_loopback(url_host(url)) {
                return Ok(());
            }
            return Err(Error::Security(format!(
                "insecure URL (https required): {url}"
            )));
        }
        Err(Error::Security(format!("unsupported URL scheme: {url}")))
    }
}

/// Host portion of a URL: authority minus userinfo and port.
pub(crate) fn url_host(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(url, |at| &url[at + 3..]);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    match host_port.strip_prefix('[') {
        // Bracketed IPv6: [::1]:8080
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => host_port.split(':').next().unwrap_or_default(),
    }
}

pub(crate) fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// Retry schedule for transient failures: 3 attempts with exponential backoff
/// from 1s. Tests inject a millisecond delay.
#[derive(Debug, Clone, Copy)]
pub struct Retry {
    pub attempts: u32,
    pub base_delay: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Retry {
            attempts: 3,
            base_delay: Duration::from_secs(1),
        }
    }
}

/// How a GET budgets the response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The whole call, body included, fits in the request timeout.
    Strict,
    /// Only DNS and connect are bounded; the response streams for as long as
    /// it takes.
    Streaming,
}

pub struct Http {
    agent: Agent,
    policy: UrlPolicy,
    retry: Retry,
    request_timeout: Duration,
}

impl Http {
    pub fn new(policy: UrlPolicy) -> Result<Self> {
        Self::with_retry(policy, Retry::default())
    }

    pub fn with_retry(policy: UrlPolicy, retry: Retry) -> Result<Self> {
        Self::with_request_timeout(policy, retry, REQUEST_TIMEOUT)
    }

    /// Full control over the request budget (tests inject a short one to pin
    /// the strict-vs-streaming timeout semantics).
    pub fn with_request_timeout(
        policy: UrlPolicy,
        retry: Retry,
        request_timeout: Duration,
    ) -> Result<Self> {
        let tls = TlsConfig::builder().root_certs(root_certs()?).build();
        let config = Agent::config_builder()
            // Non-2xx statuses are data, not errors; callers decide.
            .http_status_as_error(false)
            // Redirects are followed manually in `get`, policy-checked per hop.
            .max_redirects(0)
            .max_redirects_will_error(false)
            // Strict default: small catalog/foojay bodies fit comfortably.
            // The streaming path overrides this per request in `send`.
            .timeout_global(Some(request_timeout))
            // Transport-level backstop underneath the per-hop policy checks.
            .https_only(policy == UrlPolicy::Strict)
            .tls_config(tls)
            .build();
        Ok(Http {
            agent: config.new_agent(),
            policy,
            retry,
            request_timeout,
        })
    }

    pub fn policy(&self) -> UrlPolicy {
        self.policy
    }

    /// GET for small bodies (catalog and foojay JSON): the request timeout
    /// bounds the ENTIRE call, body included. Redirects are followed manually
    /// — the policy vets every hop and `headers` are re-sent on every hop
    /// (vendor headers such as Azul's Referer must survive redirects).
    /// Transient failures are retried.
    pub fn get(&self, url: &str, component: &str, headers: &[(&str, String)]) -> Result<Reply> {
        self.get_with(url, component, headers, Mode::Strict)
    }

    /// GET for archive downloads: the request timeout bounds DNS and connect
    /// (the hang cases), while the response streams unbounded — a ~200 MB JDK
    /// on a slow link must not die at an arbitrary wall-clock cutoff (a 30s
    /// global budget kills any download slower than ~52 Mbps). A stalled
    /// server is the user's Ctrl+C to make; integrity is guaranteed by the
    /// mandatory sha256, not by timing. Same redirect, policy and retry
    /// behavior as [`Http::get`].
    pub fn get_streaming(
        &self,
        url: &str,
        component: &str,
        headers: &[(&str, String)],
    ) -> Result<Reply> {
        self.get_with(url, component, headers, Mode::Streaming)
    }

    fn get_with(
        &self,
        url: &str,
        component: &str,
        headers: &[(&str, String)],
        mode: Mode,
    ) -> Result<Reply> {
        let mut current = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            self.policy.check(&current)?;
            let response = self.get_retrying(&current, component, headers, mode)?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        Error::Http(format!("redirect without Location from {current}"))
                    })?;
                current = absolutize(&current, location);
                continue;
            }
            return Ok(Reply {
                url: current,
                response,
            });
        }
        Err(Error::Http(format!(
            "more than {MAX_REDIRECTS} redirects starting at {url}"
        )))
    }

    fn get_retrying(
        &self,
        url: &str,
        component: &str,
        headers: &[(&str, String)],
        mode: Mode,
    ) -> Result<Response<ureq::Body>> {
        let mut delay = self.retry.base_delay;
        let mut attempt = 1;
        loop {
            let last = attempt >= self.retry.attempts;
            match self.send(url, component, headers, mode) {
                Ok(response) if retryable_status(response.status().as_u16()) && !last => {
                    let wait = retry_after(&response).unwrap_or(delay).min(RETRY_AFTER_CAP);
                    thread::sleep(wait);
                }
                Ok(response) => return Ok(response),
                Err(_) if !last => {
                    thread::sleep(delay);
                }
                Err(err) => {
                    return Err(Error::Http(format!(
                        "GET {url} failed after {attempt} attempt(s): {err}"
                    )));
                }
            }
            delay = delay.saturating_mul(2);
            attempt += 1;
        }
    }

    fn send(
        &self,
        url: &str,
        component: &str,
        headers: &[(&str, String)],
        mode: Mode,
    ) -> std::result::Result<Response<ureq::Body>, ureq::Error> {
        let mut request = self.agent.get(url);
        if mode == Mode::Streaming {
            // Per-request override on the shared agent: drop the global
            // (whole-call) budget and bound the connection phases instead,
            // leaving the response free to take as long as it needs.
            // `timeout_recv_response` is deliberately NOT set: ureq 3.3
            // attributes slow-body reads to the recv_response clock, which
            // would reintroduce the whole-call ceiling this mode removes
            // (pinned by the streaming regression test).
            request = request
                .config()
                .timeout_global(None)
                .timeout_resolve(Some(self.request_timeout))
                .timeout_connect(Some(self.request_timeout))
                .build();
        }
        let mut request = request.header("User-Agent", user_agent(component));
        for (name, value) in headers {
            request = request.header(*name, value.as_str());
        }
        request.call()
    }
}

/// 429 obeys Retry-After; 502/503/504 are gateway hiccups worth retrying.
fn retryable_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

fn retry_after(response: &Response<ureq::Body>) -> Option<Duration> {
    let seconds: u64 = response
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

/// Resolves a Location header against the URL that produced it. Handles the
/// three shapes servers send: absolute URL, absolute path, relative path.
fn absolutize(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let base = &base[..base.find(['?', '#']).unwrap_or(base.len())];
    let scheme_end = base.find("://").map_or(0, |at| at + 3);
    let path_start = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |at| scheme_end + at);
    if location.starts_with('/') {
        return format!("{}{location}", &base[..path_start]);
    }
    match base.rfind('/').filter(|&at| at >= path_start) {
        Some(at) => format!("{}{location}", &base[..=at]),
        None => format!("{base}/{location}"),
    }
}

/// Upper bound for any single JSON body (catalog files and foojay replies):
/// 32 MiB.
pub(crate) const MAX_BODY: u64 = 32 * 1024 * 1024;

/// A non-redirect response plus the final URL that produced it.
pub struct Reply {
    url: String,
    response: Response<ureq::Body>,
}

impl Reply {
    pub fn status(&self) -> u16 {
        self.response.status().as_u16()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    }

    /// Streaming body reader capped at `limit` bytes.
    pub fn reader(self, limit: u64) -> impl Read {
        self.response
            .into_body()
            .into_with_config()
            .limit(limit)
            .reader()
    }

    /// Whole body, refusing more than `limit` bytes.
    pub fn bytes(self, limit: u64) -> Result<Vec<u8>> {
        let url = self.url.clone();
        let mut data = Vec::new();
        self.reader(limit.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|err| Error::Http(format!("reading body of {url}: {err}")))?;
        if data.len() as u64 > limit {
            return Err(Error::Http(format!(
                "body of {url} exceeds the {limit}-byte limit"
            )));
        }
        Ok(data)
    }
}

/// Trust roots in precedence order: `JDK_CAFILE` (one PEM bundle) >
/// `JDK_CAPATH` (directory of .pem/.crt) > the platform store — on Windows
/// the certificate store, where corporate CAs usually live. An explicitly
/// configured source that yields nothing is a hard error, never a silent
/// fallback.
fn root_certs() -> Result<RootCerts> {
    if let Some(file) = env_path("JDK_CAFILE") {
        let pem = fs::read(&file).map_err(Error::io("read JDK_CAFILE", &file))?;
        return Ok(RootCerts::new_with_certs(&pem_certs(&pem, &file)?));
    }
    if let Some(dir) = env_path("JDK_CAPATH") {
        let mut certs = Vec::new();
        let entries = fs::read_dir(&dir).map_err(Error::io("read JDK_CAPATH", &dir))?;
        for entry in entries {
            let path = entry.map_err(Error::io("read JDK_CAPATH", &dir))?.path();
            let is_pem = path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("pem") || ext.eq_ignore_ascii_case("crt")
            });
            if !is_pem {
                continue;
            }
            let pem = fs::read(&path).map_err(Error::io("read", &path))?;
            certs.extend(pem_certs(&pem, &path)?);
        }
        if certs.is_empty() {
            return Err(Error::Security(format!(
                "JDK_CAPATH {} contains no usable .pem/.crt certificates",
                dir.display()
            )));
        }
        return Ok(RootCerts::new_with_certs(&certs));
    }

    let loaded = rustls_native_certs::load_native_certs();
    let certs: Vec<Certificate<'static>> = loaded
        .certs
        .iter()
        .map(|der| Certificate::from_der(der.as_ref()).to_owned())
        .collect();
    if certs.is_empty() {
        return Err(Error::Security(format!(
            "no trust roots in the system certificate store ({} load errors); set JDK_CAFILE or JDK_CAPATH",
            loaded.errors.len()
        )));
    }
    Ok(RootCerts::new_with_certs(&certs))
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn pem_certs(pem: &[u8], origin: &Path) -> Result<Vec<Certificate<'static>>> {
    let mut certs = Vec::new();
    for item in parse_pem(pem) {
        let item = item.map_err(|err| {
            Error::Security(format!("invalid PEM in {}: {err}", origin.display()))
        })?;
        if let PemItem::Certificate(cert) = item {
            certs.push(cert);
        }
    }
    if certs.is_empty() {
        return Err(Error::Security(format!(
            "{} contains no certificates",
            origin.display()
        )));
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_policy_is_https_only() {
        let policy = UrlPolicy::Strict;
        assert!(policy.check("https://api.foojay.io/x").is_ok());
        assert!(policy.check("http://api.foojay.io/x").is_err());
        assert!(policy.check("ftp://api.foojay.io/x").is_err());
        assert!(policy.check("https://example.com/../etc").is_err());
        assert!(policy.check("https://127.0.0.1/x").is_err());
        assert!(policy.check("https://localhost/x").is_err());
    }

    #[test]
    fn loopback_policy_allows_only_loopback_http() {
        let policy = UrlPolicy::AllowInsecureLoopback;
        assert!(policy.check("http://127.0.0.1:8080/x").is_ok());
        assert!(policy.check("http://localhost:1234/x").is_ok());
        assert!(policy.check("https://127.0.0.1:8443/x").is_ok());
        assert!(policy.check("http://example.com/x").is_err());
        assert!(policy.check("https://api.foojay.io/x").is_ok());
    }

    #[test]
    fn extracts_hosts() {
        assert_eq!(url_host("https://api.foojay.io/disco"), "api.foojay.io");
        assert_eq!(url_host("http://127.0.0.1:8080/x?y#z"), "127.0.0.1");
        assert_eq!(
            url_host("https://user:pw@host.example:443/"),
            "host.example"
        );
        assert_eq!(url_host("http://[::1]:9000/x"), "::1");
        assert_eq!(url_host("https://host"), "host");
    }

    #[test]
    fn absolutizes_locations() {
        assert_eq!(
            absolutize("https://a.example/x/y", "https://b.example/z"),
            "https://b.example/z"
        );
        assert_eq!(
            absolutize("https://a.example/x/y", "/root"),
            "https://a.example/root"
        );
        assert_eq!(
            absolutize("https://a.example/x/y", "sibling"),
            "https://a.example/x/sibling"
        );
        assert_eq!(
            absolutize("https://a.example/x/y?q=1", "sibling"),
            "https://a.example/x/sibling"
        );
        assert_eq!(
            absolutize("https://a.example", "path"),
            "https://a.example/path"
        );
        assert_eq!(
            absolutize("https://a.example", "/path"),
            "https://a.example/path"
        );
    }

    #[test]
    fn retry_covers_rate_limit_and_gateways_only() {
        for status in [429, 502, 503, 504] {
            assert!(retryable_status(status), "{status} should be retryable");
        }
        for status in [200, 304, 400, 404, 410, 500] {
            assert!(
                !retryable_status(status),
                "{status} should not be retryable"
            );
        }
    }

    #[test]
    fn user_agent_names_the_component() {
        let agent = user_agent("download");
        assert!(agent.starts_with("jdk/download/"), "{agent}");
        assert!(agent.ends_with(env!("CARGO_PKG_VERSION")), "{agent}");
    }
}
