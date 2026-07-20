//! `jdk setup`: the once-per-user step that makes Windows see the store —
//! JAVA_HOME (write-once, decision 8), PATH prepend of `shims\`, shim
//! materialization, `bin\jdk.exe`, and the `current` junction when a global
//! JDK is eligible. Idempotent: a second run reports a clean no-op.
//!
//! Defensive on foreign state: a JAVA_HOME set by another tool is only
//! replaced with consent (TTY prompt, or `--yes` for scripts), and the old
//! value+type is saved to config.toml first for a future `setup --undo`.

use crate::fail::Fail;
use jdk_core::config::JavaHomeBefore;
use jdk_core::env::{self, JavaHomeState, RegKey};
use jdk_core::{current, shims};
use jdk_resolve::config::Config;
use jdk_resolve::exit;
use jdk_resolve::store::{self, Candidate};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

pub fn run(root: &Path, yes: bool, shim_source: Option<&Path>) -> Result<(), Fail> {
    let config = crate::config(root)?;
    let (key, hermetic) = crate::user_env()?;
    let mut real = || env::broadcast_change();
    let mut none = || {};
    // The broadcast is an injected effect: hermetic runs (JDK_ENV_KEY set,
    // i.e. a disposable registry subkey) must never signal the real desktop.
    let broadcast: &mut dyn FnMut() = if hermetic { &mut none } else { &mut real };
    apply(root, &config, &key, yes, shim_source, broadcast)
}

fn apply(
    root: &Path,
    config: &Config,
    key: &RegKey,
    yes: bool,
    shim_source: Option<&Path>,
    broadcast: &mut dyn FnMut(),
) -> Result<(), Fail> {
    // Consent is settled before ANYTHING mutates: a refusal leaves the
    // machine exactly as it was.
    let junction = store::current(root);
    let state = env::java_home_state(key, &junction).map_err(Fail::engine)?;
    if let JavaHomeState::Foreign(old) = &state
        && !yes
        && !confirm_replace(old)?
    {
        return Err(Fail::new(
            exit::FAILURE,
            format!(
                "JAVA_HOME is currently {} ({}), set outside jdk — nothing was changed",
                old.text,
                old.kind()
            ),
        )
        .hint("jdk setup --yes replaces it (the old value is saved to config.toml)"));
    }

    materialize_shims(root, shim_source)?;
    place_cli(root);

    let java_home_changed = write_java_home(root, config, key, &junction, state)?;
    // bin before shims so the final PATH reads `shims;bin;…` — the shims
    // must shadow any other java on PATH; bin (jdk.exe itself) only needs
    // to be reachable from new shells.
    let bin_changed = env::prepend_path(key, &root.join("bin")).map_err(Fail::engine)?;
    if bin_changed {
        eprintln!(
            "jdk: PATH now contains {} (the jdk command itself)",
            root.join("bin").display()
        );
    } else {
        eprintln!("jdk: PATH already contains the bin directory");
    }
    let shims_changed = env::prepend_path(key, &store::shims(root)).map_err(Fail::engine)?;
    if shims_changed {
        eprintln!("jdk: PATH now starts with {}", store::shims(root).display());
    } else {
        eprintln!("jdk: PATH already contains the shims directory");
    }

    // Only a real registry mutation is worth telling the desktop about.
    if java_home_changed || bin_changed || shims_changed {
        broadcast();
        eprintln!("jdk: environment change broadcast — new consoles see it without a logoff");
    }

    ensure_junction(root, config)?;
    Ok(())
}

/// Copies `jdk-shim.exe` (next to the running jdk.exe, or `--shim-source`)
/// over the v0.1 tool set.
fn materialize_shims(root: &Path, shim_source: Option<&Path>) -> Result<(), Fail> {
    let source = match shim_source {
        Some(path) => path.to_path_buf(),
        None => sibling("jdk-shim.exe")?,
    };
    if !source.exists() {
        return Err(Fail::new(
            exit::FAILURE,
            format!("jdk-shim.exe not found at {}", source.display()),
        )
        .hint("reinstall with install.ps1, which places jdk-shim.exe next to jdk.exe")
        .hint("or point --shim-source at a jdk-shim.exe build"));
    }
    let written = shims::materialize(&source, &store::shims(root)).map_err(Fail::engine)?;
    if written.is_empty() {
        eprintln!(
            "jdk: shims already up to date ({} tools)",
            shims::TOOLS.len()
        );
    } else {
        eprintln!("jdk: materialized shims: {}", written.join(", "));
    }
    Ok(())
}

fn sibling(name: &str) -> Result<PathBuf, Fail> {
    let me = std::env::current_exe().map_err(|err| {
        Fail::new(
            exit::FAILURE,
            format!("cannot locate jdk.exe itself: {err}"),
        )
    })?;
    Ok(me.with_file_name(name))
}

