//! `jdk setup --undo`: the inverse of `setup`, and only of `setup`.
//!
//! It hands the environment back — JAVA_HOME restored to the value setup
//! displaced (or deleted, when setup found none to displace), the two PATH
//! entries setup prepended taken out, the `current` junction unlinked, and the
//! directories setup fills (`shims\`, `bin\`) removed. The store is not part of
//! that: those JDKs were downloaded on purpose and stay, with `--purge` the
//! explicit, prompted way to ask for them to go too.
//!
//! Three lines it does not cross. The junction goes as a reparse point only,
//! never the JDK behind it. A PATH entry that is not one of setup's is never
//! rewritten, reordered or re-encoded. And nothing is called removed while it
//! is still on disk: the binary running the undo cannot delete itself, so what
//! survives is named, together with an instruction that is actually
//! executable. A leftover jdk created can be collected by a later run — unless
//! that run is impossible because this jdk.exe was the leftover, in which case
//! the report says "delete it by hand" rather than pointing at a command that
//! no longer exists. A leftover jdk merely found in the way is stated and left
//! alone; no amount of re-running makes it jdk's to delete. Either way the
//! exit code stays 0, because the environment is already handed back.

use crate::fail::Fail;
use jdk_core::config::JavaHomeBefore;
use jdk_core::env::{self, EnvValue, JavaHomeState, RegKey};
use jdk_core::{current, file_ops};
use jdk_resolve::{exit, store};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

pub fn run(root: &Path, yes: bool, purge: bool) -> Result<(), Fail> {
    let (key, hermetic) = crate::user_env()?;
    let mut real = || env::broadcast_change();
    let mut none = || {};
    // The same injected effect as `setup`: a hermetic run (JDK_ENV_KEY set, a
    // disposable subkey) must never signal the real desktop.
    let broadcast: &mut dyn FnMut() = if hermetic { &mut none } else { &mut real };
    apply(root, &key, yes, purge, broadcast)
}

