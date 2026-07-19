//! Hermetic end-to-end tests: a loopback HTTP server serves an index fixture
//! and a fake-JDK zip; nothing touches the real network (the URL policy's
//! loopback mode is the injection point — no test-only switches exist in
//! production code). The acceptance test finishes with the M1 bridge: the
//! real shim executes the JDK that jdk-core just installed.

use jdk_core::Error;
use jdk_core::cache::Cache;
use jdk_core::catalog::Catalog;
use jdk_core::download::{MAX_ARCHIVE, fetch_archive, fetch_archive_capped, sha256_hex};
use jdk_core::http::{Http, Retry, UrlPolicy};
use jdk_core::index::{IndexEntry, IndexFile, current_platform};
use jdk_core::install::install;
use jdk_resolve::store;
use std::fs;
use std::io::Write as _;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use test_support::{
    Request, Response, Server, dead_url, fake_jdk_zip, package, serve_catalog, shim_binaries,
};

/// Loopback-permissive client with millisecond retries to keep tests fast.
fn http() -> Http {
    Http::with_retry(
        UrlPolicy::AllowInsecureLoopback,
        Retry {
            attempts: 3,
            base_delay: Duration::from_millis(1),
        },
    )
    .expect("build http client")
}

/// M2 acceptance: install a fake JDK from a local index, then the real shim
/// resolves a pin to it and propagates its exit code (M1 bridge).
#[test]
fn installs_a_fake_jdk_from_a_local_index_and_the_shim_runs_it() {
    let (shim, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let zip_sha = sha256_hex(&zip);
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("jdk root"); // spaces on purpose

    let server = Server::start();
    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/temurin.zip", server.url()),
        &zip_sha,
        zip.len() as u64,
    );
    serve_catalog(&server, std::slice::from_ref(&pkg));
    let body = zip.clone();
    server.route("/dl/temurin.zip", move |_| Response::ok(body.clone()));

    let http = http();
    let catalog = Catalog::with_urls(&root, server.url(), &dead_url());
    let found = catalog
        .find(&http, &"temurin@21".parse().unwrap(), "temurin")
        .unwrap();
    assert_eq!(found.version, "21.0.5+11");

    let mut events = Vec::new();
    let mut on_progress = |downloaded: u64, total: u64| events.push((downloaded, total));
    let installed = install(&root, &http, &found, Some(&mut on_progress)).unwrap();

    assert!(installed.fresh);
    assert_eq!(
        installed.dir.file_name().unwrap().to_str().unwrap(),
        "temurin@21.0.5+11"
    );
    assert!(installed.dir.join("bin").join("javac.exe").exists());
    assert!(installed.dir.join("release").exists());
    assert_eq!(
        *events.last().unwrap(),
        (zip.len() as u64, zip.len() as u64)
    );
    // The downloaded archive is cleaned up after a successful install.
    assert!(
        !store::cache(&root)
            .join("downloads")
            .join("temurin@21.0.5+11.zip")
            .exists()
    );

    // --- M1 bridge: byte-identical shim copy executes the installed JDK.
    let shims = temp.path().join("shims");
    let project = temp.path().join("proj");
    fs::create_dir_all(&shims).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::copy(&shim, shims.join("java.exe")).unwrap();
    fs::write(project.join(".sdkmanrc"), "java=21.0.5-tem\n").unwrap();

    let output = Command::new(shims.join("java.exe"))
        .args(["-version"])
        .current_dir(&project)
        .env("JDK_ROOT", &root)
        .env("FAKE_JAVA_EXIT", "7")
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("fake-java argv=[-version]"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn install_is_idempotent_and_downloads_once() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();

    let server = Server::start();
    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/t.zip", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    let body = zip.clone();
    server.route("/dl/t.zip", move |_| Response::ok(body.clone()));

    let http = http();
    let first = install(temp.path(), &http, &pkg, None).unwrap();
    let second = install(temp.path(), &http, &pkg, None).unwrap();

    assert!(first.fresh);
    assert!(!second.fresh);
    assert_eq!(second.dir, first.dir);
    assert_eq!(server.hits("/dl/t.zip"), 1);
}

#[test]
fn cache_serves_fresh_hits_and_revalidates_with_etag() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();
    server.route("/data.json", |request: &Request| {
        if request.header("if-none-match") == Some("\"v1\"") {
            Response::empty(304)
        } else {
            Response::ok(b"[1,2,3]".as_slice()).with_header("ETag", "\"v1\"")
        }
    });

    let http = http();
    // TTL zero: every read revalidates.
    let revalidating = Cache::with_ttl(temp.path(), Duration::ZERO, Duration::ZERO);
    let first = revalidating.get(&http, server.url(), "data.json").unwrap();
    let second = revalidating.get(&http, server.url(), "data.json").unwrap();
    assert_eq!(first, b"[1,2,3]");
    assert_eq!(second, first, "304 must serve the cached body");
    assert_eq!(server.hits("/data.json"), 2);
    let requests = server.requests_to("/data.json");
    assert_eq!(requests[0].header("if-none-match"), None);
    assert_eq!(requests[1].header("if-none-match"), Some("\"v1\""));

    // Default TTL: fresh hit, no network at all.
    let warm = Cache::new(temp.path());
    let third = warm.get(&http, server.url(), "data.json").unwrap();
    assert_eq!(third, first);
    assert_eq!(
        server.hits("/data.json"),
        2,
        "fresh hit must not touch the server"
    );
}

