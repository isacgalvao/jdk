//! The publication boundary: validate the copied tree, not just the generated
//! source. Script tests require Bash and rsync; Linux CI runs them unconditionally.

use jdk_core::index::{IndexEntry, IndexFile};
use std::collections::BTreeMap;
use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};
use test_support::{package, sha256_hex, stderr, stdout};

fn write_tree(dir: &Path, version: &str, arm: bool) {
    let mut files = Vec::new();
    for arch in ["x64", "aarch64"] {
        if arch == "aarch64" && !arm {
            continue;
        }
        for vendor in [
            "temurin",
            "zulu",
            "corretto",
            "liberica",
            "graalvm",
            "microsoft",
        ] {
            let mut pkg = package(version, "https://cdn.azul.com/jdk.zip", &"a".repeat(64), 1);
            pkg.vendor = vendor.to_string();
            pkg.arch = arch.to_string();
            let body = serde_json::to_vec_pretty(&vec![pkg]).unwrap();
            let path = format!("windows-{arch}/{vendor}.json");
            fs::create_dir_all(dir.join(format!("windows-{arch}"))).unwrap();
            fs::write(dir.join(&path), &body).unwrap();
            files.push(IndexEntry {
                path,
                vendor: vendor.to_string(),
                os: "windows".to_string(),
                arch: arch.to_string(),
                size: body.len() as u64,
                sha256: sha256_hex(&body),
            });
        }
    }
    let index = IndexFile {
        version: 1,
        updated: "2026-11-09T00:00:00Z".to_string(),
        files,
    };
    fs::write(
        dir.join("index.json"),
        serde_json::to_vec_pretty(&index).unwrap(),
    )
    .unwrap();
}

fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    let index = IndexFile::parse(&fs::read(dir.join("index.json")).unwrap()).unwrap();
    std::iter::once("index.json".to_string())
        .chain(index.files.into_iter().map(|entry| entry.path))
        .map(|path| {
            let file = dir.join(&path);
            let bytes = fs::read(&file).unwrap();
            let modified = fs::metadata(file).unwrap().modified().unwrap();
            (path, (bytes, modified))
        })
        .collect()
}

fn validate(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jdk-index-gen"))
        .arg("--out")
        .arg(dir)
        .args([
            "--validate-only",
            // Neither generation input may be read in this mode.
            "--foojay",
            "not-a-url",
            "--compare-to",
            "not-an-existing-directory",
        ])
        .output()
        .unwrap()
}

fn stage(source: &Path, destination: &Path) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/scripts/stage_index.sh");
    // BASH can select Git Bash instead of Windows' WSL launcher. Relative
    // tree paths keep Windows drive letters out of rsync's remote-host syntax.
    let mut command = Command::new(std::env::var_os("BASH").unwrap_or_else(|| "bash".into()));
    assert_eq!(source.parent(), destination.parent());
    command.current_dir(source.parent().unwrap());
    for path in [
        script.as_path(),
        Path::new(source.file_name().unwrap()),
        Path::new(destination.file_name().unwrap()),
        Path::new(env!("CARGO_BIN_EXE_jdk-index-gen")),
    ] {
        command.arg(path.to_str().unwrap().replace('\\', "/"));
    }
    command.output().expect("run Bash with rsync on PATH")
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