fn apply(
    root: &Path,
    key: &RegKey,
    yes: bool,
    purge: bool,
    broadcast: &mut dyn FnMut(),
) -> Result<(), Fail> {
    // Every read happens before anything mutates, so an unreadable config.toml
    // stops the undo while the machine is still whole rather than halfway
    // through it — and consent is settled last of all, on the same principle
    // `setup` follows: a refusal leaves everything exactly as it was.
    let config = crate::config(root)?;
    let backup = jdk_core::config::java_home_before(root).map_err(Fail::engine)?;
    let installed = store::installed(root).map_err(Fail::scan)?;
    // Reading the Path HERE, before the first write, is what makes the
    // sentence above true: a value whose registry type is neither REG_SZ nor
    // REG_EXPAND_SZ fails the command while the machine is still whole,
    // instead of after JAVA_HOME has already been handed back. The value
    // itself is read again by the removal below, which needs the raw bytes.
    env::read(key, env::PATH).map_err(Fail::engine)?;

    if purge && !yes && !confirm_purge(root, installed.len()) {
        return Err(
            Fail::new(exit::FAILURE, "purge refused — nothing was changed")
                .hint("jdk setup --undo undoes the setup and keeps the installed JDKs")
                .hint("jdk setup --undo --purge --yes deletes them without asking"),
        );
    }

    // The registry first: once nothing in the environment points into the
    // store, whatever is still on disk is inert.
    let java_home = restore_java_home(key, &store::current(root), backup)?;
    if matches!(java_home, JavaHome::Restored(_)) {
        // The value is back where it belongs, so the copy in config.toml has
        // done its job. Leaving it would make a later undo restore a JAVA_HOME
        // the user may since have set on purpose.
        jdk_core::config::clear_java_home_before(root, &config).map_err(Fail::engine)?;
    }
    match &java_home {
        JavaHome::Restored(value) => eprintln!(
            "jdk: JAVA_HOME restored to {} ({})",
            value.text,
            value.kind()
        ),
        JavaHome::Deleted => {
            eprintln!("jdk: JAVA_HOME deleted — setup had found none to replace");
        }
        JavaHome::Unset => eprintln!("jdk: JAVA_HOME is not set — nothing to restore"),
        JavaHome::Foreign(value) => eprintln!(
            "jdk: JAVA_HOME is {} ({}), not the junction setup wrote — left untouched",
            value.text,
            value.kind()
        ),
    }

    let entries = env::remove_path_entries(key, &[store::shims(root), root.join("bin")])
        .map_err(Fail::engine)?;
    for dir in &entries {
        eprintln!("jdk: PATH entry removed: {}", dir.display());
    }
    if entries.is_empty() {
        eprintln!("jdk: PATH has no jdk entries to remove");
    }

    let mut left = Vec::new();
    let junction = store::current(root);
    let unlinked = match current::unlink(&junction) {
        Ok(unlinked) => unlinked,
        // Something that is not a junction occupies the path — doctor's
        // "exists but is not a junction". jdk did not put it there, so there is
        // nothing of setup's to unlink and nothing here to force.
        Err(_) => {
            left.push(Leftover::theirs(
                &junction,
                "not the junction setup made, so not jdk's to remove",
            ));
            false
        }
    };
    if unlinked {
        eprintln!(
            "jdk: unlinked {} — the JDK it pointed at is untouched",
            junction.display()
        );
    }
    let swept = remove_files(root, &mut left);
    for dir in &swept.removed {
        eprintln!("jdk: removed {}", dir.display());
    }

    let mut purged = false;
    if purge && root.exists() {
        purged = purge_store(root, &mut left);
        match purged {
            true => eprintln!("jdk: purged {} — installed JDKs included", root.display()),
            false => eprintln!("jdk: purged {} except what is named below", root.display()),
        }
    } else if !purge && root.exists() {
        eprintln!(
            "jdk: kept the store at {} — {} installed JDK(s) still there",
            root.display(),
            installed.len()
        );
        eprintln!("  → `jdk setup --undo --purge` removes it, JDKs included");
    }

    // Only a real registry mutation is worth telling the desktop about.
    let registry_changed =
        matches!(java_home, JavaHome::Restored(_) | JavaHome::Deleted) || !entries.is_empty();
    if registry_changed {
        broadcast();
        eprintln!("jdk: environment change broadcast — new terminals pick this up");
    }

    for leftover in &left {
        let label = match leftover.ours {
            true => "left behind",
            false => "left alone",
        };
        eprintln!(
            "jdk: {label}: {} — {}",
            leftover.path.display(),
            leftover.why
        );
    }
    if swept.self_removed {
        // This process deleted the binary it was invoked from, so telling
        // anyone to re-run would point at a command that no longer exists.
        eprintln!(
            "  → this jdk.exe removed itself, so there is nothing to re-run: delete {} by hand once this process has exited",
            manual_targets(root, purge, &left).join(", ")
        );
    } else if left.iter().any(|leftover| leftover.ours) {
        eprintln!(
            "  → once nothing is running from the store, re-run this command to collect them"
        );
    }
    if left.is_empty() && !registry_changed && !unlinked && !purged && swept.removed.is_empty() {
        eprintln!("jdk: nothing to undo — this environment has no jdk setup left in it");
    }
    Ok(())
}

/// Something still on disk when the undo finished.
struct Leftover {
    path: PathBuf,
    why: String,
    /// jdk created it, so it goes once whatever holds it lets go — a later run
    /// collects it. `false` means jdk only found it in the way: no amount of
    /// re-running makes it jdk's to delete.
    ours: bool,
}

impl Leftover {
    fn ours(path: impl Into<PathBuf>, why: impl Into<String>) -> Leftover {
        Leftover {
            path: path.into(),
            why: why.into(),
            ours: true,
        }
    }

    fn theirs(path: impl Into<PathBuf>, why: impl Into<String>) -> Leftover {
        Leftover {
            path: path.into(),
            why: why.into(),
            ours: false,
        }
    }
}

/// The paths a person has to delete by hand, once the undo removed the very
/// command that would otherwise collect them. `--purge` was consent to lose
/// the whole root, so the root covers everything under it that stayed;
/// otherwise it is the leftovers, minus any already inside another — being
/// told to delete both a file and the directory holding it is noise, not
/// precision.
fn manual_targets(root: &Path, purge: bool, left: &[Leftover]) -> Vec<String> {
    if purge {
        return vec![root.display().to_string()];
    }
    left.iter()
        .filter(|leftover| {
            !left
                .iter()
                .any(|other| other.path != leftover.path && leftover.path.starts_with(&other.path))
        })
        .map(|leftover| leftover.path.display().to_string())
        .collect()
}

