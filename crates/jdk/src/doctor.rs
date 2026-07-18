//! `jdk doctor`: one named check per way the Windows integration can rot,
//! each with a ✓/✗/! verdict and its remediation, extended with the
//! anti-model catalog this project was built against (setx truncation,
//! literal %VAR% in REG_SZ, orphan JAVA_HOME, machine/user scope conflict,
//! duplicated PATH entries).
//!
//! Exit contract: any ✗ exits non-zero; `!` findings are informative
//! (offline is not a disease) and keep exit 0.

use crate::fail::Fail;
use crate::resolve::{self, Resolved};
use jdk_core::catalog::DEFAULT_INDEX_URL;
use jdk_core::current::{self, Current};
use jdk_core::download::sha256_hex;
use jdk_core::env::{self, JavaHomeState, RegKey};
use jdk_core::http::{Http, Retry, UrlPolicy};
use jdk_core::index::{IndexFile, safe_path_segments};
use jdk_core::{admin, shims};
use jdk_resolve::config::Config;
use jdk_resolve::{exit, store};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// User PATHs longer than this are close to the ~2047-char ceiling several
/// consumers still enforce.
const PATH_NEAR_LIMIT: usize = 1800;

pub fn run(root: &Path) -> Result<(), Fail> {
    let (user, _hermetic) = crate::user_env()?;
    let machine = machine_env();
    // A broken config is a finding, not a crash: report it and keep
    // diagnosing with defaults.
    let (config, config_check) = load_config(root);

    let mut checks = vec![
        writable(root),
        config_check,
        shims_present(root),
        junction(root),
        java_home(&user, root),
        machine_java_home(machine.as_ref()),
    ];
    checks.extend(path_checks(&user, root));
    checks.push(pin(root, &config));
    checks.push(index_reachable());
    checks.push(cache_integrity(root));
    checks.push(cli_copy(root));
    checks.push(elevation());

    let width = checks
        .iter()
        .map(|check| check.name.len())
        .max()
        .unwrap_or(0);
    let mut broken = 0;
    for check in &checks {
        match &check.outcome {
            Outcome::Pass(detail) => println!("✓ {:width$}  {detail}", check.name),
            Outcome::Note(detail) => println!("! {:width$}  {detail}", check.name),
            Outcome::Broken { detail, fix } => {
                broken += 1;
                println!("✗ {:width$}  {detail}", check.name);
                println!("  {:width$}  → {fix}", "");
            }
        }
    }

    if broken > 0 {
        Err(Fail::new(
            exit::FAILURE,
            format!("{broken} of {} checks failed", checks.len()),
        ))
    } else {
        Ok(())
    }
}

struct Check {
    name: &'static str,
    outcome: Outcome,
}

enum Outcome {
    Pass(String),
    /// Informative: worth knowing, not a failure.
    Note(String),
    Broken {
        detail: String,
        fix: String,
    },
}

fn pass(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        outcome: Outcome::Pass(detail.into()),
    }
}

fn note(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        outcome: Outcome::Note(detail.into()),
    }
}

fn broken(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        name,
        outcome: Outcome::Broken {
            detail: detail.into(),
            fix: fix.into(),
        },
    }
}

fn load_config(root: &Path) -> (Config, Check) {
    match jdk_resolve::config::load(root) {
        Ok(config) => {
            let detail = format!(
                "vendor {}, auto-install {}",
                config.vendor,
                config.auto_install.as_str()
            );
            (config, pass("config", detail))
        }
        Err(err) => (
            Config::default(),
            broken(
                "config",
                err.to_string(),
                format!("fix or delete {}", store::config(root).display()),
            ),
        ),
    }
}

fn writable(root: &Path) -> Check {
    let probe = root.join(".doctor-probe");
    let attempt = fs::create_dir_all(root).and_then(|()| fs::write(&probe, b"probe"));
    let _ = fs::remove_file(&probe);
    match attempt {
        Ok(()) => pass("store", format!("{} (writable)", root.display())),
        Err(err) => broken(
            "store",
            format!("{} is not writable: {err}", root.display()),
            "fix the directory permissions, or point JDK_ROOT somewhere writable",
        ),
    }
}