#[test]
fn stale_cache_survives_a_dead_server() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();
    server.route("/data.json", |_| Response::ok(b"payload".as_slice()));

    let http = http();
    let revalidating = Cache::with_ttl(temp.path(), Duration::ZERO, Duration::ZERO);
    revalidating.get(&http, server.url(), "data.json").unwrap();

    // Same root, but pointed at a server that no longer exists.
    let offline = revalidating.get(&http, &dead_url(), "data.json").unwrap();
    assert_eq!(offline, b"payload");
}

#[test]
fn invalidate_honors_the_refresh_grace_window() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();
    server.route("/g.json", |_| Response::ok(b"g".as_slice()));

    let http = http();
    let cache = Cache::with_ttl(
        temp.path(),
        Duration::from_secs(3600),
        Duration::from_secs(300),
    );
    cache.get(&http, server.url(), "g.json").unwrap();

    // Freshly fetched: a forced refresh may not throw it away (anti-thrash).
    assert!(!cache.invalidate("g.json").unwrap());
    let hit = cache.get(&http, server.url(), "g.json").unwrap();
    assert_eq!(hit, b"g");
    assert_eq!(server.hits("/g.json"), 1);

    // Without a grace window the same entry is dropped and refetched.
    let no_grace = Cache::with_ttl(temp.path(), Duration::from_secs(3600), Duration::ZERO);
    assert!(no_grace.invalidate("g.json").unwrap());
    no_grace.get(&http, server.url(), "g.json").unwrap();
    assert_eq!(server.hits("/g.json"), 2);
}

#[test]
fn wrong_archive_sha256_blocks_the_install() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();

    let server = Server::start();
    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/t.zip", server.url()),
        &"0".repeat(64),
        zip.len() as u64,
    );
    let body = zip.clone();
    server.route("/dl/t.zip", move |_| Response::ok(body.clone()));

    let err = install(temp.path(), &http(), &pkg, None).unwrap_err();

    assert!(matches!(err, Error::Checksum { .. }), "{err}");
    assert!(
        !store::java_candidates(temp.path())
            .join("temurin@21.0.5+11")
            .exists(),
        "nothing may reach the store"
    );
    let downloads = store::cache(temp.path()).join("downloads");
    assert!(
        !downloads.join("temurin@21.0.5+11.zip").exists()
            && !downloads.join("temurin@21.0.5+11.zip.part").exists(),
        "poisoned bytes must not be kept for resume"
    );
}

#[test]
fn missing_sha256_is_refused_before_any_io() {
    let temp = TempDir::new().unwrap();
    let pkg = package("21", "http://127.0.0.1:1/never-contacted.zip", "  ", 10);

    let err = fetch_archive(&http(), &pkg, &temp.path().join("x.zip"), None).unwrap_err();

    assert!(matches!(err, Error::Security(_)), "{err}");
    assert!(err.to_string().contains("no sha256"), "{err}");
}

