//! `jdk setup`: the once-per-user step that makes Windows see the store —
//! JAVA_HOME (write-once), PATH prepend of `shims\`, shim materialization,
//! `bin\jdk.exe`, and the `current` junction when a global JDK is eligible.
//! Idempotent: a second run reports a clean no-op.
//!
//! Defensive on foreign state: a JAVA_HOME set by another tool is only
//! replaced with consent (TTY prompt, or `--yes` for scripts), and the old
//! value+type is saved to config.toml first — that backup is what
//! `setup --undo` (`crate::undo`) hands back.

use crate::console;
use crate::fail::{self, Fail};
use jdk_core::config::JavaHomeBefore;
use jdk_core::env::{self, JavaHomeState, RegKey};
use jdk_core::{current, shims};
use jdk_resolve::config::Config;
use jdk_resolve::exit;
use jdk_resolve::store::{self, Candidate};
use std::fs;
use std::io;
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
        && !confirm_replace(old)
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

    let shim = materialize_shims(root, shim_source)?;
    place_cli(root, &shim);

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
/// over the v0.1 tool set, and returns the source it used so the same bytes
/// can be parked in `bin` for the next run.
fn materialize_shims(root: &Path, shim_source: Option<&Path>) -> Result<PathBuf, Fail> {
    let source = match shim_source {
        Some(path) => path.to_path_buf(),
        None => beside_me("jdk-shim.exe").map_err(|err| {
            Fail::new(
                exit::FAILURE,
                format!("cannot locate jdk.exe itself: {err}"),
            )
        })?,
    };
    if !source.exists() {
        return Err(Fail::new(
            exit::FAILURE,
            format!("jdk-shim.exe not found at {}", source.display()),
        )
        .hint("reinstall with install.ps1, which places jdk-shim.exe next to jdk.exe")
        .hint("or point --shim-source at a jdk-shim.exe build"));
    }
    let done = shims::materialize(&source, &store::shims(root)).map_err(Fail::engine)?;
    // A failure is fatal — a partial tool set is what `jdk setup` exists to
    // converge, so it must not go on to claim the environment is set up — and
    // it silences the "up to date" line, which would otherwise be the LIKELY
    // reading rather than a corner: the swap phase leaves a refused tool
    // stale, `jdk setup` is the remedy every hint prescribes, and on the rerun
    // the tools that landed compare equal, so `written` is empty while a shim
    // is still wrong. Announcing both at once contradicts itself.
    if !done.failed.is_empty() {
        return Err(fail::shims_failed(&done.failed));
    }
    if done.written.is_empty() {
        eprintln!(
            "jdk: shims already up to date ({} tools)",
            shims::TOOLS.len()
        );
    } else {
        eprintln!("jdk: materialized shims: {}", done.written.join(", "));
    }
    Ok(source)
}

/// `name` beside the jdk.exe that is running, which is where a release zip
/// puts jdk-shim.exe. `current_exe` reports the path the process was started
/// by, so under winget it is the `Links\jdk.exe` symlink on PATH, while the
/// zip was extracted beside the link's target in the package directory.
pub(crate) fn beside_me(name: &str) -> io::Result<PathBuf> {
    Ok(follow_link(std::env::current_exe()?).with_file_name(name))
}

/// One level of symlink, which is all winget makes; a chain the user builds
/// on top of it is left out on purpose. Not `fs::canonicalize`: that returns a
/// `\\?\` path, which would surface in every message that names the shim
/// source.
fn follow_link(path: PathBuf) -> PathBuf {
    match fs::read_link(&path) {
        // A relative target is relative to the link's directory (and keeps
        // its `..` in messages); joining an absolute one yields it unchanged.
        Ok(target) => path.parent().unwrap_or(Path::new("")).join(target),
        Err(_) => path,
    }
}

/// Fills `<root>\bin\` with the pair the installed layout needs: jdk.exe,
/// where the shim's auto-install looks for it, and the jdk-shim.exe the
/// shims were just materialized from. The shim has to travel with it
/// because `materialize_shims` resolves it as a sibling of the running exe —
/// without a copy in `bin`, `jdk setup` run from the store (the remedy doctor
/// recommends everywhere) would find nothing to materialize from.
fn place_cli(root: &Path, shim: &Path) {
    let bin = root.join("bin");
    if let Ok(me) = std::env::current_exe() {
        place_beside(&me, &bin.join("jdk.exe"));
    }
    place_beside(shim, &bin.join("jdk-shim.exe"));
}

/// One copy into `bin`, staged and swapped in so a reader never sees a partial
/// file. Best-effort: a locked or unreadable copy is a warning, and doctor
/// reports the skew. A source that already IS the destination — setup re-run
/// from the store copy — compares equal and is left alone.
fn place_beside(source: &Path, dest: &Path) {
    let payload = match fs::read(source) {
        Ok(payload) => payload,
        Err(err) => {
            eprintln!(
                "jdk: warning: cannot read {} to copy it: {err}",
                source.display()
            );
            return;
        }
    };
    if fs::read(dest).is_ok_and(|existing| existing == payload) {
        eprintln!("jdk: {} already up to date", dest.display());
        return;
    }
    let staged = dest.with_extension("exe.new");
    let placed = fs::create_dir_all(dest.parent().expect("bin dir has a parent"))
        .and_then(|()| fs::write(&staged, &payload))
        .and_then(|()| jdk_core::file_ops::atomic_rename(&staged, dest));
    match placed {
        Ok(()) => eprintln!("jdk: copied {} to {}", source.display(), dest.display()),
        Err(err) => {
            let _ = fs::remove_file(&staged);
            eprintln!("jdk: warning: cannot place {}: {err}", dest.display());
        }
    }
}