fn shims_present(root: &Path) -> Check {
    let dir = store::shims(root);
    let missing: Vec<&str> = shims::TOOLS
        .iter()
        .filter(|tool| !dir.join(format!("{}.exe", tool.name)).exists())
        .map(|tool| tool.name)
        .collect();
    if missing.is_empty() {
        let mut detail = format!("{} tools in {}", shims::TOOLS.len(), dir.display());
        if let Some(major) = global_major(root) {
            let too_new: Vec<&str> = shims::TOOLS
                .iter()
                .filter(|tool| !tool.availability.includes(major))
                .map(|tool| tool.name)
                .collect();
            if !too_new.is_empty() {
                detail.push_str(&format!(
                    " ({} need a newer JDK than the global {major})",
                    too_new.join(", ")
                ));
            }
        }
        pass("shims", detail)
    } else {
        broken(
            "shims",
            format!("missing from {}: {}", dir.display(), missing.join(", ")),
            "jdk setup",
        )
    }
}

/// Major version of the JDK the junction points at, when it parses as a
/// store candidate (`vendor@version` directory name).
fn global_major(root: &Path) -> Option<u32> {
    let Ok(Current::Junction { target }) = current::inspect(root) else {
        return None;
    };
    let name = target.file_name()?.to_str()?;
    let (_, version) = name.split_once('@')?;
    let version: jdk_resolve::version::Version = version.parse().ok()?;
    version.components.first().copied()
}

fn junction(root: &Path) -> Check {
    let path = store::current(root);
    match current::inspect(root) {
        Err(err) => broken("junction", err.to_string(), "jdk use <version>"),
        Ok(Current::Absent) => note(
            "junction",
            "no global JDK set — pins still work; `jdk use <version>` sets one",
        ),
        Ok(Current::NotJunction) => broken(
            "junction",
            format!("{} exists but is not a junction", path.display()),
            "move it out of the way, then `jdk use <version>`",
        ),
        Ok(Current::Junction { target }) => {
            if !target.exists() {
                broken(
                    "junction",
                    format!(
                        "{} → {}, which no longer exists",
                        path.display(),
                        target.display()
                    ),
                    "jdk use <version> (the target was uninstalled or moved)",
                )
            } else if !target.join("bin").join("java.exe").exists() {
                broken(
                    "junction",
                    format!(
                        "{} → {}, which has no bin\\java.exe",
                        path.display(),
                        target.display()
                    ),
                    "jdk use <version> (the target is not a JDK)",
                )
            } else {
                pass("junction", format!("→ {}", target.display()))
            }
        }
    }
}

fn java_home(user: &RegKey, root: &Path) -> Check {
    let junction = store::current(root);
    match env::java_home_state(user, &junction) {
        Err(err) => broken("JAVA_HOME", err.to_string(), "jdk setup --yes"),
        Ok(JavaHomeState::Ours) => pass("JAVA_HOME", format!("= {}", junction.display())),
        Ok(JavaHomeState::Absent) => broken("JAVA_HOME", "not set for this user", "jdk setup"),
        Ok(JavaHomeState::Foreign(value)) => {
            if !value.expandable && has_percent_var(&value.text) {
                // Anti-model 1: setx collapses REG_EXPAND_SZ to REG_SZ and
                // the literal %VAR% never expands again.
                broken(
                    "JAVA_HOME",
                    format!(
                        "literal {} stored as REG_SZ — the variable never expands (setx damage)",
                        value.text
                    ),
                    "jdk setup --yes (replaces it; the old value is saved)",
                )
            } else if !Path::new(&value.text).exists() {
                broken(
                    "JAVA_HOME",
                    format!(
                        "points to {}, which does not exist (orphan of an interrupted switcher)",
                        value.text
                    ),
                    "jdk setup --yes (replaces it; the old value is saved)",
                )
            } else {
                broken(
                    "JAVA_HOME",
                    format!("set to {} by another tool", value.text),
                    "jdk setup --yes (replaces it; the old value is saved)",
                )
            }
        }
    }
}

fn machine_java_home(machine: Option<&RegKey>) -> Check {
    let Some(machine) = machine else {
        return note(
            "machine env",
            "machine environment key unreadable — skipped",
        );
    };
    match env::read(machine, env::JAVA_HOME) {
        Ok(None) => pass("machine env", "no machine-wide JAVA_HOME (good)"),
        Ok(Some(value)) => broken(
            "machine env",
            format!(
                "machine scope sets JAVA_HOME = {} — it conflicts with the per-user junction \
                 wherever user variables are not applied",
                value.text
            ),
            "remove it from an elevated shell: reg delete \
             \"HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment\" \
             /v JAVA_HOME  (jdk never writes machine scope)",
        ),
        Err(err) => note("machine env", format!("unreadable: {err}")),
    }
}

