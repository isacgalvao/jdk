//! `jdk update`: self-update from this project's GitHub releases. The swap
//! is in-process — verified bundle into `cache\update\`, the running
//! `bin\jdk.exe` replaced through the same rename-aside the shims use
//! ([`jdk_core::file_ops::replace_running`]), the bundle's `jdk-shim.exe`
//! parked beside it, then the shims refreshed — so no second process is
//! spawned and nothing has to re-run `setup`.
//!
//! A failure AFTER the exe swap leaves a fully verified new jdk.exe with
//! possibly stale shims; the error says so, names every shim left behind
//! rather than the first one, and points at `jdk setup`, whose shim
//! materialization converges idempotently.

use crate::fail::{self, Fail};
use indicatif::{ProgressBar, ProgressStyle};
use jdk_core::Error;
use jdk_core::http::Http;
use jdk_core::{file_ops, release, shims};
use jdk_resolve::version::Version;
use jdk_resolve::{exit, store};
use std::fs;
use std::path::Path;

/// What every failure past the CLI swap has in common: the new jdk.exe is in
/// place, only the shims may still be the old build.
const SHIMS_LAG_BEHIND: &str =
    "the new jdk.exe is already in place; `jdk setup` converges the shims";

/// Extraction ceilings for a release bundle, in place of the JDK-sized
/// defaults [`jdk_core::extract::extract_zip`] carries: this archive holds
/// two executables, LICENSE and README, unpacking to some 30 MiB — not the
/// 15–30k entries and 4 GiB a real JDK justifies (BUG-10). The margin is
/// there for growth in the bundle, not for a zip bomb.
const MAX_UNPACKED: u64 = 128 * 1024 * 1024;
const MAX_FILES: usize = 64;

pub fn run(root: &Path, force: bool) -> Result<(), Fail> {
    let bin = root.join("bin");
    sweep_leftovers(&bin);
    let dest = bin.join("jdk.exe");
    guard_store_copy(&dest)?;

    let local: Version = env!("CARGO_PKG_VERSION")
        .parse()
        .expect("the crate version parses");
    let source = release::Source::resolve().map_err(Fail::engine)?;
    let http = Http::new(source.policy()).map_err(Fail::engine)?;
    let remote = release::latest(&http, &source).map_err(Fail::engine)?;
    if decide(&local, &remote, force) == Decision::Skip {
        eprintln!("jdk: already up to date ({local})");
        // The one reading of "up to date" that leaves a user wanting to go
        // the other way: this build is AHEAD of the latest release. Nothing
        // here installs a named version — the installer does (BUG-08).
        if remote < local {
            eprintln!(
                "  → this build is ahead of the latest release ({remote}); install.ps1 -Version {remote} goes back to it"
            );
        }
        return Ok(());
    }

    // Staging under the store keeps every rename below on one volume
    // (MoveFileExW stays atomic); a leftover of an interrupted run is
    // discarded rather than trusted.
    let staging = store::cache(root).join("update");
    let _ = fs::remove_dir_all(&staging);

    let bar = progress(&remote);
    let mut on_progress = |done: u64, total: u64| {
        if total > 0 {
            bar.set_length(total);
        }
        bar.set_position(done);
    };
    let bundle = release::fetch_bundle(&http, &source, &remote, &staging, Some(&mut on_progress));
    bar.finish_and_clear();
    let bundle = bundle.map_err(Fail::engine)?;

    let extracted = staging.join("stage");
    jdk_core::extract::extract_zip_capped(&bundle, &extracted, MAX_UNPACKED, MAX_FILES)
        .map_err(Fail::engine)?;
    let new_cli = extracted.join("jdk.exe");
    let new_shim = extracted.join("jdk-shim.exe");
    if !new_cli.exists() || !new_shim.exists() {
        return Err(Fail::new(
            exit::FAILURE,
            format!("the v{remote} bundle does not carry jdk.exe and jdk-shim.exe side by side"),
        )
        .hint("reinstall with install.ps1"));
    }

    // The CLI swap: staged next to the destination, then the rename-aside
    // handles the fact that `dest` is THIS running process.
    swap_in(&new_cli, &dest)?;
    // The shim template belongs beside it, and lands there BEFORE the shims
    // are rewritten: `jdk setup` — what the hints below point at —
    // materializes from the copy next to jdk.exe, and the staging carrying
    // the bundle's shim is discarded a few lines down.
    let stored_shim = bin.join("jdk-shim.exe");
    swap_in(&new_shim, &stored_shim).map_err(|fail| fail.hint(SHIMS_LAG_BEHIND))?;

    let shims = shims::materialize(&stored_shim, &store::shims(root))
        .map_err(|err| Fail::engine(err).hint(SHIMS_LAG_BEHIND))?;
    if !shims.failed.is_empty() {
        return Err(fail::shims_failed(&shims.failed).hint(SHIMS_LAG_BEHIND));
    }

    let _ = fs::remove_dir_all(&staging);
    eprintln!("jdk: updated {local} → {remote}");
    if dest.with_extension("exe.old").exists() {
        eprintln!(
            "  → the old copy was moved aside to jdk.exe.old; the next `jdk update` cleans it up"
        );
    }
    // Said right where the version changed, because the aside above is swept
    // by the next run and is no rollback artifact to count on (BUG-08). Not
    // said when `--force` reinstalled the same version: there is nowhere to
    // go back TO.
    if remote != local {
        eprintln!("  → to go back: install.ps1 -Version {local} installs a specific version");
    }
    Ok(())
}

