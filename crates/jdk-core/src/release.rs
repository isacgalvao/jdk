//! Self-update source: this project's own GitHub releases. The latest
//! version is discovered from the `/releases/latest` redirect — GitHub
//! answers it with a hop to `/releases/tag/v<version>`, so reading the final
//! URL costs one unauthenticated GET and none of the API's rate limit.
//!
//! What the release zip is verified against is `SHA256SUMS`, signed by
//! `RELEASE_PUBKEY` — a key that lives in this binary, not on the release
//! host. That is the whole point: the per-asset `.sha256` sidecar
//! `release.yml` still publishes for `install.ps1` is served by whoever
//! serves the zip, so it can only catch a corrupt transfer, never a
//! substituted release. The updater does not read it.
//!
//! The signed line names the asset, and the asset name carries the version
//! ([`ARCH`], `jdk-v<version>-windows-<arch>.zip`), so one signature binds
//! the announced version to the exact bytes that become the next `jdk.exe`.
//! Every failure BLOCKS — a release without both `SHA256SUMS` and
//! `SHA256SUMS.sig` is refused rather than installed unverified.

use crate::download::{Progress, hex};
use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use crate::http::{Http, UrlPolicy, is_loopback, url_host};
use crate::sshsig::{self, PublicKey};
use jdk_resolve::version::Version;
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

const REPO: &str = "isacgalvao/jdk";

/// The key `release.yml` signs `SHA256SUMS` with. Rotating it is a MINOR
/// release: an updater only trusts the key it shipped with, so the new key
/// has to reach machines in a release signed by the old one. RELEASING.md
/// carries the procedure and the compromise plan.
const RELEASE_PUBKEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKufZs1YJGeiBZsIVZSpxMIR/1hmAu1/biqqKJzgDxu7 jdk-release-2026";
/// SSHSIG namespace of the release signature: what keeps a signature this key
/// made for anything else from counting as a release.
const NAMESPACE: &str = "jdk-release";

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
/// Ceiling for `SHA256SUMS`: one line per published asset, a handful of them.
const MAX_SUMS: u64 = 64 * 1024;
/// Ceiling for `SHA256SUMS.sig` (an armored ed25519 signature is ~300 bytes).
const MAX_SIG: u64 = 4 * 1024;
const CHUNK: usize = 64 * 1024;

/// This repository's releases — the only host outside loopback the
/// self-update path fetches from.
fn default_base() -> String {
    format!("https://github.com/{REPO}/releases")
}

/// Where releases are fetched from, how to reach it, and the key their
/// `SHA256SUMS` must be signed with. The three travel together because the
/// override that may move the URL is the only thing that may move the key —
/// see [`Source::resolve`].
pub struct Source {
    base: String,
    policy: UrlPolicy,
    key: PublicKey,
}

impl Source {
    /// A source with an explicit trust anchor: `key` is an OpenSSH
    /// `ssh-ed25519 <base64> [comment]` line, the same format
    /// `RELEASE_PUBKEY` is written in.
    pub fn new(base: String, policy: UrlPolicy, key: &str) -> Result<Source> {
        Ok(Source {
            base,
            policy,
            key: PublicKey::parse(key)?,
        })
    }

    /// This repository's releases over strict https, verified with the pinned
    /// key — unless `JDK_RELEASES` (trimmed, empty counts as unset) moves it.
    ///
    /// That variable is a hermetic-test injection point, same as
    /// `JDK_INDEX`/`JDK_FOOJAY` and with no test-only switch in the production
    /// path, so the override is confined to loopback, over plain http if it
    /// wants. Whoever writes it already runs as this user, so this is not a
    /// privilege boundary; what it denies is turning one write into a
    /// permanent hold on the binary every shim invokes.
    ///
    /// `JDK_RELEASE_PUBKEY` replaces the pinned key by the same argument and
    /// under the same confinement — a test server cannot hold the production
    /// private key, so a hermetic release has to be signed by a test one. It
    /// is honored ONLY together with `JDK_RELEASES`: read here, where the
    /// override is already established, rather than wherever the key is
    /// consumed, so there is no path on which a lone `JDK_RELEASE_PUBKEY`
    /// re-anchors the real update source.
    pub fn resolve() -> Result<Source> {
        match env::var("JDK_RELEASES") {
            Ok(value) if !value.trim().is_empty() => {
                let key = env::var("JDK_RELEASE_PUBKEY")
                    .ok()
                    .filter(|key| !key.trim().is_empty())
                    .unwrap_or_else(|| RELEASE_PUBKEY.to_string());
                Source::new(value.trim().to_string(), UrlPolicy::LoopbackOnly, &key)
            }
            _ => Source::new(default_base(), UrlPolicy::Strict, RELEASE_PUBKEY),
        }
    }

