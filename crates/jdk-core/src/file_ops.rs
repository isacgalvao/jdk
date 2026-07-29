//! Windows-safe file primitives shared by download finalization, the cache,
//! the `current` junction swap and the shim/CLI executable swaps.

use crate::error::{Error, Result};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// How long ONE [`replace_running`] may spend waiting out refusals that are
/// nobody's fault — shared by its steps rather than granted to each, so the
/// ceiling a caller can quote is this number and not a multiple of it.
///
/// Real-time antivirus and EDR open a handle on a just-written `.exe` to scan
/// it, and a rename landing inside that window is refused with one of the two
/// [`in_use`] codes; the scan of a file this size is over in tens of
/// milliseconds. Two seconds is generous for that and still short enough that
/// a PERMANENT block fails while the user is watching, instead of after a
/// pause they would read as a hang. Only two shapes are permanent: a handle
/// nobody releases, and a DIRECTORY occupying the `.exe.old` aside name,
/// which the pre-clear below cannot shift (`remove_file` refuses a directory)
/// and a rename cannot replace, leaving the swap nowhere to move the
/// occupant. The near misses are worth naming because they look permanent and
/// are not: a read-only `dest` OR a read-only aside both give way, since
/// `fs::remove_file` clears the attribute before deleting; and a directory on
/// `dest` ITSELF is simply renamed aside like any other occupant. Nothing
/// here waits for a running executable to exit either — that refusal has its
/// own answer (the rename-aside), so there is no case a longer budget would
/// rescue.
const RETRY_BUDGET: Duration = Duration::from_secs(2);

/// First pause, doubled per attempt until [`RETRY_BUDGET`] is spent: 50, 100,
/// 200, 400, 800 ms. The first is shorter than the failed call it follows, so
/// a scan that ends promptly costs nothing anyone can perceive.
const RETRY_BACKOFF: Duration = Duration::from_millis(50);

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
/// either way), so the destination never silently vanishes.
///
/// Every step that CAN succeed on a second try gets one: an antivirus holding
/// a handle on a file this code just wrote refuses the rename with an
/// [`in_use`] code, and the handle is gone milliseconds later (BUG-06). The
/// first call is the exception, and deliberately so — its refusal is how a
/// RUNNING destination announces itself, which waiting never cures, so it goes
/// straight to the rename-aside rather than charging every self-update the
/// full [`RETRY_BUDGET`] for a call that cannot succeed while this process
/// lives.
pub fn replace_running(staging: &Path, dest: &Path) -> Result<()> {
    let deadline = Instant::now() + RETRY_BUDGET;
    match replace_existing(staging, dest) {
        Ok(()) => Ok(()),
        Err(err) if in_use(&err) && dest.exists() => {
            let aside = dest.with_extension("exe.old");
            // A leftover `.old` still running blocks the rename below; the
            // resulting error is the honest answer for that corner.
            let _ = fs::remove_file(&aside);
            retrying(deadline, dest, || fs::rename(dest, &aside))
                .map_err(Error::io("move the running executable aside from", dest))?;
            retrying(deadline, dest, || replace_existing(staging, dest)).map_err(|err| {
                let _ = fs::rename(&aside, dest);
                Error::io("place", dest)(err)
            })
        }
        // Nothing to move aside, so nothing about the destination explains the
        // refusal: whatever holds the swap holds `staging` or the directory,
        // and only time releases that.
        Err(err) if in_use(&err) => retrying(deadline, dest, || replace_existing(staging, dest))
            .map_err(Error::io("place", dest)),
        Err(err) => Err(Error::io("place", dest)(err)),
    }
}

/// Repeats `step` while Windows answers that the file is in use, until
/// `deadline`; any other error returns at once, and so does the budget running
/// out. Announced on stderr once, at the first pause — the engine has no UI,
/// and a swap that goes quiet for seconds is indistinguishable from a hang
/// (the shape `http`'s retry announcement settled on, from the first retry
/// rather than the first attempt, so a healthy swap stays silent).
fn retrying(
    deadline: Instant,
    path: &Path,
    mut step: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut delay = RETRY_BACKOFF;
    let mut announced = false;
    loop {
        let Err(err) = step() else { return Ok(()) };
        let left = deadline.saturating_duration_since(Instant::now());
        if !in_use(&err) || left.is_zero() {
            return Err(err);
        }
        if !announced {
            eprintln!(
                "jdk: {} is in use ({err}) — retrying for up to {left:.0?}",
                path.display()
            );
            announced = true;
        }
        thread::sleep(delay.min(left));
        delay = delay.saturating_mul(2);
    }
}

