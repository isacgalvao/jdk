//! M3 acceptance (plan): the pin→current→which flow is coherent with the
//! shim's resolution — same lib, proven with the REAL binaries: the path
//! `jdk which` prints is the executable a byte-identical shim copy actually
//! spawns (the fake java prints its own path as evidence).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;
use test_support::shim_binaries;

const JDK: &str = env!("CARGO_BIN_EXE_jdk");

#[test]
fn pin_current_which_and_the_shim_agree_on_the_same_executable() {
    let (shim, fake_java) = shim_binaries();
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("root");
    let project = temp.path().join("proj");
    let deep = project.join("src").join("main").join("java");
    fs::create_dir_all(&deep).unwrap();

    // Two installed candidates: resolution must pick 21, not 17.
    for name in ["temurin@21.0.4", "temurin@17.0.9"] {
        let bin = root.join("candidates").join("java").join(name).join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::copy(&fake_java, bin.join("java.exe")).unwrap();
    }
    let shims = temp.path().join("shims");
    fs::create_dir_all(&shims).unwrap();
    fs::copy(&shim, shims.join("java.exe")).unwrap();

    let jdk = |args: &[&str], dir: &std::path::Path| {
        let output = Command::new(JDK)
            .args(args)
            .current_dir(dir)
            .env("JDK_ROOT", &root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "jdk {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    // pin writes the file the shim will read.
    jdk(&["pin", "temurin@21"], &project);
    assert_eq!(
        fs::read_to_string(project.join(".jdkrc")).unwrap(),
        "java=temurin@21\n"
    );

    // current, from a deep subdirectory, explains exactly that pin.
    let current = jdk(&["current"], &deep);
    assert!(
        current.contains("temurin@21 by") && current.contains(".jdkrc"),
        "current output: {current}"
    );
    assert!(
        current.contains("resolved:  temurin@21.0.4"),
        "current output: {current}"
    );

    // which prints the executable path...
    let which = jdk(&["which"], &deep);
    let cli_path = PathBuf::from(which.trim());
    assert!(
        cli_path.ends_with("bin\\java.exe") || cli_path.ends_with("bin/java.exe"),
        "which printed: {which}"
    );

    // ...and the real shim spawns THE SAME executable.
    let output = Command::new(shims.join("java.exe"))
        .current_dir(&deep)
        .env("JDK_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "shim failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let shim_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix("fake-java exe="))
        .expect("fake java prints its own path");

    assert_eq!(
        fs::canonicalize(&cli_path).unwrap(),
        fs::canonicalize(PathBuf::from(shim_path)).unwrap(),
        "jdk which and the shim resolved different executables"
    );
    assert!(
        cli_path.to_string_lossy().contains("temurin@21.0.4"),
        "resolution picked the wrong candidate: {}",
        cli_path.display()
    );
}