#[test]
fn platform_file_diverging_from_index_sha_fails_after_one_refetch() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();
    let (os, arch) = current_platform();
    let platform_path = format!("{os}-{arch}/temurin.json");

    let advertised = serde_json::to_vec(&vec![package(
        "21",
        "https://adoptium.net/x.zip",
        &"a".repeat(64),
        1,
    )])
    .unwrap();
    let index = IndexFile {
        version: 1,
        updated: "2026-07-17T00:00:00Z".to_string(),
        files: vec![IndexEntry {
            path: platform_path.clone(),
            vendor: "temurin".to_string(),
            os: os.to_string(),
            arch: arch.to_string(),
            size: advertised.len() as u64,
            sha256: sha256_hex(&advertised),
        }],
    };
    let index_json = serde_json::to_vec(&index).unwrap();
    server.route("/index.json", move |_| Response::ok(index_json.clone()));
    // The served platform file never matches what the index promised.
    server.route(&format!("/{platform_path}"), |_| {
        Response::ok(b"[]".as_slice())
    });

    let catalog = Catalog::with_urls(temp.path(), server.url(), &dead_url());
    let err = catalog
        .vendor_packages(&http(), "temurin", os, arch)
        .unwrap_err();

    assert!(matches!(err, Error::Checksum { .. }), "{err}");
    assert_eq!(
        server.hits(&format!("/{platform_path}")),
        2,
        "one eviction + refetch heals skew; a second mismatch is fatal"
    );
}

#[test]
fn download_resumes_from_a_part_file() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let split = zip.len() / 2;
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("t.zip");
    fs::write(temp.path().join("t.zip.part"), &zip[..split]).unwrap();

    let server = Server::start();
    let body = zip.clone();
    server.route("/dl/t.zip", move |request: &Request| {
        match request.header("range") {
            Some(range) => {
                let start: usize = range
                    .strip_prefix("bytes=")
                    .and_then(|r| r.strip_suffix('-'))
                    .and_then(|n| n.parse().ok())
                    .expect("well-formed Range header");
                Response {
                    status: 206,
                    headers: vec![(
                        "Content-Range".to_string(),
                        format!("bytes {start}-{}/{}", body.len() - 1, body.len()),
                    )],
                    body: body[start..].to_vec(),
                    pace: None,
                }
            }
            None => Response::ok(body.clone()),
        }
    });

    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/t.zip", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    fetch_archive(&http(), &pkg, &dest, None).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), zip);
    assert!(!temp.path().join("t.zip.part").exists());
    let requests = server.requests_to("/dl/t.zip");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("range").unwrap(),
        format!("bytes={split}-")
    );
}

/// Regression trap for the download-timeout semantics (review CRITICAL): with
/// a short request budget, a strict GET — the OLD download behavior, whole
/// call bounded by `timeout_global`, which killed any real JDK slower than
/// ~52 Mbps at 30s — dies mid-body, while the streaming GET the download path
/// uses now finishes the same trickled body, because only DNS/connect/headers
/// are time-bounded.
#[test]
fn streaming_body_is_not_bounded_by_the_request_timeout() {
    let server = Server::start();
    let payload = vec![7u8; 256];
    let body = payload.clone();
    // 8 chunks of 32 bytes, 300ms apart: ~2.4s of body transfer.
    server.route("/slow.bin", move |_| {
        Response::ok(body.clone()).trickled(32, Duration::from_millis(300))
    });

    let http = Http::with_request_timeout(
        UrlPolicy::AllowInsecureLoopback,
        Retry {
            attempts: 1,
            base_delay: Duration::from_millis(1),
        },
        Duration::from_millis(800),
    )
    .unwrap();
    let url = format!("{}/slow.bin", server.url());

    let strict = http.get(&url, "test", &[]).unwrap().bytes(1024);
    assert!(
        strict.is_err(),
        "a strict GET must abort a body slower than the request timeout"
    );

    let streamed = http
        .get_streaming(&url, "test", &[])
        .unwrap()
        .bytes(1024)
        .unwrap();
    assert_eq!(streamed, payload);
}

