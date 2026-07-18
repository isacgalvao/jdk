//! Hermetic end-to-end tests of the shim's resolution: pin cascade, store
//! matching, tool dispatch and exit-code propagation (the sandbox lives in
//! `common`). Auto-install behavior has its own suite in `auto_install.rs`.

mod common;

use common::{FAKE_JAVA, SHIM, sandbox, sandbox_at, stderr, stdout};
use std::fs;
use std::process::Command;

#[test]
fn resolves_sdkmanrc_from_deep_subdirectory_and_forwards_args() {
    let sandbox = sandbox();
    sandbox.install("temurin@21.0.4");
    fs::write(sandbox.project.join(".sdkmanrc"), "java=21.0.4-tem\r\n").unwrap();

    let output = sandbox
        .shim("java", &sandbox.deep())
        .args(["-version", "--flag"])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("fake-java argv=[-version --flag]"),
        "stdout: {}",
        stdout(&output)
    );
}

#[test]
fn propagates_the_child_exit_code() {
    let sandbox = sandbox();
    sandbox.install("temurin@21.0.4");
    fs::write(sandbox.project.join(".sdkmanrc"), "java=21.0.4-tem\n").unwrap();

    let output = sandbox
        .shim("java", &sandbox.project)
        .env("FAKE_JAVA_EXIT", "7")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(7));
    assert!(stdout(&output).contains("fake-java argv=[]"));
}

#[test]
fn pinned_version_not_installed_is_actionable() {
    let sandbox = sandbox();
    sandbox.install("temurin@21.0.4");
    fs::write(sandbox.project.join(".sdkmanrc"), "java=22-tem\n").unwrap();

    let output = sandbox.shim("java", &sandbox.project).output().unwrap();

    assert_eq!(output.status.code(), Some(4));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("jdk install temurin@22"),
        "stderr: {stderr}"
    );
}

#[test]
fn no_pin_and_no_global_is_actionable() {
    let sandbox = sandbox();
    // A source file that does not pin java makes the project the cascade
    // boundary — keeping the test hermetic against pins above the temp dir.
    fs::write(sandbox.project.join(".tool-versions"), "nodejs 20.10.0\n").unwrap();

    let output = sandbox.shim("java", &sandbox.deep()).output().unwrap();

    assert_eq!(output.status.code(), Some(4));
    let stderr = stderr(&output);
    assert!(stderr.contains("jdk use"), "stderr: {stderr}");
}

#[test]
fn jdkrc_wins_over_sdkmanrc_in_the_same_directory() {
    let sandbox = sandbox();
    sandbox.install("temurin@21.0.4");
    fs::write(sandbox.project.join(".jdkrc"), "java=temurin@21\n").unwrap();
    // Would fail with exit 4 if .sdkmanrc won: 22.0.1 is not installed.
    fs::write(sandbox.project.join(".sdkmanrc"), "java=22.0.1-tem\n").unwrap();

    let output = sandbox.shim("java", &sandbox.project).output().unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("fake-java argv=[]"));
}

#[test]
fn no_pin_runs_the_global_current_jdk() {
    let sandbox = sandbox();
    fs::write(sandbox.project.join(".tool-versions"), "nodejs 20.10.0\n").unwrap();
    // Plain directory standing in for the junction (junction creation is M4).
    let current_bin = sandbox.root.join("current").join("bin");
    fs::create_dir_all(&current_bin).unwrap();
    fs::copy(FAKE_JAVA, current_bin.join("java.exe")).unwrap();

    let output = sandbox
        .shim("java", &sandbox.deep())
        .args(["-version"])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("fake-java argv=[-version]"));
}

#[test]
fn uppercase_shim_name_resolves_the_same_lowercase_tool() {
    let sandbox = sandbox();
    sandbox.install("temurin@21.0.4");
    fs::write(sandbox.project.join(".sdkmanrc"), "java=21.0.4-tem\n").unwrap();

    // argv[0] is exactly what the caller invokes: JAVA.EXE must resolve java.
    let output = Command::new(sandbox.shims.join("JAVA.EXE"))
        .arg("-version")
        .current_dir(&sandbox.project)
        .env("JDK_ROOT", &sandbox.root)
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("fake-java argv=[-version]"));

    // The lowercase normalization is observable in the missing-tool message —
    // NTFS case-insensitivity alone would report JAVAC.exe here.
    fs::copy(SHIM, sandbox.shims.join("JAVAC.EXE")).unwrap();
    let output = Command::new(sandbox.shims.join("JAVAC.EXE"))
        .current_dir(&sandbox.project)
        .env("JDK_ROOT", &sandbox.root)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(127));
    let stderr = stderr(&output);
    assert!(stderr.contains("javac.exe not found"), "stderr: {stderr}");
}

#[test]
fn bare_pin_resolves_with_the_config_vendor() {
    let sandbox = sandbox();
    // Only zulu is installed: a bare `21` reaches it solely through the
    // config's vendor (the M1 hardcoded temurin default is gone).
    sandbox.install("zulu@21.0.4");
    sandbox.config("vendor = \"zulu\"\n");
    fs::write(sandbox.project.join(".java-version"), "21\n").unwrap();

    let output = sandbox.shim("java", &sandbox.project).output().unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("fake-java argv=[]"));
}

#[test]
fn malformed_config_only_fails_resolutions_that_consult_it() {
    let sandbox = sandbox();
    sandbox.install("temurin@21.0.4");
    sandbox.config("vendor = zulu\n"); // unquoted: outside the written subset

    // Explicit vendor, installed: config is never read — the broken file
    // must not brick this resolution (lazy load).
    fs::write(sandbox.project.join(".jdkrc"), "java=temurin@21\n").unwrap();
    let output = sandbox.shim("java", &sandbox.project).output().unwrap();
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    // A bare pin needs the config's vendor: now the error surfaces, with
    // the actionable hint.
    fs::write(sandbox.project.join(".jdkrc"), "java=21\n").unwrap();
    let output = sandbox.shim("java", &sandbox.project).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    assert!(stderr.contains("config.toml"), "stderr: {stderr}");
    assert!(stderr.contains("fix or delete"), "stderr: {stderr}");
}

#[test]
fn handles_spaces_in_store_and_project_paths() {
    // C:\Program Files-like: every path the shim touches has a space in it.
    let sandbox = sandbox_at("jdk root x", "shim dir", "my proj");
    sandbox.install("temurin@21.0.4");
    fs::write(sandbox.project.join(".sdkmanrc"), "java=21.0.4-tem\n").unwrap();

    let output = sandbox
        .shim("java", &sandbox.deep())
        .args(["-version"])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("fake-java argv=[-version]"));
}

#[test]
fn dispatches_by_binary_name_and_reports_a_missing_tool() {
    let sandbox = sandbox();
    // The candidate ships java.exe only; the javac shim must resolve the same
    // JDK and then report that javac.exe is missing there.
    sandbox.install("temurin@21.0.4");
    fs::copy(SHIM, sandbox.shims.join("javac.exe")).unwrap();
    fs::write(sandbox.project.join(".sdkmanrc"), "java=21.0.4-tem\n").unwrap();

    let output = sandbox.shim("javac", &sandbox.project).output().unwrap();

    assert_eq!(output.status.code(), Some(127));
    let stderr = stderr(&output);
    assert!(stderr.contains("javac.exe not found"), "stderr: {stderr}");
}
