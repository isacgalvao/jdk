//! Shim auto-install (plan decision 5): what a pinned-but-missing JDK does
//! under each `auto-install` config value. The interactive TTY prompt path
//! cannot be automated honestly (stdin AND stderr must be real consoles):
//! its accept/refuse parsing is unit-tested in the shim (`accepts`) and the
//! interactive flow is a manual validation item. Everything else is covered
//! here, including the whole `always` loop end to end: shim → `jdk.exe
//! install --from-shim` against a loopback index → re-resolve → exec.

mod common;

use common::{FAKE_JAVA, sandbox, stderr, stdout};
use std::fs;
use test_support::{
    Response, Server, dead_url, fake_jdk_zip, jdk_binary, package, serve_catalog, sha256_hex,
};

#[test]
fn never_fails_actionably_with_exit_4() {
    let sandbox = sandbox();
    sandbox.config("auto-install = \"never\"\n");
    fs::write(sandbox.project.join(".jdkrc"), "java=temurin@21\n").unwrap();

    let output = sandbox.shim("java", &sandbox.project).output().unwrap();

    assert_eq!(output.status.code(), Some(4));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("temurin@21") && stderr.contains("is not installed"),
        "{stderr}"
    );
    assert!(stderr.contains("jdk install temurin@21"), "{stderr}");
}

#[test]
fn prompt_without_a_tty_fails_actionably_and_never_hangs() {
    let sandbox = sandbox();
    sandbox.config("auto-install = \"prompt\"\n");
    fs::write(sandbox.project.join(".jdkrc"), "java=temurin@21\n").unwrap();

    // `.output()` pipes stdin and stderr — the CI/IDE case. If the shim
    // prompted anyway this call would hang forever instead of exiting 4.
    let output = sandbox.shim("java", &sandbox.project).output().unwrap();

    assert_eq!(output.status.code(), Some(4));
    let stderr = stderr(&output);
    assert!(
        !stderr.contains("Install now?"),
        "no prompt off-TTY: {stderr}"
    );
    assert!(stderr.contains("jdk install temurin@21"), "{stderr}");
}

/// The full auto-install loop with the real binaries and a loopback index:
/// the shim finds the pin missing, spawns the real `jdk.exe install
/// --from-shim`, the CLI downloads and installs the fake JDK, and the shim
/// re-resolves and executes it — proven by the fake java's exit code.
#[test]
fn always_installs_via_the_real_cli_and_runs_the_tool() {
    let sandbox = sandbox();
    let zip = fake_jdk_zip(&fs::read(FAKE_JAVA).unwrap());
    let server = Server::start();
    let pkg = package(
        "21.0.5+11",
        &format!("{}/dl/t.zip", server.url()),
        &sha256_hex(&zip),
        zip.len() as u64,
    );
    serve_catalog(&server, std::slice::from_ref(&pkg));
    server.route("/dl/t.zip", move |_| Response::ok(zip.clone()));

    // Decision 7 layout: the shim looks for the CLI at <JDK_ROOT>\bin\jdk.exe.
    let bin = sandbox.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::copy(jdk_binary(), bin.join("jdk.exe")).unwrap();

    sandbox.config("auto-install = \"always\"\n");
    fs::write(sandbox.project.join(".sdkmanrc"), "java=21.0.5-tem\n").unwrap();

    let output = sandbox
        .shim("java", &sandbox.project)
        .args(["-version"])
        .env("JDK_INDEX", server.url())
        .env("JDK_FOOJAY", dead_url())
        .env("FAKE_JAVA_EXIT", "5")
        .output()
        .unwrap();

    // Exit 5 can only come from the freshly installed fake java itself.
    assert_eq!(
        output.status.code(),
        Some(5),
        "stdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("fake-java argv=[-version]"));
    assert_eq!(server.hits("/dl/t.zip"), 1);
    assert!(
        sandbox
            .root
            .join("candidates")
            .join("java")
            .join("temurin@21.0.5+11")
            .join("bin")
            .join("java.exe")
            .exists()
    );
}

#[test]
fn always_without_the_cli_anywhere_fails_actionably() {
    let sandbox = sandbox();
    sandbox.config("auto-install = \"always\"\n");
    fs::write(sandbox.project.join(".jdkrc"), "java=temurin@21\n").unwrap();

    // No <root>\bin\jdk.exe and an empty PATH: nothing to delegate to.
    let output = sandbox
        .shim("java", &sandbox.project)
        .env("PATH", "")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(stderr.contains("jdk.exe not found"), "{stderr}");
    assert!(stderr.contains("install the jdk CLI"), "{stderr}");
}