/// Copies the running jdk.exe to `<root>\bin\jdk.exe` (decision 7 layout —
/// where the shim's auto-install looks for it). Best-effort: a locked or
/// unreadable copy is a warning, and doctor reports the skew.
fn place_cli(root: &Path) {
    let Ok(me) = std::env::current_exe() else {
        return;
    };
    let dest = root.join("bin").join("jdk.exe");
    if let (Ok(a), Ok(b)) = (fs::canonicalize(&me), fs::canonicalize(&dest))
        && a == b
    {
        eprintln!("jdk: running from the store copy ({})", dest.display());
        return;
    }
    let payload = match fs::read(&me) {
        Ok(payload) => payload,
        Err(err) => {
            eprintln!(
                "jdk: warning: cannot read {} to copy it: {err}",
                me.display()
            );
            return;
        }
    };
    if fs::read(&dest).is_ok_and(|existing| existing == payload) {
        eprintln!("jdk: {} already up to date", dest.display());
        return;
    }
    let staged = dest.with_extension("exe.new");
    let placed = fs::create_dir_all(dest.parent().expect("bin dir has a parent"))
        .and_then(|()| fs::write(&staged, &payload))
        .and_then(|()| jdk_core::file_ops::atomic_rename(&staged, &dest));
    match placed {
        Ok(()) => eprintln!("jdk: copied jdk.exe to {}", dest.display()),
        Err(err) => {
            let _ = fs::remove_file(&staged);
            eprintln!("jdk: warning: cannot place {}: {err}", dest.display());
        }
    }
}

/// The write-once JAVA_HOME (decision 8): absolute junction path as REG_SZ.
/// Consent for the Foreign case was granted upstream. Returns whether the
/// registry changed.
fn write_java_home(
    root: &Path,
    config: &Config,
    key: &RegKey,
    junction: &Path,
    state: JavaHomeState,
) -> Result<bool, Fail> {
    match state {
        JavaHomeState::Ours => {
            eprintln!("jdk: JAVA_HOME already set to {}", junction.display());
            Ok(false)
        }
        JavaHomeState::Absent => {
            env::set_java_home(key, junction).map_err(Fail::engine)?;
            eprintln!(
                "jdk: JAVA_HOME set to {} (this value never changes — `jdk use` retargets the junction instead)",
                junction.display()
            );
            Ok(true)
        }
        JavaHomeState::Foreign(old) => {
            let backup = JavaHomeBefore {
                value: old.text.clone(),
                expandable: old.expandable,
            };
            // Save BEFORE overwriting, and a failed save ABORTS: replacing
            // a value we could not back up would lose it forever (a future
            // `setup --undo` depends on it).
            jdk_core::config::save_java_home_before(root, config, &backup).map_err(|err| {
                Fail::new(
                    exit::FAILURE,
                    format!("cannot back up the current JAVA_HOME, so it was NOT replaced: {err}"),
                )
                .hint(format!("the value left untouched: {}", old.text))
                .hint("fix the store (permissions/disk) or the value itself, then re-run jdk setup")
            })?;
            eprintln!(
                "jdk: previous JAVA_HOME saved to config.toml ({})",
                old.text
            );
            env::set_java_home(key, junction).map_err(Fail::engine)?;
            eprintln!("jdk: JAVA_HOME set to {}", junction.display());
            Ok(true)
        }
    }
}

/// Consent for replacing a foreign JAVA_HOME: asks only when stdin AND
/// stderr are a TTY (CI/pipes get the actionable error instead of a hang);
/// plain Enter keeps the existing value (destructive default = no).
fn confirm_replace(old: &env::EnvValue) -> Result<bool, Fail> {
    let is_tty = io::stdin().is_terminal() && io::stderr().is_terminal();
    if is_tty {
        eprint!(
            "jdk: JAVA_HOME is currently {} ({}), set outside jdk. Replace it? [y/N] ",
            old.text,
            old.kind()
        );
    }
    Ok(decide_replace(is_tty, || {
        let mut answer = String::new();
        match io::stdin().read_line(&mut answer) {
            Ok(read) if read > 0 => Some(answer),
            _ => None,
        }
    }))
}

