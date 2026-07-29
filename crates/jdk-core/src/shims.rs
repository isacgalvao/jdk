//! Shim materialization: every JDK tool of the v0.1 set (decision 10) is a
//! byte-identical copy of `jdk-shim.exe` named after the tool — the shim
//! dispatches on argv[0]. Copies, not symlinks: no admin, no Developer Mode
//! (anti-model 3), and cmd.exe/IDEs see real `.exe` files.

use crate::error::{Error, Result};
use crate::file_ops;
use std::fs;
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

/// What one [`materialize`] run did. Both lists empty means every shim was
/// already current — the idempotent second run.
#[derive(Debug, Default)]
pub struct Materialized {
    /// The shims (re)written, in [`TOOLS`] order.
    pub written: Vec<&'static str>,
    /// Every shim whose swap was refused, with the refusal. Only the swap
    /// phase fills this: a staging failure aborts before any shim is touched
    /// and comes back as `Err` instead.
    pub failed: Vec<(&'static str, Error)>,
}

/// Materializes every [`TOOLS`] shim in `shims_dir` as a byte-identical copy
/// of `source`, in two phases, because the set is only useful when its members
/// agree on a version (BUG-07).
///
/// NTFS has no transaction spanning N files, so atomicity across the set is
/// not on offer and this does not pretend otherwise. What is on offer is a
/// window narrow enough to stop mattering, and a run that never hides half of
/// what went wrong. Phase one copies every tool needing a rewrite to
/// `{tool}.exe.new` — the slow part, megabytes each plus whatever the
/// antivirus makes of them — and a failure there returns having swapped
/// nothing, leaving the set uniformly on the version it already had. Phase two
/// swaps the staged copies in by rename — microseconds each when nothing
/// objects, which is what narrows the window, though a tool that IS refused
/// spends up to [`file_ops::replace_running`]'s retry budget before the next
/// one starts — and does NOT stop at the first refusal: every tool is
/// attempted and every failure comes back in [`Materialized::failed`], so a
/// caller reports the whole truth once instead of one tool per run.
///
/// A shim that is EXECUTING right now is handled by
/// [`file_ops::replace_running`]'s rename-aside. Leftovers of earlier
/// interrupted swaps are swept first, best-effort.
pub fn materialize(source: &Path, shims_dir: &Path) -> Result<Materialized> {
    let payload = fs::read(source).map_err(Error::io("read", source))?;
    fs::create_dir_all(shims_dir).map_err(Error::io("create", shims_dir))?;
    sweep_leftovers(shims_dir);

    let mut staged = Vec::new();
    for tool in TOOLS {
        let dest = shims_dir.join(format!("{}.exe", tool.name));
        if fs::read(&dest).is_ok_and(|existing| existing == payload) {
            continue;
        }
        let staging = dest.with_extension("exe.new");
        if let Err(err) = fs::copy(source, &staging) {
            // No swap has happened yet, so the store is still consistent:
            // drop the stagings and leave it that way. The sweep is the same
            // one that ran on entry, and it clears exactly `*.exe.new`.
            sweep_leftovers(shims_dir);
            return Err(Error::io("copy shim to", &staging)(err));
        }
        staged.push((tool.name, staging, dest));
    }

    let mut done = Materialized::default();
    for (tool, staging, dest) in staged {
        match file_ops::replace_running(&staging, &dest) {
            Ok(()) => done.written.push(tool),
            Err(err) => {
                // Never leave a staging orphan behind, whatever failed.
                let _ = fs::remove_file(&staging);
                done.failed.push((tool, err));
            }
        }
    }
    Ok(done)
}

/// Clears what an interrupted replacement left behind: a `{tool}.exe.new`
/// staging is always garbage, while a `{tool}.exe.old` aside is garbage only
/// once the tool it was moved aside from is back in place — until then it is
/// the last copy of that tool the store has. Best-effort by design: a leftover
/// whose process is still alive cannot be deleted and is silently left for a
/// later run.
fn sweep_leftovers(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let sweepable = match name.strip_suffix(".old") {
            Some(live) => live.ends_with(".exe") && dir.join(live).exists(),
            None => name.ends_with(".exe.new"),
        };
        if sweepable {
            let path = entry.path();
            // A DIRECTORY can end up on either name — moved aside from a
            // `dest` that was one — and `remove_file` cannot touch it (os
            // error 5), which would strand it forever. The test above already
            // established this leftover is superseded garbage, so removing it
            // wholesale is right. A file merely in use fails both calls and is
            // left for a later run, as before.
            if fs::remove_file(&path).is_err() {
                let _ = fs::remove_dir_all(&path);
            }
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

        let done = materialize(&source, &shims).unwrap();

        assert_eq!(done.written.len(), TOOLS.len());
        assert!(done.failed.is_empty());
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

        assert!(materialize(&source, &shims).unwrap().written.is_empty());

        fs::write(shims.join("jar.exe"), b"stale").unwrap();
        assert_eq!(materialize(&source, &shims).unwrap().written, vec!["jar"]);
        assert_eq!(fs::read(shims.join("jar.exe")).unwrap(), b"shim payload v1");
        assert!(!shims.join("jar.exe.new").exists(), "no staging leftovers");
    }

    /// BUG-07, phase one: a staging copy that cannot be made aborts before ANY
    /// shim is swapped, so the set stays uniformly on the version it had
    /// instead of splitting across two. A HELD `jar.exe.new` is the hermetic
    /// stand-in for the volume filling up mid-run — it survives the entry
    /// sweep and refuses the copy onto it, so it is still in the way when
    /// phase one reaches `jar`.
    #[cfg(windows)]
    #[test]
    fn a_staging_failure_leaves_every_shim_on_the_old_version() {
        let temp = TempDir::new().unwrap();
        let v1 = temp.path().join("v1.exe");
        fs::write(&v1, b"shim payload v1").unwrap();
        let shims = temp.path().join("shims");
        materialize(&v1, &shims).unwrap();
        let v2 = temp.path().join("v2.exe");
        fs::write(&v2, b"shim payload v2").unwrap();
        let blocked = shims.join("jar.exe.new");
        fs::write(&blocked, b"in the way").unwrap();
        let _held = test_support::hold(&blocked);

        let err = materialize(&v2, &shims).unwrap_err();

        assert!(err.to_string().contains("copy shim to"), "{err}");
        for tool in TOOLS {
            assert_eq!(
                fs::read(shims.join(format!("{}.exe", tool.name))).unwrap(),
                b"shim payload v1",
                "{} must not be half-updated",
                tool.name
            );
        }
        assert!(
            !shims.join("java.exe.new").exists(),
            "the stagings taken before the failure are cleared"
        );
    }

    /// BUG-05: a death between the staging copy and the swap leaves several
    /// MiB per tool in a directory that is on the PATH, and the old filter
    /// only ever looked at `.exe.old`.
    #[test]
    fn a_staging_orphan_is_swept_on_the_next_run() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("jdk-shim.exe");
        fs::write(&source, b"shim payload v1").unwrap();
        let shims = temp.path().join("shims");
        materialize(&source, &shims).unwrap();
        fs::write(shims.join("java.exe.new"), b"orphaned staging").unwrap();

        assert!(materialize(&source, &shims).unwrap().written.is_empty());

        assert!(!shims.join("java.exe.new").exists());
    }