/// Stages `source` as `<dest>.new` and swaps it in, tolerating a `dest` that
/// is EXECUTING — the running jdk.exe is one of them. `source` must already
/// sit on the destination's volume, which is what staging the bundle under
/// `<root>\cache` buys.
fn swap_in(source: &Path, dest: &Path) -> Result<(), Fail> {
    let incoming = dest.with_extension("exe.new");
    // Through the engine error rather than a bespoke message: this is one of
    // the `.exe` renames an antivirus handle or a full volume refuses, and
    // that is where the hints for both live (BUG-11).
    file_ops::atomic_rename(source, &incoming)
        .map_err(|err| Fail::engine(Error::io("stage the new build at", &incoming)(err)))?;
    if let Err(err) = file_ops::replace_running(&incoming, dest) {
        // Never leave a staging orphan behind, whatever failed.
        let _ = fs::remove_file(&incoming);
        return Err(Fail::engine(err));
    }
    Ok(())
}

/// Whether to touch anything at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Update,
    Skip,
}

/// A strictly newer remote updates; `--force` reinstalls regardless (the
/// escape hatch for a damaged copy). A remote at or below the local version
/// never updates for free — no silent downgrade.
fn decide(local: &Version, remote: &Version, force: bool) -> Decision {
    if force || remote > local {
        Decision::Update
    } else {
        Decision::Skip
    }
}

/// Only the store copy may update itself in place: a cargo-installed binary
/// lives under `~\.cargo\bin` and belongs to cargo, and a loose build is not
/// silently replaced either (the rustup/uv stance). Junctions and 8.3 short
/// names are folded by canonicalizing both sides.
fn guard_store_copy(dest: &Path) -> Result<(), Fail> {
    let me = std::env::current_exe().map_err(|err| {
        Fail::new(
            exit::FAILURE,
            format!("cannot locate the running jdk.exe: {err}"),
        )
    })?;
    if let (Ok(a), Ok(b)) = (fs::canonicalize(&me), fs::canonicalize(dest))
        && a == b
    {
        return Ok(());
    }
    Err(Fail::new(
        exit::FAILURE,
        format!(
            "this jdk.exe runs from {}, not from the store copy {} that `jdk update` maintains",
            me.display(),
            dest.display()
        ),
    )
    .hint("installed with cargo? update with `cargo install jdk` instead")
    .hint("otherwise reinstall with install.ps1, which places the store copy"))
}

