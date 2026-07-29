//! Hermetic CLI integration: the real jdk.exe against a loopback index and
//! a JDK_ROOT in temp (`JDK_INDEX`/`JDK_FOOJAY` are the binary's URL
//! override injection points). No test touches the real network or home.

use jdk_core::current::{self, Current};
use jdk_core::index::ReleaseStatus;
use jdk_resolve::exit;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;
use test_support::{Response, Server, dead_url, fake_jdk_zip, package, serve_catalog, sha256_hex};

const JDK: &str = env!("CARGO_BIN_EXE_jdk");

struct World {
    _temp: TempDir,
    root: PathBuf,
    project: PathBuf,
    index_url: String,
    foojay_url: String,
}

impl World {
    /// Sandbox whose catalog URLs point nowhere (offline commands).
    fn offline() -> World {
        World::at(dead_url())
    }

    fn at(index_url: String) -> World {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let project = temp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        World {
            _temp: temp,
            root,
            project,
            index_url,
            foojay_url: dead_url(),
        }
    }

    fn jdk(&self, args: &[&str]) -> Output {
        Command::new(JDK)
            .args(args)
            .current_dir(&self.project)
            .env("JDK_ROOT", &self.root)
            .env("JDK_INDEX", &self.index_url)
            .env("JDK_FOOJAY", &self.foojay_url)
            .output()
            .unwrap()
    }

    /// Points the live fallback somewhere real; the index stays where it was.
    fn with_foojay(mut self, url: String) -> World {
        self.foojay_url = url;
        self
    }

    fn config(&self, text: &str) {
        fs::create_dir_all(&self.root).unwrap();
        fs::write(self.root.join("config.toml"), text).unwrap();
    }

    /// A fake installed candidate; the tool files are stubs (nothing here
    /// executes them — the acceptance test covers real execution).
    fn install_fake(&self, name: &str) -> PathBuf {
        let dir = self.root.join("candidates").join("java").join(name);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin").join("java.exe"), b"stub").unwrap();
        dir
    }

