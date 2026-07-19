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
    let line_with = |needle: &str| {
        listing
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle} in:\n{listing}"))
            .to_string()
    };
    assert!(line_with("temurin@21.0.5").contains("LTS"));
    assert!(line_with("temurin@24-ea").contains("EA"));
    assert!(!line_with("temurin@23.0.1").contains("LTS"));

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

#[test]
fn install_with_no_reachable_catalog_reports_both_causes() {
    let world = World::offline();

    let output = world.jdk(&["install", "temurin@21"]);

    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let stderr = stderr(&output);
    assert!(stderr.contains("index:"), "{stderr}");
    assert!(stderr.contains("foojay fallback:"), "{stderr}");
}
