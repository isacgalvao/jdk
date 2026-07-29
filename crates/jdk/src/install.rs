//! `jdk install <selector>`: catalog resolution + verified install, with an
//! indicatif progress bar over jdk-core's plain byte callback. `--from-shim`
//! keeps the same install but drops the next-step hints (the shim's caller
//! is mid-`java` invocation, not exploring) and declines proprietary license
//! terms — the machine's standing `accept-license` included — instead of
//! accepting them on that caller's behalf.

use crate::fail::Fail;
use crate::{remote, uninstall};
use indicatif::{ProgressBar, ProgressStyle};
use jdk_core::catalog::Origin;
use jdk_core::current::{self, Current};
use jdk_core::index::{Package, ReleaseStatus};
use jdk_resolve::selector::Selector;
use jdk_resolve::version::Version;
use jdk_resolve::{exit, store};
use std::io::{self, IsTerminal};
use std::path::Path;

pub fn run(root: &Path, selector: &str, from_shim: bool, accept_license: bool) -> Result<(), Fail> {
    uninstall::sweep_orphans(root);
    let selector = crate::parse_selector(selector)?;
    let config = crate::config(root)?;
    let (http, catalog) = remote::client(root)?;

    let (package, origin) = catalog
        .find(&http, &selector, &config.vendor)
        .map_err(Fail::engine)?;
    let name = format!("{}@{}", package.vendor, package.version);

    // Early access off the live API is the one resolution worth a line. A GA
    // build resolved live is the banal case — a release the index has not
    // picked up yet — and announcing it would put a stderr line in the middle
    // of every `java` that installs one in CI. Not a word about verification:
    // the sha256 is mandatory on BOTH paths, so the difference is which
    // catalog named the build, never whether the bytes were checked.
    if origin == Origin::Foojay && package.release_status == ReleaseStatus::Ea {
        eprintln!(
            "jdk: {name} resolved live from foojay — the index carries no \
             early-access build for this selector"
        );
    }

    // Proprietary-vendor terms, shown AND accepted before the binary is
    // fetched: the download carries the cookie by which the vendor records
    // that acceptance (`jdk_core::download` sends it), so consenting on the
    // user's behalf is not this tool's to give. A refusal returns here, with
    // no download of any kind attempted.
    if let Some(notice) = jdk_core::download::license_notice(&package.vendor)
        && !already_installed(root, &package)
    {
        let granted = Consent::of(accept_license, config.accept_license, from_shim);
        accept_terms(&selector, &name, notice, granted, from_shim)?;
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

/// Whether the store already holds exactly this package, keyed the way
/// `jdk_core::install` names its directories (vendor + parsed version) so the
/// answer means what it must here: the install will fetch nothing, hence
/// there is nothing to consent to, and `jdk install` stays idempotent for
/// proprietary vendors too. An unscannable store answers "no" — that install
/// is about to fail on the same store anyway, and asking is the safe side.
fn already_installed(root: &Path, package: &Package) -> bool {
    let Ok(version) = package.version.parse::<Version>() else {
        return false;
    };
    store::installed(root).is_ok_and(|candidates| {
        candidates
            .iter()
            .any(|candidate| candidate.vendor == package.vendor && candidate.version == version)
    })
}

/// Where a standing acceptance of proprietary terms came from, so the line
/// this prints names the thing the user would have to undo to withdraw it.
/// Saying "--accept-license" for a config-granted consent would send someone
/// hunting a flag they never typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consent {
    /// Nothing granted it up front; the prompt decides.
    Ask,
    /// `--accept-license` on this invocation.
    Flag,
    /// `accept-license = true` in `config.toml`.
    ConfigFile,
}

impl Consent {
    /// The flag settles it for one run, the config key for the whole machine;
    /// either is enough, and both at once is a redundant yes, not a conflict.
    ///
    /// The config key alone does NOT carry the shim path. The flag is typed on
    /// the very command that installs, so it is consent for this install by
    /// the person running it; the config key is ambient — written earlier,
    /// possibly by whoever set the machine up — and the shim's caller only
    /// typed `java`. Standing consent to install unattended is not consent for
    /// a third party's `java` to enter a license agreement in your name.
    fn of(flag: bool, config: bool, from_shim: bool) -> Consent {
        match (flag, config && !from_shim) {
            (true, _) => Consent::Flag,
            (false, true) => Consent::ConfigFile,
            (false, false) => Consent::Ask,
        }
    }

    /// How the acceptance was given, or `None` when it was not given at all.
    fn granted_by(self) -> Option<&'static str> {
        match self {
            Consent::Ask => None,
            Consent::Flag => Some("--accept-license"),
            Consent::ConfigFile => Some("accept-license = true in config.toml"),
        }
    }
}

