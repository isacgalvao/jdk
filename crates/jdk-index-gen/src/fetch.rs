//! Foojay Disco API → [`Package`] lists, one vendor+arch at a time.
//!
//! The platform filters are the exact Windows foojay queries — GA-only
//! included: a trial run with `release_status=ea,ga` dragged in vendors' EA
//! *nightlies* (temurin alone: dozens of beta builds per version), bloating
//! the index and churning it daily, so EA stays out of v0.1 by decision. One
//! generator-only widening over the runtime fallback in `jdk_core::foojay`:
//! `javafx_bundled=false` (an FX bundle would collide with the plain JDK
//! under the same version key). The `release_status` → EA mapping is kept for
//! schema completeness should the query ever widen.
//!
//! # sha256 resolution chain
//!
//! The index contract makes sha256 mandatory, but foojay only carries one
//! for some vendors (2026-07 survey: temurin/zulu inline; graalvm/microsoft
//! via `checksum_uri`; liberica sha1-only; corretto nothing). Per package,
//! in order:
//!
//! 1. inline `checksum` when `checksum_type` is sha256;
//! 2. the vendor-hosted file behind `checksum_uri` (Oracle's bare-hash
//!    `.sha256`, Microsoft's `sha256sum`-style `.sha256sum.txt`);
//! 3. the sha256 the PUBLISHED index already records for the identical URL
//!    (release artifacts are immutable — reuse is what keeps step 4 a
//!    one-time cost);
//! 4. trust-on-first-use: stream the archive once and hash it, guarded by a
//!    zip magic-number check, a size floor and a 2 GiB cap (an error page or
//!    an endless body must never be published as a JDK), cross-checked
//!    against the sha1 foojay announces when that is all it has (liberica),
//!    and capped by `--hash-budget` per vendor+arch — over-budget packages
//!    are dropped today and backfilled by later runs.
//!
//! Every published URL must also pass the SAME vendor-host allowlist the
//! client enforces on install (`jdk_core::download::check_trusted`), applied
//! both at listing time and again right before any TOFU download — a
//! compromised foojay must not be able to smuggle foreign hosts into the
//! index.
//!
//! The listing endpoint carries no checksum at all, so every listed package
//! costs one `ids/<id>` call — fanned out over `jobs` scoped threads feeding
//! a channel. A failed detail CALL aborts the run (dropping it would
//! silently shrink the index — anti-model 7); a package that finishes the
//! chain without a sha256 is dropped with a counted warning.

use crate::validate::Published;
use jdk_core::download::check_trusted;
use jdk_core::error::{Error, Result};
use jdk_core::http::{Http, UrlPolicy};
use jdk_core::index::{Package, ReleaseStatus};
use jdk_resolve::version::Version;
use serde::Deserialize;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

pub(crate) const MAX_BODY: u64 = 32 * 1024 * 1024;
const MAX_CHECKSUM_FILE: u64 = 64 * 1024;
/// Bigger than any real JDK zip (~200–300 MB) but small enough to bound the
/// time an endless body can waste; over the cap the hash is discarded.
const MAX_ARCHIVE: u64 = 2 * 1024 * 1024 * 1024;
/// Smaller than any real JDK zip; together with the magic check it keeps an
/// error page from being hashed and published as an archive.
const MIN_ARCHIVE: u64 = 1024 * 1024;
const ZIP_MAGIC: [u8; 4] = [b'P', b'K', 3, 4];

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    result: Vec<T>,
}

/// `packages` listing item — no checksum here by API design.
#[derive(Debug, Deserialize)]
struct Listing {
    id: String,
    java_version: String,
    #[serde(default)]
    term_of_support: Option<String>,
    #[serde(default)]
    release_status: Option<String>,
    /// Signed because foojay reports `-1` for unknown sizes (seen on zulu);
    /// negatives publish as 0 — the client only uses it as a progress hint.
    #[serde(default)]
    size: i64,
}

/// `ids/<id>` detail — the only endpoint with checksum data.
#[derive(Debug, Deserialize)]
struct Details {
    direct_download_uri: String,
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    checksum_type: String,
    #[serde(default)]
    checksum_uri: String,
}