/// What became of JAVA_HOME.
enum JavaHome {
    /// Put back to the value `setup` displaced, registry type included.
    Restored(EnvValue),
    /// Removed, because setup found none to displace: "before" was "not set".
    Deleted,
    Unset,
    /// Not the junction setup wrote — left exactly as found.
    Foreign(EnvValue),
}

/// Puts JAVA_HOME back the way `setup` found it, and only while it still holds
/// the junction setup wrote. A JAVA_HOME saying anything else is somebody's
/// deliberate choice: undoing our change never means overwriting theirs.
///
/// The backup it consumes carries the semantics `setup` gives it, warts named.
/// `config.toml` gets a `java-home-before` ONLY when setup replaced a foreign
/// value, and a later replacement overwrites it — so the recorded value is the
/// one the last `jdk setup` displaced, not necessarily the one that predates
/// jdk. No record at all means setup found nothing to displace, which undoes to
/// deleting the value it wrote.
///
/// The check and the write are two registry calls, so a JAVA_HOME another tool
/// sets in the gap between them is overwritten. Nothing here can close that
/// window — the registry offers no compare-and-swap for a value — and the race
/// needs a second environment writer running in the same second as an
/// uninstall, so it is stated rather than guarded against.
fn restore_java_home(
    key: &RegKey,
    junction: &Path,
    backup: Option<JavaHomeBefore>,
) -> Result<JavaHome, Fail> {
    match env::java_home_state(key, junction).map_err(Fail::engine)? {
        JavaHomeState::Absent => Ok(JavaHome::Unset),
        JavaHomeState::Foreign(value) => Ok(JavaHome::Foreign(value)),
        JavaHomeState::Ours => match backup {
            Some(backup) => {
                let value = EnvValue {
                    text: backup.value,
                    expandable: backup.expandable,
                };
                env::write(key, env::JAVA_HOME, &value).map_err(Fail::engine)?;
                Ok(JavaHome::Restored(value))
            }
            None => {
                env::delete(key, env::JAVA_HOME).map_err(Fail::engine)?;
                Ok(JavaHome::Deleted)
            }
        },
    }
}

/// What the file half of the undo managed.
struct Swept {
    removed: Vec<PathBuf>,
    /// The store's `bin\jdk.exe` was the binary running this undo and had to be
    /// renamed aside — so there is no `jdk` left to re-run.
    self_removed: bool,
}

/// Removes the files and directories `setup` creates. `shims\` holds nothing
/// but shim copies, so it goes WHOLE: there is no per-tool aside here, and a
/// shim a running `java` still holds keeps the whole directory, which is what
/// the report then names. `bin\` is where the binary running this undo lives,
/// so its two files go one at a time — through the rename-aside a mapped image
/// needs — and the directory only once nothing is left in it. What survives is
/// recorded in `left` instead of raising: the environment is already handed
/// back by this point, so a held file is a leftover to name, not a reason to
/// unwind.
fn remove_files(root: &Path, left: &mut Vec<Leftover>) -> Swept {
    let mut removed = Vec::new();
    let shims = store::shims(root);
    match fs::remove_dir_all(&shims) {
        Ok(()) => removed.push(shims),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => left.push(Leftover::ours(shims, err.to_string())),
    }

    let bin = root.join("bin");
    // Answered BEFORE the rename below, which is what takes the file the
    // question is about out of existence.
    let from_store = running_from_store(&bin);
    let mut swept = Swept {
        removed,
        self_removed: false,
    };
    let mut ours_left = false;
    for name in ["jdk.exe", "jdk-shim.exe"] {
        // The aside an earlier undo could not delete while it was still
        // running: THIS run is the sweep that finishes it. Unconditional,
        // unlike `update::sweep_leftovers` — that one spares an aside whose
        // executable is missing, because it is then the machine's only copy of
        // the CLI, and here being the last copy is exactly the point.
        let _ = fs::remove_file(bin.join(format!("{name}.old")));
        match file_ops::remove_running(&bin.join(name)) {
            Ok(None) => {}
            Ok(Some(aside)) => {
                ours_left = true;
                swept.self_removed |= from_store && name == "jdk.exe";
                left.push(Leftover::ours(
                    aside,
                    "Windows cannot delete a running program",
                ));
            }
            Err(err) => {
                ours_left = true;
                left.push(Leftover::ours(bin.join(name), err.to_string()));
            }
        }
    }
    match fs::remove_dir(&bin) {
        Ok(()) => swept.removed.push(bin),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) if ours_left => left.push(Leftover::ours(bin, "it still holds the copy above")),
        Err(_) => left.push(Leftover::theirs(
            bin,
            "it holds something jdk did not create",
        )),
    }
    swept
}