    /// BUG-04, from the shim side: `replace_running` empties `dest` before the
    /// replacement lands, so an aside whose tool is still missing is the only
    /// copy left — sweeping it there would destroy it. The superseded half is
    /// a DIRECTORY, the shape a `dest` that was one leaves behind: it is
    /// garbage by the same test as any other superseded aside, and
    /// `remove_file` alone would strand it in a directory that is on the PATH.
    #[test]
    fn an_aside_is_spared_while_its_tool_is_missing() {
        let temp = TempDir::new().unwrap();
        let shims = temp.path().join("shims");
        fs::create_dir_all(&shims).unwrap();
        fs::write(shims.join("java.exe.old"), b"the only copy left").unwrap();
        fs::write(shims.join("jar.exe"), b"jar").unwrap();
        fs::create_dir_all(shims.join("jar.exe.old").join("inside")).unwrap();

        sweep_leftovers(&shims);

        assert_eq!(
            fs::read(shims.join("java.exe.old")).unwrap(),
            b"the only copy left"
        );
        assert!(!shims.join("jar.exe.old").exists(), "jar.exe is back");
    }

    #[test]
    fn availability_gates_by_major() {
        assert!(Availability::Always.includes(8));
        assert!(!Availability::Since(9).includes(8));
        assert!(Availability::Since(9).includes(9));
        let jshell = TOOLS.iter().find(|tool| tool.name == "jshell").unwrap();
        assert_eq!(jshell.availability, Availability::Since(9));
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
        let done = materialize(&source2, &shims).unwrap();

        // Every copy differs from v2, so every tool was rewritten — java.exe
        // through the rename-aside (it is executing), the rest directly.
        assert_eq!(done.written.len(), TOOLS.len());
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
        assert!(materialize(&source2, &shims).unwrap().written.is_empty());
        assert!(
            !shims.join("java.exe.old").exists(),
            "orphan .old swept once the process exited"
        );
    }

    /// BUG-07, phase two: one tool that cannot be swapped is reported, and the
    /// other five still land. Aborting at the first refusal — java is the
    /// first of the six — is what used to leave the store straddling two
    /// versions AND name only one of its problems.
    #[cfg(windows)]
    #[test]
    fn a_blocked_swap_is_reported_without_holding_back_the_others() {
        let temp = TempDir::new().unwrap();
        let (_, fake_java) = test_support::shim_binaries();
        let shims = temp.path().join("shims");
        materialize(&fake_java, &shims).unwrap();
        let v1 = fs::read(&fake_java).unwrap();

        let mut child = running_shim(&shims);
        // A HELD file squatting the aside name: undeletable by the sweep and
        // by the swap's own pre-clear, unrenameable-over — the aside path is
        // fully blocked, which is what makes a RUNNING java.exe unswappable.
        let squatter = shims.join("java.exe.old");
        fs::write(&squatter, b"immovable").unwrap();
        let _held = test_support::hold(&squatter);
        let (source2, v2) = v2_source(&temp, &fake_java);

        let done = materialize(&source2, &shims).unwrap();

        let (tool, err) = done.failed.first().expect("java could not be swapped");
        assert_eq!(*tool, "java");
        assert!(
            err.to_string().contains("aside"),
            "the failure names the aside step: {err}"
        );
        assert_eq!(done.failed.len(), 1);
        assert_eq!(done.written.len(), TOOLS.len() - 1, "the rest went through");
        assert!(!shims.join("java.exe.new").exists(), "no staging orphan");
        assert_eq!(
            fs::read(shims.join("java.exe")).unwrap(),
            v1,
            "the running copy is untouched"
        );
        assert_eq!(fs::read(shims.join("jar.exe")).unwrap(), v2);

        child.kill().unwrap();
        child.wait().unwrap();
    }
}