/// Consent for a vendor under proprietary terms. `--accept-license` or
/// `accept-license = true` in `config.toml` settles it; otherwise the terms
/// are shown and the answer read from an interactive console, with refusal as
/// the default on every other path — plain Enter, EOF, or no console at all
/// (CI and IDE pipes decline instead of hanging).
///
/// The shim path never asks: `java` reaching for a missing JDK is not a
/// moment of informed consent, and someone who only typed `java` did not ask
/// to enter a license agreement. It refuses with the command that does.
fn accept_terms(
    selector: &Selector,
    name: &str,
    notice: &str,
    granted: Consent,
    from_shim: bool,
) -> Result<(), Fail> {
    eprintln!("jdk: {notice}");
    if let Some(via) = granted.granted_by() {
        eprintln!("jdk: terms accepted via {via}");
        return Ok(());
    }

    let interactive = !from_shim && io::stdin().is_terminal() && io::stderr().is_terminal();
    if interactive {
        eprint!("jdk: accept these terms and install {name}? [y/N] ");
    }
    let consented = decide_accept(interactive, || {
        let mut answer = String::new();
        match io::stdin().read_line(&mut answer) {
            // Ok(0) is EOF: no answer to honor, and the default is no.
            Ok(read) if read > 0 => Some(answer),
            _ => None,
        }
    });
    if consented {
        return Ok(());
    }
    // CONFIG, not FAILURE: nothing broke — the invocation is missing consent
    // the user alone can give, and the hint is the invocation that carries it.
    let mut fail = Fail::new(
        exit::CONFIG,
        format!("{name} was not installed: its license terms were not accepted"),
    )
    .hint(format!(
        "accept them with `jdk install {selector} --accept-license`"
    ));
    // Only off the shim path: there the config key is deliberately not
    // honored, so naming it would send someone to edit a file that changes
    // nothing about the `java` they just ran.
    if !from_shim {
        fail = fail.hint("or once for this machine with `accept-license = true` in config.toml");
    }
    Err(fail.hint("`jdk available` lists the open-source builds, which need no acceptance"))
}

/// The wiring at the heart of [`accept_terms`], pulled out so it can be
/// exercised without a real TTY or stdin: given whether the console is
/// interactive and a line reader, decide accept vs. refuse — no I/O of its
/// own. `read_answer` is only ever invoked when `interactive` is true; the
/// off-console refusal never consults it (same shape as `setup`'s
/// `decide_replace` and jdk-shim's `decide_install`). A bare Enter refuses:
/// accepting a license is never a default.
fn decide_accept(interactive: bool, read_answer: impl FnOnce() -> Option<String>) -> bool {
    if !interactive {
        return false;
    }
    match read_answer() {
        Some(answer) => {
            let answer = answer.trim();
            answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
        }
        None => false,
    }
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
    fn license_consent_defaults_to_refusal() {
        // No console (CI, the shim path, a pipe): refuse without reading —
        // a hang here would block someone's build for an answer they cannot
        // give.
        assert!(!decide_accept(false, || panic!(
            "an off-console prompt must not read stdin"
        )));
        // EOF and a bare Enter are both "no": accepting a license is never
        // what silence means.
        assert!(!decide_accept(true, || None));
        for answer in ["\n", "\r\n", " ", "n", "no", "sure", "yep"] {
            assert!(
                !decide_accept(true, || Some(answer.to_string())),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn license_consent_accepts_only_an_explicit_yes() {
        for answer in ["y", "Y", "yes", "YES", " yes \r\n"] {
            assert!(
                decide_accept(true, || Some(answer.to_string())),
                "{answer:?}"
            );
        }
    }

    /// Either route grants consent, and the message names the one that did:
    /// pointing at `--accept-license` when it was the config file sends the
    /// user hunting a flag they never typed.
    #[test]
    fn consent_comes_from_the_flag_or_the_config_and_says_which() {
        assert_eq!(Consent::of(false, false, false), Consent::Ask);
        assert_eq!(Consent::of(true, false, false), Consent::Flag);
        assert_eq!(Consent::of(false, true, false), Consent::ConfigFile);
        // Both: the flag is the one typed here, so it is the one named.
        assert_eq!(Consent::of(true, true, false), Consent::Flag);

        assert_eq!(Consent::of(false, false, false).granted_by(), None);
        assert_eq!(
            Consent::of(true, false, false).granted_by(),
            Some("--accept-license")
        );
        assert_eq!(
            Consent::of(false, true, false).granted_by(),
            Some("accept-license = true in config.toml")
        );
    }

    /// The shim path takes no ambient consent: `accept-license = true` was
    /// written for unattended installs, not to let someone else's `java` enter
    /// a license agreement in the machine owner's name.
    #[test]
    fn the_config_key_does_not_consent_on_the_shim_path() {
        assert_eq!(Consent::of(false, true, true), Consent::Ask);
        assert_eq!(Consent::of(false, false, true), Consent::Ask);
        assert_eq!(Consent::of(false, true, true).granted_by(), None);
        // Neither does the prompt, which the shim path never reaches.
        assert!(!decide_accept(false, || panic!(
            "the shim path must not ask"
        )));
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