/// Deletes an executable, tolerating one that is RUNNING — which is the normal
/// case for `setup --undo`, invoked through the very `bin\jdk.exe` it is
/// removing. Win32 refuses to delete an executable in use but allows renaming
/// it, the same asymmetry [`replace_running`] leans on, so the file is moved
/// aside to `<name>.exe.old` and deleted best-effort. While the process lives
/// that delete cannot succeed, so the aside is what stays behind: it comes back
/// as `Ok(Some(..))` for the caller to REPORT rather than to quietly count as
/// removed. A file already gone is `Ok(None)` — the goal state, not a failure.
pub fn remove_running(path: &Path) -> Result<Option<PathBuf>> {
    match fs::remove_file(path) {
        Ok(()) => Ok(None),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) if in_use(&err) => {
            let aside = path.with_extension("exe.old");
            // A leftover `.old` from an earlier run would block the rename;
            // if it too is undeletable the rename's error is the honest answer.
            let _ = fs::remove_file(&aside);
            fs::rename(path, &aside)
                .map_err(Error::io("move the running executable aside from", path))?;
            Ok(fs::remove_file(&aside).is_err().then_some(aside))
        }
        Err(err) => Err(Error::io("remove", path)(err)),
    }
}

/// Whether Windows refused because something else is using the file — the
/// refusal a rename can still get around, and the one worth waiting out. There
/// are TWO of them, and taking only the first makes the outcome a race:
/// ACCESS_DENIED (5) is what a mapped executable image raises,
/// SHARING_VIOLATION (32) what an open handle without `FILE_SHARE_DELETE`
/// raises, and a starting process gives one or the other depending on how far
/// its loader has got.
pub fn in_use(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows) && err.raw_os_error() == Some(32))
}

