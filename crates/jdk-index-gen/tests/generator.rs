//! Hermetic generator tests: a loopback fake of the foojay Disco API
//! (listing + `ids/<id>` details) drives the real binary end to end — tree
//! shape, mandatory sha256, deterministic bytes, shrink guard. No network.

use jdk_core::index::{IndexFile, Package, ReleaseStatus};
use sha1::Digest;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use test_support::{Response, Server};

const UPDATED: &str = "2026-07-17T00:00:00Z";
const VENDORS: [&str; 6] = [
    "temurin",
    "zulu",
    "corretto",
    "liberica",
    "graalvm",
    "microsoft",
];

/// One fake foojay package: listing fields plus the `ids/<id>` detail.
struct Fake {
    vendor: &'static str,
    arch: &'static str,
    id: String,
    version: String,
    lts: bool,
    ea: bool,
    checksum_type: &'static str,
    /// `false` serves an empty inline checksum (the microsoft/corretto
    /// shape) so the generator has to walk the rest of the chain.
    inline: bool,
    /// Overrides the served inline checksum (the sha1 cross-check tests
    /// need a value that really matches — or really mismatches — a stream).
    checksum_override: Option<String>,
    checksum_uri: String,
    url: String,
}

impl Fake {
    fn ga(vendor: &'static str, arch: &'static str, version: &str) -> Fake {
        Fake {
            vendor,
            arch,
            id: format!("{vendor}-{arch}-{version}"),
            version: version.to_string(),
            lts: false,
            ea: false,
            checksum_type: "sha256",
            inline: true,
            checksum_override: None,
            checksum_uri: String::new(),
            // A host on the client's vendor allowlist — the generator now
            // refuses to publish anything off it.
            url: format!("https://cdn.azul.com/fake/{vendor}-{version}-{arch}.zip"),
        }
    }

    fn lts(mut self) -> Fake {
        self.lts = true;
        self
    }

    fn ea(mut self) -> Fake {
        self.ea = true;
        self
    }

    fn checksum_type(mut self, kind: &'static str) -> Fake {
        self.checksum_type = kind;
        self
    }

    fn checksum_uri(mut self, uri: &str) -> Fake {
        self.checksum_uri = uri.to_string();
        self
    }

    fn no_inline(mut self) -> Fake {
        self.inline = false;
        self
    }

    fn announce(mut self, checksum: &str) -> Fake {
        self.checksum_override = Some(checksum.to_string());
        self
    }

    fn url(mut self, url: &str) -> Fake {
        self.url = url.to_string();
        self
    }

    /// Deterministic fake digest — uppercase on purpose: the generator must
    /// publish it lowercased.
    fn checksum(&self) -> String {
        let mut digest = format!("{:X}", self.id.len() as u64 + 0xABCD).repeat(20);
        digest.truncate(64);
        digest
    }

    /// What the `ids/<id>` route puts in `checksum`: empty for the
    /// no-inline shape, any override, 40 hex for sha1, 64 hex otherwise.
    fn served_checksum(&self) -> String {
        if !self.inline {
            return String::new();
        }
        if let Some(checksum) = &self.checksum_override {
            return checksum.clone();
        }
        match self.checksum_type {
            "sha1" => self.checksum()[..40].to_string(),
            _ => self.checksum(),
        }
    }
}

