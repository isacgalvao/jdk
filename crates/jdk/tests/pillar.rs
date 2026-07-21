//! The M4 acceptance suite (the Windows pillar), fully hermetic. GOLDEN
//! RULE: no test touches the real `HKCU\Environment`, the real PATH, or
//! broadcasts a real WM_SETTINGCHANGE — every registry operation goes to a
//! disposable `HKCU\Software\jdk-test-*` subkey injected into the binary via
//! `JDK_ENV_KEY` / `JDK_MACHINE_ENV_KEY` (whose presence also suppresses the
//! broadcast; the injected-broadcast count itself is pinned by the unit
//! tests in `src\setup.rs`).
//!
//! The "already-open console" proof runs the real shim copy: its resolution
//! goes through the `current` junction on every invocation, so after
//! `jdk use` the SAME process environment resolves the new JDK.

use jdk_core::current::{self, Current};
use jdk_core::env::{self, RegType};
use jdk_core::index::{IndexEntry, current_platform};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
use test_support::reg::TestKey;
use test_support::{Response, Server, dead_url, index_json, sha256_hex, shim_binaries};

const JDK: &str = env!("CARGO_BIN_EXE_jdk");

struct World {
    _temp: TempDir,
    root: PathBuf,
    project: PathBuf,
    user: TestKey,
    machine: TestKey,
    shim_source: PathBuf,
    fake_java: PathBuf,
    /// `JDK_RELEASES` for the spawned binary: a dead port by default (the
    /// doctor's release probe must stay hermetic); tests point it at a
    /// loopback release server.
    releases: String,
}

impl World {
    fn new() -> World {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let project = temp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        // A non-java boundary: the cascade stops here, so resolution is
        // global regardless of what exists above the temp dir.
        fs::write(project.join(".tool-versions"), "nodejs 20.10.0\n").unwrap();
        let (shim_source, fake_java) = shim_binaries();
        World {
            _temp: temp,
            root,
            project,
            user: TestKey::create(),
            machine: TestKey::create(),
            shim_source,
            fake_java,
            releases: dead_url(),
        }
    }

    fn jdk(&self, args: &[&str]) -> Output {
        Command::new(JDK)
            .args(args)
            .current_dir(&self.project)
            .env("JDK_ROOT", &self.root)
            .env("JDK_ENV_KEY", self.user.path())
            .env("JDK_MACHINE_ENV_KEY", self.machine.path())
            .env("JDK_INDEX", dead_url())
            .env("JDK_FOOJAY", dead_url())
            .env("JDK_RELEASES", &self.releases)
            .output()
            .unwrap()
    }

    fn setup(&self, extra: &[&str]) -> Output {
        let source = self.shim_source.to_string_lossy().into_owned();
        let mut args = vec!["setup", "--shim-source", &source];
        args.extend_from_slice(extra);
        self.jdk(&args)
    }

    /// An installed fake candidate whose java.exe is the real `fake_java`
    /// fixture — executable, and it prints its own path when run.
    fn install_fake(&self, name: &str) -> PathBuf {
        let dir = self.root.join("candidates").join("java").join(name);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::copy(&self.fake_java, dir.join("bin").join("java.exe")).unwrap();
        dir
    }

    /// Runs the materialized shim copy the way an already-open console
    /// would: same process environment across invocations, only JDK_ROOT
    /// set. The shim never reads the registry — that is the point.
    fn run_shim(&self) -> Output {
        Command::new(self.root.join("shims").join("java.exe"))
            .arg("-version")
            .current_dir(&self.project)
            .env("JDK_ROOT", &self.root)
            .output()
            .unwrap()
    }