/// A `.part` longer than the remote file makes the Range unsatisfiable (416):
/// the download discards it and restarts unranged.
#[test]
fn a_416_resume_response_restarts_from_scratch() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("t.zip");
    fs::write(temp.path().join("t.zip.part"), vec![0u8; zip.len() + 100]).unwrap();

    let server = Server::start();
    let body = zip.clone();
    server.route("/dl/t.zip", move |request: &Request| {
        if request.header("range").is_some() {
            Response::empty(416)
        } else {
            Response::ok(body.clone())
        }
    });

    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/t.zip", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    fetch_archive(&http(), &pkg, &dest, None).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), zip);
    let requests = server.requests_to("/dl/t.zip");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].header("range").is_some());
    assert!(
        requests[1].header("range").is_none(),
        "the retry after 416 must be a full, unranged download"
    );
}

#[test]
fn a_server_ignoring_range_restarts_the_download_cleanly() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("t.zip");
    // Garbage .part: if the client appended instead of restarting, the
    // checksum would fail.
    fs::write(
        temp.path().join("t.zip.part"),
        b"garbage that is not the zip",
    )
    .unwrap();

    let server = Server::start();
    let body = zip.clone();
    server.route("/dl/t.zip", move |_| Response::ok(body.clone()));

    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/t.zip", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    fetch_archive(&http(), &pkg, &dest, None).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), zip);
}

#[test]
fn vendor_headers_survive_redirects() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();

    let server = Server::start();
    server.route("/dl/first", |_| Response::redirect("/dl/second"));
    let body = zip.clone();
    server.route("/dl/second", move |_| Response::ok(body.clone()));

    let mut pkg = package(
        "21.0.5+11",
        &format!("{}/dl/first", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    pkg.vendor = "zulu".to_string();

    fetch_archive(&http(), &pkg, &temp.path().join("z.zip"), None).unwrap();

    for hop in ["/dl/first", "/dl/second"] {
        let requests = server.requests_to(hop);
        assert_eq!(requests.len(), 1, "{hop}");
        assert_eq!(
            requests[0].header("referer"),
            Some("http://www.azul.com/downloads/zulu/"),
            "the vendor header must reach {hop}"
        );
    }
}

#[test]
fn redirect_to_non_loopback_http_is_blocked() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();

    let server = Server::start();
    server.route("/dl/offsite", |_| {
        Response::redirect("http://example.com/x.zip")
    });

    let pkg = package(
        "21",
        &format!("{}/dl/offsite", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    let err = fetch_archive(&http(), &pkg, &temp.path().join("x.zip"), None).unwrap_err();

    // The second hop is rejected by policy before any connection is made.
    assert!(matches!(err, Error::Security(_)), "{err}");
    assert!(err.to_string().contains("https required"), "{err}");
}

#[test]
fn untrusted_download_hosts_are_refused_without_contact() {
    let temp = TempDir::new().unwrap();
    let pkg = package("21", "https://evil.example/jdk.zip", &"a".repeat(64), 10);

    let err = fetch_archive(&http(), &pkg, &temp.path().join("x.zip"), None).unwrap_err();

    assert!(matches!(err, Error::Security(_)), "{err}");
    assert!(err.to_string().contains("not a known JDK vendor"), "{err}");
}

#[test]
fn transient_gateway_failures_are_retried_and_hard_errors_are_not() {
    let server = Server::start();
    let flaky_calls = AtomicUsize::new(0);
    server.route("/flaky", move |_| {
        if flaky_calls.fetch_add(1, Ordering::SeqCst) < 2 {
            Response::empty(503)
        } else {
            Response::ok(b"finally".as_slice())
        }
    });
    server.route("/fatal", |_| Response::empty(500));

    let http = http();
    let reply = http
        .get(&format!("{}/flaky", server.url()), "test", &[])
        .unwrap();
    assert_eq!(reply.status(), 200);
    assert_eq!(server.hits("/flaky"), 3, "two 503s then success");

    let reply = http
        .get(&format!("{}/fatal", server.url()), "test", &[])
        .unwrap();
    assert_eq!(reply.status(), 500);
    assert_eq!(server.hits("/fatal"), 1, "500 is not retryable");
}

#[test]
fn foojay_fallback_when_the_index_is_unreachable() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();
    let (os, arch) = current_platform();

    let server = Server::start();
    let listing = format!(
        r#"{{"result":[{{"id":"abc123","java_version":"21.0.5+11","distribution":"temurin","term_of_support":"lts","release_status":"ga","size":{}}}]}}"#,
        zip.len()
    );
    server.route("/packages", move |_| Response::ok(listing.clone()));
    let details = format!(
        r#"{{"result":[{{"filename":"t.zip","direct_download_uri":"{}/dl/t.zip","checksum":"{}","checksum_type":"sha256"}}]}}"#,
        server.url(),
        sha256_hex(&zip)
    );
    server.route("/ids/abc123", move |_| Response::ok(details.clone()));
    let body = zip.clone();
    server.route("/dl/t.zip", move |_| Response::ok(body.clone()));

    let http = http();
    let catalog = Catalog::with_urls(temp.path(), &dead_url(), server.url());
    let found = catalog
        .find(&http, &"21".parse().unwrap(), "temurin")
        .unwrap();
    assert_eq!(found.version, "21.0.5+11");
    assert_eq!(found.sha256, sha256_hex(&zip));
    assert!(found.lts);

    // The listing request must carry the exact foojay query.
    let arch_alias = match arch {
        "x64" => "amd64,x64",
        "aarch64" => "arm64,aarch64",
        other => other,
    };
    let requests = server.requests_to("/packages");
    assert_eq!(
        requests[0].path,
        format!(
            "/packages?operating_system={os}&architecture={arch_alias}&archive_type=zip&lib_c_type=c_std_lib&package_type=jdk&release_status=ga&distribution=temurin"
        )
    );

    let installed = install(temp.path(), &http, &found, None).unwrap();
    assert!(installed.fresh);
    assert!(installed.dir.join("bin").join("java.exe").exists());
}

#[test]
fn foojay_without_sha256_is_refused() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();
    server.route("/packages", |_| {
        Response::ok(
            r#"{"result":[{"id":"abc","java_version":"21.0.5","distribution":"temurin","release_status":"ga","size":1}]}"#
                .as_bytes()
                .to_vec(),
        )
    });
    server.route("/ids/abc", |_| {
        Response::ok(
            r#"{"result":[{"filename":"t.zip","direct_download_uri":"https://adoptium.net/t.zip","checksum":"aabb","checksum_type":"md5"}]}"#
                .as_bytes()
                .to_vec(),
        )
    });

    let catalog = Catalog::with_urls(temp.path(), &dead_url(), server.url());
    let err = catalog
        .find(&http(), &"21".parse().unwrap(), "temurin")
        .unwrap_err();

    assert!(err.to_string().contains("no sha256"), "{err}");
}

