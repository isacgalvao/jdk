//! Shim materialization: every JDK tool of the v0.1 set (decision 10) is a
//! byte-identical copy of `jdk-shim.exe` named after the tool — the shim
//! dispatches on argv[0]. Copies, not symlinks: no admin, no Developer Mode
//! (anti-model 3), and cmd.exe/IDEs see real `.exe` files.

use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use std::fs;
use std::io;
use std::path::Path;

/// JDK major-version availability of a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Always,
    /// First JDK major that ships the tool.
    Since(u32),
}

impl Availability {
    pub fn includes(self, major: u32) -> bool {
        match self {
            Availability::Always => true,
            Availability::Since(min) => major >= min,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Tool {
    pub name: &'static str,
    pub availability: Availability,
}

/// The v0.1 shim set (decision 10). None of these six is distribution-exclusive,
/// so the only availability dimension that matters is the JDK major.
pub const TOOLS: [Tool; 6] = [
    Tool {
        name: "java",
        availability: Availability::Always,
    },
    Tool {
        name: "javac",
        availability: Availability::Always,
    },
    Tool {
        name: "jar",
        availability: Availability::Always,
    },
    Tool {
        name: "javadoc",
        availability: Availability::Always,
    },
    Tool {
        name: "jshell",
        availability: Availability::Since(9),
    },
    Tool {
        name: "keytool",
        availability: Availability::Always,
    },
];

/// Duplicates the shim executable to `dest`, then re-reads the written file's
/// length and checks it against the source, so a truncated write fails loudly
/// instead of leaving a half-formed tool behind.
fn copy_shim(source: &Path, dest: &Path) -> io::Result<()> {
    fs::copy(source, dest)?;

    let expected = fs::metadata(source)?.len();
    let written = fs::metadata(dest)?.len();
    if written != expected {
        return Err(io::Error::other(format!(
            "short copy to {}: wrote {written} of {expected} bytes",
            dest.display()
        )));
    }
    Ok(())
}

/// Materializes every [`TOOLS`] shim in `shims_dir` as a byte-identical copy
/// of `source`, and returns the names it (re)wrote — empty means everything
/// was already current (idempotent second run). Each write is staged
/// (`{tool}.exe.new`) and swapped in; a shim that is EXECUTING right now is
/// handled by [`place`]'s rename-aside. `.old` leftovers of earlier
/// while-running swaps are swept first, best-effort.
pub fn materialize(source: &Path, shims_dir: &Path) -> Result<Vec<&'static str>> {
    let payload = fs::read(source).map_err(Error::io("read", source))?;
    fs::create_dir_all(shims_dir).map_err(Error::io("create", shims_dir))?;
    sweep_aside(shims_dir);

    let mut written = Vec::new();
    for tool in TOOLS {
        let dest = shims_dir.join(format!("{}.exe", tool.name));
        if fs::read(&dest).is_ok_and(|existing| existing == payload) {
            continue;
        }
        let staging = dest.with_extension("exe.new");
        copy_shim(source, &staging).map_err(Error::io("copy shim to", &staging))?;
        if let Err(err) = place(&staging, &dest) {
            // Never leave a staging orphan behind, whatever failed.
            let _ = fs::remove_file(&staging);
            return Err(err);
        }
        written.push(tool.name);
    }
    Ok(written)
}

/// Swaps `staging` into `dest`. The Win32 semantics that shape this: a
/// RUNNING exe cannot be deleted or replaced (the implicit delete inside
/// `MOVEFILE_REPLACE_EXISTING` fails with ACCESS_DENIED while the image is
/// mapped), but RENAMING it to another name is allowed. So on that refusal
/// the live exe is moved aside to `{tool}.exe.old` and the staging lands on
/// the freed name; the `.old` stays until [`sweep_aside`] catches it once
/// the process has exited.
fn place(staging: &Path, dest: &Path) -> Result<()> {
    match atomic_rename(staging, dest) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied && dest.exists() => {
            let aside = dest.with_extension("exe.old");
            // A leftover `.old` still running blocks the rename below; the
            // resulting error is the honest answer for that corner.
            let _ = fs::remove_file(&aside);
            fs::rename(dest, &aside)
                .map_err(Error::io("move the running shim aside from", dest))?;
            atomic_rename(staging, dest).map_err(Error::io("place shim at", dest))
        }
        Err(err) => Err(Error::io("place shim at", dest)(err)),
    }
}

/// Clears `{tool}.exe.old` leftovers of earlier while-running replacements.
/// Best-effort by design: an `.old` whose process is still alive cannot be
/// deleted and is silently left for a later run.
fn sweep_aside(shims_dir: &Path) {
    let Ok(entries) = fs::read_dir(shims_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".exe.old"))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn materializes_byte_identical_copies_for_every_tool() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("jdk-shim.exe");
        fs::write(&source, b"shim payload v1").unwrap();
        let shims = temp.path().join("shims");

        let written = materialize(&source, &shims).unwrap();

