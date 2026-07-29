//! Hermetic `jdk update` e2e: the store copy of the real jdk.exe updates
//! itself against a loopback release server (`JDK_RELEASES` is the URL
//! injection point, like `JDK_INDEX`, and `JDK_RELEASE_PUBKEY` the trust
//! anchor that goes with it — a test server cannot hold the production
//! signing key). The fake bundle carries marker payloads that never execute
//! — the swap is in-process, which is exactly what makes this test possible.

use jdk_core::{release, shims};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
use test_support::{
    Response, Server, dead_url, release_zip, release_zip_padded, sha256_hex, sshsig,
};

const JDK: &str = env!("CARGO_BIN_EXE_jdk");
/// What the binary under test believes its own version is.
const LOCAL: &str = env!("CARGO_PKG_VERSION");

struct World {
    _temp: TempDir,
    root: PathBuf,
    releases: String,
    /// The key the updater under test is told to trust; every test but
    /// [`update_refuses_a_release_signed_by_an_untrusted_key`] uses the one
    /// the fixtures are signed with.
    pubkey: String,
}

impl World {
    fn new(releases: String) -> World {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        World {
            _temp: temp,
            root,
            releases,
            pubkey: sshsig::pubkey(),
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
            .env("JDK_RELEASE_PUBKEY", &self.pubkey)
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
/// bundle zip and the signed `SHA256SUMS` covering it.
fn serve_release(server: &Server, version: &str, jdk_exe: &[u8], shim_exe: &[u8]) {
    serve_latest(server, version);
    let zip = release_zip(jdk_exe, shim_exe);
    let sums = format!("{}  {}\n", sha256_hex(&zip), asset(version));
    server.route(
        &format!("/download/v{version}/{}", asset(version)),
        move |_| Response::ok(zip.clone()),
    );
    serve_sums(server, version, &sums, &sums);
}

/// The release's two signature artifacts: `served` as the `SHA256SUMS` body,
/// signed over `signed`. Passing different values is how a test tampers with
/// the sums after they were signed.
fn serve_sums(server: &Server, version: &str, served: &str, signed: &str) {
    let signature = sshsig::sign(signed.as_bytes(), "jdk-release");
    let served = served.to_string();
    server.route(&format!("/download/v{version}/SHA256SUMS"), move |_| {
        Response::ok(served.clone())
    });
    server.route(&format!("/download/v{version}/SHA256SUMS.sig"), move |_| {
        Response::ok(signature.clone())
    });
}

#[test]
fn update_swaps_the_running_store_copy_and_rewrites_the_shims() {
    let server = Server::start();
    serve_release(&server, "9.9.9", b"new jdk payload", b"new shim payload");
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let old = fs::read(&exe).unwrap();
    // Leftovers of an earlier interrupted update: the sweep must clear both
    // (jdk.exe is in place, so the aside is superseded rather than precious),
    // and the swap then recreates the `.old` with the CURRENT old bytes.
    fs::write(
        world.root.join("bin").join("jdk.exe.old"),
        b"stale leftover",
    )
    .unwrap();
    fs::write(
        world.root.join("bin").join("jdk.exe.new"),
        b"orphaned staging",
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
    // BUG-01: the bundle's shim stays in `bin` after the staging is gone, so
    // the `jdk setup` doctor recommends still has a source to materialize from.
    assert_eq!(
        fs::read(world.root.join("bin").join("jdk-shim.exe")).unwrap(),
        b"new shim payload",
        "the update must leave jdk-shim.exe next to the new jdk.exe"
    );
    assert!(
        !world.root.join("bin").join("jdk.exe.new").exists(),
        "no staging orphan survives the update"
    );
    let message = stderr(&output);
    assert!(message.contains(&format!("{LOCAL} → 9.9.9")), "{message}");
    assert!(
        !message.contains("retrying"),
        "BUG-13's announcement must stay out of a healthy run: {message}"
    );
    assert!(
        !world.root.join("cache").join("update").exists(),
        "staging is cleaned up"
    );
}

/// BUG-13: on a bad link the retry schedule used to look like a hang. One
/// line per retry, on stderr, from the first one — and only from the first
/// one, which is what [`update_swaps_the_running_store_copy_and_rewrites_the_shims`]
/// pins from the other side by staying silent.
#[test]
fn a_retried_request_says_so_on_stderr() {
    let server = Server::start();
    let target = format!("{}/tag/v{LOCAL}", server.url());
    let calls = std::sync::atomic::AtomicUsize::new(0);
    server.route("/latest", move |_| {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            // Retry-After: 0 keeps the backoff out of the test's wall clock.
            Response::empty(503).with_header("Retry-After", "0")
        } else {
            Response::redirect(&target)
        }
    });
    server.route(&format!("/tag/v{LOCAL}"), |_| Response::ok("release page"));
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();

    let output = world.update(&exe, &[]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let message = stderr(&output);
    let announced = message.matches("retrying in").count();
    assert_eq!(announced, 2, "one line per retry, no more: {message}");
    assert!(message.contains("attempt 2 of 3"), "{message}");
    assert!(message.contains("attempt 3 of 3"), "{message}");
    assert!(message.contains("503"), "the reason is named: {message}");
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
    assert_eq!(
        server.hits(&format!("/download/v{LOCAL}/SHA256SUMS")),
        0,
        "an update that is not happening fetches no signature either"
    );
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
    // The signed sums promise the hash of DIFFERENT bytes.
    let sums = format!("{}  {}\n", sha256_hex(b"tampered"), asset("9.9.9"));
    server.route(&format!("/download/v9.9.9/{}", asset("9.9.9")), move |_| {
        Response::ok(zip.clone())
    });
    serve_sums(&server, "9.9.9", &sums, &sums);
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

/// SEC-01 end to end: a release host that hands out a payload of its choosing
/// cannot make the updater install it, because it cannot produce a signature
/// for the sums that describe it. This is the scenario the whole anchor
/// exists for — the update path replaces the binary every shim invokes.
#[test]
fn a_release_whose_sums_were_edited_after_signing_never_reaches_the_store() {
    let server = Server::start();
    serve_latest(&server, "9.9.9");
    let zip = release_zip(b"malicious jdk", b"malicious shim");
    let route = format!("/download/v9.9.9/{}", asset("9.9.9"));
    let honest = format!("{}  {}\n", sha256_hex(b"the real release"), asset("9.9.9"));
    let forged = format!("{}  {}\n", sha256_hex(&zip), asset("9.9.9"));
    server.route(&route, move |_| Response::ok(zip.clone()));
    serve_sums(&server, "9.9.9", &forged, &honest);
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let before = fs::read(&exe).unwrap();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    let message = stderr(&output);
    assert!(message.contains("does not verify"), "{message}");
    assert_eq!(fs::read(&exe).unwrap(), before, "bin\\jdk.exe is untouched");
    assert_eq!(server.hits(&route), 0, "the payload was never downloaded");
}

/// The `JDK_RELEASE_PUBKEY` override is not decoration: point the updater at
/// a key the release was not signed with and the same release stops
/// installing. A key that were merely read and ignored would pass this test
/// only by installing.
#[test]
fn update_refuses_a_release_signed_by_an_untrusted_key() {
    let server = Server::start();
    serve_release(&server, "9.9.9", b"new jdk payload", b"new shim payload");
    let mut world = World::new(server.url().to_string());
    world.pubkey = sshsig::STRANGER.to_string();
    let exe = world.place_store_copy();
    let before = fs::read(&exe).unwrap();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    assert!(
        stderr(&output).contains("other than the one"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(&exe).unwrap(), before, "bin\\jdk.exe is untouched");
}

/// Anti-stripping end to end: the zip and its sums are there, the signature
/// is not. Refusing rather than falling back to the sums alone is what keeps
/// "unsigned" from being a downgrade a release host can choose.
#[test]
fn update_refuses_a_release_with_no_signature() {
    let server = Server::start();
    serve_release(&server, "9.9.9", b"new jdk payload", b"new shim payload");
    server.route("/download/v9.9.9/SHA256SUMS.sig", |_| Response::empty(404));
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let before = fs::read(&exe).unwrap();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    let message = stderr(&output);
    assert!(message.contains("is not signed"), "{message}");
    assert!(message.contains("install.ps1"), "{message}");
    assert_eq!(fs::read(&exe).unwrap(), before, "bin\\jdk.exe is untouched");
}

/// The zip is published and signed for, but the signed sums cover another
/// release's asset — BUG-09's replay shape, since the asset name carries the
/// version the updater was promised.
#[test]
fn update_refuses_a_zip_the_signed_sums_do_not_cover() {
    let server = Server::start();
    serve_release(&server, "9.9.9", b"new jdk payload", b"new shim payload");
    let stale = format!("{}  {}\n", sha256_hex(b"whatever"), asset("9.9.8"));
    serve_sums(&server, "9.9.9", &stale, &stale);
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let before = fs::read(&exe).unwrap();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    assert!(
        stderr(&output).contains("does not cover it"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(&exe).unwrap(), before, "bin\\jdk.exe is untouched");
}

/// BUG-10: the update extracts with bundle-sized ceilings, not the 4 GiB and
/// 200k entries a real JDK needs. The signature verifies and the hash matches
/// — the archive is simply not shaped like a release bundle — so with the old
/// `extract_zip` this installs cleanly and the test fails by SUCCEEDING.
#[test]
fn update_refuses_a_bundle_past_the_entry_ceiling() {
    let server = Server::start();
    serve_latest(&server, "9.9.9");
    let zip = release_zip_padded(b"new jdk payload", b"new shim payload", 80);
    let sums = format!("{}  {}\n", sha256_hex(&zip), asset("9.9.9"));
    server.route(&format!("/download/v9.9.9/{}", asset("9.9.9")), move |_| {
        Response::ok(zip.clone())
    });
    serve_sums(&server, "9.9.9", &sums, &sums);
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();
    let before = fs::read(&exe).unwrap();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    assert!(
        stderr(&output).contains("64-entry ceiling"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(&exe).unwrap(), before, "bin\\jdk.exe is untouched");
}

#[test]
fn a_missing_release_asset_reports_the_arm64_best_effort_case() {
    let server = Server::start();
    serve_latest(&server, "9.9.9");
    // The release is signed and complete, it just carries no build for this
    // architecture — so its sums name some other asset and the zip 404s.
    let other = format!("{}  jdk-v9.9.9-windows-other.zip\n", sha256_hex(b"other"));
    serve_sums(&server, "9.9.9", &other, &other);
    let world = World::new(server.url().to_string());
    let exe = world.place_store_copy();

    let output = world.update(&exe, &[]);

    assert_ne!(output.status.code(), Some(0));
    assert!(stderr(&output).contains("arm64"), "{}", stderr(&output));
}