    pub fn policy(&self) -> UrlPolicy {
        self.policy
    }

    /// Vets the source before a single request goes out: this repository's
    /// releases, or a loopback server. Checked here, rather than left to the
    /// policy [`Source::resolve`] pairs with the URL, so the refusal can name
    /// the variable that caused it — the policy still guards every redirect
    /// hop, and it is the only thing standing between a loopback override and
    /// a `Location` header pointing anywhere.
    fn check(&self) -> Result<()> {
        if self.base == default_base() || is_loopback(url_host(&self.base)) {
            return Ok(());
        }
        Err(Error::Security(format!(
            "refusing {} as the update source: JDK_RELEASES only points the updater \
             at a loopback server (it exists for hermetic tests, not as a release mirror)",
            self.base
        )))
    }
}

/// The newest released version, read from where the `{base}/latest` redirect
/// lands; the response body is discarded.
///
/// The redirect must land back on the source's own host. This is the one
/// place a `Location` header gets to decide what the updater believes, and
/// the policy alone would not stop it: an https hop to any host passes
/// [`UrlPolicy::Strict`], and the version would then be read off a URL
/// nobody vouched for. Deliberately NOT applied to the asset downloads in
/// [`fetch_bundle`] — GitHub redirects those to its object storage as a
/// matter of course, and what vouches for those bytes is the signed
/// `SHA256SUMS`, not the hostname they arrived from.
pub fn latest(http: &Http, source: &Source) -> Result<Version> {
    source.check()?;
    let base = &source.base;
    let url = format!("{base}/latest");
    let reply = http.get(&url, "update", &[])?;
    let status = reply.status();
    if status != 200 {
        return Err(Error::Http(format!(
            "release check at {url} returned {status}"
        )));
    }
    let landed = url_host(reply.url());
    if landed != url_host(base) {
        return Err(Error::Security(format!(
            "release check at {url} was redirected to {landed}; refusing to read a \
             version from a host that does not publish this project's releases"
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
/// verified against the signed `SHA256SUMS` of that release, and returns the
/// zip path. The bytes are hashed as they stream and staged next to the
/// destination; anything short of a signature that verifies and a matching
/// hash removes the staging and leaves nothing behind.
///
/// The signature is fetched and checked BEFORE the zip: an unsigned release
/// is the cheapest failure available, and there is no reason to spend a
/// download discovering it.
pub fn fetch_bundle(
    http: &Http,
    source: &Source,
    version: &Version,
    dest_dir: &Path,
    mut progress: Option<Progress<'_>>,
) -> Result<PathBuf> {
    source.check()?;
    let base = &source.base;
    let asset = format!("jdk-v{version}-windows-{ARCH}.zip");
    let assets = format!("{base}/download/v{version}");
    let url = format!("{assets}/{asset}");
    let sums = signed_sums(http, &assets, version, &source.key)?;
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
    // Looked up only once the zip has answered 200: a release with no build
    // for this ARCH carries no line for it either, and that case belongs to
    // the arm64 hint above, not to a missing-line refusal.
    let expected = sum_for(&sums, &asset).ok_or_else(|| {
        Error::Security(format!(
            "release v{version} serves {asset} but its signed SHA256SUMS does not \
             cover it; refusing an unverifiable download"
        ))
    })?;
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

/// The release's `SHA256SUMS`, returned only once its detached
/// `SHA256SUMS.sig` verifies under `key`. A release missing either file is
/// refused rather than trusted: every release from 0.6.0 on publishes both,
/// so their absence is not an old release the updater might meet — the update
/// path only ever moves forward — but an anchor that was stripped.
fn signed_sums(http: &Http, assets: &str, version: &Version, key: &PublicKey) -> Result<String> {
    let sums = fetch_anchor(http, &format!("{assets}/SHA256SUMS"), version, MAX_SUMS)?;
    let signature = fetch_anchor(http, &format!("{assets}/SHA256SUMS.sig"), version, MAX_SIG)?;
    let sums = String::from_utf8(sums)
        .map_err(|_| Error::Security(format!("SHA256SUMS of release v{version} is not text")))?;
    let signature = String::from_utf8(signature).map_err(|_| {
        Error::Security(format!(
            "SHA256SUMS.sig of release v{version} is not an armored signature"
        ))
    })?;
    sshsig::verify(&signature, sums.as_bytes(), key, NAMESPACE)?;
    Ok(sums)
}

/// One of the two signature artifacts, whose absence is a security refusal
/// naming the release rather than a bare 404.
fn fetch_anchor(http: &Http, url: &str, version: &Version, limit: u64) -> Result<Vec<u8>> {
    let reply = http.get(url, "update", &[])?;
    match reply.status() {
        200 => reply.bytes(limit),
        404 => Err(Error::Security(format!(
            "release v{version} is not signed ({url} is missing); refusing an \
             unverifiable download — reinstall with install.ps1 if this persists"
        ))),
        status => Err(Error::Http(format!("GET {url} returned {status}"))),
    }
}

/// The hash `sums` records for `asset`: the `<hash>  <name>` line naming it,
/// lowercased. The name is matched exactly, so the line for
/// `jdk-v1.2.3-windows-x64.zip` can never answer for another version's.
fn sum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?.to_ascii_lowercase();
        (fields.next()? == asset
            && fields.next().is_none()
            && hash.len() == 64
            && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(hash)
    })
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

    /// A source whose base is `base`; the key never matters to these tests,
    /// so it is the pinned one.
    fn at(base: &str) -> Source {
        Source::new(base.to_string(), UrlPolicy::Strict, RELEASE_PUBKEY).unwrap()
    }

    #[test]
    fn the_pinned_key_is_a_usable_ed25519_line() {
        // A typo in the constant would otherwise surface only on a machine
        // trying to update, against a release it cannot verify.
        assert!(PublicKey::parse(RELEASE_PUBKEY).is_ok());
    }

    #[test]
    fn sum_lines_answer_for_exactly_their_own_asset() {
        let hash = "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f";
        let sums = format!(
            "{}  jdk-v9.9.9-windows-x64.zip\n{hash}  install.ps1\n",
            hash.to_uppercase()
        );
        assert_eq!(sum_for(&sums, "jdk-v9.9.9-windows-x64.zip").unwrap(), hash);
        assert_eq!(sum_for(&sums, "install.ps1").unwrap(), hash);

        // BUG-09: the asset name carries the version, so a SHA256SUMS signed
        // for another release covers no line the updater will ask for.
        assert!(sum_for(&sums, "jdk-v9.9.8-windows-x64.zip").is_none());
        assert!(sum_for(&sums, "windows-x64.zip").is_none(), "not a suffix");
        assert!(
            sum_for(&sums, "jdk-v9.9.9-windows").is_none(),
            "not a prefix"
        );

        assert!(sum_for("", "anything").is_none());
        assert!(sum_for("not-a-hash  install.ps1", "install.ps1").is_none());
        assert!(
            sum_for("abc123  install.ps1", "install.ps1").is_none(),
            "short"
        );
        assert!(
            sum_for(&format!("{hash}  install.ps1  extra"), "install.ps1").is_none(),
            "a name with spaces is not this name"
        );
    }

    #[test]
    fn update_source_is_this_repo_or_loopback() {
        assert!(at(&default_base()).check().is_ok());
        assert!(at("http://127.0.0.1:8080/releases").check().is_ok());
        assert!(at("https://localhost:8443/releases").check().is_ok());
    }

    #[test]
    fn update_source_refuses_a_redirected_host() {
        let err = at("https://releases.evil.example/jdk")
            .check()
            .unwrap_err()
            .to_string();
        // The message has to name the variable: nothing else in the run tells
        // the user why an update they did not reconfigure just stopped.
        assert!(err.contains("JDK_RELEASES"), "{err}");

        // A host that only looks like the real one, and the real host under a
        // path that is not this repository's releases.
        assert!(
            at("https://github.com.evil.example/isacgalvao/jdk/releases")
                .check()
                .is_err()
        );
        assert!(
            at("https://github.com/someone-else/jdk/releases")
                .check()
                .is_err()
        );
    }

    #[test]
    fn arch_names_a_release_asset_flavor() {
        assert!(ARCH == "x64" || ARCH == "arm64");
    }
}