fn path_checks(user: &RegKey, root: &Path) -> Vec<Check> {
    let shims_dir = store::shims(root);
    let value = match env::read(user, env::PATH) {
        Err(err) => return vec![broken("PATH", err.to_string(), "jdk setup")],
        Ok(None) => {
            return vec![broken(
                "PATH",
                "the user PATH is not set — shims are unreachable",
                "jdk setup",
            )];
        }
        Ok(Some(value)) => value,
    };

    let mut checks = Vec::new();
    checks.push(match env::path_count(&value.text, &shims_dir) {
        0 => broken(
            "PATH",
            format!("does not contain {}", shims_dir.display()),
            "jdk setup",
        ),
        1 => pass("PATH", format!("contains {} once", shims_dir.display())),
        n => broken(
            "PATH",
            format!(
                "contains {} {n} times (duplicated entries)",
                shims_dir.display()
            ),
            "edit the user PATH (SystemPropertiesAdvanced.exe) and keep a single entry",
        ),
    });

    let bin_dir = root.join("bin");
    checks.push(match env::path_count(&value.text, &bin_dir) {
        0 => broken(
            "PATH bin",
            format!(
                "does not contain {} — `jdk` is not callable from new shells",
                bin_dir.display()
            ),
            "jdk setup",
        ),
        1 => pass("PATH bin", format!("contains {} once", bin_dir.display())),
        n => broken(
            "PATH bin",
            format!(
                "contains {} {n} times (duplicated entries)",
                bin_dir.display()
            ),
            "edit the user PATH (SystemPropertiesAdvanced.exe) and keep a single entry",
        ),
    });

    checks.push(if !value.expandable && has_percent_var(&value.text) {
        // Anti-model 1 again, on PATH: a literal %VAR% in REG_SZ is dead.
        broken(
            "PATH type",
            "stored as REG_SZ with literal %VAR% entries — they never expand (setx damage)",
            "recreate the user PATH as REG_EXPAND_SZ (regedit), then re-add entries",
        )
    } else {
        pass("PATH type", value.kind().to_string())
    });

    let length = value.text.chars().count();
    checks.push(if (1015..=1024).contains(&length) {
        note(
            "PATH length",
            format!(
                "{length} chars — right at setx's 1024 truncation point; entries may have been cut"
            ),
        )
    } else if length > PATH_NEAR_LIMIT {
        note(
            "PATH length",
            format!("{length} chars — close to the ~2047 limit some consumers enforce"),
        )
    } else {
        pass("PATH length", format!("{length} chars"))
    });
    checks
}

fn pin(root: &Path, config: &Config) -> Check {
    match resolve::from_cwd(root, config) {
        Err(err) => broken("pin", err.to_string(), "fix that pin file"),
        Ok((_, Resolved::Pinned { pin, candidate })) => match candidate {
            Some(candidate) => pass(
                "pin",
                format!(
                    "{} (by {}) → {}@{}",
                    pin.selector,
                    pin.file.display(),
                    candidate.vendor,
                    candidate.version
                ),
            ),
            None => broken(
                "pin",
                format!(
                    "{} (pinned by {}) is not installed",
                    pin.selector,
                    pin.file.display()
                ),
                format!("jdk install {}", pin.selector),
            ),
        },
        Ok((cwd, Resolved::Global { exists, .. })) => {
            if exists {
                pass(
                    "pin",
                    format!(
                        "no pin from {} — the global junction applies",
                        cwd.display()
                    ),
                )
            } else {
                note(
                    "pin",
                    format!(
                        "no pin from {} and no global JDK — a shim would fail here",
                        cwd.display()
                    ),
                )
            }
        }
    }
}

/// Fast reachability probe: one attempt, short timeout. Failure is a note,
/// never a failure — offline is not a disease; install/available say more.
fn index_reachable() -> Check {
    let index_url = crate::remote::env_url("JDK_INDEX");
    let policy = if index_url.is_some() || crate::remote::env_url("JDK_FOOJAY").is_some() {
        UrlPolicy::AllowInsecureLoopback
    } else {
        UrlPolicy::Strict
    };
    let url = format!(
        "{}/index.json",
        index_url.as_deref().unwrap_or(DEFAULT_INDEX_URL)
    );
    let quick = Retry {
        attempts: 1,
        base_delay: Duration::ZERO,
    };
    let reply = Http::with_request_timeout(policy, quick, Duration::from_secs(3))
        .and_then(|http| http.get(&url, "doctor", &[]));
    match reply {
        Ok(reply) if reply.status() == 200 => pass("index", format!("reachable ({url})")),
        Ok(reply) => note("index", format!("{url} answered {}", reply.status())),
        Err(err) => note(
            "index",
            format!("unreachable ({err}) — fine offline; install/available need it"),
        ),
    }
}