    fn raw_value(&self, name: &str) -> (Vec<u8>, RegType) {
        let raw = self.user.key.get_raw_value(name).unwrap();
        (raw.bytes.to_vec(), raw.vtype)
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_ok(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(output));
}

#[test]
fn setup_provisions_shims_registry_and_junction_then_reruns_as_a_no_op() {
    let world = World::new();
    world.install_fake("temurin@21.0.5");
    // Pre-existing user PATH with an unexpanded variable: value AND type
    // must survive the prepend byte-for-byte.
    world
        .user
        .key
        .set_raw_value(
            "Path",
            &env::string_value(r"%USERPROFILE%\bin;C:\tools", RegType::REG_EXPAND_SZ),
        )
        .unwrap();
    // The bytes of the binary about to run setup — its self-copy must match.
    let cli_payload = fs::read(JDK).unwrap();

    assert_ok(&world.setup(&[]));

    // Shims: byte-identical copies of jdk-shim.exe for the whole v0.1 set.
    let shim_payload = fs::read(&world.shim_source).unwrap();
    for tool in ["java", "javac", "jar", "javadoc", "jshell", "keytool"] {
        let copy = fs::read(world.root.join("shims").join(format!("{tool}.exe"))).unwrap();
        assert!(copy == shim_payload, "{tool}.exe must be byte-identical");
    }
    // The CLI itself lands in the store (decision 7 layout).
    let stored_cli = fs::read(world.root.join("bin").join("jdk.exe")).unwrap();
    assert!(
        stored_cli == cli_payload,
        "bin\\jdk.exe must be a byte-identical copy of the running jdk.exe"
    );
    // JAVA_HOME: the junction path, REG_SZ, in the DISPOSABLE key.
    let java_home = env::read(&world.user.key, "JAVA_HOME").unwrap().unwrap();
    assert_eq!(java_home.text, world.root.join("current").to_string_lossy());
    assert!(!java_home.expandable, "decision 8: absolute path as REG_SZ");
    // PATH: shims then bin prepended once each, original text and
    // REG_EXPAND_SZ type preserved.
    let path = env::read(&world.user.key, "Path").unwrap().unwrap();
    assert_eq!(
        path.text,
        format!(
            r"{};{};%USERPROFILE%\bin;C:\tools",
            world.root.join("shims").display(),
            world.root.join("bin").display()
        )
    );
    assert!(path.expandable, "REG_EXPAND_SZ must stay REG_EXPAND_SZ");
    // Junction: created towards the only eligible global.
    assert_eq!(
        current::inspect(&world.root).unwrap(),
        Current::Junction {
            target: world
                .root
                .join("candidates")
                .join("java")
                .join("temurin@21.0.5")
        }
    );

    // Second run: clean no-op — same registry bytes, no duplication.
    let before_java_home = world.raw_value("JAVA_HOME");
    let before_path = world.raw_value("Path");
    let rerun = world.setup(&[]);
    assert_ok(&rerun);
    let messages = stderr(&rerun);
    assert!(messages.contains("shims already up to date"), "{messages}");
    assert!(messages.contains("JAVA_HOME already set"), "{messages}");
    assert!(
        messages.contains("PATH already contains the shims directory"),
        "{messages}"
    );
    assert!(
        messages.contains("PATH already contains the bin directory"),
        "{messages}"
    );
    assert!(messages.contains("junction already set"), "{messages}");
    assert_eq!(world.raw_value("JAVA_HOME"), before_java_home);
    assert_eq!(world.raw_value("Path"), before_path);
    let path = env::read(&world.user.key, "Path").unwrap().unwrap();
    assert_eq!(
        env::path_count(&path.text, &world.root.join("shims")),
        1,
        "never duplicated"
    );
    assert_eq!(
        env::path_count(&path.text, &world.root.join("bin")),
        1,
        "never duplicated"
    );
}

#[test]
fn setup_without_a_global_candidate_reports_the_next_step() {
    let world = World::new();

    let output = world.setup(&[]);

    assert_ok(&output);
    let messages = stderr(&output);
    assert!(messages.contains("no JDK installed yet"), "{messages}");
    assert!(messages.contains("jdk install"), "{messages}");
    assert_eq!(current::inspect(&world.root).unwrap(), Current::Absent);
    // JAVA_HOME is still written — the junction path is fixed by contract
    // and becomes valid on the first `jdk use`.
    assert!(env::read(&world.user.key, "JAVA_HOME").unwrap().is_some());
}

#[test]
fn setup_refuses_a_foreign_java_home_without_consent_and_backs_it_up_with_yes() {
    let world = World::new();
    world.install_fake("temurin@21.0.5");
    world
        .user
        .key
        .set_raw_value(
            "JAVA_HOME",
            &env::string_value(r"%JAVA17%\home", RegType::REG_EXPAND_SZ),
        )
        .unwrap();

    // Non-TTY without --yes: actionable error, NOTHING mutated.
    let refused = world.setup(&[]);
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    let untouched = env::read(&world.user.key, "JAVA_HOME").unwrap().unwrap();
    assert_eq!(untouched.text, r"%JAVA17%\home");
    assert!(untouched.expandable);
    assert!(
        env::read(&world.user.key, "Path").unwrap().is_none(),
        "a refused setup mutates nothing"
    );

    // --yes: replaced, and the old value+type saved for a future undo.
    assert_ok(&world.setup(&["--yes"]));
    let replaced = env::read(&world.user.key, "JAVA_HOME").unwrap().unwrap();
    assert_eq!(replaced.text, world.root.join("current").to_string_lossy());
    let backup = jdk_core::config::java_home_before(&world.root)
        .unwrap()
        .expect("the foreign value must be backed up");
    assert_eq!(backup.value, r"%JAVA17%\home");
    assert!(backup.expandable, "the registry TYPE is part of the backup");
}

#[test]
fn use_retargets_atomically_and_an_open_console_resolves_the_new_jdk() {
    let world = World::new();
    let jdk17 = world.install_fake("temurin@17.0.9");
    let jdk21 = world.install_fake("temurin@21.0.5");
    assert_ok(&world.setup(&[]));
    assert_eq!(
        current::inspect(&world.root).unwrap(),
        Current::Junction {
            target: jdk21.clone()
        }
    );

    // "Console already open": the shim copy runs with a FIXED process
    // environment (JDK_ROOT only) before and after the switch. The spawn
    // path it reports IS the junction path (resolution goes through the
    // junction on every invocation); canonicalizing it names the JDK that
    // actually ran.
    assert_eq!(
        shim_ran(&world),
        canonical(&jdk21.join("bin").join("java.exe"))
    );

    let switched = world.jdk(&["use", "17"]);
    assert_ok(&switched);
    assert!(
        stderr(&switched).contains("temurin@17.0.9"),
        "{}",
        stderr(&switched)
    );
    assert_eq!(
        current::inspect(&world.root).unwrap(),
        Current::Junction {
            target: jdk17.clone()
        }
    );

    // Same environment, new JDK — the junction did the switch.
    assert_eq!(
        shim_ran(&world),
        canonical(&jdk17.join("bin").join("java.exe"))
    );
    // No staging leftovers from the swap.
    assert!(!world.root.join("current.new").exists());
}

/// Runs the shim and returns the java.exe that actually executed, resolved
/// to its store candidate. Asserts on the way that the shim spawned THROUGH
/// the junction path, not a resolved copy of it.
fn shim_ran(world: &World) -> PathBuf {
    let output = world.run_shim();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let listing = stdout(&output);
    let reported = listing
        .lines()
        .find_map(|line| line.strip_prefix("fake-java exe="))
        .unwrap_or_else(|| panic!("no fake-java exe marker in:\n{listing}"));
    assert_eq!(
        PathBuf::from(reported),
        world.root.join("current").join("bin").join("java.exe"),
        "the shim resolves the global VIA the junction"
    );
    canonical(Path::new(reported))
}

/// Canonicalizes a path and strips the `\\?\` prefix. Both sides of a shim
/// path comparison must go through this: a runner whose temp dir sits under an
/// 8.3 short name (e.g. `RUNNER~1` for `runneradmin`) canonicalizes to the long
/// name, so an un-canonicalized expected path would spuriously differ.
fn canonical(path: &Path) -> PathBuf {
    let resolved = fs::canonicalize(path).expect("path resolves");
    match resolved.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => resolved,
    }
}