/// The write-once JAVA_HOME: absolute junction path as REG_SZ. Consent for
/// the Foreign case was granted upstream. Returns whether the registry
/// changed.
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
            // A backup an EARLIER setup took is stale now: this run displaced
            // nothing, so "before" is "not set". Leaving the record would let
            // `setup --undo` resurrect a JAVA_HOME the user deleted on purpose
            // between the two setups. Dropped before the write, so a store that
            // cannot record the change does not get the change either.
            if jdk_core::config::java_home_before(root)
                .map_err(Fail::engine)?
                .is_some()
            {
                jdk_core::config::clear_java_home_before(root, config).map_err(|err| {
                    Fail::new(
                        exit::FAILURE,
                        format!("cannot drop the stale JAVA_HOME backup in config.toml: {err}"),
                    )
                    .hint("fix the store (permissions/disk), then re-run jdk setup")
                })?;
                eprintln!(
                    "jdk: dropped the saved JAVA_HOME backup — there was none to replace this time"
                );
            }
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
            // Save BEFORE overwriting, and a failed save ABORTS: replacing a
            // value we could not back up would lose it forever, and it is what
            // `setup --undo` restores from. A second replacement OVERWRITES
            // this record, so it always names the value the LAST setup
            // displaced — `undo::restore_java_home` documents what that means.
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
fn confirm_replace(old: &env::EnvValue) -> bool {
    console::confirm(
        console::interactive(),
        format_args!(
            "jdk: JAVA_HOME is currently {} ({}), set outside jdk. Replace it? [y/N] ",
            old.text,
            old.kind()
        ),
    )
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

    /// winget's layout: the release zip extracted into a package directory,
    /// and only a `jdk.exe` symlink in `Links`. Creating a symlink needs admin
    /// or Developer Mode; without either the test can only say so (visible
    /// with `--nocapture`, since a passing test's output is captured), except
    /// on GitHub Actions (admin), where it must run rather than pass unseen.
    fn winget_layout(temp: &TempDir, target: &Path) -> Option<PathBuf> {
        let links = temp.path().join("Links");
        fs::create_dir_all(&links).unwrap();
        let link = links.join("jdk.exe");
        match std::os::windows::fs::symlink_file(target, &link) {
            Ok(()) => Some(link),
            Err(err) if std::env::var_os("GITHUB_ACTIONS").is_none() => {
                eprintln!("skipped: cannot create a symlink here ({err})");
                None
            }
            Err(err) => panic!("GitHub Actions runs as admin, symlink failed: {err}"),
        }
    }

    fn package(temp: &TempDir) -> PathBuf {
        let pkg = temp.path().join("Packages").join("isacgalvao.jdk");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("jdk.exe"), b"exe").unwrap();
        fs::write(pkg.join("jdk-shim.exe"), b"shim").unwrap();
        pkg
    }

    #[test]
    fn follows_an_absolute_link_to_where_the_zip_was_extracted() {
        let temp = TempDir::new().unwrap();
        let pkg = package(&temp);
        let Some(link) = winget_layout(&temp, &pkg.join("jdk.exe")) else {
            return;
        };
        let shim = follow_link(link).with_file_name("jdk-shim.exe");
        assert_eq!(shim, pkg.join("jdk-shim.exe"));
        assert!(shim.is_file());
    }

    #[test]
    fn follows_a_relative_link_from_the_link_directory() {
        let temp = TempDir::new().unwrap();
        package(&temp);
        let relative = Path::new(r"..\Packages\isacgalvao.jdk\jdk.exe");
        let Some(link) = winget_layout(&temp, relative) else {
            return;
        };
        let shim = follow_link(link).with_file_name("jdk-shim.exe");
        assert!(
            shim.is_file(),
            "{} should be the package's shim",
            shim.display()
        );
        assert_eq!(fs::read(shim).unwrap(), b"shim");
    }

    #[test]
    fn a_plain_file_is_its_own_location() {
        let temp = TempDir::new().unwrap();
        let exe = package(&temp).join("jdk.exe");
        assert_eq!(follow_link(exe.clone()), exe);
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

    /// The sort key `(pre_release.is_none(), version, vendor)`: stable outranks
    /// a higher-versioned EA, and the vendor is the final tiebreak between two
    /// otherwise-equal stables.
    #[test]
    fn eligible_global_prefers_stable_over_higher_ea_then_vendor_breaks_the_tie() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let candidates = store::java_candidates(root);
        for name in ["zulu@22.0.0-ea", "temurin@21.0.5", "zulu@21.0.5"] {
            fs::create_dir_all(candidates.join(name)).unwrap();
        }

        let best = eligible_global(root).unwrap().unwrap();

        assert_eq!(best.vendor, "zulu", "vendor breaks the 21.0.5 stable tie");
        assert_eq!(
            best.version.to_string(),
            "21.0.5",
            "the 21.0.5 stable beats the EA at 22"
        );
    }
}