/// Verifies every cached catalog file against the sha256 its cached
/// index.json recorded — a corrupt cache would poison installs silently.
fn cache_integrity(root: &Path) -> Check {
    let dir = store::cache(root).join("index");
    let index_path = dir.join("index.json");
    let bytes = match fs::read(&index_path) {
        Err(_) => return pass("cache", "empty — nothing fetched yet"),
        Ok(bytes) => bytes,
    };
    let index = match IndexFile::parse(&bytes) {
        Ok(index) => index,
        Err(err) => {
            return broken(
                "cache",
                format!("cached index.json is corrupt: {err}"),
                format!("delete {} (it re-downloads)", dir.display()),
            );
        }
    };

    let mut bad = Vec::new();
    for entry in &index.files {
        let Some(path) = cached_path(&dir, &entry.path) else {
            continue;
        };
        let Ok(body) = fs::read(&path) else {
            continue; // not cached yet — nothing to verify
        };
        if sha256_hex(&body) != entry.sha256 {
            bad.push(entry.path.clone());
        }
    }
    if bad.is_empty() {
        pass("cache", "cached catalog files match their sha256")
    } else {
        broken(
            "cache",
            format!("sha256 mismatch: {}", bad.join(", ")),
            format!("delete {} (it re-downloads)", dir.display()),
        )
    }
}

fn cached_path(dir: &Path, relpath: &str) -> Option<PathBuf> {
    let mut path = dir.to_path_buf();
    for segment in safe_path_segments(relpath).ok()? {
        path.push(segment);
    }
    Some(path)
}

/// The store's `bin\jdk.exe` (what the shim's auto-install spawns) vs the
/// running binary. Informative: skew means setup should be re-run.
fn cli_copy(root: &Path) -> Check {
    let dest = root.join("bin").join("jdk.exe");
    let Ok(me) = std::env::current_exe() else {
        return note("jdk.exe", "cannot locate the running jdk.exe");
    };
    if !dest.exists() {
        return note(
            "jdk.exe",
            format!("no {} yet — jdk setup places it", dest.display()),
        );
    }
    if let (Ok(a), Ok(b)) = (fs::canonicalize(&me), fs::canonicalize(&dest))
        && a == b
    {
        return pass("jdk.exe", "running the store copy");
    }
    match (fs::read(&me), fs::read(&dest)) {
        (Ok(mine), Ok(stored)) if mine == stored => pass(
            "jdk.exe",
            format!("{} matches the running binary", dest.display()),
        ),
        (Ok(_), Ok(_)) => note(
            "jdk.exe",
            format!(
                "{} differs from the running binary — `jdk setup` refreshes it",
                dest.display()
            ),
        ),
        _ => note("jdk.exe", "cannot compare the store copy"),
    }
}

/// Informative ONLY: jdk writes per-user state and never
/// needs elevation — this exists so "why is it elevated?" has an answer.
fn elevation() -> Check {
    if admin::is_admin() {
        note("elevation", "running elevated — jdk never requires it")
    } else {
        pass("elevation", "not elevated (jdk never needs admin)")
    }
}

fn machine_env() -> Option<RegKey> {
    match std::env::var("JDK_MACHINE_ENV_KEY") {
        Ok(path) if !path.trim().is_empty() => env::hkcu_subkey(path.trim()).ok(),
        _ => env::machine_key().ok(),
    }
}

/// Whether `text` contains a `%NAME%` expansion pattern — a variable-name
/// run enclosed by two percent signs. A lone `%` (`C:\50%discount`) is not
/// one, and `%%` is cmd's escape for a literal percent. No regex: split on
/// `%` and inspect the runs that sit between two of them.
fn has_percent_var(text: &str) -> bool {
    let parts: Vec<&str> = text.split('%').collect();
    parts.len() >= 3
        && parts[1..parts.len() - 1].iter().any(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_()".contains(&byte))
        })
}

#[cfg(test)]
mod tests {
    use super::has_percent_var;

    #[test]
    fn percent_var_detection_wants_a_full_pattern() {
        assert!(has_percent_var(r"%USERPROFILE%\.jdk\current"));
        assert!(has_percent_var(r"C:\x;%SystemRoot%\bin"));
        assert!(has_percent_var(r"%ProgramFiles(x86)%\Java"));

        assert!(!has_percent_var(r"C:\50%discount\bin"));
        assert!(!has_percent_var(r"C:\a%%b"), "%% escapes a literal percent");
        assert!(!has_percent_var(r"C:\plain\path"));
        assert!(!has_percent_var(r"100% legit;also 50% here"));
    }
}