/// A detail plus the sha256 the worker could resolve cheaply (inline or via
/// `checksum_uri`); `None` falls through to reuse/TOFU on the main thread.
/// `announced_sha1` rides along for the TOFU cross-check (liberica announces
/// only sha1).
struct Resolved {
    details: Details,
    sha256: Option<String>,
    announced_sha1: Option<String>,
}

/// One listing package on its way through dedup and checksum resolution.
struct Candidate {
    package: Package,
    cheap_sha256: Option<String>,
    announced_sha1: Option<String>,
}

/// One vendor+arch listing, resolved and filtered. `dropped` counts packages
/// discarded for missing sha256, unusable URL, unparseable version, budget
/// exhaustion or duplicate version — each already warned to stderr.
pub struct Fetched {
    pub packages: Vec<Package>,
    pub dropped: usize,
}

pub fn vendor_packages(
    http: &Http,
    base_url: &str,
    vendor: &str,
    arch: &str,
    jobs: usize,
    published: Option<&Published>,
    hash_budget: Option<u32>,
) -> Result<Fetched> {
    let url = packages_url(base_url, vendor, arch);
    let listing: Envelope<Listing> = fetch_json(http, &url)?;
    let items = listing.result;

    let resolved = fetch_details(http, base_url, &items, jobs)?;

    let mut dropped = 0;
    let mut keyed: Vec<(Version, Candidate)> = Vec::new();
    for (item, resolved) in items.iter().zip(resolved) {
        let label = format!("{vendor}@{} ({arch}, id {})", item.java_version, item.id);
        let Ok(version) = item.java_version.parse::<Version>() else {
            eprintln!("warning: dropping {label}: unparseable java_version");
            dropped += 1;
            continue;
        };
        if let Err(err) = usable_url(http.policy(), &resolved.details.direct_download_uri) {
            eprintln!("warning: dropping {label}: {err}");
            dropped += 1;
            continue;
        }
        keyed.push((
            version,
            Candidate {
                package: Package {
                    tool: "java".to_string(),
                    vendor: vendor.to_string(),
                    version: item.java_version.clone(),
                    os: "windows".to_string(),
                    arch: arch.to_string(),
                    release_status: match item.release_status.as_deref() {
                        Some("ea") => ReleaseStatus::Ea,
                        _ => ReleaseStatus::Ga,
                    },
                    lts: item.term_of_support.as_deref() == Some("lts"),
                    size: item.size.max(0) as u64,
                    // Filled below; packages that finish the resolution
                    // chain empty are dropped before anything is written.
                    sha256: String::new(),
                    url: resolved.details.direct_download_uri.clone(),
                },
                cheap_sha256: resolved.sha256,
                announced_sha1: resolved.announced_sha1,
            },
        ));
    }

    // Deterministic order — newest first, URL breaking ties — then one
    // package per version: the version is the key the client resolves by,
    // and duplicates (vendor repacks) would surface twice in `jdk available`.
    keyed
        .sort_by(|(va, ca), (vb, cb)| vb.cmp(va).then_with(|| ca.package.url.cmp(&cb.package.url)));
    let mut survivors: Vec<Candidate> = Vec::new();
    for (_, candidate) in keyed {
        if survivors
            .last()
            .is_some_and(|kept| kept.package.version == candidate.package.version)
        {
            eprintln!(
                "warning: dropping duplicate {vendor}@{} ({arch}): keeping the first of the sorted pair, dropped {}",
                candidate.package.version, candidate.package.url
            );
            dropped += 1;
            continue;
        }
        survivors.push(candidate);
    }

    // Reuse/TOFU only for survivors, newest first, so a tight budget spends
    // itself on the versions people actually install.
    let mut budget = hash_budget.unwrap_or(u32::MAX);
    let mut packages = Vec::with_capacity(survivors.len());
    for candidate in survivors {
        let Candidate {
            mut package,
            cheap_sha256,
            announced_sha1,
        } = candidate;
        let label = format!("{vendor}@{} ({arch})", package.version);
        let reused = || published.and_then(|p| p.sha256_by_url.get(&package.url).cloned());
        package.sha256 = if let Some(sha256) = cheap_sha256.or_else(reused) {
            sha256
        } else if budget == 0 {
            eprintln!(
                "warning: dropping {label}: no vendor sha256 and the hash budget is spent; a later run will backfill it"
            );
            dropped += 1;
            continue;
        } else {
            budget -= 1;
            match tofu_sha256(http, &package.url, announced_sha1.as_deref()) {
                Ok(sha256) => {
                    println!("hashed {label} on first sight ({})", package.url);
                    sha256
                }
                Err(err) => {
                    eprintln!("warning: dropping {label}: {err}");
                    dropped += 1;
                    continue;
                }
            }
        };
        packages.push(package);
    }

    Ok(Fetched { packages, dropped })
}

