//! Windows-safe file primitives shared by download finalization, the cache
//! and (from M4 on) the `current` junction swap.

use std::fs;
use std::io;
use std::path::Path;

/// Rename that atomically replaces an existing destination FILE. On Windows
/// `fs::rename` fails with `AlreadyExists` when the target exists; the
/// fallback is `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`. Directories
/// cannot be replaced this way — callers renaming directories must guarantee
/// the target does not exist.
#[cfg(windows)]
pub fn atomic_rename(from: &Path, to: &Path) -> io::Result<()> {
    // A plain rename settles the move whenever `to` is free. Windows reports an
    // occupied destination as `AlreadyExists`, and only that case is handed to
    // the replacing move below; every other outcome goes back to the caller.
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => replace_existing(from, to),
        Err(err) => Err(err),
    }
}

/// Overwrites an existing `to` with `from` in one durable step:
/// `MOVEFILE_REPLACE_EXISTING` supplants the occupant and `MOVEFILE_WRITE_THROUGH`
/// holds the call until the change is flushed, so a crash cannot tear the swap.
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

#[cfg(not(windows))]
pub fn atomic_rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
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
}