    fn candidate(&self, name: &str) -> PathBuf {
        self.root.join("candidates").join("java").join(name)
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A served zip + its package entry, ready for `serve_catalog`.
fn served_package(server: &Server, version: &str) -> jdk_core::index::Package {
    let zip = fake_jdk_zip(b"stub jdk payload");
    let route = format!("/dl/{version}.zip");
    let pkg = package(
        version,
        &format!("{}{route}", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    server.route(&route, move |_| Response::ok(zip.clone()));
    pkg
}

/// A foojay that resolves exactly one build: the listing, the `ids/<id>`
/// details where the only checksum lives, and the zip. Paired with an index
/// that carries a DIFFERENT line, so the index answers, misses, and the live
/// fallback takes over — the real shape of a live resolution, without the
/// retry backoff an unreachable index would spend first.
fn serve_foojay(server: &Server, version: &str, release_status: &str) {
    let zip = fake_jdk_zip(b"stub jdk payload");
    let listing = format!(
        r#"{{"result":[{{"id":"abc123","java_version":"{version}","term_of_support":"lts","release_status":"{release_status}","size":{}}}]}}"#,
        zip.len()
    );
    let details = format!(
        r#"{{"result":[{{"filename":"t.zip","direct_download_uri":"{}/dl/live.zip","checksum":"{}","checksum_type":"sha256"}}]}}"#,
        server.url(),
        sha256_hex(&zip)
    );
    server.route("/packages", move |_| Response::ok(listing.clone()));
    server.route("/ids/abc123", move |_| Response::ok(details.clone()));
    server.route("/dl/live.zip", move |_| Response::ok(zip.clone()));
}

/// An index that answers for temurin but carries only 17, so any other
/// selector misses it and falls through to foojay.
fn serve_index_without(server: &Server) {
    let other = package(
        "17.0.9",
        &format!("{}/dl/17.zip", server.url()),
        &"a".repeat(64),
        1,
    );
    serve_catalog(server, std::slice::from_ref(&other));
}

/// DEBT-07: a GA build resolved live says NOTHING. It is the banal case — a
/// release the index has not picked up yet — and a line about it would land
/// in the middle of every `java` that auto-installs one in CI.
#[test]
fn a_live_ga_resolution_is_silent() {
    let server = Server::start();
    serve_index_without(&server);
    serve_foojay(&server, "21.0.5+11", "ga");
    let world = World::at(server.url().to_string()).with_foojay(server.url().to_string());

    let output = world.jdk(&["install", "temurin@21"]);
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("installed temurin@21.0.5+11"), "{stderr}");
    assert!(
        !stderr.contains("resolved live"),
        "a GA build off the live API must not announce itself: {stderr}"
    );
}

/// DEBT-07: an early-access build resolved live DOES say so, because there
/// the fact is worth having — the index carries no build for that selector at
/// all. The old message fired for both cases and claimed the download was
/// "unverified against the index's pinned sha256", which was never true: the
/// sha256 is mandatory on both paths.
#[test]
fn a_live_early_access_resolution_says_so() {
    let server = Server::start();
    serve_index_without(&server);
    serve_foojay(&server, "27-ea+31", "ea");
    let world = World::at(server.url().to_string()).with_foojay(server.url().to_string());

    let output = world.jdk(&["install", "temurin@27-ea"]);
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("installed temurin@27-ea+31"), "{stderr}");
    assert!(stderr.contains("resolved live from foojay"), "{stderr}");
    assert!(stderr.contains("no early-access build"), "{stderr}");
    assert!(
        !stderr.contains("unverified"),
        "both paths verify the sha256; the line must not suggest otherwise: {stderr}"
    );
}

#[test]
fn install_from_the_local_index_is_idempotent_and_list_shows_it() {
    let server = Server::start();
    let pkg = served_package(&server, "21.0.5+11");
    serve_catalog(&server, std::slice::from_ref(&pkg));
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["install", "temurin@21"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("installed temurin@21.0.5+11"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        world
            .candidate("temurin@21.0.5+11")
            .join("bin")
            .join("javac.exe")
            .exists()
    );
    // First install wins (the e2e regression trap): the CLI wired the global
    // junction to the just-installed candidate and announced it.
    assert!(
        stderr(&output).contains("is now the global default"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        current::inspect(&world.root).unwrap(),
        Current::Junction {
            target: world.candidate("temurin@21.0.5+11")
        }
    );

    let output = world.jdk(&["list"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("temurin@21.0.5+11"));

    let output = world.jdk(&["install", "temurin@21"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stderr(&output).contains("already installed"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(server.hits("/dl/21.0.5+11.zip"), 1, "one download total");
}

#[test]
fn bare_selector_installs_the_config_vendor() {
    let server = Server::start();
    let mut pkg = served_package(&server, "21.0.5+11");
    pkg.vendor = "zulu".to_string();
    serve_catalog(&server, std::slice::from_ref(&pkg));
    let world = World::at(server.url().to_string());
    world.config("vendor = \"zulu\"\n");

    let output = world.jdk(&["install", "21"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(world.candidate("zulu@21.0.5+11").exists());
}

/// D2: a line the catalog carries only as early access is refused for a
/// plain GA selector — no nightly arrives unannounced — and the refusal
/// names the pre-release selector, which then installs it with no flag.
#[test]
fn an_early_access_only_line_is_refused_and_the_named_selector_installs_it() {
    let server = Server::start();
    let mut nightly = served_package(&server, "27-ea+31");
    nightly.release_status = ReleaseStatus::Ea;
    nightly.lts = false;
    serve_catalog(&server, std::slice::from_ref(&nightly));
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["install", "temurin@27"]);

    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let refusal = stderr(&output);
    assert!(
        refusal.contains("no general-availability build"),
        "{refusal}"
    );
    assert!(refusal.contains("jdk install temurin@27-ea"), "{refusal}");
    assert_eq!(
        server.hits("/dl/27-ea+31.zip"),
        0,
        "a refused selector downloads nothing"
    );

    let output = world.jdk(&["install", "temurin@27-ea"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(world.candidate("temurin@27-ea+31").exists());
}

#[test]
fn uninstall_removes_a_free_candidate_and_blocks_an_in_use_one() {
    let world = World::offline();
    let dir = world.install_fake("temurin@21.0.4");

    let output = world.jdk(&["uninstall", "21"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("uninstalled temurin@21.0.4"));
    assert!(!dir.exists());

    // Recreate it and hold a handle open inside: the rename-probe must
    // refuse and leave the candidate exactly where it was.
    let dir = world.install_fake("temurin@21.0.4");
    let hold = fs::File::open(dir.join("bin").join("java.exe")).unwrap();
    let output = world.jdk(&["uninstall", "temurin@21.0.4"]);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("in use"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(dir.exists(), "an in-use candidate must not be touched");
    assert!(
        !world.candidate("temurin@21.0.4.removing").exists(),
        "no half-removed state may remain"
    );

    drop(hold);
    let output = world.jdk(&["uninstall", "temurin@21.0.4"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(!dir.exists());
}

#[test]
fn uninstall_miss_is_exit_4_and_names_what_is_installed() {
    let world = World::offline();
    world.install_fake("temurin@17.0.9");

    let output = world.jdk(&["uninstall", "zulu@21"]);

    assert_eq!(output.status.code(), Some(4));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("no installed JDK matches zulu@21"),
        "{stderr}"
    );
    assert!(stderr.contains("temurin@17.0.9"), "{stderr}");
}

#[test]
fn orphaned_removing_dirs_are_swept_and_never_listed() {
    let world = World::offline();
    world.install_fake("temurin@17.0.9");
    let orphan = world.candidate("temurin@21.0.4.removing");
    fs::create_dir_all(orphan.join("bin")).unwrap();
    fs::write(orphan.join("bin").join("java.exe"), b"junk").unwrap();

    let output = world.jdk(&["list"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("temurin@17.0.9"));
    assert!(!stdout(&output).contains("removing"));
    assert!(
        !orphan.exists(),
        "list must sweep crashed-uninstall leftovers"
    );
}

#[test]
fn available_lists_flags_filters_and_trims_to_latest() {
    let server = Server::start();
    let mut ea = package("24-ea", "https://example.invalid/a.zip", &"a".repeat(64), 1);
    ea.release_status = ReleaseStatus::Ea;
    ea.lts = false;
    let mut plain = package(
        "23.0.1",
        "https://example.invalid/b.zip",
        &"b".repeat(64),
        1,
    );
    plain.lts = false;
    let older = package(
        "21.0.4",
        "https://example.invalid/c.zip",
        &"c".repeat(64),
        1,
    );
    let newer = package(
        "21.0.5",
        "https://example.invalid/d.zip",
        &"d".repeat(64),
        1,
    );
    serve_catalog(&server, &[ea, plain, older, newer]);
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["available"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let listing = stdout(&output);
    let line_with = |listing: &str, needle: &str| {
        listing
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle} in:\n{listing}"))
            .to_string()
    };
    assert!(line_with(&listing, "temurin@21.0.5").contains("LTS"));
    assert!(!line_with(&listing, "temurin@23.0.1").contains("LTS"));
    // Early-access is hidden unless --ea is asked for.
    assert!(
        !listing.contains("24-ea"),
        "EA hidden by default:\n{listing}"
    );

    let output = world.jdk(&["available", "--ea"]);
    let listing = stdout(&output);
    assert!(line_with(&listing, "temurin@24-ea").contains("EA"));

    let output = world.jdk(&["available", "temurin@21"]);
    let listing = stdout(&output);
    assert!(listing.contains("21.0.5") && listing.contains("21.0.4"));
    assert!(!listing.contains("23.0.1") && !listing.contains("24-ea"));

    let output = world.jdk(&["available", "--latest", "21"]);
    let listing = stdout(&output);
    assert!(listing.contains("21.0.5"), "{listing}");
    assert!(
        !listing.contains("21.0.4"),
        "--latest keeps one per major: {listing}"
    );

    let output = world.jdk(&["available", "99"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "", "no data on stdout for an empty match");
    assert!(stderr(&output).contains("nothing"), "{}", stderr(&output));
}

/// The listing and the installer must agree on what exists: a version only
/// early access satisfies is shown instead of denied, and the pre-release
/// selector needs no more flags here than it does for `jdk install`.
#[test]
fn available_shows_the_early_access_line_that_install_resolves() {
    let server = Server::start();
    let mut nightly = package(
        "27-ea+31",
        "https://example.invalid/a.zip",
        &"a".repeat(64),
        1,
    );
    nightly.release_status = ReleaseStatus::Ea;
    nightly.lts = false;
    let stable = package(
        "21.0.5",
        "https://example.invalid/b.zip",
        &"b".repeat(64),
        1,
    );
    serve_catalog(&server, &[nightly, stable]);
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["available", "temurin@27"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("temurin@27-ea+31"),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("only early-access builds match temurin@27"),
        "the switch is announced: {}",
        stderr(&output)
    );

    let output = world.jdk(&["available", "temurin@27-ea"]);
    assert!(
        stdout(&output).contains("temurin@27-ea+31"),
        "a pre-release filter implies --ea: {}",
        stdout(&output)
    );

    // A stable filter is untouched by all this: no widening, no EA rows.
    let output = world.jdk(&["available", "temurin@21"]);
    let listing = stdout(&output);
    assert!(
        listing.contains("temurin@21.0.5") && !listing.contains("27-ea"),
        "{listing}"
    );
}

/// `--ea --latest` used to cancel out: grouping by vendor+major alone let
/// every stable line swallow its own preview, so early access survived only
/// for majors with no GA at all.
#[test]
fn latest_with_early_access_keeps_both_the_ga_and_the_ea_of_a_line() {
    let server = Server::start();
    let mut nightly = package(
        "21.0.6-ea",
        "https://example.invalid/a.zip",
        &"a".repeat(64),
        1,
    );
    nightly.release_status = ReleaseStatus::Ea;
    let older = package(
        "21.0.4",
        "https://example.invalid/b.zip",
        &"b".repeat(64),
        1,
    );
    let newer = package(
        "21.0.5",
        "https://example.invalid/c.zip",
        &"c".repeat(64),
        1,
    );
    serve_catalog(&server, &[nightly, older, newer]);
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["available", "--latest", "--ea", "temurin"]);
    let listing = stdout(&output);
    assert!(listing.contains("temurin@21.0.5"), "{listing}");
    assert!(listing.contains("temurin@21.0.6-ea"), "{listing}");
    assert!(
        !listing.contains("temurin@21.0.4"),
        "still one per status: {listing}"
    );

    let output = world.jdk(&["available", "--latest", "temurin"]);
    let listing = stdout(&output);
    assert!(
        listing.contains("temurin@21.0.5") && !listing.contains("21.0.6-ea"),
        "without the flag the line keeps only its stable build: {listing}"
    );
}

/// A vendor with no early access at all says so, instead of leaving `--ea`
/// looking broken.
#[test]
fn ea_on_a_vendor_that_publishes_none_says_so() {
    let server = Server::start();
    let stable = package(
        "21.0.5",
        "https://example.invalid/a.zip",
        &"a".repeat(64),
        1,
    );
    serve_catalog(&server, std::slice::from_ref(&stable));
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["available", "temurin@99", "--ea"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("temurin has no early-access builds"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn available_skips_a_broken_vendor_and_lists_the_healthy_ones() {
    let server = Server::start();
    let healthy = package(
        "21.0.5",
        "https://example.invalid/a.zip",
        &"a".repeat(64),
        1,
    );
    let mut broken = package(
        "17.0.9",
        "https://example.invalid/b.zip",
        &"b".repeat(64),
        1,
    );
    broken.vendor = "zulu".to_string();
    serve_catalog(&server, &[healthy, broken]);
    // Corrupt zulu's platform file AFTER the index advertised its sha256:
    // the checksum check fails, the foojay fallback is dead → zulu fails.
    let (os, arch) = jdk_core::index::current_platform();
    server.route(&format!("/{os}-{arch}/zulu.json"), |_| {
        Response::ok(b"garbage".as_slice())
    });
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["available"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("temurin@21.0.5"),
        "{}",
        stdout(&output)
    );
    assert!(!stdout(&output).contains("zulu"), "{}", stdout(&output));
    let stderr = stderr(&output);
    assert!(stderr.contains("warning: skipping zulu"), "{stderr}");
}

#[test]
fn available_without_reachable_index_is_a_network_failure() {
    let world = World::offline();

    let output = world.jdk(&["available"]);

    assert_eq!(
        output.status.code(),
        Some(20),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("vendor filter"),
        "the hint must offer the vendor-filtered escape: {}",
        stderr(&output)
    );
}

#[test]
fn pin_creates_updates_and_preserves_the_rest_of_jdkrc() {
    let world = World::offline();
    world.install_fake("temurin@21.0.4");

    // Fresh file.
    let output = world.jdk(&["pin", "temurin@21"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let path = world.project.join(".jdkrc");
    assert_eq!(fs::read_to_string(&path).unwrap(), "java=temurin@21\n");
    assert!(
        !stderr(&output).contains("not installed"),
        "21.0.4 satisfies the pin: {}",
        stderr(&output)
    );

    // Existing file: comments, CRLF and other tools survive byte-for-byte.
    fs::write(
        &path,
        "# team toolchain\r\nmaven=3.9\r\njava=zulu@17 # legacy\r\nkotlin=2.0\r\n",
    )
    .unwrap();
    let output = world.jdk(&["pin", "22"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# team toolchain\r\nmaven=3.9\r\njava=22 # legacy\r\nkotlin=2.0\r\n"
    );
    // Pinning something missing warns and hints, without failing.
    let stderr = stderr(&output);
    assert!(stderr.contains("not installed yet"), "{stderr}");
    assert!(stderr.contains("jdk install 22"), "{stderr}");
}

#[test]
fn current_explains_pin_resolution_and_global_fallback() {
    let world = World::offline();
    world.install_fake("temurin@21.0.4");
    fs::write(world.project.join(".jdkrc"), "java=temurin@21\n").unwrap();

    let output = world.jdk(&["current"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let explained = stdout(&output);
    assert!(explained.contains("temurin@21 by"), "{explained}");
    assert!(explained.contains(".jdkrc"), "{explained}");
    assert!(
        explained.contains("resolved:  temurin@21.0.4"),
        "{explained}"
    );
    assert!(explained.contains("temurin@21.0.4"), "{explained}");

    // A bare pin names the vendor the config supplied.
    fs::write(world.project.join(".jdkrc"), "java=21\n").unwrap();
    let output = world.jdk(&["current"]);
    assert!(
        stdout(&output).contains("vendor:    temurin (config default)"),
        "{}",
        stdout(&output)
    );

    // Pinned but not installed: explanation on stdout, error contract on
    // stderr + exit code.
    fs::write(world.project.join(".jdkrc"), "java=temurin@22\n").unwrap();
    let output = world.jdk(&["current"]);
    assert_eq!(output.status.code(), Some(4));
    assert!(
        stdout(&output).contains("not installed"),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("jdk install temurin@22"),
        "{}",
        stderr(&output)
    );

    // No pin, no global: the project boundary comes from a non-java source.
    fs::remove_file(world.project.join(".jdkrc")).unwrap();
    fs::write(world.project.join(".tool-versions"), "nodejs 20.10.0\n").unwrap();
    let output = world.jdk(&["current"]);
    assert_eq!(output.status.code(), Some(4));
    assert!(
        stdout(&output).contains("global:    none"),
        "{}",
        stdout(&output)
    );
    assert!(stderr(&output).contains("jdk pin"), "{}", stderr(&output));

    // Global present (a plain dir stands in — `current` only checks
    // existence; the junction proper is pillar.rs territory).
    fs::create_dir_all(world.root.join("current").join("bin")).unwrap();
    let output = world.jdk(&["current"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("global:"), "{}", stdout(&output));
    assert!(
        !stdout(&output).contains("global:    none"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn which_prints_the_exact_tool_path_and_contract_exit_codes() {
    let world = World::offline();
    world.install_fake("temurin@21.0.4");
    fs::write(world.project.join(".jdkrc"), "java=temurin@21\n").unwrap();

    let output = world.jdk(&["which"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        PathBuf::from(stdout(&output).trim()),
        world
            .candidate("temurin@21.0.4")
            .join("bin")
            .join("java.exe")
    );

    // `.exe` is tolerated and normalized.
    let output = world.jdk(&["which", "java.exe"]);
    assert_eq!(output.status.code(), Some(0));

    let output = world.jdk(&["which", "javac"]);
    assert_eq!(
        output.status.code(),
        Some(127),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("javac.exe not found"),
        "{}",
        stderr(&output)
    );

    let output = world.jdk(&["which", "ja\\va"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn which_without_pin_uses_the_global_junction_path() {
    let world = World::offline();
    fs::write(world.project.join(".tool-versions"), "nodejs 20.10.0\n").unwrap();

    let output = world.jdk(&["which"]);
    assert_eq!(output.status.code(), Some(4), "no global configured yet");

    let current_bin = world.root.join("current").join("bin");
    fs::create_dir_all(&current_bin).unwrap();
    fs::write(current_bin.join("java.exe"), b"stub").unwrap();
    let output = world.jdk(&["which"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    // The junction path itself, exactly what the shim spawns — stable across
    // `jdk use` retargets.
    assert_eq!(
        PathBuf::from(stdout(&output).trim()),
        world.root.join("current").join("bin").join("java.exe")
    );
}

#[test]
fn selector_and_config_errors_exit_with_the_config_code() {
    let world = World::offline();

    let output = world.jdk(&["install", "banana"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("vendor@version"),
        "the hint teaches the shape: {}",
        stderr(&output)
    );

    world.config("vendor = zulu\n"); // unquoted: outside the subset
    fs::write(world.project.join(".jdkrc"), "java=21\n").unwrap();
    let output = world.jdk(&["current"]);
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("config.toml"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn malformed_pin_file_exits_with_the_config_code() {
    // Distinct from a corrupt config.toml (`selector_and_config_errors_exit_with_the_config_code`):
    // here config.toml is fine, the pin ITSELF is unparseable.
    let world = World::offline();
    fs::write(world.project.join(".jdkrc"), "java=banana\n").unwrap();

    let output = world.jdk(&["current"]);

    assert_eq!(
        output.status.code(),
        Some(exit::CONFIG),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains(".jdkrc"),
        "the error must name the offending pin file: {}",
        stderr(&output)
    );
}

#[test]
fn best_candidate_prefers_stable_over_a_higher_prerelease_through_the_real_binary() {
    let world = World::offline();
    world.install_fake("temurin@21.0.5");
    // Numerically higher than 21.0.5, but a pre-release: must still lose.
    world.install_fake("temurin@21.0.6-ea");
    fs::write(world.project.join(".jdkrc"), "java=temurin@21\n").unwrap();

    let output = world.jdk(&["which"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        PathBuf::from(stdout(&output).trim()),
        world
            .candidate("temurin@21.0.5")
            .join("bin")
            .join("java.exe"),
        "the stable release must win over the numerically higher EA build"
    );
}

#[test]
fn use_sweeps_orphaned_removing_dirs_before_switching() {
    let world = World::offline();
    world.install_fake("temurin@21.0.4");
    let orphan = world.candidate("zulu@17.0.9.removing");
    fs::create_dir_all(orphan.join("bin")).unwrap();
    fs::write(orphan.join("bin").join("java.exe"), b"junk").unwrap();

    let output = world.jdk(&["use", "21"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        !orphan.exists(),
        "jdk use must sweep crashed-uninstall leftovers on the way in"
    );
}

#[test]
fn install_sweeps_orphaned_removing_dirs_before_installing() {
    let server = Server::start();
    let pkg = served_package(&server, "21.0.5+11");
    serve_catalog(&server, std::slice::from_ref(&pkg));
    let world = World::at(server.url().to_string());
    let orphan = world.candidate("zulu@17.0.9.removing");
    fs::create_dir_all(orphan.join("bin")).unwrap();
    fs::write(orphan.join("bin").join("java.exe"), b"junk").unwrap();

    let output = world.jdk(&["install", "temurin@21"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        !orphan.exists(),
        "jdk install must sweep crashed-uninstall leftovers on the way in"
    );
}

#[test]
fn pin_file_with_bom_crlf_and_comment_resolves_successfully() {
    let world = World::offline();
    world.install_fake("temurin@21.0.4");
    // BOM + CRLF + a trailing `#` comment on the pin line itself.
    let content = "\u{feff}# team toolchain\r\njava=21.0.4-tem # LTS\r\n";
    fs::write(world.project.join(".sdkmanrc"), content).unwrap();

    let output = world.jdk(&["which"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "BOM/CRLF/comment must not be mistaken for CONFIG damage: stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        PathBuf::from(stdout(&output).trim()),
        world
            .candidate("temurin@21.0.4")
            .join("bin")
            .join("java.exe")
    );
}

/// D4: Oracle's terms are not this tool's to accept on the user's behalf.
/// Without consent the install stops before any download — the archive route
/// is never touched — and the refusal names the flag that carries consent.
/// A test harness has no console, which is the same position CI and the shim
/// are in: refuse, never hang waiting for an answer nobody can give.
#[test]
fn a_proprietary_vendor_needs_consent_before_anything_is_downloaded() {
    let server = Server::start();
    let mut oracle = served_package(&server, "25.0.2");
    oracle.vendor = "oracle".to_string();
    serve_catalog(&server, std::slice::from_ref(&oracle));
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["install", "oracle@25"]);

    assert_eq!(
        output.status.code(),
        Some(exit::CONFIG),
        "stderr: {}",
        stderr(&output)
    );
    let refusal = stderr(&output);
    assert!(refusal.contains("NFTC"), "the terms come first: {refusal}");
    assert!(refusal.contains("--accept-license"), "{refusal}");
    assert_eq!(
        server.hits("/dl/25.0.2.zip"),
        0,
        "no download of any kind without consent"
    );
    assert!(!world.candidate("oracle@25.0.2").exists());
}

/// The other half of D4: the flag consents, and only then does the download
/// carry the cookie by which Oracle records that acceptance — the mechanism
/// that made an unattended install a problem in the first place.
#[test]
fn accept_license_consents_and_the_oracle_cookie_reaches_the_wire() {
    let server = Server::start();
    let mut oracle = served_package(&server, "25.0.2");
    oracle.vendor = "oracle".to_string();
    serve_catalog(&server, std::slice::from_ref(&oracle));
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["install", "oracle@25", "--accept-license"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(world.candidate("oracle@25.0.2").exists());
    let requests = server.requests_to("/dl/25.0.2.zip");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("cookie"),
        Some("oraclelicense=accept-securebackup-cookie"),
        "the acceptance cookie rides on a consented download"
    );

    // Already in the store: the re-run fetches nothing, so it asks nothing
    // either — consent gates the download, not the command.
    let output = world.jdk(&["install", "oracle@25"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("already installed"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(server.hits("/dl/25.0.2.zip"), 1, "one download total");
}

/// The shim path never asks: someone who typed `java` did not ask to enter a
/// license agreement, so auto-install refuses proprietary vendors outright.
#[test]
fn the_shim_install_path_refuses_proprietary_terms_instead_of_accepting_them() {
    let server = Server::start();
    let mut oracle = served_package(&server, "25.0.2");
    oracle.vendor = "oracle".to_string();
    serve_catalog(&server, std::slice::from_ref(&oracle));
    let world = World::at(server.url().to_string());

    let output = world.jdk(&["install", "oracle@25", "--from-shim"]);

    assert_eq!(
        output.status.code(),
        Some(exit::CONFIG),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(server.hits("/dl/25.0.2.zip"), 0);
}

/// The config route to the same consent, end to end: a real `config.toml`, the
/// real binary reading it, a real download gated on it. The key is parsed in
/// jdk-resolve and weighed in `install`'s unit tests, but nothing pinned the
/// wire between them, and this is legal consent — a regression that stopped
/// reading the key would turn every CI install into a refusal, and one that
/// read it too eagerly would accept Oracle's terms for someone who never said
/// yes. Neither announces itself.
///
/// One world, three runs, the config line the only thing that moves: no
/// console appears between them, so nothing but the key can explain the
/// different answers.
#[test]
fn the_config_key_consents_to_proprietary_terms_and_still_not_for_the_shim() {
    let server = Server::start();
    let mut oracle = served_package(&server, "25.0.2");
    oracle.vendor = "oracle".to_string();
    serve_catalog(&server, std::slice::from_ref(&oracle));
    let world = World::at(server.url().to_string());

    // Explicitly withheld, and no TTY to ask on: refuse before any download.
    world.config("accept-license = false\n");
    let output = world.jdk(&["install", "oracle@25"]);
    assert_eq!(
        output.status.code(),
        Some(exit::CONFIG),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(server.hits("/dl/25.0.2.zip"), 0);

    world.config("accept-license = true\n");

    // Still refused on the shim path, where the key deliberately does not
    // reach: a `.jdkrc` comes from a repository, and standing consent is not
    // consent for someone else's project to enter a license agreement.
    let output = world.jdk(&["install", "oracle@25", "--from-shim"]);
    assert_eq!(
        output.status.code(),
        Some(exit::CONFIG),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        server.hits("/dl/25.0.2.zip"),
        0,
        "no download from the shim"
    );

    // The command the key was written for: unattended, no prompt, installed.
    let output = world.jdk(&["install", "oracle@25"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(world.candidate("oracle@25.0.2").exists());
    let granted = stderr(&output);
    assert!(
        granted.contains("accept-license = true in config.toml"),
        "the grant is named so it can be withdrawn: {granted}"
    );
    // And consent recorded on the wire, as the flag route does: the cookie is
    // the acceptance, so it may only ride once the key has been honored.
    let requests = server.requests_to("/dl/25.0.2.zip");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("cookie"),
        Some("oraclelicense=accept-securebackup-cookie")
    );
}

#[test]
fn install_with_no_reachable_catalog_reports_both_causes() {
    let world = World::offline();

    let output = world.jdk(&["install", "temurin@21"]);

    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let stderr = stderr(&output);
    assert!(stderr.contains("index:"), "{stderr}");
    assert!(stderr.contains("foojay fallback:"), "{stderr}");
}