/// A resumed download whose Content-Range declares a total over the REAL
/// `MAX_ARCHIVE` ceiling: the ceiling trips on the header alone, before the
/// body loop ever runs (the read loop is never entered, so the served bytes
/// are irrelevant), and the pre-existing `.part` is left exactly as it was —
/// nothing further is written to it. This one goes through the public
/// `fetch_archive` entry point (not the capped test seam below) to pin the
/// real production constant, not just the mechanism.
#[test]
fn declared_total_over_the_archive_ceiling_is_rejected_before_reading_any_body() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("t.zip");
    let part = temp.path().join("t.zip.part");
    let partial = zip[..8].to_vec();
    fs::write(&part, &partial).unwrap();

    let server = Server::start();
    let huge_total = MAX_ARCHIVE + 1;
    server.route("/dl/t.zip", move |request: &Request| {
        assert!(
            request.header("range").is_some(),
            "a non-empty .part must resume with a Range request"
        );
        Response {
            status: 206,
            headers: vec![(
                "Content-Range".to_string(),
                format!("bytes 8-15/{huge_total}"),
            )],
            body: vec![0u8; 8],
            pace: None,
        }
    });

    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/t.zip", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    let err = fetch_archive(&http(), &pkg, &dest, None).unwrap_err();

    assert!(matches!(err, Error::Security(_)), "{err}");
    assert!(err.to_string().contains("ceiling"), "{err}");
    assert!(!dest.exists(), "the archive must never be finalized");
    assert_eq!(
        fs::read(&part).unwrap(),
        partial,
        "the ceiling check must trip before a single additional byte is written"
    );
}

