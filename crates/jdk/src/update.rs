//! `jdk update`: self-update from this project's GitHub releases. The swap
//! is in-process — verified bundle into `cache\update\`, the running
//! `bin\jdk.exe` replaced through the same rename-aside the shims use
//! ([`jdk_core::file_ops::replace_running`]), then the shims refreshed — so
//! no second process is spawned and nothing has to re-run `setup`.
//!
//! A failure AFTER the exe swap leaves a fully verified new jdk.exe with
//! possibly stale shims; the error says so and points at `jdk setup`, whose
//! shim materialization converges idempotently.

use crate::fail::Fail;
use indicatif::{ProgressBar, ProgressStyle};
use jdk_core::http::Http;
use jdk_core::{file_ops, release, shims};
use jdk_resolve::version::Version;
use jdk_resolve::{exit, store};
use std::fs;
use std::path::Path;

pub fn run(root: &Path, force: bool) -> Result<(), Fail> {
    let bin = root.join("bin");
    sweep_old(&bin);
    let dest = bin.join("jdk.exe");
    guard_store_copy(&dest)?;

    let local: Version = env!("CARGO_PKG_VERSION")
        .parse()
        .expect("the crate version parses");
    let (base, policy) = release::base_url();
    let http = Http::new(policy).map_err(Fail::engine)?;
    let remote = release::latest(&http, &base).map_err(Fail::engine)?;
    if decide(&local, &remote, force) == Decision::Skip {
        eprintln!("jdk: already up to date ({local})");
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
    let bundle = release::fetch_bundle(&http, &base, &remote, &staging, Some(&mut on_progress));
    bar.finish_and_clear();
    let bundle = bundle.map_err(Fail::engine)?;

    let extracted = staging.join("stage");
    jdk_core::extract::extract_zip(&bundle, &extracted).map_err(Fail::engine)?;
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
    let incoming = dest.with_extension("exe.new");
    file_ops::atomic_rename(&new_cli, &incoming).map_err(|err| {
        Fail::new(
            exit::FAILURE,
            format!(
                "cannot stage the new jdk.exe at {}: {err}",
                incoming.display()
            ),
        )
    })?;
    if let Err(err) = file_ops::replace_running(&incoming, &dest) {
        // Never leave a staging orphan behind, whatever failed.
        let _ = fs::remove_file(&incoming);
        return Err(Fail::engine(err));
    }

    shims::materialize(&new_shim, &store::shims(root)).map_err(|err| {
        Fail::engine(err)
            .hint("the new jdk.exe is already in place; `jdk setup` converges the shims")
    })?;

    let _ = fs::remove_dir_all(&staging);
    eprintln!("jdk: updated {local} → {remote}");
    if dest.with_extension("exe.old").exists() {
        eprintln!(
            "  → the old copy was moved aside to jdk.exe.old; the next `jdk update` cleans it up"
        );
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
/// names are folded by canonicalizing both sides, same as setup's
/// `place_cli`.
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

/// Clears `bin\*.exe.old` leftovers of a previous while-running update.
/// Best-effort by design: an `.old` whose process is still alive cannot be
/// deleted and is silently left for a later run.
fn sweep_old(bin: &Path) {
    let Ok(entries) = fs::read_dir(bin) else {
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
}
