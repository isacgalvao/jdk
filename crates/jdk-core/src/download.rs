//! Atomic archive download: resumable `.part` staging, sha256 hashed as the
//! bytes stream in (mismatch or absent checksum BLOCKS — never
//! warn-and-continue), size ceilings, vendor-scoped extra headers, and a
//! curated allowlist of vendor download hosts.

use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use crate::http::{Http, Reply, UrlPolicy, is_loopback, url_host};
use crate::index::Package;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

/// Hard ceiling for any archive (largest real JDKs are well under 1 GiB).
pub const MAX_ARCHIVE: u64 = 4 * 1024 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

/// Download progress callback: `(bytes_downloaded, total_bytes)`, total 0 when
/// unknown. Rendering (progress bars) is the CLI's job, not this crate's.
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// Downloads `package` to `dest`, verified. Idempotent: an existing `dest`
/// whose sha256 already matches is reused without touching the network, and
/// an interrupted run leaves `<dest>.part` behind to resume from (`Range`).
pub fn fetch_archive(
    http: &Http,
    package: &Package,
    dest: &Path,
    mut progress: Option<Progress<'_>>,
) -> Result<()> {
    let expected = package.sha256.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return Err(Error::Security(format!(
            "package {}@{} carries no sha256; refusing an unverifiable download",
            package.vendor, package.version
        )));
    }
    check_trusted(&package.url, http.policy())?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(Error::io("create", parent))?;
    }

    // A complete, verified archive from a previous run is reused as-is.
    if dest.exists() {
        if sha256_file(dest)? == expected {
            return Ok(());
        }
        fs::remove_file(dest).map_err(Error::io("remove corrupt", dest))?;
    }

    let part = part_path(dest);
    let mut hasher = Sha256::new();
    let start = match fs::metadata(&part) {
        Ok(_) => hash_file_into(&mut hasher, &part)?,
        Err(_) => 0,
    };

    let mut headers = vendor_headers(&package.vendor);
    if start > 0 {
        headers.push(("Range", format!("bytes={start}-")));
    }

    // Streaming GET: connection phases are time-bounded, the body is not — a
    // large archive on a slow link takes what it takes; sha256 settles it.
    let reply = http.get_streaming(&package.url, "download", &headers)?;
    let status = reply.status();
    let (mut file, mut downloaded) = match status {
        206 if start > 0 => {
            let file = OpenOptions::new()
                .append(true)
                .open(&part)
                .map_err(Error::io("append to", &part))?;
            (file, start)
        }
        200 => {
            // Full body — either a fresh download or a server that ignored
            // our Range: restart from zero.
            hasher = Sha256::new();
            let file = File::create(&part).map_err(Error::io("create", &part))?;
            (file, 0)
        }
        // Our .part outgrew the remote file (changed upstream or corrupt):
        // discard it and start over; without a Range a 416 cannot repeat.
        416 if start > 0 => {
            let _ = fs::remove_file(&part);
            return fetch_archive(http, package, dest, progress);
        }
        _ => {
            return Err(Error::Http(format!(
                "download of {} returned {status}",
                package.url
            )));
        }
    };

    let total = total_size(&reply, downloaded);
    if total > MAX_ARCHIVE {
        return Err(Error::Security(format!(
            "{} declares {total} bytes, over the {MAX_ARCHIVE}-byte ceiling",
            package.url
        )));
    }
    if let Some(report) = progress.as_deref_mut() {
        report(downloaded, total);
    }

    let mut reader = reply.reader(MAX_ARCHIVE.saturating_add(1));
    let mut buffer = vec![0u8; CHUNK];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buffer[..n]);
                file.write_all(&buffer[..n])
                    .map_err(Error::io("write", &part))?;
                downloaded += n as u64;
                if downloaded > MAX_ARCHIVE {
                    drop(file);
                    let _ = fs::remove_file(&part);
                    return Err(Error::Security(format!(
                        "{} exceeded the {MAX_ARCHIVE}-byte ceiling",
                        package.url
                    )));
                }
                if let Some(report) = progress.as_deref_mut() {
                    report(downloaded, total);
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                // Keep the partial file: the next run resumes from it.
                return Err(Error::Http(format!(
                    "download of {} interrupted: {err}",
                    package.url
                )));
            }
        }
    }
    file.flush().map_err(Error::io("flush", &part))?;
    drop(file);

    let actual = hex(&hasher.finalize());
    if actual != expected {
        // Poisoned bytes are not resumable.
        let _ = fs::remove_file(&part);
        return Err(Error::Checksum {
            subject: package.url.clone(),
            expected,
            actual,
        });
    }
    atomic_rename(&part, dest).map_err(Error::io("finalize", dest))?;
    Ok(())
}

