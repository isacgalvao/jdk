//! The `<JDK_ROOT>\current` junction — the fixed path JAVA_HOME points at
//! (decision 8). `jdk use` only retargets it; the JAVA_HOME value never
//! changes, so consoles and IDEs already open see the switch immediately.
//!
//! Retarget is ATOMIC: a staging junction is created next to `current` and
//! swapped in with [`atomic_rename`] — never a remove → registry →
//! recreate sequence (anti-model 2), whose interruption window leaves
//! JAVA_HOME pointing at nothing. Junctions, not symlinks (anti-model 3):
//! creating one needs no admin or Developer Mode.

use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use jdk_resolve::store;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What sits at `<root>\current` right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Current {
    Absent,
    /// A junction; `target` is what it points at (which may no longer exist).
    Junction {
        target: PathBuf,
    },
    /// Something else occupies the path (a real directory or file) — not
    /// ours to touch; `jdk doctor` names it.
    NotJunction,
}

pub fn inspect(root: &Path) -> Result<Current> {
    Ok(classify(&store::current(root)))
}

/// `junction::get_target` is the junction test: it opens the reparse point
/// itself, so it works even when the target no longer exists —
/// `junction::exists` would follow the link and call a dead junction absent.
fn classify(path: &Path) -> Current {
    if fs::symlink_metadata(path).is_err() {
        return Current::Absent;
    }
    match junction::get_target(path) {
        Ok(target) => Current::Junction { target },
        Err(_) => Current::NotJunction,
    }
}

/// Points `<root>\current` at `target`, atomically replacing whatever
/// junction was there: a staging junction (`current.new`) is created first
/// and swapped in with one rename, so `current` always resolves to either
/// the old target or the new one — never to nothing.
pub fn retarget(root: &Path, target: &Path) -> Result<()> {
    let current = store::current(root);
    let staging = staging_path(&current);

    remove_junction(&staging)?; // leftovers of a crashed swap
    junction::create(target, &staging).map_err(Error::io("create junction", &staging))?;

    if let Err(swap) = atomic_rename(&staging, &current) {
        let _ = remove_junction(&staging);
        return Err(Error::io("swap junction into", &current)(swap));
    }
    Ok(())
}

fn staging_path(current: &Path) -> PathBuf {
    let mut staging = current.to_path_buf().into_os_string();
    staging.push(".new");
    PathBuf::from(staging)
}

/// Removes a junction (only the reparse point, never the target's content).
/// Refuses to remove anything that is not a junction.
fn remove_junction(path: &Path) -> Result<()> {
    match classify(path) {
        Current::Absent => Ok(()),
        Current::NotJunction => Err(Error::Io {
            action: "replace",
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::AlreadyExists, "exists and is not a junction"),
        }),
        Current::Junction { .. } => {
            junction::delete(path).map_err(Error::io("delete junction", path))?;
            // `delete` leaves an empty directory behind; drop it too.
            fs::remove_dir(path).map_err(Error::io("remove", path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn candidate(root: &Path, name: &str) -> PathBuf {
        let dir = store::java_candidates(root).join(name);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin").join("java.exe"), name.as_bytes()).unwrap();
        dir
    }

    #[test]
    fn creates_and_inspects_the_junction() {
        let temp = TempDir::new().unwrap();
        let jdk_a = candidate(temp.path(), "temurin@21.0.5");

        retarget(temp.path(), &jdk_a).unwrap();

        assert_eq!(
            inspect(temp.path()).unwrap(),
            Current::Junction {
                target: jdk_a.clone()
            }
        );
        // The junction resolves like a directory: reads go to the target.
        let through = fs::read(store::current(temp.path()).join("bin").join("java.exe")).unwrap();
        assert_eq!(through, b"temurin@21.0.5");
    }

    #[test]
    fn retarget_replaces_an_existing_junction_atomically() {
        let temp = TempDir::new().unwrap();
        let jdk_a = candidate(temp.path(), "temurin@17.0.9");
        let jdk_b = candidate(temp.path(), "temurin@21.0.5");
        retarget(temp.path(), &jdk_a).unwrap();

        // The heart of the milestone: the swap must succeed OVER an existing
        // junction (MoveFileExW REPLACE_EXISTING on a reparse point).
        retarget(temp.path(), &jdk_b).unwrap();

        assert_eq!(
            inspect(temp.path()).unwrap(),
            Current::Junction { target: jdk_b }
        );
        let through = fs::read(store::current(temp.path()).join("bin").join("java.exe")).unwrap();
        assert_eq!(through, b"temurin@21.0.5");
        assert!(
            fs::symlink_metadata(staging_path(&store::current(temp.path()))).is_err(),
            "no staging leftovers"
        );
        // Retargeting never harms the old target's content.
        assert!(jdk_a.join("bin").join("java.exe").exists());
    }

    #[test]
    fn retarget_survives_staging_leftovers_of_a_crashed_swap() {
        let temp = TempDir::new().unwrap();
        let jdk_a = candidate(temp.path(), "temurin@17.0.9");
        let jdk_b = candidate(temp.path(), "temurin@21.0.5");
        let staging = staging_path(&store::current(temp.path()));
        junction::create(&jdk_a, &staging).unwrap();

        retarget(temp.path(), &jdk_b).unwrap();

        assert_eq!(
            inspect(temp.path()).unwrap(),
            Current::Junction { target: jdk_b }
        );
    }

    #[test]
    fn a_planted_real_directory_is_never_touched() {
        let temp = TempDir::new().unwrap();
        let jdk_b = candidate(temp.path(), "temurin@21.0.5");
        let current = store::current(temp.path());
        fs::create_dir_all(current.join("precious")).unwrap();

        assert_eq!(inspect(temp.path()).unwrap(), Current::NotJunction);
        // The swap fails (a real directory cannot be replaced) and the
        // directory and its content survive.
        assert!(retarget(temp.path(), &jdk_b).is_err());
        assert!(current.join("precious").exists());
    }

    #[test]
    fn retargets_away_from_a_dead_target() {
        let temp = TempDir::new().unwrap();
        let jdk_a = candidate(temp.path(), "temurin@17.0.9");
        retarget(temp.path(), &jdk_a).unwrap();
        fs::remove_dir_all(&jdk_a).unwrap(); // uninstall under the junction

        let jdk_b = candidate(temp.path(), "temurin@21.0.5");
        retarget(temp.path(), &jdk_b).unwrap();

        assert_eq!(
            inspect(temp.path()).unwrap(),
            Current::Junction { target: jdk_b }
        );
        let through = fs::read(store::current(temp.path()).join("bin").join("java.exe")).unwrap();
        assert_eq!(through, b"temurin@21.0.5");
    }

    #[test]
    fn inspect_reports_a_missing_junction_and_a_dead_target() {
        let temp = TempDir::new().unwrap();
        assert_eq!(inspect(temp.path()).unwrap(), Current::Absent);

        let jdk_a = candidate(temp.path(), "temurin@17.0.9");
        retarget(temp.path(), &jdk_a).unwrap();
        fs::remove_dir_all(&jdk_a).unwrap();
        // The junction still reports its (now dead) target — doctor's case.
        assert_eq!(
            inspect(temp.path()).unwrap(),
            Current::Junction { target: jdk_a }
        );
    }
}