/// Same declared-size ceiling as above, but the FRESH (200, no resume) path,
/// now expressible without transferring gigabytes: `fetch_archive_capped`
/// takes a small `max_bytes`, and the test server's honest Content-Length
/// (it always reports the real body size) is already over it.
///
/// A quirk of today's implementation worth calling out precisely: `dest` is
/// never created, but the `.part` staging file IS opened (`File::create`)
/// before the declared-size check runs — that ordering is unchanged by this
/// pass (behavior-preserving refactor only), so an empty `.part` stub can be
/// left on disk even though the ceiling trips before a single body byte is
/// read. This test pins the precise, honest guarantee — no DATA byte ever
/// reaches disk — rather than the stronger "no `.part` file exists at all",
/// which the current code does not provide on this specific path.
#[test]
fn declared_total_over_a_capped_ceiling_on_a_fresh_download_is_rejected_before_reading_any_body() {
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("t.zip");
    let part = temp.path().join("t.zip.part");
    let cap = 1024u64;

    let server = Server::start();
    let oversized = vec![0xABu8; cap as usize + 200];
    server.route("/dl/fresh.zip", move |_| Response::ok(oversized.clone()));

    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/fresh.zip", server.url()),
        &"a".repeat(64), // never reached: rejected before hashing settles
        cap + 200,
    );
    let err = fetch_archive_capped(&http(), &pkg, &dest, None, cap).unwrap_err();

    assert!(matches!(err, Error::Security(_)), "{err}");
    assert!(err.to_string().contains("ceiling"), "{err}");
    assert!(!dest.exists(), "the archive must never be finalized");
    assert_eq!(
        part.metadata().map(|meta| meta.len()).unwrap_or(0),
        0,
        "no body byte may reach disk even though an empty .part stub is opened first"
    );
}

/// The OTHER ceiling: actual streamed bytes over `max_bytes` when the total
/// is unknown up front (no Content-Length). The pre-check passes trivially
/// (`total_size` returns 0 when the header is absent), so only the running
/// byte count inside the copy loop catches it — and unlike the declared-size
/// branch above, THIS branch explicitly deletes the `.part`.
///
/// `test_support::Server` always emits a truthful, computed Content-Length
/// for every response, so it cannot express "unknown length" — this test
/// talks to a minimal hand-rolled loopback listener instead, replying with no
/// Content-Length and relying on `Connection: close` (a real "close-
/// delimited" HTTP/1.1 message, RFC 9112 §6.3 case 7) to mark the body's end.
#[test]
fn streamed_bytes_over_a_capped_ceiling_are_rejected_and_the_part_is_removed() {
    use std::io::BufRead;
    use std::net::TcpListener;

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
        // write/flush error on the body is that expected early hangup, not a
        // server fault — it must not panic (and so must not fail the join).
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    }

    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("t.zip");
    let part = temp.path().join("t.zip.part");
    let cap = 1024u64;
    let oversized = vec![0xABu8; cap as usize + 200];

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let body = oversized.clone();
    let server = thread::spawn(move || serve_without_content_length(listener, body));

    let pkg = package(
        "21.0.5+11",
        &format!("http://{addr}/dl/big.zip"),
        &"a".repeat(64), // never reached: rejected before hashing settles
        cap + 200,
    );
    let err = fetch_archive_capped(&http(), &pkg, &dest, None, cap).unwrap_err();

    assert!(matches!(err, Error::Security(_)), "{err}");
    assert!(err.to_string().contains("ceiling"), "{err}");
    assert!(!dest.exists());
    assert!(
        !part.exists(),
        "the poisoned partial must be deleted, not left truncated on disk"
    );

    // Surface any panic from the server's required phases (accept/read/head)
    // as a test failure instead of silent stderr noise on a detached thread.
    server
        .join()
        .expect("server thread panicked before serving the response");
}

/// The cache's single lock must cover the WHOLE round trip — read, the HTTP
/// GET, and the write — not just the disk read. Two threads racing
/// `Cache::get` on the same entry, with the server made deliberately slow,
/// prove this: if the lock released before the network call (the regression
/// this guards), both threads would see an empty cache and both would hit
/// the server. With the real (correct) locking, the second caller blocks
/// until the first's write lands, then reads it back — exactly one fetch.
#[test]
fn cache_get_serializes_concurrent_fetches_across_the_http_round_trip() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();
    server.route("/data.json", |_| {
        thread::sleep(Duration::from_millis(200));
        Response::ok(b"[1,2,3]".as_slice())
    });

    let root = temp.path().to_path_buf();
    let url = server.url().to_string();
    let spawn_reader = || {
        let root = root.clone();
        let url = url.clone();
        thread::spawn(move || Cache::new(&root).get(&http(), &url, "data.json").unwrap())
    };
    let first = spawn_reader();
    let second = spawn_reader();

    let (body_a, body_b) = (first.join().unwrap(), second.join().unwrap());
    assert_eq!(body_a, b"[1,2,3]");
    assert_eq!(body_b, body_a);
    assert_eq!(
        server.hits("/data.json"),
        1,
        "the whole-call lock must serialize the second caller onto the first's write"
    );
}