/// The wiring at the heart of `confirm_replace`, pulled out so it can be
/// exercised without a real TTY or stdin: given whether the console is
/// interactive and a line reader, decide replace vs. refuse — no I/O of its
/// own. `read_answer` is only ever invoked when `is_tty` is true; the
/// off-TTY refusal never consults it (same shape as jdk-shim's
/// `decide_install`).
fn decide_replace(is_tty: bool, read_answer: impl FnOnce() -> Option<String>) -> bool {
    if !is_tty {
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

/// First-time junction: absent + something installed → point it at the best
/// candidate (stable preferred, then highest version). Anything already at
/// `current` is left alone — retargeting belongs to `jdk use`.
fn ensure_junction(root: &Path, config: &Config) -> Result<(), Fail> {
    match current::inspect(root).map_err(Fail::engine)? {
        current::Current::Junction { target } => {
            eprintln!(
                "jdk: global JDK junction already set → {}",
                target.display()
            );
            Ok(())
        }
        current::Current::NotJunction => {
            eprintln!(
                "jdk: warning: {} exists but is not a junction — `jdk doctor` explains",
                store::current(root).display()
            );
            Ok(())
        }
        current::Current::Absent => {
            match eligible_global(root).map_err(Fail::scan)? {
                Some(candidate) => {
                    current::retarget(root, &candidate.dir).map_err(Fail::engine)?;
                    eprintln!(
                        "jdk: global JDK → {}@{} (change it anytime with `jdk use`)",
                        candidate.vendor, candidate.version
                    );
                }
                None => {
                    eprintln!("jdk: no JDK installed yet, so no global was set");
                    eprintln!(
                        "  → `jdk install {}@21` then `jdk use 21` completes the setup",
                        config.vendor
                    );
                }
            }
            Ok(())
        }
    }
}

/// Highest installed candidate, stable releases first — the same ranking
/// the store applies inside a version line.
fn eligible_global(root: &Path) -> std::io::Result<Option<Candidate>> {
    Ok(store::installed(root)?.into_iter().max_by(|a, b| {
        (a.version.pre_release.is_none(), &a.version, &a.vendor).cmp(&(
            b.version.pre_release.is_none(),
            &b.version,
            &b.vendor,
        ))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdk_core::env::RegType;
    use tempfile::TempDir;
    use test_support::reg::TestKey;

    fn fake_shim(temp: &TempDir) -> PathBuf {
        let source = temp.path().join("jdk-shim.exe");
        fs::write(&source, b"fake shim payload").unwrap();
        source
    }

    /// The broadcast contract, asserted on an injected fake: exactly one
    /// broadcast on the mutating run, zero on the idempotent second run.
    #[test]
    fn broadcasts_once_on_mutation_and_never_when_idempotent() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let test = TestKey::create();
        let source = fake_shim(&temp);
        let config = Config::default();

        let mut broadcasts = 0;
        apply(&root, &config, &test.key, false, Some(&source), &mut || {
            broadcasts += 1;
        })
        .unwrap();
        assert_eq!(broadcasts, 1, "one broadcast after the registry mutation");

        apply(&root, &config, &test.key, false, Some(&source), &mut || {
            broadcasts += 1;
        })
        .unwrap();
        assert_eq!(broadcasts, 1, "idempotent run must not broadcast");
    }

    #[test]
    fn foreign_java_home_without_tty_or_yes_is_an_actionable_error() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let test = TestKey::create();
        let source = fake_shim(&temp);
        test.key
            .set_raw_value(
                "JAVA_HOME",
                &env::string_value(r"C:\Program Files\Java\jdk-17", RegType::REG_SZ),
            )
            .unwrap();

        let mut broadcasts = 0;
        // cargo test runs without a TTY on stdin, so the prompt path refuses.
        let err = apply(
            &root,
            &Config::default(),
            &test.key,
            false,
            Some(&source),
            &mut || broadcasts += 1,
        )
        .unwrap_err();

        assert!(err.to_string().contains("--yes"), "{err}");
        let untouched = env::read(&test.key, "JAVA_HOME").unwrap().unwrap();
        assert_eq!(untouched.text, r"C:\Program Files\Java\jdk-17");
        assert_eq!(
            env::read(&test.key, "Path").unwrap(),
            None,
            "a refusal mutates nothing, PATH included"
        );
        assert!(!root.exists(), "a refusal touches no files either");
        assert_eq!(broadcasts, 0, "a refused setup must not broadcast");
    }

    #[test]
    fn an_unbackupable_foreign_java_home_aborts_before_overwriting() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let test = TestKey::create();
        let source = fake_shim(&temp);
        // A value the flat config subset cannot hold: backing it up fails,
        // and consent (--yes) must NOT be enough to destroy it.
        let hostile = "C:\\evil\"quote";
        test.key
            .set_raw_value("JAVA_HOME", &env::string_value(hostile, RegType::REG_SZ))
            .unwrap();

        let mut broadcasts = 0;
        let err = apply(
            &root,
            &Config::default(),
            &test.key,
            true,
            Some(&source),
            &mut || broadcasts += 1,
        )
        .unwrap_err();

        assert!(err.to_string().contains("NOT replaced"), "{err}");
        let untouched = env::read(&test.key, "JAVA_HOME").unwrap().unwrap();
        assert_eq!(untouched.text, hostile, "the value survives the abort");
        assert_eq!(broadcasts, 0, "an aborted setup must not broadcast");
    }

    #[test]
    fn decide_replace_accepts_y_or_yes_case_insensitively() {
        for answer in ["y\n", "Y\n", "yes\r\n", "YES\n", "  y  \n"] {
            assert!(
                decide_replace(true, || Some(answer.to_string())),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn decide_replace_refuses_no_empty_eof_or_garbage() {
        for answer in ["n\n", "N\n", "no\n", "\n", "banana\n"] {
            assert!(
                !decide_replace(true, || Some(answer.to_string())),
                "{answer:?}"
            );
        }
        assert!(!decide_replace(true, || None), "EOF (0 bytes read) refuses");
    }

    #[test]
    fn decide_replace_off_tty_refuses_without_reading() {
        let decided = decide_replace(false, || panic!("read_answer must not be called off-TTY"));
        assert!(!decided);
    }
}
