//! `jdk uninstall <selector>` with in-use detection via rename-probe: the
//! candidate is atomically renamed to `<name>.removing` and only then
//! deleted. A rename refusal proves something holds a handle inside it (a
//! running java.exe, a shell cd'd into it) and leaves the store untouched;
//! a crash mid-delete leaves a `.removing` orphan that [`sweep_orphans`]
//! clears on the next store-touching command. This is deliberately simple
//! and atomic — no scanning the process table via NtQuerySystemInformation
//! (~100 unsafe lines), never a half-deleted candidate. Naming WHICH process
//! holds the JDK stays post-v0.1.

use crate::fail::Fail;
use jdk_resolve::{exit, store};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

pub fn run(root: &Path, selector: &str) -> Result<(), Fail> {
    sweep_orphans(root);
    let selector = crate::parse_selector(selector)?;
    let config = crate::config(root)?;

    let scan_fail = |err| Fail::new(exit::FAILURE, format!("cannot scan the store: {err}"));
    let candidate = store::best_candidate(root, &selector, &config.vendor).map_err(scan_fail)?;
    let Some(candidate) = candidate else {
        let installed = store::installed(root).map_err(scan_fail)?;
        let mut message = format!("no installed JDK matches {selector}");
        if !installed.is_empty() {
            let names: Vec<String> = installed
                .iter()
                .map(|c| format!("{}@{}", c.vendor, c.version))
                .collect();
            message.push_str(&format!("\n  installed: {}", names.join(", ")));
        }
        return Err(
            Fail::new(exit::NOT_INSTALLED, message).hint("`jdk list` shows what is installed")
        );
    };

    let name = format!("{}@{}", candidate.vendor, candidate.version);
    match remove(&candidate.dir, &name)? {
        Removal::Removed => eprintln!("jdk: uninstalled {name}"),
        // Already renamed away — resolution can no longer pick it; leftovers
        // are swept by the next store-touching command.
        Removal::Deferred => {
            eprintln!("jdk: uninstalled {name} (leftovers will be swept on the next run)");
        }
        Removal::AlreadyGone => {
            eprintln!("jdk: {name} is already removed (or being removed by another process)");
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Removal {
    Removed,
    /// Renamed away but the delete failed; the orphan sweep finishes later.
    Deferred,
    /// The candidate vanished between the scan and the rename: a concurrent
    /// uninstall won the race, so the goal state is already reached.
    AlreadyGone,
}

/// Rename-probe removal of one candidate directory. The rename's error kind
/// tells the causes apart: a missing source means a concurrent uninstall; on
/// Windows `PermissionDenied` (os error 5) is what BOTH a locked directory
/// and an ACL denial raise, so that message names the two.
fn remove(dir: &Path, name: &str) -> Result<Removal, Fail> {
    remove_with(dir, name, |dir: &Path| fs::remove_dir_all(dir))
}

/// [`remove`]'s rename-then-delete, with the delete step injectable: unit
/// tests drive the `Deferred` outcome (delete fails after a successful
/// rename) without needing a real locked directory (same seam shape as
/// `decide_install` in jdk-shim).
fn remove_with(
    dir: &Path,
    name: &str,
    remove: impl Fn(&Path) -> io::Result<()>,
) -> Result<Removal, Fail> {
    let mut removing = dir.to_path_buf().into_os_string();
    removing.push(".removing");
    let removing = PathBuf::from(removing);

    match fs::rename(dir, &removing) {
        Ok(()) => match remove(&removing) {
            Ok(()) => Ok(Removal::Removed),
            Err(_) => Ok(Removal::Deferred),
        },
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(Removal::AlreadyGone),
        Err(err) if err.kind() == ErrorKind::PermissionDenied => Err(Fail::new(
            exit::FAILURE,
            format!(
                "cannot uninstall {name}: the JDK is in use by a running process, \
                 or you lack permission to move it ({err})"
            ),
        )
        .hint("stop the running Java processes (or shells inside it) and retry")
        .hint("if nothing is running, check the store directory's permissions")),
        Err(err) => Err(Fail::new(
            exit::FAILURE,
            format!("cannot uninstall {name}: the JDK is in use ({err})"),
        )
        .hint("stop the running Java processes (or shells inside it) and retry")),
    }
}

/// Clears `*.removing` orphans a crashed uninstall left in the store. Runs
/// at the start of every command that scans or mutates candidates; failures
/// are ignored (still in use → a later run gets it).
pub fn sweep_orphans(root: &Path) {
    let Ok(entries) = fs::read_dir(store::java_candidates(root)) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".removing"))
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_vanished_candidate_is_already_gone_not_an_error() {
        // The race a CLI test cannot reach: the directory disappears between
        // the store scan and the rename (concurrent uninstall won).
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("temurin@21.0.4");

        let outcome = remove(&dir, "temurin@21.0.4").unwrap();

        assert_eq!(outcome, Removal::AlreadyGone);
        assert!(!temp.path().join("temurin@21.0.4.removing").exists());
    }

    #[test]
    fn a_free_candidate_is_removed_without_leftovers() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("temurin@21.0.4");
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin").join("java.exe"), b"stub").unwrap();

        let outcome = remove(&dir, "temurin@21.0.4").unwrap();

        assert_eq!(outcome, Removal::Removed);
        assert!(!dir.exists());
        assert!(!temp.path().join("temurin@21.0.4.removing").exists());
    }

    #[test]
    fn a_delete_failure_after_a_successful_rename_defers_and_a_later_sweep_clears_it() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let dir = store::java_candidates(root).join("temurin@21.0.4");
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin").join("java.exe"), b"stub").unwrap();

        // The rename succeeds (real fs::rename); only the delete step is
        // faked to fail, the way a still-open handle inside `.removing`
        // would make the real `fs::remove_dir_all` fail.
        let outcome = remove_with(&dir, "temurin@21.0.4", |_| {
            Err(io::Error::new(ErrorKind::PermissionDenied, "locked"))
        })
        .unwrap();

        let removing = store::java_candidates(root).join("temurin@21.0.4.removing");
        assert_eq!(outcome, Removal::Deferred);
        assert!(
            !dir.exists(),
            "renamed away — resolution can no longer see it"
        );
        assert!(
            removing.exists(),
            "the orphan is left on disk for the sweep"
        );

        sweep_orphans(root);

        assert!(!removing.exists(), "a later sweep clears the leftover");
    }
}
