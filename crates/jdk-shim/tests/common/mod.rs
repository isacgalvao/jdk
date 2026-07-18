//! Shared shim-test sandbox: fake store in a temp dir, the shim copied as
//! java.exe (byte-identical copies ARE the deployment model), no real home.
//! `JDK_ROOT` always points into the sandbox.
//!
//! Each integration-test binary compiles its own copy of this module and
//! uses a different subset of it, so unused-item lints are off here.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

pub const SHIM: &str = env!("CARGO_BIN_EXE_jdk-shim");
pub const FAKE_JAVA: &str = env!("CARGO_BIN_EXE_fake_java");

pub struct Sandbox {
    pub _temp: TempDir,
    pub root: PathBuf,
    pub shims: PathBuf,
    pub project: PathBuf,
}

pub fn sandbox() -> Sandbox {
    sandbox_at("store", "shims", "proj")
}

/// Sandbox with chosen directory names (e.g. names containing spaces).
pub fn sandbox_at(root: &str, shims: &str, project: &str) -> Sandbox {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join(root);
    let shims = temp.path().join(shims);
    let project = temp.path().join(project);
    fs::create_dir_all(&shims).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::copy(SHIM, shims.join("java.exe")).unwrap();
    Sandbox {
        _temp: temp,
        root,
        shims,
        project,
    }
}

impl Sandbox {
    /// Installs a fake candidate: `candidates\java\<name>\bin\java.exe`.
    pub fn install(&self, name: &str) {
        let bin = self
            .root
            .join("candidates")
            .join("java")
            .join(name)
            .join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::copy(FAKE_JAVA, bin.join("java.exe")).unwrap();
    }

    /// Writes `<root>\config.toml`.
    pub fn config(&self, text: &str) {
        fs::create_dir_all(&self.root).unwrap();
        fs::write(self.root.join("config.toml"), text).unwrap();
    }

    /// A shim invocation (`java.exe` unless another tool copy is asked for),
    /// run from `dir` with JDK_ROOT pointing into the sandbox.
    pub fn shim(&self, tool: &str, dir: &Path) -> Command {
        let mut command = Command::new(self.shims.join(format!("{tool}.exe")));
        command.current_dir(dir).env("JDK_ROOT", &self.root);
        command
    }

    /// Deep package-style subdirectory inside the project.
    pub fn deep(&self) -> PathBuf {
        let deep = ["src", "main", "java", "com", "acme"]
            .iter()
            .fold(self.project.clone(), |dir, part| dir.join(part));
        fs::create_dir_all(&deep).unwrap();
        deep
    }
}

pub fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