/// `ids/<id>` for every listing item, `jobs` workers over a shared cursor,
/// results funneled through a channel; the worker also resolves the cheap
/// checksum sources (inline value, `checksum_uri` file) while it is at it.
/// Any failed detail call fails the whole fetch.
fn fetch_details(
    http: &Http,
    base_url: &str,
    items: &[Listing],
    jobs: usize,
) -> Result<Vec<Resolved>> {
    let (sender, receiver) = mpsc::channel::<(usize, Result<Resolved>)>();
    let cursor = &AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, items.len().max(1)) {
            let sender = sender.clone();
            scope.spawn(move || {
                loop {
                    let at = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(at) else { break };
                    let result = fetch_detail(http, base_url, &item.id).map(|details| {
                        let sha256 = cheap_sha256(http, &details);
                        let announced_sha1 = announced_sha1(&details);
                        Resolved {
                            details,
                            sha256,
                            announced_sha1,
                        }
                    });
                    if sender.send((at, result)).is_err() {
                        break;
                    }
                }
            });
        }
    });
    drop(sender);

    let mut slots: Vec<Option<Resolved>> = Vec::new();
    slots.resize_with(items.len(), || None);
    for (at, result) in receiver {
        slots[at] = Some(result?);
    }
    Ok(slots
        .into_iter()
        .map(|slot| slot.expect("every listing item got a detail result"))
        .collect())
}

fn fetch_detail(http: &Http, base_url: &str, id: &str) -> Result<Details> {
    let url = format!("{}/ids/{id}", base_url.trim_end_matches('/'));
    let detail: Envelope<Details> = fetch_json(http, &url)?;
    detail
        .result
        .into_iter()
        .next()
        .ok_or_else(|| Error::Catalog(format!("foojay returned no details for package id {id}")))
}

/// Steps 1–2 of the chain: the inline sha256, else the `checksum_uri` file.
/// Best-effort — any miss falls through to reuse/TOFU.
fn cheap_sha256(http: &Http, details: &Details) -> Option<String> {
    if details.checksum_type.eq_ignore_ascii_case("sha256") {
        let inline = details.checksum.trim().to_ascii_lowercase();
        if is_hex_sha256(&inline) {
            return Some(inline);
        }
    }
    let uri = details.checksum_uri.trim();
    if uri.is_empty() || http.policy().check(uri).is_err() {
        return None;
    }
    let reply = http.get(uri, "jdk-index-gen", &[]).ok()?;
    if reply.status() != 200 {
        return None;
    }
    let body = reply.bytes(MAX_CHECKSUM_FILE).ok()?;
    parse_checksum_file(&body)
}

/// The sha1 foojay announces for the package, when that is all it has
/// (liberica) — the TOFU cross-check compares the streamed bytes against it.
fn announced_sha1(details: &Details) -> Option<String> {
    if !details.checksum_type.eq_ignore_ascii_case("sha1") {
        return None;
    }
    let sha1 = details.checksum.trim().to_ascii_lowercase();
    (sha1.len() == 40 && sha1.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sha1)
}

/// First 64-hex token of a vendor checksum file — covers both the bare-hash
/// shape (Oracle `.sha256`) and `sha256sum` output (Microsoft
/// `.sha256sum.txt`: `<hash> <filename>`).
fn parse_checksum_file(body: &[u8]) -> Option<String> {
    let text = str::from_utf8(body).ok()?;
    text.split_whitespace()
        .map(str::to_ascii_lowercase)
        .find(|token| is_hex_sha256(token))
}