#[test]
fn use_of_a_missing_candidate_is_an_actionable_not_installed_error() {
    let world = World::new();
    world.install_fake("temurin@17.0.9");

    let output = world.jdk(&["use", "zulu@21"]);

    assert_eq!(output.status.code(), Some(4));
    let message = stderr(&output);
    assert!(
        message.contains("no installed JDK matches zulu@21"),
        "{message}"
    );
    assert!(message.contains("jdk install zulu@21"), "{message}");
    assert!(message.contains("temurin@17.0.9"), "{message}");
}

/// A fully provisioned sandbox: setup done, global set, healthy registry.
fn healthy() -> World {
    let world = World::new();
    world.install_fake("temurin@21.0.5");
    assert_ok(&world.setup(&[]));
    world
}

/// The verdict line of one named check. Names can prefix each other
/// ("PATH", "PATH type"), so a match requires the two-space padding gap the
/// report prints after the name column.
fn doctor_line<'a>(report: &'a str, name: &str) -> &'a str {
    report
        .lines()
        .find(|line| {
            let Some(marker) = line.chars().next() else {
                return false;
            };
            if !matches!(marker, '✓' | '✗' | '!') {
                return false;
            }
            line[marker.len_utf8()..]
                .strip_prefix(' ')
                .and_then(|rest| rest.strip_prefix(name))
                .is_some_and(|after| after.starts_with("  "))
        })
        .unwrap_or_else(|| panic!("no `{name}` line in:\n{report}"))
}