/// Whether Windows refused for want of space, which no retry and no rename
/// gets around — the caller's only use for it is telling the user WHY. Two
/// codes again: DISK_FULL (112) for a write that found no room,
/// HANDLE_DISK_FULL (39) for one that ran the volume out mid-flush. The kind
/// covers both on a current toolchain; the raw codes are checked as well so
/// the answer does not hinge on std's mapping table.
pub fn disk_full(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::StorageFull
        || (cfg!(windows) && matches!(err.raw_os_error(), Some(39 | 112)))
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

    /// First placement, where there is no occupant at all: `jdk setup` on a
    /// machine with an empty shims directory, and the `bin\jdk-shim.exe` an
    /// update parks beside the CLI. The callers reach it only through
    /// `shims::materialize`, whose tests are `cfg(windows)`, so nothing on a
    /// non-Windows build exercised it. Catches any precondition that assumes
    /// `dest` is occupied — a rename-aside taken unconditionally would look for
    /// something to move and fail every fresh install.
    #[test]
    fn replace_running_places_a_destination_that_does_not_exist_yet() {
        let temp = TempDir::new().unwrap();
        let staging = temp.path().join("java.exe.new");
        let dest = temp.path().join("java.exe");
        fs::write(&staging, b"v1").unwrap();

        replace_running(&staging, &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"v1");
        assert!(!staging.exists());
        assert!(!temp.path().join("java.exe.old").exists());
    }

    #[test]
    fn remove_running_deletes_a_file_nobody_is_executing_and_forgives_a_missing_one() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("jdk.exe");
        fs::write(&path, b"v1").unwrap();

        assert_eq!(remove_running(&path).unwrap(), None);

        assert!(!path.exists());
        assert!(!temp.path().join("jdk.exe.old").exists(), "no aside needed");
        assert_eq!(
            remove_running(&path).unwrap(),
            None,
            "already gone is the goal state"
        );
    }

    /// The self-removal `setup --undo` performs: the binary doing the removing
    /// is the one being removed. It cannot vanish while its image is mapped, so
    /// what the caller gets back is the aside it must own up to.
    #[cfg(windows)]
    #[test]
    fn remove_running_moves_a_live_executable_aside_and_reports_the_leftover() {
        let temp = TempDir::new().unwrap();
        let (_, fake_java) = test_support::shim_binaries();
        let path = temp.path().join("jdk.exe");
        fs::copy(&fake_java, &path).unwrap();
        let mut child = std::process::Command::new(&path)
            .env("FAKE_JAVA_SLEEP_MS", "30000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the copy so its image is mapped");

        let leftover = remove_running(&path).unwrap();

        assert_eq!(leftover, Some(temp.path().join("jdk.exe.old")));
        assert!(!path.exists(), "the name is free again");
        assert!(leftover.unwrap().exists(), "and the aside is still on disk");

        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// Windows tells the two refusals apart by raw code, not by kind, so the
    /// mapping is worth pinning: 32 and 39 have no `ErrorKind` of their own
    /// and would fall through a kind-only test.
    #[test]
    fn the_two_windows_refusals_are_recognized_by_code() {
        assert!(in_use(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(disk_full(&io::Error::from(io::ErrorKind::StorageFull)));
        if cfg!(windows) {
            assert!(in_use(&io::Error::from_raw_os_error(32)), "sharing");
            assert!(disk_full(&io::Error::from_raw_os_error(39)), "handle full");
            assert!(disk_full(&io::Error::from_raw_os_error(112)), "disk full");
        }
        assert!(!in_use(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(!disk_full(&io::Error::from(io::ErrorKind::NotFound)));
    }

    /// BUG-06: the swap that lands mid-scan used to be a hard failure, and on
    /// a machine with real-time protection that is a coin toss on every
    /// update. Nothing is executing here — the refusal comes from a handle
    /// alone — so only the retry can make this succeed.
    #[cfg(windows)]
    #[test]
    fn a_swap_waits_out_a_handle_that_is_released() {
        let temp = TempDir::new().unwrap();
        let staging = temp.path().join("jdk.exe.new");
        let dest = temp.path().join("jdk.exe");
        fs::write(&staging, b"v2").unwrap();
        fs::write(&dest, b"v1").unwrap();

        let held = test_support::hold(&dest);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            drop(held);
        });

        replace_running(&staging, &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"v2");
        assert!(!staging.exists());
    }

    /// The other end of the budget: a handle nobody ever releases is a
    /// permanent block, and the swap must say so rather than wait forever.
    /// The destination keeps its bytes — the aside is what could not be taken.
    #[cfg(windows)]
    #[test]
    fn a_swap_gives_up_on_a_handle_that_is_never_released() {
        let temp = TempDir::new().unwrap();
        let staging = temp.path().join("jdk.exe.new");
        let dest = temp.path().join("jdk.exe");
        fs::write(&staging, b"v2").unwrap();
        fs::write(&dest, b"v1").unwrap();
        let _held = test_support::hold(&dest);

        let err = replace_running(&staging, &dest).unwrap_err();

        assert!(err.to_string().contains("aside"), "{err}");
        assert_eq!(fs::read(&dest).unwrap(), b"v1", "the live copy survives");
    }

    /// The rollback: a handle on the STAGING blocks the placement but not the
    /// aside, so `dest` is emptied and then cannot be refilled — the one path
    /// where the destination would vanish. Holding a real handle is what makes
    /// this reachable without fault injection.
    #[cfg(windows)]
    #[test]
    fn a_placement_that_fails_after_the_aside_puts_the_destination_back() {
        let temp = TempDir::new().unwrap();
        let staging = temp.path().join("jdk.exe.new");
        let dest = temp.path().join("jdk.exe");
        fs::write(&staging, b"v2").unwrap();
        fs::write(&dest, b"v1").unwrap();
        let _held = test_support::hold(&staging);

        let err = replace_running(&staging, &dest).unwrap_err();

        assert!(err.to_string().contains("place"), "{err}");
        assert_eq!(
            fs::read(&dest).unwrap(),
            b"v1",
            "the aside was rolled back onto the destination"
        );
        assert!(!temp.path().join("jdk.exe.old").exists(), "and consumed");
    }

    /// A swap that cannot happen must cost the destination nothing. The
    /// tempting Windows "fix" for the ACCESS_DENIED a running image returns is
    /// to delete `dest` before renaming over it; with a staging that never
    /// arrived, that shortcut leaves the machine with no `jdk.exe` and no
    /// aside to restore from — the bricked state BUG-04 was about. A missing
    /// staging is the one reachable failure on both platforms (ERROR_FILE_NOT_FOUND
    /// and ENOENT both surface as `NotFound`, never `PermissionDenied`), so it
    /// takes the plain arm and may not touch `dest`.
    #[test]
    fn a_swap_that_cannot_run_leaves_the_destination_untouched() {
        let temp = TempDir::new().unwrap();
        let staging = temp.path().join("jdk.exe.new");
        let dest = temp.path().join("jdk.exe");
        fs::write(&dest, b"v1").unwrap();

        let err = replace_running(&staging, &dest).unwrap_err();

        assert!(err.to_string().contains("place"), "{err}");
        assert_eq!(fs::read(&dest).unwrap(), b"v1", "the live copy survives");
        assert!(
            !temp.path().join("jdk.exe.old").exists(),
            "only a PermissionDenied refusal may move the destination aside"
        );
    }
}