/// Clears what an interrupted update left in `bin`: a `*.exe.new` staging is
/// always garbage, while a `*.exe.old` aside is garbage only once the
/// executable it was moved aside from is back in place. That condition is the
/// whole point — `replace_running` empties `jdk.exe` before the replacement
/// lands, so a death in that window leaves the aside as the machine's only
/// copy of the CLI, the one jdk-shim restores from. Best-effort otherwise: a
/// leftover whose process is still alive cannot be deleted and is silently
/// left for a later run.
fn sweep_leftovers(bin: &Path) {
    let Ok(entries) = fs::read_dir(bin) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let sweepable = match name.strip_suffix(".old") {
            Some(live) => live.ends_with(".exe") && bin.join(live).exists(),
            None => name.ends_with(".exe.new"),
        };
        if sweepable {
            let path = entry.path();
            // A DIRECTORY can end up on either name — moved aside from a
            // `dest` that was one — and `remove_file` cannot touch it (os
            // error 5), which would strand it forever against the promise
            // `run` prints. The test above already established this leftover
            // is superseded garbage, so removing it wholesale is right. A file
            // merely in use fails both calls and is left for a later run.
            if fs::remove_file(&path).is_err() {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
}

/// Byte-progress bar on stderr, same shape as install's; indicatif hides it
/// when stderr is not a terminal.
fn progress(version: &Version) -> ProgressBar {
    let bar = ProgressBar::new(1);
    bar.set_style(
        ProgressStyle::with_template("{msg} {bytes}/{total_bytes} [{bar:32}] {bytes_per_sec}")
            .expect("static template")
            .progress_chars("=> "),
    );
    bar.set_message(format!("downloading jdk v{version}"));
    bar
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn updates_only_to_a_strictly_newer_release() {
        assert_eq!(decide(&v("0.3.0"), &v("0.4.0"), false), Decision::Update);
        assert_eq!(decide(&v("0.3.0"), &v("0.3.0"), false), Decision::Skip);
        assert_eq!(
            decide(&v("0.4.0"), &v("0.3.0"), false),
            Decision::Skip,
            "never a silent downgrade"
        );
    }

    #[test]
    fn force_reinstalls_wherever_the_versions_stand() {
        assert_eq!(decide(&v("0.3.0"), &v("0.3.0"), true), Decision::Update);
        assert_eq!(decide(&v("0.4.0"), &v("0.3.0"), true), Decision::Update);
    }

    /// The settled state: the swap finished, so the aside has been superseded
    /// and the staging is an orphan (BUG-05 — the old filter ignored `.new`).
    /// A DIRECTORY aside goes too: a `dest` that was a directory is renamed
    /// aside as one, and `remove_file` alone would strand it in `bin` forever
    /// while `run` promises the next update cleans it up.
    #[test]
    fn sweep_clears_the_staging_and_a_superseded_aside() {
        let temp = TempDir::new().unwrap();
        let bin = temp.path();
        fs::write(bin.join("jdk.exe"), b"live").unwrap();
        fs::write(bin.join("jdk.exe.old"), b"superseded").unwrap();
        fs::write(bin.join("jdk.exe.new"), b"orphaned staging").unwrap();
        fs::write(bin.join("config.old"), b"someone else's file").unwrap();
        fs::create_dir_all(bin.join("jdk-shim.exe.old").join("inside")).unwrap();
        fs::write(bin.join("jdk-shim.exe"), b"live shim").unwrap();

        sweep_leftovers(bin);

        assert!(!bin.join("jdk.exe.old").exists());
        assert!(!bin.join("jdk.exe.new").exists());
        assert!(
            !bin.join("jdk-shim.exe.old").exists(),
            "directory aside too"
        );
        assert_eq!(fs::read(bin.join("jdk.exe")).unwrap(), b"live");
        assert!(
            bin.join("config.old").exists(),
            "only executable leftovers are ours to delete"
        );
    }

    /// BUG-04: killed inside the swap window, `bin` holds no jdk.exe and the
    /// aside is the only copy of the CLI left on the machine. Deleting it here
    /// is what turned a crash into a bricked installation.
    #[test]
    fn sweep_spares_the_aside_while_jdk_exe_is_missing() {
        let temp = TempDir::new().unwrap();
        let bin = temp.path();
        fs::write(bin.join("jdk.exe.old"), b"the only copy left").unwrap();
        fs::write(bin.join("jdk.exe.new"), b"orphaned staging").unwrap();

        sweep_leftovers(bin);

        assert_eq!(
            fs::read(bin.join("jdk.exe.old")).unwrap(),
            b"the only copy left"
        );
        assert!(
            !bin.join("jdk.exe.new").exists(),
            "a staging orphan is garbage either way"
        );
    }
}