        assert_eq!(written.len(), TOOLS.len());
        for tool in TOOLS {
            let copy = fs::read(shims.join(format!("{}.exe", tool.name))).unwrap();
            assert_eq!(copy, b"shim payload v1", "{}", tool.name);
        }
    }

    #[test]
    fn second_run_is_a_no_op_and_a_stale_copy_is_refreshed() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("jdk-shim.exe");
        fs::write(&source, b"shim payload v1").unwrap();
        let shims = temp.path().join("shims");
        materialize(&source, &shims).unwrap();

        assert_eq!(materialize(&source, &shims).unwrap(), Vec::<&str>::new());

        fs::write(shims.join("jar.exe"), b"stale").unwrap();
        assert_eq!(materialize(&source, &shims).unwrap(), vec!["jar"]);
        assert_eq!(fs::read(shims.join("jar.exe")).unwrap(), b"shim payload v1");
        assert!(!shims.join("jar.exe.new").exists(), "no staging leftovers");
    }

    #[test]
    fn availability_gates_by_major() {
        assert!(Availability::Always.includes(8));
        assert!(!Availability::Since(9).includes(8));
        assert!(Availability::Since(9).includes(9));
        let jshell = TOOLS.iter().find(|tool| tool.name == "jshell").unwrap();
        assert_eq!(jshell.availability, Availability::Since(9));
    }

    #[test]
    fn copy_shim_verifies_size() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("src.exe");
        fs::write(&source, b"payload").unwrap();
        copy_shim(&source, &temp.path().join("dst.exe")).unwrap();
        assert_eq!(fs::read(temp.path().join("dst.exe")).unwrap(), b"payload");
    }

    /// A running child of `shims\java.exe`, image mapped for the duration.
    fn running_shim(shims: &Path) -> std::process::Child {
        std::process::Command::new(shims.join("java.exe"))
            .env("FAKE_JAVA_SLEEP_MS", "30000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the shim copy")
    }

    /// The v2 source: the real executable with overlay bytes appended, so
    /// content differs while the file stays a valid spawn target.
    fn v2_source(temp: &TempDir, fake_java: &Path) -> (std::path::PathBuf, Vec<u8>) {
        let mut payload = fs::read(fake_java).unwrap();
        payload.extend_from_slice(b"-v2-overlay");
        let source = temp.path().join("v2.exe");
        fs::write(&source, &payload).unwrap();
        (source, payload)
    }

    #[test]
    fn replaces_a_running_shim_via_rename_aside_and_sweeps_it_later() {
        let temp = TempDir::new().unwrap();
        let (_, fake_java) = test_support::shim_binaries();
        let shims = temp.path().join("shims");
        materialize(&fake_java, &shims).unwrap();
        let v1 = fs::read(&fake_java).unwrap();

        let mut child = running_shim(&shims);
        let (source2, v2) = v2_source(&temp, &fake_java);
        let written = materialize(&source2, &shims).unwrap();

        // Every copy differs from v2, so every tool was rewritten — java.exe
        // through the rename-aside (it is executing), the rest directly.
        assert_eq!(written.len(), TOOLS.len());
        assert_eq!(fs::read(shims.join("java.exe")).unwrap(), v2);
        assert_eq!(
            fs::read(shims.join("java.exe.old")).unwrap(),
            v1,
            "the running copy was moved aside, not destroyed"
        );
        assert!(!shims.join("java.exe.new").exists(), "no staging leftovers");
        assert!(
            !shims.join("jar.exe.old").exists(),
            "non-running copies are replaced without an aside"
        );

        child.kill().unwrap();
        child.wait().unwrap();

        // Process gone: the next run sweeps the aside and is otherwise a
        // no-op.
        assert_eq!(materialize(&source2, &shims).unwrap(), Vec::<&str>::new());
        assert!(
            !shims.join("java.exe.old").exists(),
            "orphan .old swept once the process exited"
        );
    }

    #[test]
    fn a_blocked_swap_fails_without_staging_orphans_or_harm() {
        let temp = TempDir::new().unwrap();
        let (_, fake_java) = test_support::shim_binaries();
        let shims = temp.path().join("shims");
        materialize(&fake_java, &shims).unwrap();
        let v1 = fs::read(&fake_java).unwrap();

        let mut child = running_shim(&shims);
        // A non-empty DIRECTORY squatting the aside name: undeletable by the
        // sweep, unrenameable-over — the aside path is fully blocked.
        fs::create_dir_all(shims.join("java.exe.old").join("squatter")).unwrap();
        let (source2, _) = v2_source(&temp, &fake_java);

        let err = materialize(&source2, &shims).unwrap_err();

        assert!(
            err.to_string().contains("aside"),
            "the failure names the aside step: {err}"
        );
        assert!(!shims.join("java.exe.new").exists(), "no staging orphan");
        assert_eq!(
            fs::read(shims.join("java.exe")).unwrap(),
            v1,
            "the running copy is untouched"
        );

        child.kill().unwrap();
        child.wait().unwrap();
    }
}