/// `<file>.part` sibling used as the resumable staging name.
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Total size: Content-Range (`bytes N-M/total`) when resuming, else
/// Content-Length plus what is already on disk; 0 when unknown.
fn total_size(reply: &Reply, already: u64) -> u64 {
    if let Some(range) = reply.header("content-range")
        && let Some((_, total)) = range.rsplit_once('/')
        && let Ok(total) = total.trim().parse()
    {
        return total;
    }
    match reply.header("content-length").and_then(|v| v.parse().ok()) {
        Some(length @ 1u64..) => already + length,
        _ => 0,
    }
}

/// Archives may only come from known vendor hosts, or from loopback when the
/// policy allows it (hermetic tests). Matching is prefix + boundary —
/// `https://adoptium.net.evil.example` does not pass, which plain starts_with
/// would allow — with scheme and host compared case-insensitively (paths stay
/// case-sensitive: fail-closed).
///
/// BY DESIGN this allowlist gates only the INITIAL download URL, not redirect
/// hops: vendor release CDNs (GitHub releases, objects.githubusercontent.com
/// and friends) legitimately redirect to hosts no curated list can track.
/// After the first hop, the load-bearing integrity control is the MANDATORY
/// sha256 over the delivered bytes; the HTTPS-only policy still vets every
/// hop (`Http::get_streaming`).
const TRUSTED: &[&str] = &[
    "https://api.foojay.io",
    "https://adoptium.net",
    "https://download.eclipse.org",
    "https://github.com/adoptium",
    "https://cdn.azul.com",
    "https://corretto.aws",
    "https://download.bell-sw.com",
    "https://github.com/bell-sw",
    "https://download.oracle.com",
    "https://download.java.net",
    "https://download.graalvm.org",
    "https://github.com/graalvm",
    "https://builds.openlogic.com",
    "https://github.com/SAP",
    "https://github.com/SapMachine",
    "https://github.com/dragonwell-project",
    "https://aka.ms",
    "https://download.microsoft.com",
];

/// Public for the index generator: index and client must apply the SAME
/// vendor allowlist, so a compromised catalog source cannot smuggle foreign
/// hosts into the published index in the first place.
pub fn check_trusted(url: &str, policy: UrlPolicy) -> Result<()> {
    let bounded = |rest: &str| rest.is_empty() || rest.starts_with(['/', ':', '?']);
    let normalized = lowercase_origin(url);
    if TRUSTED
        .iter()
        .any(|prefix| normalized.strip_prefix(prefix).is_some_and(&bounded))
    {
        return Ok(());
    }
    if policy == UrlPolicy::AllowInsecureLoopback && is_loopback(url_host(url)) {
        return Ok(());
    }
    Err(Error::Security(format!(
        "download host is not a known JDK vendor: {url}"
    )))
}

/// Scheme and authority lowercased (hosts are case-insensitive), path and
/// query left untouched (they are not).
fn lowercase_origin(url: &str) -> String {
    let authority_end = url.find("://").map_or(0, |at| {
        let after = at + 3;
        after
            + url[after..]
                .find(['/', '?', '#'])
                .unwrap_or(url.len() - after)
    });
    let mut normalized = url[..authority_end].to_ascii_lowercase();
    normalized.push_str(&url[authority_end..]);
    normalized
}