/// Asserts the named check is ✗, its remediation mentions `fix`, and the
/// whole run exits non-zero.
fn assert_broken(world: &World, name: &str, detail: &str, fix: &str) {
    let output = world.jdk(&["doctor"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor must fail; stdout:\n{}",
        stdout(&output)
    );
    let report = stdout(&output);
    let line = doctor_line(&report, name);
    assert!(line.starts_with('✗'), "expected ✗ on {name}: {line}");
    assert!(line.contains(detail), "expected {detail:?} in: {line}");
    assert!(
        report.contains(fix),
        "expected remediation {fix:?} in:\n{report}"
    );
}

#[test]
fn doctor_reports_all_clear_in_a_healthy_sandbox() {
    let world = healthy();

    let output = world.jdk(&["doctor"]);

    assert_ok(&output);
    let report = stdout(&output);
    for name in [
        "store",
        "config",
        "shims",
        "junction",
        "JAVA_HOME",
        "machine env",
        "PATH",
        "PATH bin",
        "PATH type",
        "PATH length",
        "pin",
        "cache",
        "jdk.exe",
    ] {
        let line = doctor_line(&report, name);
        assert!(
            line.starts_with('✓'),
            "expected ✓ on {name} in a healthy sandbox: {line}"
        );
    }
    // Offline is NOT a disease: the dead index and the dead release source
    // are notes, never failures.
    assert!(doctor_line(&report, "index").starts_with('!'), "{report}");
    let update = doctor_line(&report, "update");
    assert!(update.starts_with('!'), "{report}");
    assert!(update.contains("unreachable"), "{update}");
}

/// A loopback release source whose `/latest` redirect lands on `version`.
fn release_server(version: &str) -> Server {
    let server = Server::start();
    let target = format!("{}/tag/v{version}", server.url());
    server.route("/latest", move |_| Response::redirect(&target));
    server.route(&format!("/tag/v{version}"), |_| {
        Response::ok("release page html")
    });
    server
}

#[test]
fn doctor_notes_a_newer_release_without_failing() {
    let mut world = healthy();
    let server = release_server("99.0.0");
    world.releases = server.url().to_string();

    let output = world.jdk(&["doctor"]);

    assert_ok(&output);
    let report = stdout(&output);
    let line = doctor_line(&report, "update");
    assert!(line.starts_with('!'), "informative, not broken: {line}");
    assert!(line.contains("jdk update"), "{line}");
    assert!(line.contains("99.0.0"), "{line}");
}

#[test]
fn doctor_passes_the_update_check_on_the_latest_release() {
    let mut world = healthy();
    let server = release_server(env!("CARGO_PKG_VERSION"));
    world.releases = server.url().to_string();

    let output = world.jdk(&["doctor"]);

    assert_ok(&output);
    let report = stdout(&output);
    let line = doctor_line(&report, "update");
    assert!(line.starts_with('✓'), "{line}");
    assert!(line.contains("latest release"), "{line}");
}

#[test]
fn doctor_names_a_dead_junction_target() {
    let world = healthy();
    fs::remove_dir_all(
        world
            .root
            .join("candidates")
            .join("java")
            .join("temurin@21.0.5"),
    )
    .unwrap();

    assert_broken(&world, "junction", "no longer exists", "jdk use <version>");
}

#[test]
fn doctor_names_a_path_missing_the_shims_entry() {
    let world = healthy();
    world
        .user
        .key
        .set_raw_value(
            "Path",
            &env::string_value(r"C:\tools", RegType::REG_EXPAND_SZ),
        )
        .unwrap();

    assert_broken(&world, "PATH", "does not contain", "jdk setup");
}

#[test]
fn doctor_names_a_path_missing_the_bin_entry() {
    let world = healthy();
    // Shims present, bin gone: every java tool works but `jdk` itself
    // vanishes from new shells — the M6 product gap this check guards.
    world
        .user
        .key
        .set_raw_value(
            "Path",
            &env::string_value(
                &world.root.join("shims").display().to_string(),
                RegType::REG_EXPAND_SZ,
            ),
        )
        .unwrap();

    assert_broken(&world, "PATH bin", "not callable", "jdk setup");
}

#[test]
fn doctor_names_duplicated_shims_entries() {
    let world = healthy();
    let shims = world.root.join("shims");
    world
        .user
        .key
        .set_raw_value(
            "Path",
            &env::string_value(
                &format!("{};{}", shims.display(), shims.display()),
                RegType::REG_EXPAND_SZ,
            ),
        )
        .unwrap();

    assert_broken(&world, "PATH", "2 times", "keep a single entry");
}

#[test]
fn doctor_names_a_literal_percent_var_stored_as_reg_sz() {
    let world = healthy();
    // Anti-model 1: what setx leaves behind — the literal never expands.
    world
        .user
        .key
        .set_raw_value(
            "JAVA_HOME",
            &env::string_value(r"%USERPROFILE%\.jdk\current", RegType::REG_SZ),
        )
        .unwrap();

    assert_broken(&world, "JAVA_HOME", "setx damage", "jdk setup --yes");
}

#[test]
fn doctor_names_a_reg_sz_path_with_literal_variables() {
    let world = healthy();
    let shims = world.root.join("shims");
    world
        .user
        .key
        .set_raw_value(
            "Path",
            &env::string_value(
                &format!(r"{};%SystemRoot%\bin", shims.display()),
                RegType::REG_SZ,
            ),
        )
        .unwrap();

    assert_broken(&world, "PATH type", "setx damage", "REG_EXPAND_SZ");
}

#[test]
fn doctor_accepts_a_lone_percent_in_a_reg_sz_path() {
    let world = healthy();
    let shims = world.root.join("shims");
    let bin = world.root.join("bin");
    // A bare `%` is not an expansion pattern: REG_SZ here is odd but not
    // the setx damage the check hunts for.
    world
        .user
        .key
        .set_raw_value(
            "Path",
            &env::string_value(
                &format!(r"{};{};C:\50%discount\bin", shims.display(), bin.display()),
                RegType::REG_SZ,
            ),
        )
        .unwrap();

    let output = world.jdk(&["doctor"]);

    assert_ok(&output);
    let report = stdout(&output);
    let line = doctor_line(&report, "PATH type");
    assert!(line.starts_with('✓'), "no false positive: {line}");
}

#[test]
fn doctor_names_a_java_home_pointing_nowhere() {
    let world = healthy();
    world
        .user
        .key
        .set_raw_value(
            "JAVA_HOME",
            &env::string_value(r"C:\ghost\of\an\interrupted\switcher", RegType::REG_SZ),
        )
        .unwrap();

    assert_broken(&world, "JAVA_HOME", "does not exist", "jdk setup --yes");
}

#[test]
fn doctor_names_a_machine_scope_java_home_conflict() {
    let world = healthy();
    world
        .machine
        .key
        .set_raw_value(
            "JAVA_HOME",
            &env::string_value(r"C:\Program Files\Java\jdk8", RegType::REG_SZ),
        )
        .unwrap();

    assert_broken(
        &world,
        "machine env",
        "machine scope sets JAVA_HOME",
        "reg delete",
    );
}

#[test]
fn doctor_names_a_corrupt_cache() {
    let world = healthy();
    let (os, arch) = current_platform();
    let body = b"tampered bytes".to_vec();
    let index = index_json(vec![IndexEntry {
        path: format!("{os}-{arch}/temurin.json"),
        vendor: "temurin".to_string(),
        os: os.to_string(),
        arch: arch.to_string(),
        size: body.len() as u64,
        sha256: sha256_hex(b"what the index PROMISED"),
    }]);
    let cache = world.root.join("cache").join("index");
    fs::create_dir_all(cache.join(format!("{os}-{arch}"))).unwrap();
    fs::write(cache.join("index.json"), index).unwrap();
    fs::write(
        cache.join(format!("{os}-{arch}")).join("temurin.json"),
        &body,
    )
    .unwrap();

    assert_broken(&world, "cache", "sha256 mismatch", "re-downloads");
}

#[test]
fn doctor_notes_a_path_at_the_setx_truncation_point() {
    let world = healthy();
    let shims = world.root.join("shims").display().to_string();
    let bin = world.root.join("bin").display().to_string();
    // Exactly 1024 chars, shims and bin included — the setx fingerprint.
    let mut text = format!("{shims};{bin};");
    text.push_str(&"x".repeat(1024 - text.chars().count()));
    world
        .user
        .key
        .set_raw_value("Path", &env::string_value(&text, RegType::REG_EXPAND_SZ))
        .unwrap();

    let output = world.jdk(&["doctor"]);
    assert_ok(&output);
    let report = stdout(&output);
    let line = doctor_line(&report, "PATH length");
    assert!(line.starts_with('!'), "informative, not broken: {line}");
    assert!(line.contains("truncation"), "{line}");
}