/// Whether this process is running the store's own `bin\jdk.exe` — the normal
/// case, since `setup` puts that copy on the PATH. It decides what the report
/// may honestly recommend: a jdk that just deleted itself has nothing to
/// re-run. Same identity test as doctor's `jdk.exe` check.
fn running_from_store(bin: &Path) -> bool {
    let stored = fs::canonicalize(bin.join("jdk.exe"));
    match (std::env::current_exe().and_then(fs::canonicalize), stored) {
        (Ok(me), Ok(stored)) => me == stored,
        _ => false,
    }
}

/// Deletes the store root and everything in it, child by child with `bin\`
/// LAST, and reports whether the root itself went. Not
/// `fs::remove_dir_all(root)`: that walks the children in directory order,
/// where `bin\` comes first — and `bin\` is exactly where the aside of the
/// binary running this purge sits. One refusal there aborts the entire walk,
/// so the JDKs, the cache and the config survive a command that reported a
/// purge. Child by child, an undeletable `bin\` costs `bin\` and nothing else.
fn purge_store(root: &Path, left: &mut Vec<Leftover>) -> bool {
    let bin = root.join("bin");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return true,
        Err(err) => {
            left.push(Leftover::ours(root, err.to_string()));
            return false;
        }
    };

    let mut deferred = None;
    for entry in entries.flatten() {
        match entry.path() == bin {
            true => deferred = Some(entry.path()),
            false => remove_child(&entry.path(), left),
        }
    }
    if let Some(bin) = deferred {
        remove_child(&bin, left);
    }

    // The root goes only when every child did; anything still recorded lives
    // under it and would make the removal fail anyway.
    if !left.is_empty() {
        return false;
    }
    match fs::remove_dir(root) {
        Ok(()) => true,
        Err(err) => {
            left.push(Leftover::ours(root, err.to_string()));
            false
        }
    }
}

/// One child of the store root, file or directory. `current` has been unlinked
/// properly by this point, but `current.new` — the staging leftover a crashed
/// junction swap leaves behind — has not, so a reparse point still turns up
/// here and has to be removed as the link it is, never as a door into its
/// target.
fn remove_child(path: &Path, left: &mut Vec<Leftover>) {
    // A child that already failed, or that holds something that did, has been
    // named once; naming it again would only repeat the same fact.
    if left.iter().any(|leftover| leftover.path.starts_with(path)) {
        return;
    }
    // A reparse point classifies as a SYMLINK and NOT as a directory, so asking
    // `is_dir()` alone sent a junction down the `remove_file` path, which
    // refuses — and one refusal here keeps the whole root alive. `remove_dir_all`
    // takes the link itself without following it.
    let as_directory =
        fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir() || meta.file_type().is_symlink());
    let outcome = match as_directory {
        true => fs::remove_dir_all(path),
        false => fs::remove_file(path),
    };
    if let Err(err) = outcome
        && err.kind() != io::ErrorKind::NotFound
    {
        left.push(Leftover::ours(path, err.to_string()));
    }
}

/// Consent for `--purge`, the one destructive half of the undo: asks only when
/// stdin AND stderr are a TTY (CI and pipes get the refusal instead of a hang),
/// and plain Enter keeps the JDKs.
fn confirm_purge(root: &Path, installed: usize) -> bool {
    let is_tty = io::stdin().is_terminal() && io::stderr().is_terminal();
    if is_tty {
        eprint!(
            "jdk: --purge deletes {} and the {installed} JDK(s) in it. Continue? [y/N] ",
            root.display()
        );
    }
    decide_purge(is_tty, || {
        let mut answer = String::new();
        match io::stdin().read_line(&mut answer) {
            Ok(read) if read > 0 => Some(answer),
            _ => None,
        }
    })
}