/// Serves `packages` as a fake foojay: one `/packages` route dispatching on
/// the query string (distribution + architecture), one `/ids/<id>` route per
/// package. Every one of the six vendors answers (empty when absent from
/// `packages`) so a run can pass the required-vendor floor.
fn serve_foojay(server: &Server, packages: Vec<Fake>) {
    for fake in &packages {
        let body = format!(
            r#"{{"result":[{{"filename":"jdk.zip","direct_download_uri":"{}","checksum":"{}","checksum_type":"{}","checksum_uri":"{}"}}]}}"#,
            fake.url,
            fake.served_checksum(),
            fake.checksum_type,
            fake.checksum_uri,
        );
        server.route(&format!("/ids/{}", fake.id), move |_| {
            Response::ok(body.clone())
        });
    }

    server.route("/packages", move |request| {
        let query = &request.path;
        let arch = if query.contains("architecture=arm64,aarch64") {
            "aarch64"
        } else {
            "x64"
        };
        let items: Vec<String> = packages
            .iter()
            .filter(|fake| {
                fake.arch == arch && query.contains(&format!("distribution={}", fake.vendor))
            })
            .map(|fake| {
                format!(
                    r#"{{"id":"{}","java_version":"{}","term_of_support":"{}","release_status":"{}","size":1000}}"#,
                    fake.id,
                    fake.version,
                    if fake.lts { "lts" } else { "sts" },
                    if fake.ea { "ea" } else { "ga" },
                )
            })
            .collect();
        Response::ok(format!(r#"{{"result":[{}]}}"#, items.join(",")))
    });
}

/// One GA package per required vendor — the smallest fixture that validates.
fn baseline() -> Vec<Fake> {
    VENDORS
        .iter()
        .map(|vendor| Fake::ga(vendor, "x64", "21.0.5+11").lts())
        .collect()
}

fn generate(server: &Server, out: &Path, extra_args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jdk-index-gen"));
    command
        .arg("--out")
        .arg(out)
        .arg("--foojay")
        .arg(server.url());
    if !extra_args.contains(&"--updated") {
        command.args(["--updated", UPDATED]);
    }
    if !extra_args.contains(&"--compare-to") {
        command.args(["--compare-to", "none"]);
    }
    command.args(extra_args);
    command.output().expect("run jdk-index-gen")
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "generator failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn read_index(dir: &Path) -> IndexFile {
    IndexFile::parse(&fs::read(dir.join("index.json")).unwrap()).unwrap()
}

fn read_packages(dir: &Path, relpath: &str) -> Vec<Package> {
    let path = dir.join(relpath.replace('/', std::path::MAIN_SEPARATOR_STR));
    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap()
}

#[test]
fn generates_the_contract_tree() {
    let server = Server::start();
    let mut packages = baseline();
    packages.push(Fake::ga("temurin", "x64", "24.0.1+9"));
    packages.push(Fake::ga("temurin", "x64", "25-ea+3").ea());
    packages.push(Fake::ga("zulu", "aarch64", "21.0.5+11").lts());
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &[]);
    assert_ok(&output);

    let index = read_index(out.path());
    assert_eq!(index.version, 1);
    assert_eq!(index.updated, UPDATED);

    // 6 x64 files + zulu's best-effort aarch64, sorted by path, sha256/size
    // matching the bytes on disk.
    let paths: Vec<&str> = index.files.iter().map(|e| e.path.as_str()).collect();
    let mut expected: Vec<String> = VENDORS
        .iter()
        .map(|v| format!("windows-x64/{v}.json"))
        .collect();
    expected.push("windows-aarch64/zulu.json".to_string());
    expected.sort();
    assert_eq!(paths, expected);
    for entry in &index.files {
        let body = fs::read(
            out.path()
                .join(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        )
        .unwrap();
        assert_eq!(entry.size, body.len() as u64, "{}", entry.path);
        assert_eq!(
            entry.sha256,
            test_support::sha256_hex(&body),
            "{}",
            entry.path
        );
    }

    // Newest first, EA/LTS flags carried, sha256 lowercased, direct URLs.
    let temurin = read_packages(out.path(), "windows-x64/temurin.json");
    let versions: Vec<&str> = temurin.iter().map(|p| p.version.as_str()).collect();
    assert_eq!(versions, ["25-ea+3", "24.0.1+9", "21.0.5+11"]);
    assert_eq!(temurin[0].release_status, ReleaseStatus::Ea);
    assert_eq!(temurin[1].release_status, ReleaseStatus::Ga);
    assert!(temurin[2].lts);
    for package in &temurin {
        assert_eq!(package.tool, "java");
        assert_eq!(package.os, "windows");
        assert_eq!(package.arch, "x64");
        assert_eq!(package.sha256, package.sha256.to_ascii_lowercase());
        assert!(
            package
                .url
                .starts_with("https://cdn.azul.com/fake/temurin-"),
            "{}",
            package.url
        );
    }

    let zulu_arm = read_packages(out.path(), "windows-aarch64/zulu.json");
    assert_eq!(zulu_arm.len(), 1);
    assert_eq!(zulu_arm[0].arch, "aarch64");
}

#[test]
fn drops_unverifiable_packages_with_warning() {
    let server = Server::start();
    let mut packages = baseline();
    // sha1-only (the liberica shape): without hash budget there is no route
    // to a sha256, so the package must be dropped, not published.
    packages.push(Fake::ga("temurin", "x64", "17.0.9+9").checksum_type("sha1"));
    packages.push(
        Fake::ga("temurin", "x64", "11.0.21+9")
            .url("https://api.foojay.io/disco/v3.0/ids/eph123/redirect"),
    );
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &["--hash-budget", "0"]);
    assert_ok(&output);

    let temurin = read_packages(out.path(), "windows-x64/temurin.json");
    let versions: Vec<&str> = temurin.iter().map(|p| p.version.as_str()).collect();
    assert_eq!(
        versions,
        ["21.0.5+11"],
        "unverifiable packages must be gone"
    );

    let warnings = stderr(&output);
    assert!(warnings.contains("hash budget is spent"), "{warnings}");
    assert!(warnings.contains("ephemeral foojay link"), "{warnings}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("2 dropped"), "{stdout}");
}

#[test]
fn checksum_uri_fallback_covers_both_vendor_file_shapes() {
    let server = Server::start();
    let oracle_hash = "c".repeat(64);
    let ms_hash = "d".repeat(64);
    server.route("/sums/oracle.sha256", {
        let hash = oracle_hash.to_uppercase();
        move |_| Response::ok(hash.clone())
    });
    server.route("/sums/ms.sha256sum.txt", {
        let line = format!("{} microsoft-jdk.zip\n", ms_hash.to_uppercase());
        move |_| Response::ok(line.clone())
    });

    let mut packages = baseline();
    packages.push(
        Fake::ga("graalvm", "x64", "25.0.3")
            .no_inline()
            .checksum_uri(&format!("{}/sums/oracle.sha256", server.url())),
    );
    packages.push(
        Fake::ga("microsoft", "x64", "25.0.3+7")
            .no_inline()
            .checksum_uri(&format!("{}/sums/ms.sha256sum.txt", server.url())),
    );
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    // Budget 0: the checksum_uri path alone must be enough, no downloads.
    let output = generate(&server, out.path(), &["--hash-budget", "0"]);
    assert_ok(&output);

    let graalvm = read_packages(out.path(), "windows-x64/graalvm.json");
    let ms = read_packages(out.path(), "windows-x64/microsoft.json");
    assert_eq!(
        graalvm
            .iter()
            .find(|p| p.version == "25.0.3")
            .unwrap()
            .sha256,
        oracle_hash
    );
    assert_eq!(
        ms.iter().find(|p| p.version == "25.0.3+7").unwrap().sha256,
        ms_hash
    );
}

#[test]
fn tofu_hashes_once_then_reuses_the_published_hash() {
    let server = Server::start();
    // A plausible little archive: zip magic + 1.5 MiB of body.
    let mut archive = vec![b'P', b'K', 3, 4];
    archive.extend(std::iter::repeat_n(0xABu8, 1536 * 1024));
    let expected = test_support::sha256_hex(&archive);
    server.route("/archives/corretto.zip", {
        let archive = archive.clone();
        move |_| Response::ok(archive.clone())
    });

    let mut packages = baseline();
    packages.push(
        Fake::ga("corretto", "x64", "22.0.2")
            .checksum_type("")
            .no_inline()
            .url(&format!("{}/archives/corretto.zip", server.url())),
    );
    serve_foojay(&server, packages);

    // First sight: the archive is streamed and hashed once.
    let first = tempfile::tempdir().unwrap();
    let output = generate(&server, first.path(), &[]);
    assert_ok(&output);
    let corretto = read_packages(first.path(), "windows-x64/corretto.json");
    let hashed = corretto.iter().find(|p| p.version == "22.0.2").unwrap();
    assert_eq!(hashed.sha256, expected);
    assert_eq!(server.hits("/archives/corretto.zip"), 1);

    // Same catalog against the published tree: the hash is reused, the
    // archive is NOT downloaded again (budget 0 would drop it otherwise).
    let compare = first.path().to_str().unwrap().to_string();
    let second = tempfile::tempdir().unwrap();
    let output = generate(
        &server,
        second.path(),
        &["--compare-to", &compare, "--hash-budget", "0"],
    );
    assert_ok(&output);
    let corretto = read_packages(second.path(), "windows-x64/corretto.json");
    assert_eq!(
        corretto
            .iter()
            .find(|p| p.version == "22.0.2")
            .unwrap()
            .sha256,
        expected
    );
    assert_eq!(server.hits("/archives/corretto.zip"), 1, "no re-download");
}

#[test]
fn tofu_refuses_to_hash_what_is_not_a_zip() {
    let server = Server::start();
    server.route("/archives/error.zip", |_| {
        Response::ok("<html>subscription required</html>")
    });

    let mut packages = baseline();
    packages.push(
        Fake::ga("corretto", "x64", "22.0.2")
            .checksum_type("")
            .no_inline()
            .url(&format!("{}/archives/error.zip", server.url())),
    );
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &[]);
    assert_ok(&output);
    let corretto = read_packages(out.path(), "windows-x64/corretto.json");
    assert!(
        !corretto.iter().any(|p| p.version == "22.0.2"),
        "an error page must never be published as an archive hash"
    );
    assert!(
        stderr(&output).contains("does not look like a JDK zip"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn fails_when_a_required_vendor_is_empty() {
    let server = Server::start();
    let packages = baseline()
        .into_iter()
        .filter(|fake| fake.vendor != "microsoft")
        .collect();
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &[]);
    assert!(
        !output.status.success(),
        "an empty required vendor must fail"
    );
    assert!(
        stderr(&output).contains("required vendor microsoft"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_best_effort_vendor_transport_error_does_not_abort_the_publish() {
    let server = Server::start();
    let baseline = baseline();
    // Detail routes for the six required vendors (oracle never gets that far).
    for fake in &baseline {
        let body = format!(
            r#"{{"result":[{{"filename":"jdk.zip","direct_download_uri":"{}","checksum":"{}","checksum_type":"sha256","checksum_uri":""}}]}}"#,
            fake.url,
            fake.served_checksum(),
        );
        server.route(&format!("/ids/{}", fake.id), move |_| {
            Response::ok(body.clone())
        });
    }
    // The required vendors list normally; the best-effort oracle query
    // hard-fails at the transport layer (HTTP 500 on either arch).
    let items: Vec<(String, String)> = baseline
        .iter()
        .map(|fake| {
            (
                fake.vendor.to_string(),
                format!(
                    r#"{{"id":"{}","java_version":"{}","term_of_support":"lts","release_status":"ga","size":1000}}"#,
                    fake.id, fake.version,
                ),
            )
        })
        .collect();
    server.route("/packages", move |request| {
        let query = &request.path;
        if query.contains("distribution=oracle") {
            return Response::empty(500);
        }
        if query.contains("architecture=arm64,aarch64") {
            return Response::ok(r#"{"result":[]}"#.to_string());
        }
        let listed: Vec<String> = items
            .iter()
            .filter(|(vendor, _)| query.contains(&format!("distribution={vendor}")))
            .map(|(_, item)| item.clone())
            .collect();
        Response::ok(format!(r#"{{"result":[{}]}}"#, listed.join(",")))
    });

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &[]);
    // Oracle's 500 must NOT abort the run — the required six still publish.
    assert_ok(&output);

    let paths: Vec<String> = read_index(out.path())
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert!(
        !paths.iter().any(|path| path.contains("oracle")),
        "a best-effort vendor's outage must omit it, not fail the run: {paths:?}"
    );
    for vendor in VENDORS {
        assert!(
            paths.contains(&format!("windows-x64/{vendor}.json")),
            "required vendor {vendor} must still publish"
        );
    }
    let warnings = stderr(&output);
    assert!(
        warnings.contains("best-effort") && warnings.contains("oracle"),
        "{warnings}"
    );
}

#[test]
fn duplicate_versions_collapse_deterministically() {
    let server = Server::start();
    let mut packages = baseline();
    // Same version twice (vendor repack): URL order decides the survivor
    // ("...fake/zulu-..." sorts before "...fake/zzz-repack...").
    let mut repack = Fake::ga("zulu", "x64", "21.0.5+11").lts();
    repack.id = "zulu-repack".to_string();
    repack.url = "https://cdn.azul.com/fake/zzz-repack.zip".to_string();
    packages.push(repack);
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &[]);
    assert_ok(&output);

    let zulu = read_packages(out.path(), "windows-x64/zulu.json");
    assert_eq!(zulu.len(), 1);
    assert_eq!(
        zulu[0].url, "https://cdn.azul.com/fake/zulu-21.0.5+11-x64.zip",
        "the first URL in sort order must win"
    );
    assert!(stderr(&output).contains("duplicate"), "{}", stderr(&output));
}

#[test]
fn runs_are_byte_reproducible() {
    let server = Server::start();
    let mut packages = baseline();
    packages.push(Fake::ga("temurin", "x64", "24.0.1+9"));
    packages.push(Fake::ga("liberica", "aarch64", "21.0.5+11").lts());
    serve_foojay(&server, packages);

    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    assert_ok(&generate(&server, first.path(), &[]));
    assert_ok(&generate(&server, second.path(), &[]));

    let index = read_index(first.path());
    let mut relpaths = vec!["index.json".to_string()];
    relpaths.extend(index.files.iter().map(|e| e.path.clone()));
    for relpath in relpaths {
        let native = relpath.replace('/', std::path::MAIN_SEPARATOR_STR);
        let a = fs::read(first.path().join(&native)).unwrap();
        let b = fs::read(second.path().join(&native)).unwrap();
        assert!(a == b, "{relpath} differs between identical runs");
    }
}

#[test]
fn shrink_guard_fails_warns_and_passes() {
    let server = Server::start();
    let mut old = baseline();
    for i in 0..19 {
        old.push(Fake::ga("temurin", "x64", &format!("20.0.{i}+1")));
    }
    serve_foojay(&server, old);
    // Published baseline: temurin 20 + 5 others = 25 packages.
    let published = tempfile::tempdir().unwrap();
    assert_ok(&generate(&server, published.path(), &[]));
    let published_dir = published.path().to_str().unwrap().to_string();

    // 25 -> 6 (-76%): over the 15% limit, refuse.
    let shrunk = Server::start();
    serve_foojay(&shrunk, baseline());
    let out = tempfile::tempdir().unwrap();
    let output = generate(&shrunk, out.path(), &["--compare-to", &published_dir]);
    assert!(!output.status.success(), "a 76% shrink must fail");
    assert!(
        stderr(&output).contains("refusing to publish"),
        "{}",
        stderr(&output)
    );

    // 25 -> 23 globally (-8%), temurin 20 -> 18 (-10%): warning band on
    // both the global and the per-file guard, publish anyway.
    let softer = Server::start();
    let mut softer_catalog = baseline();
    for i in 0..17 {
        softer_catalog.push(Fake::ga("temurin", "x64", &format!("20.0.{i}+1")));
    }
    serve_foojay(&softer, softer_catalog);
    let out = tempfile::tempdir().unwrap();
    let output = generate(&softer, out.path(), &["--compare-to", &published_dir]);
    assert_ok(&output);
    assert!(
        stderr(&output).contains("within the 15% limit"),
        "{}",
        stderr(&output)
    );

    // Identical catalog: clean pass.
    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &["--compare-to", &published_dir]);
    assert_ok(&output);
    assert!(!stderr(&output).contains("shrank"), "{}", stderr(&output));
}

#[test]
fn per_file_shrink_fails_even_when_the_global_total_passes() {
    let server = Server::start();
    let mut old = baseline();
    for i in 1..10 {
        old.push(Fake::ga("temurin", "x64", &format!("19.0.{i}+1")));
    }
    for i in 1..60 {
        old.push(Fake::ga("zulu", "x64", &format!("17.0.{i}+1")));
    }
    serve_foojay(&server, old);
    let published = tempfile::tempdir().unwrap();
    assert_ok(&generate(&server, published.path(), &[]));
    let published_dir = published.path().to_str().unwrap().to_string();

    // temurin collapses 10 -> 2 (-80%) while the global total only drops
    // 74 -> 66 (-11%): the per-file guard must refuse what the global one
    // would wave through.
    let collapsed = Server::start();
    let mut new = baseline();
    new.push(Fake::ga("temurin", "x64", "19.0.1+1"));
    for i in 1..60 {
        new.push(Fake::ga("zulu", "x64", &format!("17.0.{i}+1")));
    }
    serve_foojay(&collapsed, new);
    let out = tempfile::tempdir().unwrap();
    let output = generate(&collapsed, out.path(), &["--compare-to", &published_dir]);
    assert!(!output.status.success(), "a per-file collapse must fail");
    let warnings = stderr(&output);
    assert!(warnings.contains("windows-x64/temurin.json"), "{warnings}");
    assert!(warnings.contains("refusing to publish"), "{warnings}");
}

#[test]
fn identical_catalog_reuses_the_published_updated() {
    let server = Server::start();
    serve_foojay(&server, baseline());

    let published = tempfile::tempdir().unwrap();
    assert_ok(&generate(&server, published.path(), &[]));
    let published_dir = published.path().to_str().unwrap().to_string();

    // Same catalog, different --updated: the published stamp wins and
    // index.json reproduces byte for byte — the workflow's only-on-diff
    // commit then genuinely publishes nothing.
    let again = tempfile::tempdir().unwrap();
    let output = generate(
        &server,
        again.path(),
        &[
            "--compare-to",
            &published_dir,
            "--updated",
            "2027-02-02T00:00:00Z",
        ],
    );
    assert_ok(&output);
    let a = fs::read(published.path().join("index.json")).unwrap();
    let b = fs::read(again.path().join("index.json")).unwrap();
    assert!(
        a == b,
        "an unchanged catalog must reproduce index.json byte for byte"
    );

    // A real catalog change: the fresh stamp is used.
    let changed = Server::start();
    let mut more = baseline();
    more.push(Fake::ga("temurin", "x64", "24.0.1+9"));
    serve_foojay(&changed, more);
    let out = tempfile::tempdir().unwrap();
    let output = generate(
        &changed,
        out.path(),
        &[
            "--compare-to",
            &published_dir,
            "--updated",
            "2027-02-02T00:00:00Z",
        ],
    );
    assert_ok(&output);
    assert_eq!(read_index(out.path()).updated, "2027-02-02T00:00:00Z");
}

#[test]
fn urls_off_the_vendor_allowlist_are_dropped() {
    let server = Server::start();
    let mut packages = baseline();
    packages.push(
        Fake::ga("temurin", "x64", "18.0.2+9")
            .checksum_type("")
            .no_inline()
            .url("https://evil.example/jdk-x64.zip"),
    );
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &[]);
    assert_ok(&output);

    let temurin = read_packages(out.path(), "windows-x64/temurin.json");
    assert!(
        !temurin.iter().any(|p| p.version == "18.0.2+9"),
        "a foreign-host package must not be published"
    );
    let warnings = stderr(&output);
    assert!(warnings.contains("evil.example"), "{warnings}");
    assert!(warnings.contains("not a known JDK vendor"), "{warnings}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("1 dropped"), "{stdout}");
}

#[test]
fn tofu_cross_checks_the_announced_sha1() {
    let server = Server::start();
    let mut archive = vec![b'P', b'K', 3, 4];
    archive.extend(std::iter::repeat_n(0xCDu8, 1536 * 1024));
    let real_sha1 = jdk_core::download::hex(&sha1::Sha1::digest(&archive));
    let expected_sha256 = test_support::sha256_hex(&archive);
    server.route("/archives/liberica.zip", {
        let archive = archive.clone();
        move |_| Response::ok(archive.clone())
    });
    let archive_url = format!("{}/archives/liberica.zip", server.url());

    let mut packages = baseline();
    // foojay announces the RIGHT sha1: cross-check passes, sha256 published.
    packages.push(
        Fake::ga("liberica", "x64", "23.0.1+11")
            .checksum_type("sha1")
            .announce(&real_sha1)
            .url(&archive_url),
    );
    // foojay announces a WRONG sha1: the stream disagrees, nothing published.
    packages.push(
        Fake::ga("liberica", "x64", "22.0.1+10")
            .checksum_type("sha1")
            .url(&archive_url),
    );
    serve_foojay(&server, packages);

    let out = tempfile::tempdir().unwrap();
    let output = generate(&server, out.path(), &[]);
    assert_ok(&output);

    let liberica = read_packages(out.path(), "windows-x64/liberica.json");
    let good = liberica.iter().find(|p| p.version == "23.0.1+11").unwrap();
    assert_eq!(good.sha256, expected_sha256);
    assert!(
        !liberica.iter().any(|p| p.version == "22.0.1+10"),
        "a sha1 mismatch must never be published"
    );
    assert!(
        stderr(&output).contains("sha1 mismatch"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn shrink_guard_skips_when_nothing_is_published_yet() {
    let server = Server::start();
    serve_foojay(&server, baseline());

    // The compare URL 404s — the very first run, nothing published yet.
    let out = tempfile::tempdir().unwrap();
    let compare = format!("{}/jdk-index/main", server.url());
    let output = generate(&server, out.path(), &["--compare-to", &compare]);
    assert_ok(&output);
    assert!(
        stderr(&output).contains("skipping the shrink guard"),
        "{}",
        stderr(&output)
    );
}
