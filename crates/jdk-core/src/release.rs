//! Self-update source: this project's own GitHub releases. The latest
//! version is discovered from the `/releases/latest` redirect — GitHub
//! answers it with a hop to `/releases/tag/v<version>`, so reading the final
//! URL costs one unauthenticated GET and none of the API's rate limit — and
//! the release zip is fetched with its mandatory `.sha256` sidecar (the one
//! `release.yml` publishes next to every asset). A mismatch or a missing
//! sidecar BLOCKS, same stance as [`crate::download`].

use crate::download::{Progress, hex};
use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use crate::http::{Http, UrlPolicy, is_loopback, url_host};
use jdk_resolve::version::Version;
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

const REPO: &str = "isacgalvao/jdk";

/// Architecture of the RUNNING binary, decided at compile time — an x64 jdk
/// emulated on an arm64 machine must keep updating to x64, so the machine's
/// architecture is deliberately not consulted.
pub const ARCH: &str = if cfg!(target_arch = "aarch64") {
    "arm64"
} else {
    "x64"
};

/// Ceiling for one release bundle: the real zip is ~10 MiB, so 64 MiB is
/// generous and cheap.
const MAX_BUNDLE: u64 = 64 * 1024 * 1024;
/// Ceiling for the `.sha256` sidecar (one hash line).
const MAX_SIDECAR: u64 = 1024;
const CHUNK: usize = 64 * 1024;

/// This repository's releases — the only host outside loopback the
/// self-update path fetches from.
fn default_base() -> String {
    format!("https://github.com/{REPO}/releases")
}

/// The releases base URL and the policy to reach it: `JDK_RELEASES` (trimmed,
/// empty counts as unset) overrides the URL, and since that variable is a
/// hermetic-test injection point — same as `JDK_INDEX`/`JDK_FOOJAY`, with no
/// test-only switch in the production path — the override is confined to
/// loopback, over plain http if it wants. Whoever writes the variable already
/// runs as this user, so this is not a privilege boundary; what it denies is
/// turning one write into a permanent hold on the binary every shim invokes,
/// whose checksum sidecar comes from the same host as the zip. The
/// no-override default is this repository's releases over strict https.
pub fn base_url() -> (String, UrlPolicy) {
    match env::var("JDK_RELEASES") {
        Ok(value) if !value.trim().is_empty() => {
            (value.trim().to_string(), UrlPolicy::LoopbackOnly)
        }
        _ => (default_base(), UrlPolicy::Strict),
    }
}

/// Vets an update source before a single request goes out: this repository's
/// releases, or a loopback server. Checked here, rather than left to the
/// policy [`base_url`] pairs with the URL, so the refusal can name the
/// variable that caused it — the policy still guards every redirect hop, and
/// it is the only thing standing between a loopback override and a `Location`
/// header pointing anywhere.
fn check_source(base: &str) -> Result<()> {
    if base == default_base() || is_loopback(url_host(base)) {
        return Ok(());
    }
    Err(Error::Security(format!(
        "refusing {base} as the update source: JDK_RELEASES only points the updater \
         at a loopback server (it exists for hermetic tests, not as a release mirror)"
    )))
}

/// The newest released version, read from where the `{base}/latest` redirect
/// lands; the response body is discarded.
pub fn latest(http: &Http, base: &str) -> Result<Version> {
    check_source(base)?;
    let url = format!("{base}/latest");
    let reply = http.get(&url, "update", &[])?;
    let status = reply.status();
    if status != 200 {
        return Err(Error::Http(format!(
            "release check at {url} returned {status}"
        )));
    }
    tag_version(reply.url()).ok_or_else(|| {
        Error::Http(format!(
            "cannot read a release version from {} (no /tag/v<version> suffix); \
             if this persists, reinstall with install.ps1",
            reply.url()
        ))
    })
}

/// The version a release-tag URL names: the segment after `/tag/`, with the
/// conventional `v` prefix tolerated either way.
fn tag_version(url: &str) -> Option<Version> {
    let (_, tag) = url.rsplit_once("/tag/")?;
    tag.strip_prefix('v').unwrap_or(tag).parse().ok()
}