/// Step 4: trust-on-first-use. Streams the archive once and hashes it; the
/// zip magic, a size floor and a size cap reject error pages and endless
/// bodies. When foojay announced a sha1 (liberica), the same stream is
/// cross-checked against it — a mismatch means the CDN served different
/// bytes than the catalog promised, and nothing gets published. The sha256
/// pins what the vendor served today — exactly what every later client
/// download is verified against.
fn tofu_sha256(http: &Http, url: &str, announced_sha1: Option<&str>) -> Result<String> {
    // Same allowlist the client enforces on install; `usable_url` already
    // vetted the URL, this is the belt to that suspender.
    check_trusted(url, http.policy())?;
    let reply = http.get_streaming(url, "jdk-index-gen", &[])?;
    if reply.status() != 200 {
        return Err(Error::Http(format!(
            "TOFU hash: GET {url} returned {}",
            reply.status()
        )));
    }
    let mut reader = reply.reader(MAX_ARCHIVE + 1);
    let mut sha256 = Sha256::new();
    let mut sha1 = announced_sha1.map(|_| Sha1::new());
    let mut buffer = vec![0u8; 128 * 1024];
    let mut first = [0u8; 4];
    let mut total = 0u64;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|err| Error::Http(format!("TOFU hash: reading {url}: {err}")))?;
        if n == 0 {
            break;
        }
        if total < 4 {
            let take = ((4 - total) as usize).min(n);
            first[total as usize..total as usize + take].copy_from_slice(&buffer[..take]);
        }
        total += n as u64;
        sha256.update(&buffer[..n]);
        if let Some(sha1) = &mut sha1 {
            sha1.update(&buffer[..n]);
        }
    }
    if total > MAX_ARCHIVE {
        return Err(Error::Security(format!(
            "TOFU hash: {url} exceeds the {} GiB archive cap; refusing to publish its hash",
            MAX_ARCHIVE / (1024 * 1024 * 1024)
        )));
    }
    if total < MIN_ARCHIVE || first != ZIP_MAGIC {
        return Err(Error::Security(format!(
            "TOFU hash: {url} does not look like a JDK zip ({total} bytes); refusing to publish its hash"
        )));
    }
    if let (Some(sha1), Some(announced)) = (sha1, announced_sha1) {
        let streamed = jdk_core::download::hex(&sha1.finalize());
        if streamed != announced {
            return Err(Error::Security(format!(
                "TOFU hash: {url} sha1 mismatch: foojay announced {announced}, the stream hashed to {streamed} — CDN tamper or stale catalog data; refusing to publish"
            )));
        }
    }
    Ok(jdk_core::download::hex(&sha256.finalize()))
}

/// GET `url`, erroring on any non-200, and read the body capped at `cap` bytes.
pub(crate) fn get_ok_bytes(http: &Http, url: &str, cap: u64) -> Result<Vec<u8>> {
    let reply = http.get(url, "jdk-index-gen", &[])?;
    if reply.status() != 200 {
        return Err(Error::Http(format!("GET {url} returned {}", reply.status())));
    }
    reply.bytes(cap)
}

fn fetch_json<T: serde::de::DeserializeOwned>(http: &Http, url: &str) -> Result<T> {
    serde_json::from_slice(&get_ok_bytes(http, url, MAX_BODY)?)
        .map_err(|err| Error::Catalog(format!("unparseable foojay response from {url}: {err}")))
}

/// A URL the index may publish: passes the run's policy (https-only on real
/// runs), is not hosted on foojay itself — `api.foojay.io` URLs are the
/// ephemeral `ids/<id>/redirect` family, which rot between generator runs —
/// and sits on the SAME vendor allowlist the client enforces on install, so
/// a compromised foojay cannot point the index at foreign hosts.
fn usable_url(policy: UrlPolicy, url: &str) -> Result<()> {
    policy.check(url)?;
    if url.contains("api.foojay.io") {
        return Err(Error::Catalog(format!(
            "not a direct vendor URL (ephemeral foojay link): {url}"
        )));
    }
    check_trusted(url, policy)?;
    // Oracle publishes both an immutable versioned path (.../java/25/archive/
    // jdk-25.0.2_...) and a mutable /latest/ alias whose bytes change on every
    // patch — a pinned sha256 over the latter rots on the next release. Drop
    // it (best-effort: the package is skipped, the index still publishes) so
    // only the immutable form is ever indexed.
    if is_mutable_oracle(url) {
        return Err(Error::Catalog(format!(
            "mutable Oracle URL (/latest/ rots on each patch); expected the versioned /archive/ path: {url}"
        )));
    }
    Ok(())
}

