//! Windows-safe file primitives shared by download finalization, the cache,
//! the `current` junction swap and the shim/CLI executable swaps.

use crate::error::{Error, Result};
use std::fs;
use std::io;
use std::path::Path;

/// Rename that atomically replaces an existing destination FILE: `fs::rename`
/// asks Windows for `MOVEFILE_REPLACE_EXISTING`, so an occupied target is
/// overwritten rather than refused. Directories cannot be replaced this way —
/// callers renaming directories must guarantee the target does not exist.
pub fn atomic_rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// Overwrites an existing `to` with `from` in one durable step:
/// `MOVEFILE_REPLACE_EXISTING` supplants the occupant and `MOVEFILE_WRITE_THROUGH`
/// holds the call until the change is flushed, so a crash cannot tear the swap.
/// That flush is the only thing [`atomic_rename`] does not already give, which
/// is why only [`replace_running`] — the executable swap, where a torn result
/// leaves nothing to run — pays for the direct Win32 call.
#[cfg(windows)]
fn replace_existing(from: &Path, to: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = wide_nul(from);
    let target = wide_nul(to);
    let status = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    match status {
        0 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

/// A NUL-terminated UTF-16 rendering of `path`, as the wide Win32 calls expect.
#[cfg(windows)]
fn wide_nul(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    units.push(0);
    units
}

/// Off Windows there is no write-through flag to ask for, so the durable
/// replacement degrades to the plain rename. Exists so [`replace_running`]
/// stays one code path across the unit tests that run on both.
#[cfg(not(windows))]
fn replace_existing(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// Swaps `staging` into `dest`, tolerating a `dest` that is EXECUTING. The
/// Win32 semantics that shape this: a RUNNING exe cannot be deleted or
/// replaced (the implicit delete inside `MOVEFILE_REPLACE_EXISTING` fails
/// with ACCESS_DENIED while the image is mapped), but RENAMING it to another
/// name is allowed. So on that refusal the live exe is moved aside to
/// `<name>.exe.old` and the staging lands on the freed name; the `.old`
/// stays behind until the caller's sweep catches it once the process has
/// exited — and, while `dest` is still missing, is the only copy the machine
/// has left, which is why a sweep must spare it (`jdk-shim` restores from it).
///
/// Should the final rename fail AFTER the aside emptied `dest`, the aside is
/// rolled back onto `dest` (best-effort — the original error is reported
/// either way), so the destination never silently vanishes. That failure has
/// no deterministic simulation without fault injection, so the rollback is
/// documented here rather than pinned by a test.
pub fn replace_running(staging: &Path, dest: &Path) -> Result<()> {
    match replace_existing(staging, dest) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied && dest.exists() => {
            let aside = dest.with_extension("exe.old");
            // A leftover `.old` still running blocks the rename below; the
            // resulting error is the honest answer for that corner.
            let _ = fs::remove_file(&aside);
            fs::rename(dest, &aside)
                .map_err(Error::io("move the running executable aside from", dest))?;
            replace_existing(staging, dest).map_err(|err| {
                let _ = fs::rename(&aside, dest);
                Error::io("place", dest)(err)
            })
        }
        Err(err) => Err(Error::io("place", dest)(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn renames_to_a_new_name() {
        let temp = TempDir::new().unwrap();
        let from = temp.path().join("a");
        let to = temp.path().join("b");
        fs::write(&from, b"payload").unwrap();

        atomic_rename(&from, &to).unwrap();

        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"payload");
    }

    #[test]
    fn replaces_an_existing_destination() {
        let temp = TempDir::new().unwrap();
        let from = temp.path().join("a");
        let to = temp.path().join("b");
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"old").unwrap();

        atomic_rename(&from, &to).unwrap();

        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"new");
    }

    /// The durable path taken when nothing holds `dest`: no aside is produced,
    /// so a sweep has nothing to reason about.
    #[test]
    fn replace_running_overwrites_a_destination_nobody_is_executing() {
        let temp = TempDir::new().unwrap();
        let staging = temp.path().join("jdk.exe.new");
        let dest = temp.path().join("jdk.exe");
        fs::write(&staging, b"v2").unwrap();
        fs::write(&dest, b"v1").unwrap();

        replace_running(&staging, &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"v2");
        assert!(!staging.exists());
        assert!(!temp.path().join("jdk.exe.old").exists());
    }
}
