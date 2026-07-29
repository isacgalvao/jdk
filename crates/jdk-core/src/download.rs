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
    progress: Option<Progress<'_>>,
) -> Result<()> {
    fetch_archive_capped(http, package, dest, progress, MAX_ARCHIVE)
}

/// `fetch_archive` with the archive-size ceiling injected. PRIVATE, and that
/// is the point: [`fetch_archive`] is the only way into this code from
/// outside, so [`MAX_ARCHIVE`] is an invariant of the crate rather than a
/// convention every caller has to keep. The parameter survives for the one
/// ceiling this file's own tests cannot reach any other way — the running
/// byte count on a body of unknown length, which at 4 GiB is not a test
/// anyone can run. The declared-size ceiling needs no seam: a server that
/// lies about Content-Length pins it through [`fetch_archive`] itself.
fn fetch_archive_capped(
    http: &Http,
    package: &Package,
    dest: &Path,
    mut progress: Option<Progress<'_>>,
    max_bytes: u64,
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
            return fetch_archive_capped(http, package, dest, progress, max_bytes);
        }
        _ => {
            return Err(Error::Http(format!(
                "download of {} returned {status}",
                package.url
            )));
        }
    };

    let total = total_size(&reply, downloaded);
    if total > max_bytes {
        return Err(Error::Security(format!(
            "{} declares {total} bytes, over the {max_bytes}-byte ceiling",
            package.url
        )));
    }
    if let Some(report) = progress.as_deref_mut() {
        report(downloaded, total);
    }

    let mut reader = reply.reader(max_bytes.saturating_add(1));
    let mut buffer = vec![0u8; CHUNK];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buffer[..n]);
                file.write_all(&buffer[..n])
                    .map_err(Error::io("write", &part))?;
                downloaded += n as u64;
                if downloaded > max_bytes {
                    drop(file);
                    let _ = fs::remove_file(&part);
                    return Err(Error::Security(format!(
                        "{} exceeded the {max_bytes}-byte ceiling",
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

/// The cookie by which Oracle records that the downloader accepted its license
/// terms. Not a transport detail: sending it IS the act of accepting, which is
/// why it is derived from [`license_notice`] rather than listed independently.
const ACCEPTANCE_COOKIE: &str = "oraclelicense=accept-securebackup-cookie";

/// Extra request headers some vendors require, keyed by the INDEX vendor id —
/// never by URL substring (URLs are attacker-influenced, the vendor field is
/// not). `Http::get_streaming` re-sends these on every redirect hop.
///
/// The acceptance cookie rides exactly on the vendors [`license_notice`] calls
/// proprietary, because that notice is what the CLI gates the download behind:
/// the two used to be independent lists and had drifted apart in BOTH
/// directions — `oracle_open_jdk` (the GPL build, no notice, so no prompt) was
/// sent the cookie anyway, declaring an acceptance nobody was asked for, while
/// `graalvm` was prompted for the GFTC and then downloaded without the cookie
/// that records the answer. Deriving one from the other makes "the cookie only
/// ever leaves after consent" a property of the code, not of two lists staying
/// in sync; `the_acceptance_cookie_rides_with_the_license_gate` fails if they
/// are ever split again.
fn vendor_headers(vendor: &str) -> Vec<(&'static str, String)> {
    let mut headers = match vendor {
        "zulu" => vec![("Referer", "http://www.azul.com/downloads/zulu/".to_string())],
        _ => Vec::new(),
    };
    if license_notice(vendor).is_some() {
        headers.push(("Cookie", ACCEPTANCE_COOKIE.to_string()));
    }
    headers
}

/// One-line license notice for vendors under proprietary terms — the only
/// non-open-source distributions in the catalog. Keyed by the INDEX vendor id,
/// like [`vendor_headers`]; the CLI prints it and takes the user's answer
/// before any download happens. `None` for the open-source vendors, which need
/// no notice and, by [`vendor_headers`], send no acceptance cookie.
pub fn license_notice(vendor: &str) -> Option<&'static str> {
    match vendor {
        "oracle" => Some(
            "Oracle JDK is under the Oracle No-Fee Terms and Conditions (NFTC); \
             older releases may revert to the more restrictive OTN license — \
             https://www.oracle.com/downloads/licenses/no-fee-license.html",
        ),
        "graalvm" => Some(
            "Oracle GraalVM is under the GraalVM Free Terms and Conditions (GFTC) — \
             https://www.oracle.com/downloads/licenses/graal-free-license.html",
        ),
        // `oracle_open_jdk` is deliberately absent: those builds are Oracle's
        // GPLv2+CPE OpenJDK, off the OTN gate the cookie was ever about, so
        // neither a notice nor the cookie applies. Should some Oracle host
        // turn out to demand it, the archive comes back as an HTML license
        // page and the mandatory sha256 rejects it loudly — a visible failure,
        // not a silent one, and never a bad JDK on disk.
        _ => None,
    }
}

/// Lowercase hex sha256 of an in-memory buffer (index platform files).
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lowercase hex sha256 of a file, streamed in 8 KiB chunks.
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
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

/// Lowercase hex of a digest. Shared with jdk-index-gen, which hex-encodes
/// its streaming sha1/sha256 finalize output the same way.
pub fn hex(digest: &[u8]) -> String {
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
    fn license_notice_only_for_proprietary_vendors() {
        assert!(license_notice("oracle").unwrap().contains("NFTC"));
        assert!(license_notice("graalvm").unwrap().contains("GFTC"));
        // Open-source vendors get no notice.
        assert!(license_notice("temurin").is_none());
        assert!(license_notice("oracle_open_jdk").is_none());
    }

    /// The invariant behind the whole consent gate: the cookie that declares
    /// acceptance goes out for a vendor exactly when the CLI stopped to ask
    /// about that vendor's terms. Either half drifting — a cookie with no
    /// notice, or a notice with no cookie — fails here.
    #[test]
    fn the_acceptance_cookie_rides_with_the_license_gate() {
        let cookie = |vendor: &str| {
            vendor_headers(vendor)
                .into_iter()
                .find_map(|(name, value)| (name == "Cookie").then_some(value))
        };
        // Index vendors plus the foojay-only ids a selector can still name:
        // `jdk install oracle_open_jdk@21` goes straight to the live fallback.
        for vendor in [
            "temurin",
            "zulu",
            "corretto",
            "liberica",
            "microsoft",
            "graalvm",
            "graalvm_community",
            "oracle",
            "oracle_open_jdk",
            "semeru",
        ] {
            assert_eq!(
                cookie(vendor).as_deref(),
                license_notice(vendor).map(|_| ACCEPTANCE_COOKIE),
                "{vendor}: acceptance cookie and license notice disagree"
            );
        }
        // Spelled out for the three that used to disagree.
        assert_eq!(cookie("oracle").as_deref(), Some(ACCEPTANCE_COOKIE));
        assert_eq!(cookie("graalvm").as_deref(), Some(ACCEPTANCE_COOKIE));
        assert_eq!(cookie("oracle_open_jdk"), None);
    }

    #[test]
    fn part_path_appends_the_suffix() {
        assert_eq!(
            part_path(Path::new("C:\\x\\temurin@21.zip")),
            Path::new("C:\\x\\temurin@21.zip.part")
        );
    }

    /// The ceiling that only the running byte count can catch: a body of
    /// unknown length (no Content-Length, so `total_size` returns 0 and the
    /// declared-size pre-check passes trivially) that outgrows `max_bytes`
    /// mid-stream. Unlike the declared-size branch — pinned end to end
    /// against the real `MAX_ARCHIVE` in `hermetic.rs` — this one cannot be
    /// reached without actually transferring past the ceiling, so it is
    /// tested HERE, where `fetch_archive_capped` is in scope and the ceiling
    /// can be 1 KiB. THIS branch, and only this one, deletes the `.part`.
    ///
    /// The listener is hand-rolled because a server that reports no length
    /// at all is not something `test_support::Server` can express: it ends
    /// the body with `Connection: close` instead (a real close-delimited
    /// HTTP/1.1 message, RFC 9112 §6.3 case 7).
    #[test]
    fn streamed_bytes_over_the_ceiling_are_rejected_and_the_part_is_removed() {
        use crate::http::{Http, Retry, UrlPolicy};
        use crate::index::ReleaseStatus;
        use std::io::BufRead;
        use std::net::TcpListener;
        use std::thread;
        use std::time::Duration;

        fn serve_without_content_length(listener: TcpListener, body: Vec<u8>) {
            let (stream, _) = listener.accept().expect("accept one connection");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone stream"));
            let mut stream = reader.get_ref().try_clone().expect("clone stream");
            let mut line = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).expect("read request line");
                if line.trim().is_empty() {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .expect("write response head");
            // The client legitimately hangs up once it hits the ceiling, so a
            // write/flush error on the body is that expected early hangup,
            // not a server fault — it must not panic (and so must not fail
            // the join).
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }

        let temp = TempDir::new().unwrap();
        let dest = temp.path().join("t.zip");
        let part = temp.path().join("t.zip.part");
        let cap = 1024u64;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let server = thread::spawn(move || {
            serve_without_content_length(listener, vec![0xABu8; cap as usize + 200])
        });

        let (os, arch) = crate::index::current_platform();
        let package = Package {
            tool: "java".to_string(),
            vendor: "temurin".to_string(),
            version: "21.0.5+11".to_string(),
            os: os.to_string(),
            arch: arch.to_string(),
            release_status: ReleaseStatus::Ga,
            lts: true,
            size: cap + 200,
            sha256: "a".repeat(64), // never reached: rejected before hashing settles
            url: format!("http://{addr}/dl/big.zip"),
        };
        let http = Http::with_retry(
            UrlPolicy::AllowInsecureLoopback,
            Retry {
                attempts: 1,
                base_delay: Duration::from_millis(1),
            },
        )
        .expect("build http client");
        let err = fetch_archive_capped(&http, &package, &dest, None, cap).unwrap_err();

        assert!(matches!(err, Error::Security(_)), "{err}");
        assert!(err.to_string().contains("ceiling"), "{err}");
        assert!(!dest.exists());
        assert!(
            !part.exists(),
            "the poisoned partial must be deleted, not left truncated on disk"
        );

        // Surface any panic from the server's required phases (accept/read/
        // head) as a test failure instead of silent stderr noise on a
        // detached thread.
        server
            .join()
            .expect("server thread panicked before serving the response");
    }
}
