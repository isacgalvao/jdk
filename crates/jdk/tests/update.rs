//! Hermetic `jdk update` e2e: the store copy of the real jdk.exe updates
//! itself against a loopback release server (`JDK_RELEASES` is the URL
//! injection point, like `JDK_INDEX`). The fake bundle carries marker
//! payloads that never execute — the swap is in-process, which is exactly
//! what makes this test possible.

use jdk_core::{release, shims};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
use test_support::{Response, Server, dead_url, release_zip, sha256_hex};

const JDK: &str = env!("CARGO_BIN_EXE_jdk");
/// What the binary under test believes its own version is.
const LOCAL: &str = env!("CARGO_PKG_VERSION");

struct World {
    _temp: TempDir,
    root: PathBuf,
    releases: String,
}

impl World {
    fn new(releases: String) -> World {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        World {
            _temp: temp,
            root,
            releases,
        }
    }

    /// Places the built jdk.exe at `<root>\bin\jdk.exe` — the only copy
    /// `jdk update` agrees to replace — and returns its path.
    fn place_store_copy(&self) -> PathBuf {
        let bin = self.root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let dest = bin.join("jdk.exe");
        fs::copy(JDK, &dest).unwrap();
        dest
    }

    fn update(&self, exe: &Path, args: &[&str]) -> Output {
        Command::new(exe)
            .arg("update")
            .args(args)
            .env("JDK_ROOT", &self.root)
            .env("JDK_RELEASES", &self.releases)
            .output()
            .unwrap()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The release asset name [`release::fetch_bundle`] will ask for.
fn asset(version: &str) -> String {
    format!("jdk-v{version}-windows-{}.zip", release::ARCH)
}

/// Routes the `/latest` redirect for `version` onto `server`.
fn serve_latest(server: &Server, version: &str) {
    let target = format!("{}/tag/v{version}", server.url());
    server.route("/latest", move |_| Response::redirect(&target));
    server.route(&format!("/tag/v{version}"), |_| {
        Response::ok("release page html")
    });
}

/// Routes a full release for `version`: the `/latest` redirect plus the
/// bundle zip and its correct sidecar under the GitHub asset path.
fn serve_release(server: &Server, version: &str, jdk_exe: &[u8], shim_exe: &[u8]) {
    serve_latest(server, version);
    let zip = release_zip(jdk_exe, shim_exe);
    let route = format!("/download/v{version}/{}", asset(version));
    let sidecar = format!("{}  {}\n", sha256_hex(&zip), asset(version));
    server.route(&route, move |_| Response::ok(zip.clone()));
    server.route(&format!("{route}.sha256"), move |_| {
        Response::ok(sidecar.clone())
    });
}

#[test]
fn update_swaps_the_running_store_copy_and_rewrites_the_shims() {
    let server = Server::start();
    serve_release(&server, "9.9.9", b"new jdk payload", b"new shim payload");
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let old = fs::read(&exe).unwrap();
    // An `.old` leftover of an earlier while-running update: the sweep must
    // clear it, and the swap then recreates it with the CURRENT old bytes.
    fs::write(
        world.root.join("bin").join("jdk.exe.old"),
        b"stale leftover",
    )
    .unwrap();

    let output = world.update(&exe, &[]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        fs::read(world.root.join("bin").join("jdk.exe")).unwrap(),
        b"new jdk payload"
    );
    assert_eq!(
        fs::read(world.root.join("bin").join("jdk.exe.old")).unwrap(),
        old,
        "the running exe was moved aside, not destroyed"
    );
    for tool in shims::TOOLS {
        assert_eq!(
            fs::read(world.root.join("shims").join(format!("{}.exe", tool.name))).unwrap(),
            b"new shim payload",
            "{}",
            tool.name
        );
    }
    let message = stderr(&output);
    assert!(message.contains(&format!("{LOCAL} → 9.9.9")), "{message}");
    assert!(
        !world.root.join("cache").join("update").exists(),
        "staging is cleaned up"
    );
}

#[test]
fn update_skips_when_already_on_the_latest_release() {
    let server = Server::start();
    serve_latest(&server, LOCAL);
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let before = fs::read(&exe).unwrap();

    let output = world.update(&exe, &[]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("already up to date"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(&exe).unwrap(), before, "nothing was touched");
    let route = format!("/download/v{LOCAL}/{}", asset(LOCAL));
    assert_eq!(server.hits(&route), 0, "no zip download");
    assert_eq!(server.hits(&format!("{route}.sha256")), 0, "no sidecar GET");
}

#[test]
fn update_refuses_a_binary_running_outside_the_store_bin() {
    let world = World::new(dead_url());

    // The build artifact itself — not the store copy at <root>\bin\jdk.exe.
    let output = world.update(Path::new(JDK), &[]);

    assert_ne!(output.status.code(), Some(0));
    let message = stderr(&output);
    assert!(message.contains("cargo install"), "{message}");
    assert!(message.contains("install.ps1"), "{message}");
}

#[test]
fn a_corrupt_release_hash_blocks_before_touching_the_store() {
    let server = Server::start();
    serve_latest(&server, "9.9.9");
    let zip = release_zip(b"new jdk payload", b"new shim payload");
    let route = format!("/download/v9.9.9/{}", asset("9.9.9"));
    // The sidecar promises the hash of DIFFERENT bytes.
    let sidecar = format!("{}  {}\n", sha256_hex(b"tampered"), asset("9.9.9"));
    server.route(&route, move |_| Response::ok(zip.clone()));
    server.route(&format!("{route}.sha256"), move |_| {
        Response::ok(sidecar.clone())
    });
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let before = fs::read(&exe).unwrap();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    assert!(
        stderr(&output).contains("sha256 mismatch"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(&exe).unwrap(), before, "bin\\jdk.exe is untouched");
    assert!(!world.root.join("bin").join("jdk.exe.old").exists());
}

#[test]
fn a_missing_release_asset_reports_the_arm64_best_effort_case() {
    let server = Server::start();
    serve_latest(&server, "9.9.9");
    // Neither the zip nor its sidecar is routed — the real shape of a
    // release without a build for this architecture (release.yml only
    // writes sidecars for assets it packaged).
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    assert!(stderr(&output).contains("arm64"), "{}", stderr(&output));
}