#[test]
fn validation_is_read_only_and_does_not_fetch() {
    let dir = tempfile::tempdir().unwrap();
    write_tree(dir.path(), "21.0.5", true);
    let before = snapshot(dir.path());
    let output = validate(dir.path());
    assert_ok(&output);
    assert!(stdout(&output).contains("index validated at"));
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn validation_rejects_equal_size_corruption_without_repairing_it() {
    let dir = tempfile::tempdir().unwrap();
    write_tree(dir.path(), "21.0.5", false);
    let path = dir.path().join("windows-x64/temurin.json");
    let body = fs::read_to_string(&path).unwrap();
    let corrupt = body.replace("21.0.5", "21.0.6");
    assert_eq!(body.len(), corrupt.len());
    fs::write(&path, corrupt).unwrap();
    let before = snapshot(dir.path());

    let output = validate(dir.path());
    assert!(!output.status.success());
    assert!(stderr(&output).contains("sha256 mismatch"), "{output:?}");
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn validation_rejects_missing_files_and_does_not_create_a_missing_tree() {
    let dir = tempfile::tempdir().unwrap();
    write_tree(dir.path(), "21.0.5", false);
    fs::remove_file(dir.path().join("windows-x64/temurin.json")).unwrap();
    let output = validate(dir.path());
    assert!(!output.status.success());
    assert!(stderr(&output).contains("temurin.json"), "{output:?}");

    let missing = dir.path().join("missing");
    assert!(!validate(&missing).status.success());
    assert!(!missing.exists());
}

#[test]
fn validation_enforces_schema_and_required_vendors() {
    let dir = tempfile::tempdir().unwrap();
    for (version, expected) in [(1, "no windows-x64 packages"), (2, "schema version 2")] {
        let index = IndexFile {
            version,
            updated: "2026-11-09T00:00:00Z".to_string(),
            files: Vec::new(),
        };
        fs::write(
            dir.path().join("index.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        let output = validate(dir.path());
        assert!(!output.status.success());
        assert!(stderr(&output).contains(expected), "{output:?}");
    }
}

#[test]
#[cfg_attr(not(unix), ignore = "requires Bash and rsync; exercised by Linux CI")]
fn staging_updates_equal_size_equal_mtime_files_on_both_architectures() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("staged tree");
    let destination = dir.path().join("published tree");
    write_tree(&source, "21.0.6", true);
    write_tree(&destination, "21.0.5", true);
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let expected = snapshot(&source);
    for (path, (body, _)) in &expected {
        if path == "index.json" {
            continue;
        }
        let old = fs::read(destination.join(path)).unwrap();
        assert_eq!(body.len(), old.len(), "fixture must defeat the size check");
        assert_ne!(*body, old);
        for tree in [&source, &destination] {
            File::options()
                .write(true)
                .open(tree.join(path))
                .unwrap()
                .set_times(FileTimes::new().set_modified(time))
                .unwrap();
            assert_eq!(
                fs::metadata(tree.join(path)).unwrap().modified().unwrap(),
                time
            );
        }
    }
    // Sync only the catalog directories, pruning obsolete entries without
    // touching the checkout's Git metadata or unrelated repository files.
    for arch in ["x64", "aarch64"] {
        fs::write(
            destination.join(format!("windows-{arch}/obsolete.json")),
            "old",
        )
        .unwrap();
    }
    fs::create_dir(destination.join(".git")).unwrap();
    fs::write(destination.join(".git/config"), "keep git").unwrap();
    fs::write(destination.join("README.md"), "keep readme").unwrap();

    assert_ok(&stage(&source, &destination));
    for (path, (body, _)) in expected {
        assert_eq!(fs::read(destination.join(path)).unwrap(), body);
    }
    for arch in ["x64", "aarch64"] {
        assert!(
            !destination
                .join(format!("windows-{arch}/obsolete.json"))
                .exists()
        );
    }
    assert_eq!(
        fs::read(destination.join(".git/config")).unwrap(),
        b"keep git"
    );
    assert_eq!(
        fs::read(destination.join("README.md")).unwrap(),
        b"keep readme"
    );

    let before = snapshot(&destination);
    assert_ok(&stage(&source, &destination));
    // cp may update index.json's mtime, but a repeated publication has no byte diff.
    for (path, (body, _)) in before {
        assert_eq!(fs::read(destination.join(path)).unwrap(), body);
    }
}

#[test]
#[cfg_attr(not(unix), ignore = "requires Bash and rsync; exercised by Linux CI")]
fn staging_removes_an_optional_architecture_absent_from_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("staged");
    let destination = dir.path().join("published");
    write_tree(&source, "21.0.6", false);
    write_tree(&destination, "21.0.5", true);
    assert_ok(&stage(&source, &destination));
    assert!(!destination.join("windows-aarch64").exists());
    assert_ok(&validate(&destination));
}

#[test]
#[cfg_attr(not(unix), ignore = "requires Bash and rsync; exercised by Linux CI")]
fn staging_fails_when_the_copied_tree_does_not_match_its_index() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("staged");
    let destination = dir.path().join("published");
    write_tree(&source, "21.0.6", true);
    write_tree(&destination, "21.0.5", true);
    let path = "windows-aarch64/temurin.json";
    let corrupt = fs::read_to_string(source.join(path))
        .unwrap()
        .replace("21.0.6", "21.0.7");
    fs::write(source.join(path), &corrupt).unwrap();

    let output = stage(&source, &destination);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("sha256 mismatch"), "{output:?}");
    assert_eq!(fs::read_to_string(destination.join(path)).unwrap(), corrupt);
}
