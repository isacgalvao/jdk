//! `jdk install <selector>`: catalog resolution + verified install, with an
//! indicatif progress bar over jdk-core's plain byte callback. `--from-shim`
//! keeps the same install but drops the next-step hints (the shim's caller
//! is mid-`java` invocation, not exploring).

use crate::fail::Fail;
use crate::{remote, uninstall};
use indicatif::{ProgressBar, ProgressStyle};
use jdk_core::current::{self, Current};
use std::path::Path;

pub fn run(root: &Path, selector: &str, from_shim: bool) -> Result<(), Fail> {
    uninstall::sweep_orphans(root);
    let selector = crate::parse_selector(selector)?;
    let config = crate::config(root)?;
    let (http, catalog) = remote::client(root)?;

    let package = catalog
        .find(&http, &selector, &config.vendor)
        .map_err(Fail::engine)?;
    let name = format!("{}@{}", package.vendor, package.version);

    // Proprietary-vendor terms, shown before the binary is fetched — in the
    // shim path too, where the user is otherwise handed the download silently.
    if let Some(notice) = jdk_core::download::license_notice(&package.vendor) {
        eprintln!("jdk: {notice}");
    }

    let bar = progress(&name, package.size);
    let mut on_progress = |done: u64, total: u64| {
        if total > 0 {
            bar.set_length(total);
        }
        bar.set_position(done);
    };
    let installed = jdk_core::install::install(root, &http, &package, Some(&mut on_progress));
    bar.finish_and_clear();
    let installed = installed.map_err(Fail::engine)?;

    let name = format!("{}@{}", installed.vendor, installed.version);
    if installed.fresh {
        eprintln!("jdk: installed {name}");
    } else {
        eprintln!("jdk: {name} is already installed");
    }

    // First install wins: with no valid global yet, this JDK becomes it —
    // symmetric to `setup` electing the best when a JDK already exists. A
    // healthy global is never disturbed (that stays `jdk use`).
    if establish_global_if_unset(root, &installed.dir)? && !from_shim {
        eprintln!("  → {name} is now the global default (change it with `jdk use`)");
    }

    if !from_shim {
        eprintln!("  → `jdk pin {name}` pins it for the current project");
    }
    Ok(())
}

/// Makes `installed_dir` the global — retargeting the `current` junction —
/// only when there is no valid global: nothing at `current`, or a junction
/// whose target no longer exists. A healthy junction and a foreign directory
/// are both left alone (explicit switches are `jdk use`, anomalies are
/// `jdk doctor`). Returns whether the global was (re)established.
fn establish_global_if_unset(root: &Path, installed_dir: &Path) -> Result<bool, Fail> {
    let establish = match current::inspect(root).map_err(Fail::engine)? {
        Current::Absent => true,
        Current::Junction { target } => !target.exists(),
        Current::NotJunction => false,
    };
    if establish {
        current::retarget(root, installed_dir).map_err(Fail::engine)?;
    }
    Ok(establish)
}

/// Byte-progress bar on stderr; indicatif hides it when stderr is not a
/// terminal, so shim spawns in CI stay clean.
fn progress(name: &str, size: u64) -> ProgressBar {
    let bar = ProgressBar::new(size.max(1));
    bar.set_style(
        ProgressStyle::with_template("{msg} {bytes}/{total_bytes} [{bar:32}] {bytes_per_sec}")
            .expect("static template")
            .progress_chars("=> "),
    );
    bar.set_message(format!("downloading {name}"));
    bar
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdk_resolve::store;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// An installed candidate with a runnable-looking `bin\java.exe`, the shape
    /// `establish_global_if_unset` retargets the junction at.
    fn fake_candidate(root: &Path, name: &str) -> PathBuf {
        let dir = store::java_candidates(root).join(name);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin").join("java.exe"), name.as_bytes()).unwrap();
        dir
    }

    #[test]
    fn first_install_becomes_the_global() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let jdk = fake_candidate(root, "temurin@21.0.5");
        assert_eq!(current::inspect(root).unwrap(), Current::Absent);

        assert!(establish_global_if_unset(root, &jdk).unwrap());
        assert_eq!(
            current::inspect(root).unwrap(),
            Current::Junction { target: jdk }
        );
    }

    #[test]
    fn a_second_install_does_not_steal_a_healthy_global() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let first = fake_candidate(root, "temurin@17.0.9");
        let second = fake_candidate(root, "temurin@21.0.5");
        current::retarget(root, &first).unwrap();

        assert!(!establish_global_if_unset(root, &second).unwrap());
        assert_eq!(
            current::inspect(root).unwrap(),
            Current::Junction { target: first },
            "a healthy global stays put; switching is `jdk use`"
        );
    }

    #[test]
    fn a_dead_global_is_reestablished() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let gone = fake_candidate(root, "temurin@17.0.9");
        current::retarget(root, &gone).unwrap();
        fs::remove_dir_all(&gone).unwrap(); // the global's target uninstalled

        let fresh = fake_candidate(root, "temurin@21.0.5");
        assert!(establish_global_if_unset(root, &fresh).unwrap());
        assert_eq!(
            current::inspect(root).unwrap(),
            Current::Junction { target: fresh }
        );
    }

    #[test]
    fn a_foreign_current_directory_is_left_for_doctor() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let jdk = fake_candidate(root, "temurin@21.0.5");
        fs::create_dir_all(store::current(root).join("precious")).unwrap();

        assert!(!establish_global_if_unset(root, &jdk).unwrap());
        assert_eq!(current::inspect(root).unwrap(), Current::NotJunction);
        assert!(store::current(root).join("precious").exists());
    }
}