/// An empty cache plus an unreachable server must propagate the raw HTTP
/// error (the `None => Err(err)` arm) — never a silent `Ok(vec![])` and never
/// a panic on the absent cache entry.
#[test]
fn get_on_an_empty_cache_with_an_unreachable_server_propagates_the_http_error() {
    let temp = TempDir::new().unwrap();
    let cache = Cache::new(temp.path());

    let err = cache.get(&http(), &dead_url(), "index.json").unwrap_err();

    assert!(matches!(err, Error::Http(_)), "{err}");
}

/// `checksum_type` says "sha256" but the checksum itself is empty or
/// whitespace-only: the OTHER side of `checksum_type != "sha256" ||
/// checksum.trim().is_empty()` than the existing md5 test covers.
#[test]
fn foojay_sha256_labeled_but_blank_checksum_is_refused() {
    // The JSON escape for a tab, not a raw control byte (illegal unescaped
    // inside a JSON string) — parses to the same whitespace-only checksum.
    for blank in ["", "   ", "\\t"] {
        let temp = TempDir::new().unwrap();
        let server = Server::start();
        server.route("/packages", |_| {
            Response::ok(
                r#"{"result":[{"id":"abc","java_version":"21.0.5","distribution":"temurin","release_status":"ga","size":1}]}"#
                    .as_bytes()
                    .to_vec(),
            )
        });
        let details = format!(
            r#"{{"result":[{{"filename":"t.zip","direct_download_uri":"https://adoptium.net/t.zip","checksum":"{blank}","checksum_type":"sha256"}}]}}"#
        );
        server.route("/ids/abc", move |_| {
            Response::ok(details.clone().into_bytes())
        });

        let catalog = Catalog::with_urls(temp.path(), &dead_url(), server.url());
        let err = catalog
            .find(&http(), &"21".parse().unwrap(), "temurin")
            .unwrap_err();

        assert!(err.to_string().contains("no sha256"), "{blank:?}: {err}");
    }
}

/// Two callers racing `install()` for the same `vendor@version` against the
/// same loopback index: only the lock winner downloads, the loser's post-lock
/// re-check finds the winner's candidate already in the store ("theirs
/// wins"), and both callers agree on the final directory.
#[test]
fn concurrent_installs_of_the_same_candidate_download_once_and_agree_on_the_winner() {
    let (_, fake_java) = shim_binaries();
    let zip = fake_jdk_zip(&fs::read(&fake_java).unwrap());
    let temp = TempDir::new().unwrap();

    let server = Server::start();
    let body = zip.clone();
    server.route("/dl/race.zip", move |_| {
        thread::sleep(Duration::from_millis(100));
        Response::ok(body.clone())
    });
    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/race.zip", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );

    let root = temp.path().to_path_buf();
    let spawn_install = || {
        let root = root.clone();
        let pkg = pkg.clone();
        thread::spawn(move || install(&root, &http(), &pkg, None).unwrap())
    };
    let first = spawn_install();
    let second = spawn_install();
    let (installed_a, installed_b) = (first.join().unwrap(), second.join().unwrap());

    assert_eq!(
        server.hits("/dl/race.zip"),
        1,
        "only the lock winner may download"
    );
    assert_eq!(installed_a.dir, installed_b.dir);
    assert_ne!(
        installed_a.fresh, installed_b.fresh,
        "exactly one caller observes a fresh install, the other the winner's candidate"
    );
    assert!(installed_a.dir.join("bin").join("javac.exe").exists());
}