/// Extra request headers some vendors require, keyed by the INDEX vendor id —
/// never by URL substring (URLs are attacker-influenced, the vendor field is
/// not). `Http::get_streaming` re-sends these on every redirect hop.
fn vendor_headers(vendor: &str) -> Vec<(&'static str, String)> {
    match vendor {
        "zulu" => vec![("Referer", "http://www.azul.com/downloads/zulu/".to_string())],
        "oracle" | "oracle_open_jdk" => vec![(
            "Cookie",
            "oraclelicense=accept-securebackup-cookie".to_string(),
        )],
        _ => Vec::new(),
    }
}

/// Lowercase hex sha256 of an in-memory buffer (index platform files).
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lowercase hex sha256 of a file, streamed in 8 KiB chunks.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_file_into(&mut hasher, path)?;
    Ok(hex(&hasher.finalize()))
}

/// Feeds a whole file into `hasher`, returning how many bytes were hashed.
fn hash_file_into(hasher: &mut Sha256, path: &Path) -> Result<u64> {
    let mut file = File::open(path).map_err(Error::io("open", path))?;
    let mut buffer = vec![0u8; 8192];
    let mut hashed = 0u64;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(hashed),
            Ok(n) => {
                hasher.update(&buffer[..n]);
                hashed += n as u64;
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(Error::io("read", path)(err)),
        }
    }
}

fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sha256_matches_a_known_vector() {
        // sha256("Hello, World!") — the canonical test vector.
        assert_eq!(
            sha256_hex(b"Hello, World!"),
            "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f"
        );
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("hello");
        fs::write(&file, b"Hello, World!").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f"
        );
    }

    #[test]
    fn trusted_hosts_are_boundary_matched() {
        let strict = UrlPolicy::Strict;
        assert!(check_trusted("https://adoptium.net/x.zip", strict).is_ok());
        assert!(check_trusted("https://aka.ms/download-jdk/x.zip", strict).is_ok());
        assert!(
            check_trusted(
                "https://github.com/adoptium/temurin21-binaries/releases/x.zip",
                strict
            )
            .is_ok()
        );
        assert!(check_trusted("https://cdn.azul.com:443/zulu/x.zip", strict).is_ok());

        assert!(check_trusted("https://adoptium.net.evil.example/x.zip", strict).is_err());
        assert!(check_trusted("https://github.com/evil/x.zip", strict).is_err());
        assert!(check_trusted("https://example.com/x.zip", strict).is_err());
        assert!(check_trusted("http://adoptium.net/x.zip", strict).is_err());
    }

    #[test]
    fn trusted_hosts_match_case_insensitively_but_paths_do_not() {
        let strict = UrlPolicy::Strict;
        assert!(check_trusted("HTTPS://ADOPTIUM.NET/x.zip", strict).is_ok());
        assert!(check_trusted("https://GitHub.com/adoptium/x.zip", strict).is_ok());
        // Path case stays significant: unknown spelling fails closed.
        assert!(check_trusted("https://github.com/ADOPTIUM/x.zip", strict).is_err());
    }

    #[test]
    fn loopback_is_trusted_only_under_the_loopback_policy() {
        let url = "http://127.0.0.1:8080/fake.zip";
        assert!(check_trusted(url, UrlPolicy::AllowInsecureLoopback).is_ok());
        assert!(check_trusted(url, UrlPolicy::Strict).is_err());
    }

    #[test]
    fn vendor_headers_come_from_the_vendor_id() {
        assert_eq!(
            vendor_headers("zulu"),
            vec![("Referer", "http://www.azul.com/downloads/zulu/".to_string())]
        );
        assert_eq!(vendor_headers("oracle")[0].0, "Cookie");
        assert!(vendor_headers("temurin").is_empty());
        // URL substrings must play no role; only the vendor id decides.
        assert!(vendor_headers("not-zulu-either").is_empty());
    }

    #[test]
    fn part_path_appends_the_suffix() {
        assert_eq!(
            part_path(Path::new("C:\\x\\temurin@21.zip")),
            Path::new("C:\\x\\temurin@21.zip.part")
        );
    }
}