/// Downloads the `version` release zip for [`ARCH`] into `dest_dir`,
/// verified against its `.sha256` sidecar, and returns the zip path. The
/// bytes are hashed as they stream and staged next to the destination; the
/// sidecar is read after the download, and a mismatch — or a sidecar that
/// cannot be read — removes the staging and leaves nothing behind.
pub fn fetch_bundle(
    http: &Http,
    base: &str,
    version: &Version,
    dest_dir: &Path,
    mut progress: Option<Progress<'_>>,
) -> Result<PathBuf> {
    check_source(base)?;
    let asset = format!("jdk-v{version}-windows-{ARCH}.zip");
    let url = format!("{base}/download/v{version}/{asset}");
    fs::create_dir_all(dest_dir).map_err(Error::io("create", dest_dir))?;

    let reply = http.get_streaming(&url, "update", &[])?;
    match reply.status() {
        200 => {}
        404 => {
            // Mirrors install.ps1: arm64 zips are best-effort and a release
            // may legitimately not carry one.
            return Err(Error::Http(format!(
                "no {asset} in release v{version} — arm64 builds are best-effort and \
                 may be absent from a release; try x64, or a newer release"
            )));
        }
        status => {
            return Err(Error::Http(format!("download of {url} returned {status}")));
        }
    }
    let declared: u64 = reply
        .header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if declared > MAX_BUNDLE {
        return Err(Error::Security(format!(
            "{url} declares {declared} bytes, over the {MAX_BUNDLE}-byte ceiling"
        )));
    }
    if let Some(report) = progress.as_deref_mut() {
        report(0, declared);
    }

    let dest = dest_dir.join(&asset);
    let part = dest_dir.join(format!("{asset}.part"));
    let mut file = File::create(&part).map_err(Error::io("create", &part))?;
    let mut hasher = Sha256::new();
    let mut reader = reply.reader(MAX_BUNDLE.saturating_add(1));
    let mut buffer = vec![0u8; CHUNK];
    let mut downloaded = 0u64;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buffer[..n]);
                file.write_all(&buffer[..n])
                    .map_err(Error::io("write", &part))?;
                downloaded += n as u64;
                if downloaded > MAX_BUNDLE {
                    drop(file);
                    let _ = fs::remove_file(&part);
                    return Err(Error::Security(format!(
                        "{url} exceeded the {MAX_BUNDLE}-byte ceiling"
                    )));
                }
                if let Some(report) = progress.as_deref_mut() {
                    report(downloaded, declared);
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                let _ = fs::remove_file(&part);
                return Err(Error::Http(format!("download of {url} interrupted: {err}")));
            }
        }
    }
    file.flush().map_err(Error::io("flush", &part))?;
    drop(file);

    // The sidecar comes AFTER the zip on purpose: release.yml only writes a
    // sidecar for an asset it packaged, so a release without this ARCH
    // 404s on both — and must surface as the zip's arm64 hint above, never
    // as a sidecar complaint.
    let expected = match sidecar_sha256(http, &url, version, &asset) {
        Ok(expected) => expected,
        Err(err) => {
            let _ = fs::remove_file(&part);
            return Err(err);
        }
    };
    let actual = hex(&hasher.finalize());
    if actual != expected {
        let _ = fs::remove_file(&part);
        return Err(Error::Checksum {
            subject: url,
            expected,
            actual,
        });
    }
    atomic_rename(&part, &dest).map_err(Error::io("finalize", &dest))?;
    Ok(dest)
}

/// The expected hash from the `<zip url>.sha256` sidecar (`<hash>  <name>`,
/// as release.yml writes it). Absent or unreadable is a refusal, never a
/// warn-and-continue.
fn sidecar_sha256(http: &Http, zip_url: &str, version: &Version, asset: &str) -> Result<String> {
    let url = format!("{zip_url}.sha256");
    let reply = http.get(&url, "update", &[])?;
    match reply.status() {
        200 => {}
        404 => {
            return Err(Error::Security(format!(
                "release v{version} has no {asset}.sha256 sidecar; \
                 refusing an unverifiable download"
            )));
        }
        status => {
            return Err(Error::Http(format!("GET {url} returned {status}")));
        }
    }
    let body = reply.bytes(MAX_SIDECAR)?;
    parse_sidecar(&body)
        .ok_or_else(|| Error::Security(format!("malformed sha256 sidecar at {url}; refusing")))
}

/// First token of a sidecar body when it looks like a sha256: 64 hex chars,
/// lowercased.
fn parse_sidecar(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let hash = text.split_whitespace().next()?.to_ascii_lowercase();
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_version_reads_the_redirect_target() {
        let parsed = tag_version("https://github.com/isacgalvao/jdk/releases/tag/v0.3.0").unwrap();
        assert_eq!(parsed.to_string(), "0.3.0");
        // The `v` prefix is conventional, not required.
        assert_eq!(
            tag_version("https://host/releases/tag/1.2.3")
                .unwrap()
                .to_string(),
            "1.2.3"
        );
    }

    #[test]
    fn tag_version_rejects_unversioned_urls() {
        // No tag segment: the redirect did not land where a release lives.
        assert!(tag_version("https://github.com/isacgalvao/jdk/releases").is_none());
        // A tag that is not a version.
        assert!(tag_version("https://host/releases/tag/nightly").is_none());
        assert!(tag_version("https://host/releases/tag/").is_none());
    }

    #[test]
    fn sidecar_wants_one_sha256_token() {
        let hash = "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f";
        let line = format!("{}  jdk-v9.9.9-windows-x64.zip\n", hash.to_uppercase());
        assert_eq!(parse_sidecar(line.as_bytes()).unwrap(), hash);

        assert!(parse_sidecar(b"").is_none());
        assert!(parse_sidecar(b"not-a-hash  file.zip").is_none());
        assert!(parse_sidecar(b"abc123  file.zip").is_none(), "too short");
    }

    #[test]
    fn update_source_is_this_repo_or_loopback() {
        assert!(check_source(&default_base()).is_ok());
        assert!(check_source("http://127.0.0.1:8080/releases").is_ok());
        assert!(check_source("https://localhost:8443/releases").is_ok());
    }

    #[test]
    fn update_source_refuses_a_redirected_host() {
        let err = check_source("https://releases.evil.example/jdk")
            .unwrap_err()
            .to_string();
        // The message has to name the variable: nothing else in the run tells
        // the user why an update they did not reconfigure just stopped.
        assert!(err.contains("JDK_RELEASES"), "{err}");

        // A host that only looks like the real one, and the real host under a
        // path that is not this repository's releases.
        assert!(check_source("https://github.com.evil.example/isacgalvao/jdk/releases").is_err());
        assert!(check_source("https://github.com/someone-else/jdk/releases").is_err());
    }

    #[test]
    fn arch_names_a_release_asset_flavor() {
        assert!(ARCH == "x64" || ARCH == "arm64");
    }
}