/// The decision inside [`confirm_purge`], pulled out so it can be exercised
/// without a real TTY or stdin — the same seam shape as setup's
/// `decide_replace` and jdk-shim's `decide_install`. The `&&` short-circuit IS
/// the off-TTY refusal, so `read_answer` is never consulted there; only `y` and
/// `yes` agree, and Enter, EOF and garbage all keep the store.
fn decide_purge(is_tty: bool, read_answer: impl FnOnce() -> Option<String>) -> bool {
    is_tty
        && read_answer().is_some_and(|answer| {
            let answer = answer.trim();
            answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdk_core::current::Current;
    use jdk_core::env::RegType;
    use jdk_resolve::config::Config;
    use tempfile::TempDir;
    use test_support::reg::TestKey;

    /// The pre-existing user PATH every sandbox is built on top of — the
    /// entries an undo must give back exactly as it found them.
    const FOREIGN_PATH: &str = r"%USERPROFILE%\bin;C:\tools";

    /// A store that has been through `jdk setup`: shims and `bin` filled, the
    /// junction pointing at an installed JDK, JAVA_HOME written and the two
    /// entries prepended to a PATH that already had a life of its own. Built by
    /// hand rather than by running setup — that the two are inverses is what
    /// the pillar suite proves, with the real binaries.
    struct Sandbox {
        _temp: TempDir,
        root: PathBuf,
        key: TestKey,
        jdk: PathBuf,
    }

    fn sandbox() -> Sandbox {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let key = TestKey::create();

        let jdk = store::java_candidates(&root).join("temurin@21.0.5");
        fs::create_dir_all(jdk.join("bin")).unwrap();
        fs::write(jdk.join("bin").join("java.exe"), b"the JDK itself").unwrap();
        fs::create_dir_all(store::shims(&root)).unwrap();
        fs::write(store::shims(&root).join("java.exe"), b"shim").unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        for name in ["jdk.exe", "jdk-shim.exe"] {
            fs::write(root.join("bin").join(name), b"cli").unwrap();
        }
        current::retarget(&root, &jdk).unwrap();

        key.key
            .set_raw_value(
                env::PATH,
                &env::string_value(FOREIGN_PATH, RegType::REG_EXPAND_SZ),
            )
            .unwrap();
        env::set_java_home(&key.key, &store::current(&root)).unwrap();
        env::prepend_path(&key.key, &root.join("bin")).unwrap();
        env::prepend_path(&key.key, &store::shims(&root)).unwrap();

        Sandbox {
            _temp: temp,
            root,
            key,
            jdk,
        }
    }

    impl Sandbox {
        /// Runs the undo with the broadcast injected, and reports how many
        /// times it fired.
        fn undo(&self, yes: bool, purge: bool) -> (Result<(), Fail>, usize) {
            let mut broadcasts = 0;
            let outcome = apply(&self.root, &self.key.key, yes, purge, &mut || {
                broadcasts += 1;
            });
            (outcome, broadcasts)
        }

        fn java_home(&self) -> Option<EnvValue> {
            env::read(&self.key.key, env::JAVA_HOME).unwrap()
        }

        fn path(&self) -> Option<EnvValue> {
            env::read(&self.key.key, env::PATH).unwrap()
        }

        fn save_backup(&self, value: &str, expandable: bool) -> JavaHomeBefore {
            let backup = JavaHomeBefore {
                value: value.to_string(),
                expandable,
            };
            jdk_core::config::save_java_home_before(&self.root, &Config::default(), &backup)
                .unwrap();
            backup
        }
    }

    /// Restoring the text without the type is half a restore: a `%JAVA17%\home`
    /// put back as REG_SZ is a literal that never expands again.
    #[test]
    fn java_home_comes_back_with_the_registry_type_setup_saved() {
        for (before, expandable) in [
            (r"C:\Program Files\Java\jdk-17", false),
            (r"%JAVA17%\home", true),
        ] {
            let sandbox = sandbox();
            sandbox.save_backup(before, expandable);

            let (outcome, broadcasts) = sandbox.undo(false, false);

            outcome.unwrap();
            let restored = sandbox.java_home().expect("JAVA_HOME is back");
            assert_eq!(restored.text, before);
            assert_eq!(restored.expandable, expandable, "{before}");
            assert_eq!(broadcasts, 1);
            assert_eq!(
                jdk_core::config::java_home_before(&sandbox.root).unwrap(),
                None,
                "the backup is consumed, so a later undo cannot re-impose it"
            );
        }
    }

    #[test]
    fn java_home_is_deleted_when_setup_found_none_to_replace() {
        let sandbox = sandbox();

        let (outcome, broadcasts) = sandbox.undo(false, false);

        outcome.unwrap();
        assert_eq!(
            sandbox.java_home(),
            None,
            "no record means setup found nothing there"
        );
        assert_eq!(broadcasts, 1);
    }

    #[test]
    fn a_java_home_that_is_not_our_junction_is_left_alone() {
        let sandbox = sandbox();
        let backup = sandbox.save_backup(r"C:\Program Files\Java\jdk-17", false);
        // Somebody pointed JAVA_HOME elsewhere after setup ran. Undoing our
        // change is not licence to overwrite theirs.
        sandbox
            .key
            .key
            .set_raw_value(
                env::JAVA_HOME,
                &env::string_value(r"D:\their\jdk", RegType::REG_SZ),
            )
            .unwrap();

        sandbox.undo(false, false).0.unwrap();

        assert_eq!(sandbox.java_home().unwrap().text, r"D:\their\jdk");
        assert_eq!(
            jdk_core::config::java_home_before(&sandbox.root).unwrap(),
            Some(backup),
            "and the backup stays, so a later undo can still spend it"
        );
    }

    #[test]
    fn only_the_path_entries_setup_added_are_removed() {
        let sandbox = sandbox();

        sandbox.undo(false, false).0.unwrap();

        let path = sandbox.path().unwrap();
        assert_eq!(path.text, FOREIGN_PATH, "the user's own PATH, untouched");
        assert!(path.expandable, "REG_EXPAND_SZ must stay REG_EXPAND_SZ");
    }

    #[test]
    fn what_setup_made_goes_and_the_store_stays() {
        let sandbox = sandbox();

        sandbox.undo(false, false).0.unwrap();

        assert_eq!(current::inspect(&sandbox.root).unwrap(), Current::Absent);
        assert!(!store::shims(&sandbox.root).exists());
        assert!(!sandbox.root.join("bin").exists());
        assert_eq!(
            fs::read(sandbox.jdk.join("bin").join("java.exe")).unwrap(),
            b"the JDK itself",
            "unlinking the junction is not uninstalling what it pointed at"
        );
    }

    /// A real directory at `current` is not the junction setup made, so there
    /// is nothing there to unlink — and nothing there to delete either.
    #[test]
    fn a_real_directory_at_current_survives_the_undo() {
        let sandbox = sandbox();
        current::unlink(&store::current(&sandbox.root)).unwrap();
        let precious = store::current(&sandbox.root).join("precious");
        fs::create_dir_all(&precious).unwrap();

        sandbox.undo(false, false).0.unwrap();

        assert!(precious.exists(), "jdk deletes only what jdk created");
    }

    #[test]
    fn purge_without_a_tty_refuses_and_changes_nothing() {
        let sandbox = sandbox();
        let before = sandbox.path();

        // cargo test runs off a TTY, so the prompt path refuses — the same
        // shape as a pipe or a CI runner.
        let (outcome, broadcasts) = sandbox.undo(false, true);

        let err = outcome.unwrap_err().to_string();
        assert!(err.contains("nothing was changed"), "{err}");
        assert!(err.contains("--purge --yes"), "{err}");
        assert!(sandbox.java_home().is_some(), "JAVA_HOME untouched");
        assert_eq!(sandbox.path(), before);
        assert!(sandbox.jdk.exists());
        assert!(store::shims(&sandbox.root).exists());
        assert_eq!(
            current::inspect(&sandbox.root).unwrap(),
            Current::Junction {
                target: sandbox.jdk.clone()
            }
        );
        assert_eq!(broadcasts, 0);
    }

    /// H1: `fs::remove_dir_all(root)` walks the children in directory order,
    /// and `bin\` — which holds the aside of the running binary — comes first.
    /// The single refusal there aborted the whole walk, so the JDKs, the cache
    /// and the config survived a command that reported a purge.
    #[test]
    fn purge_takes_the_jdks_even_when_bin_cannot_go() {
        let sandbox = sandbox();
        let cache = store::cache(&sandbox.root);
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("index.json"), b"{}").unwrap();
        sandbox.save_backup(r"C:\Program Files\Java\jdk-17", false);
        // A real, RUNNING executable in bin: what makes bin undeletable.
        let (_, fake_java) = test_support::shim_binaries();
        let running = sandbox.root.join("bin").join("jdk.exe");
        fs::copy(&fake_java, &running).unwrap();
        let mut child = std::process::Command::new(&running)
            .env("FAKE_JAVA_SLEEP_MS", "30000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the copy so its image is mapped");

        sandbox.undo(true, true).0.unwrap();

        assert!(!sandbox.jdk.exists(), "the JDKs go even though bin cannot");
        assert!(!cache.exists(), "and the cache");
        assert!(!store::config(&sandbox.root).exists(), "and the config");
        assert!(
            sandbox.root.join("bin").join("jdk.exe.old").exists(),
            "only the aside of the running copy stays"
        );

        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// A junction is a reparse point, which `symlink_metadata` reports as a
    /// SYMLINK and not as a directory — so a purge that asked `is_dir()` alone
    /// tried to `remove_file` it, was refused, and kept the entire root alive
    /// behind that one refusal. `current.new` is the shape that reaches it:
    /// `current::retarget` calls it "leftovers of a crashed swap", and nothing
    /// unlinks it before the purge does.
    #[test]
    fn purge_removes_a_leftover_junction_without_touching_its_target() {
        let sandbox = sandbox();
        let outside = sandbox._temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keepme.txt"), b"not the store's to delete").unwrap();
        // A junction to `outside` parked on the staging name, with `current`
        // still live: `retarget` is the only junction builder this crate can
        // reach, and it clears `current.new` on its way through — so the plant
        // is renamed out of the way first and back into place last.
        let staging = sandbox.root.join("current.new");
        let parked = sandbox.root.join("parked");
        current::retarget(&sandbox.root, &outside).unwrap();
        fs::rename(store::current(&sandbox.root), &parked).unwrap();
        current::retarget(&sandbox.root, &sandbox.jdk).unwrap();
        fs::rename(&parked, &staging).unwrap();

        sandbox.undo(true, true).0.unwrap();

        assert!(
            !sandbox.root.exists(),
            "one leftover junction must not hold the whole root"
        );
        assert_eq!(
            fs::read(outside.join("keepme.txt")).unwrap(),
            b"not the store's to delete",
            "the junction went as a link, never as a door into its target"
        );
    }

    /// A machine that never ran `jdk setup` has nothing to keep and nothing to
    /// offer purging — and the undo must not conjure a store on its way out.
    #[test]
    fn an_untouched_machine_creates_nothing() {
        let temp = TempDir::new().unwrap();
        let key = TestKey::create();
        let root = temp.path().join("never-set-up");
        let mut broadcasts = 0;

        apply(&root, &key.key, false, false, &mut || broadcasts += 1).unwrap();

        assert_eq!(broadcasts, 0);
        assert!(!root.exists());
    }

    #[test]
    fn purge_with_yes_takes_the_root_and_the_jdks_with_it() {
        let sandbox = sandbox();

        sandbox.undo(true, true).0.unwrap();

        assert!(!sandbox.root.exists(), "store root and all");
        assert_eq!(sandbox.java_home(), None);
        assert_eq!(sandbox.path().unwrap().text, FOREIGN_PATH);
    }

    #[test]
    fn a_second_undo_is_a_no_op() {
        let sandbox = sandbox();
        assert_eq!(sandbox.undo(false, false).1, 1, "the first run mutates");

        let (outcome, broadcasts) = sandbox.undo(false, false);

        outcome.unwrap();
        assert_eq!(broadcasts, 0, "nothing changed, so nothing to announce");
        assert_eq!(sandbox.java_home(), None);
        assert_eq!(sandbox.path().unwrap().text, FOREIGN_PATH);
    }

    #[test]
    fn decide_purge_accepts_y_or_yes_case_insensitively() {
        for answer in ["y\n", "Y\n", "yes\r\n", "YES\n", "  y  \n"] {
            assert!(
                decide_purge(true, || Some(answer.to_string())),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn decide_purge_refuses_no_empty_eof_or_garbage() {
        for answer in ["n\n", "N\n", "no\n", "\n", "banana\n"] {
            assert!(
                !decide_purge(true, || Some(answer.to_string())),
                "{answer:?}"
            );
        }
        assert!(!decide_purge(true, || None), "EOF (0 bytes read) refuses");
    }

    #[test]
    fn decide_purge_off_tty_refuses_without_reading() {
        assert!(!decide_purge(false, || panic!(
            "read_answer must not be called off-TTY"
        )));
    }
}