/// Oracle's `/latest/` alias on `download.oracle.com` — the mutable form the
/// index must never pin a hash to.
fn is_mutable_oracle(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("download.oracle.com") && lower.contains("/latest/")
}

pub(crate) fn is_hex_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The exact Windows foojay platform filters (GA-only) plus the generator's
/// `javafx_bundled=false` — see the module doc.
fn packages_url(base_url: &str, vendor: &str, arch: &str) -> String {
    let arch_param = match arch {
        "x64" => "amd64,x64",
        "aarch64" => "arm64,aarch64",
        other => other,
    };
    format!(
        "{}/packages?operating_system=windows&architecture={arch_param}&archive_type=zip&lib_c_type=c_std_lib&package_type=jdk&release_status=ga&javafx_bundled=false&distribution={vendor}",
        base_url.trim_end_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_ga_plus_the_fx_filter() {
        let url = packages_url("https://api.foojay.io/disco/v3.0", "temurin", "x64");
        assert_eq!(
            url,
            "https://api.foojay.io/disco/v3.0/packages?operating_system=windows&architecture=amd64,x64&archive_type=zip&lib_c_type=c_std_lib&package_type=jdk&release_status=ga&javafx_bundled=false&distribution=temurin"
        );
    }

    #[test]
    fn aarch64_query_uses_the_arm_alias() {
        let url = packages_url("https://x.example/", "zulu", "aarch64");
        assert!(url.contains("architecture=arm64,aarch64"), "{url}");
        assert!(url.contains("distribution=zulu"), "{url}");
    }

    #[test]
    fn foojay_hosted_urls_are_not_publishable() {
        let policy = UrlPolicy::Strict;
        assert!(usable_url(policy, "https://cdn.azul.com/x.zip").is_ok());
        assert!(
            usable_url(
                policy,
                "https://api.foojay.io/disco/v3.0/ids/abc123/redirect"
            )
            .is_err()
        );
        assert!(usable_url(policy, "http://cdn.azul.com/x.zip").is_err());
    }

    #[test]
    fn urls_off_the_vendor_allowlist_are_not_publishable() {
        let policy = UrlPolicy::Strict;
        let err = usable_url(policy, "https://evil.example/jdk.zip").unwrap_err();
        assert!(err.to_string().contains("evil.example"), "{err}");
        // Loopback fixtures stay usable under the hermetic policy.
        assert!(
            usable_url(
                UrlPolicy::AllowInsecureLoopback,
                "http://127.0.0.1:8137/archives/x.zip"
            )
            .is_ok()
        );
    }

    #[test]
    fn oracle_latest_alias_is_not_publishable() {
        let policy = UrlPolicy::Strict;
        // Immutable versioned path is fine.
        assert!(
            usable_url(
                policy,
                "https://download.oracle.com/java/25/archive/jdk-25.0.2_windows-x64_bin.zip"
            )
            .is_ok()
        );
        // The mutable /latest/ alias rots a pinned hash — dropped.
        let err = usable_url(
            policy,
            "https://download.oracle.com/java/25/latest/jdk-25_windows-x64_bin.zip",
        )
        .unwrap_err();
        assert!(err.to_string().contains("/latest/"), "{err}");
    }

    #[test]
    fn sha256_shape_is_enforced() {
        assert!(is_hex_sha256(&"a".repeat(64)));
        assert!(!is_hex_sha256(&"a".repeat(63)));
        assert!(!is_hex_sha256(&"g".repeat(64)));
        assert!(!is_hex_sha256(""));
    }

    #[test]
    fn checksum_files_parse_in_both_vendor_shapes() {
        let bare = "A".repeat(64);
        assert_eq!(
            parse_checksum_file(bare.as_bytes()).as_deref(),
            Some("a".repeat(64).as_str())
        );
        let sha256sum = format!("{}  microsoft-jdk.zip\n", "b".repeat(64));
        assert_eq!(
            parse_checksum_file(sha256sum.as_bytes()).as_deref(),
            Some("b".repeat(64).as_str())
        );
        assert_eq!(parse_checksum_file(b"not a hash"), None);
        assert_eq!(parse_checksum_file(b"<html>error</html>"), None);
    }
}