/// A hostile zip (path traversal) served through the FULL `install()`
/// pipeline, not `extract_zip` directly: the security error must propagate
/// out of `install`, nothing may land in the final store, and the staging
/// directory must be cleaned up.
#[test]
fn hostile_zip_traversal_is_rejected_by_the_full_install_pipeline() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();

    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    writer
        .start_file("../evil.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"boom").unwrap();
    writer.finish().unwrap();
    let evil_zip = cursor.into_inner();

    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/evil.zip", server.url()),
        &sha256_hex(&evil_zip),
        evil_zip.len() as u64,
    );
    serve_catalog(&server, std::slice::from_ref(&pkg));
    let body = evil_zip.clone();
    server.route("/dl/evil.zip", move |_| Response::ok(body.clone()));

    let http = http();
    let catalog = Catalog::with_urls(temp.path(), server.url(), &dead_url());
    let found = catalog
        .find(&http, &"temurin@21".parse().unwrap(), "temurin")
        .unwrap();

    let err = install(temp.path(), &http, &found, None).unwrap_err();

    assert!(matches!(err, Error::Security(_)), "{err}");
    assert!(
        !store::java_candidates(temp.path())
            .join("temurin@21.0.5+11")
            .exists(),
        "nothing may reach the store"
    );
    let staging = store::cache(temp.path())
        .join("staging")
        .join("temurin@21.0.5+11");
    assert!(!staging.exists(), "staging must be cleaned up on failure");
}

/// The index is reachable but its `vendor_packages` list is empty for this
/// platform (as opposed to entirely missing the vendor, which errors) — the
/// `Ok(_) if list.is_empty()` arm of `Catalog::available` must still fall
/// through to the live foojay listing.
#[test]
fn available_falls_back_to_foojay_when_the_index_lists_nothing_for_the_platform() {
    let temp = TempDir::new().unwrap();
    let (os, arch) = current_platform();
    let server = Server::start();

    let empty_body = b"[]".to_vec();
    let platform_path = format!("{os}-{arch}/temurin.json");
    let index = IndexFile {
        version: 1,
        updated: "2026-07-17T00:00:00Z".to_string(),
        files: vec![IndexEntry {
            path: platform_path.clone(),
            vendor: "temurin".to_string(),
            os: os.to_string(),
            arch: arch.to_string(),
            size: empty_body.len() as u64,
            sha256: sha256_hex(&empty_body),
        }],
    };
    let index_json = serde_json::to_vec(&index).unwrap();
    server.route("/index.json", move |_| Response::ok(index_json.clone()));
    let platform_body = empty_body.clone();
    server.route(&format!("/{platform_path}"), move |_| {
        Response::ok(platform_body.clone())
    });
    server.route("/packages", |_| {
        Response::ok(
            r#"{"result":[{"id":"abc","java_version":"21.0.5+11","distribution":"temurin","term_of_support":"lts","release_status":"ga","size":1}]}"#
                .as_bytes()
                .to_vec(),
        )
    });

    let catalog = Catalog::with_urls(temp.path(), server.url(), server.url());
    let list = catalog.available(&http(), "temurin", os, arch).unwrap();

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].version, "21.0.5+11");
    assert_eq!(
        server.hits("/packages"),
        1,
        "an empty index listing must still be consulted through foojay"
    );
}

/// The index is reachable and answers, but simply has no version matching
/// the selector — distinct from an unreachable index, which fails at the
/// network level instead. The combined error must name that specific cause
/// ("index has no ...") and the foojay fallback must actually be consulted.
#[test]
fn index_reachable_but_no_matching_version_falls_through_to_foojay_with_a_combined_error() {
    let temp = TempDir::new().unwrap();
    let server = Server::start();

    let old = package("17.0.9", "https://adoptium.net/x.zip", &"a".repeat(64), 1);
    serve_catalog(&server, std::slice::from_ref(&old));
    server.route("/packages", |_| {
        Response::ok(r#"{"result":[]}"#.as_bytes().to_vec())
    });

    let catalog = Catalog::with_urls(temp.path(), server.url(), server.url());
    let err = catalog
        .find(&http(), &"temurin@99".parse().unwrap(), "temurin")
        .unwrap_err();

    assert!(err.to_string().contains("index has no"), "{err}");
    assert_eq!(
        server.hits("/packages"),
        1,
        "the index miss must still fall through to a foojay lookup"
    );
}
