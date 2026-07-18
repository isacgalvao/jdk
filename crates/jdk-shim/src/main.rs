use jdk_resolve::cascade::{self, Pin};
use jdk_resolve::config::{self, AutoInstall, Config};
use jdk_resolve::store::Candidate;
use jdk_resolve::{exit, store};
use std::env;
use std::io::{self, ErrorKind, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;
use windows_sys::Win32::Foundation::{FALSE, TRUE};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows_sys::core::BOOL;

fn main() {
    // A real handler, not SetConsoleCtrlHandler(None, TRUE): the NULL "ignore"
    // attribute is inherited by child processes, so it would stop the child JVM
    // from ever receiving Ctrl+C and running its shutdown hooks. A real handler
    // is per-process and not inherited — the shim swallows the signal to keep
    // waiting on the child, while the JVM still handles its own.
    unsafe { SetConsoleCtrlHandler(Some(keep_waiting), TRUE) };
    std::process::exit(run());
}

/// Returns TRUE for Ctrl+C / Ctrl+Break so the shim survives them and stays in
/// the child's wait; every other control type falls through to default handling.
unsafe extern "system" fn keep_waiting(ctrl_type: u32) -> BOOL {
    if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
        TRUE
    } else {
        FALSE
    }
}

fn run() -> i32 {
    let tool = match tool_name() {
        Ok(tool) => tool,
        Err(message) => return fail(&message, exit::FAILURE),
    };
    let Some(root) = store::root() else {
        return fail(
            "cannot determine the home directory; set JDK_ROOT",
            exit::FAILURE,
        );
    };
    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            return fail(
                &format!("cannot read the current directory: {err}"),
                exit::FAILURE,
            );
        }
    };
    let resolution = match cascade::resolve(&cwd) {
        Ok(resolution) => resolution,
        Err(err) => return fail(&err.to_string(), err.exit_code()),
    };

    let tool_exe = match &resolution.pin {
        Some(pin) => match pinned_tool_exe(&root, pin, &tool) {
            Ok(tool_exe) => tool_exe,
            Err(code) => return code,
        },
        None => {
            let current = store::current(&root);
            if !current.exists() {
                eprintln!(
                    "jdk-shim: no java pin found from {} and no global JDK is set",
                    cwd.display()
                );
                eprintln!("  → jdk use <version>");
                return exit::NOT_INSTALLED;
            }
            current.join("bin").join(format!("{tool}.exe"))
        }
    };

    if !tool_exe.exists() {
        return fail(
            &format!(
                "{}.exe not found in the resolved JDK ({})",
                tool,
                tool_exe.display()
            ),
            exit::TOOL_NOT_FOUND,
        );
    }

    match Command::new(&tool_exe)
        .args(env::args_os().skip(1))
        .status()
    {
        Ok(status) => status.code().unwrap_or(exit::FAILURE),
        Err(err) => fail(
            &format!("cannot run {}: {err}", tool_exe.display()),
            exit::FAILURE,
        ),
    }
}

/// The pinned candidate's tool path, auto-installing per config when the
/// pin is not installed yet. config.toml is read lazily, only where it is
/// consumed — the vendor default for a bare pin, and the auto-install
/// policy on a miss — so a malformed config never bricks a resolution that
/// would not consult it (and the global path does zero config I/O).
fn pinned_tool_exe(root: &Path, pin: &Pin, tool: &str) -> Result<PathBuf, i32> {
    // best_candidate ignores the default vendor when the selector names one
    // (documented contract), so an explicit-vendor pin skips the load.
    let vendor = match &pin.selector.vendor {
        Some(_) => config::DEFAULT_VENDOR.to_string(),
        None => load_config(root)?.vendor,
    };
    let candidate = match find_candidate(root, pin, &vendor)? {
        Some(candidate) => candidate,
        None => auto_install(root, pin, &vendor)?,
    };
    Ok(candidate.dir.join("bin").join(format!("{tool}.exe")))
}

fn find_candidate(root: &Path, pin: &Pin, vendor: &str) -> Result<Option<Candidate>, i32> {
    store::best_candidate(root, &pin.selector, vendor).map_err(|err| {
        fail(
            &format!(
                "cannot scan {}: {err}",
                store::java_candidates(root).display()
            ),
            exit::FAILURE,
        )
    })
}

fn load_config(root: &Path) -> Result<Config, i32> {
    config::load(root).map_err(|err| {
        eprintln!("jdk-shim: {err}");
        eprintln!("  → fix or delete {}", store::config(root).display());
        exit::CONFIG
    })
}

/// Pinned but not installed (plan decision 5): `never` fails actionably;
/// `prompt` asks inline when stdin AND stderr are a TTY (anything else —
/// CI, IDE pipes — fails actionably instead of hanging); `always` installs
/// without asking. The download is delegated to `jdk.exe install
/// --from-shim` (HTTP stays out of the shim), then the store is re-scanned.
/// This miss path is cold: it loads config for the policy even when the
/// bare-vendor lookup already read it once.
fn auto_install(root: &Path, pin: &Pin, vendor: &str) -> Result<Candidate, i32> {
    let config = load_config(root)?;
    match config.auto_install {
        AutoInstall::Never => Err(not_installed(pin)),
        AutoInstall::Prompt => {
            if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
                return Err(not_installed(pin));
            }
            eprint!(
                "jdk-shim: {} (pinned by {}) is not installed. Install now? [Y/n] ",
                pin.selector,
                pin.file.display()
            );
            let mut answer = String::new();
            match io::stdin().read_line(&mut answer) {
                // Ok(0) is EOF: no terminal line to honor a default with.
                Ok(read) if read > 0 && accepts(&answer) => {}
                _ => return Err(not_installed(pin)),
            }
            install_via_cli(root, pin, vendor)
        }
        AutoInstall::Always => install_via_cli(root, pin, vendor),
    }
}

/// Prompt decision: plain Enter takes the [Y] default; anything but y/yes
/// refuses. Kept as a pure function for unit tests — the interactive path
/// itself is validated manually.
fn accepts(answer: &str) -> bool {
    let answer = answer.trim();
    answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

fn install_via_cli(root: &Path, pin: &Pin, vendor: &str) -> Result<Candidate, i32> {
    let packaged = root.join("bin").join("jdk.exe");
    // Decision 7 layout first; otherwise let CreateProcess search the PATH.
    let jdk_exe = if packaged.exists() {
        packaged.clone()
    } else {
        PathBuf::from("jdk")
    };

    // Inherited stdio: the CLI's progress bar and messages reach the user.
    let status = Command::new(&jdk_exe)
        .arg("install")
        .arg("--from-shim")
        .arg(pin.selector.to_string())
        .status();
    match status {
        Ok(status) if status.success() => {}
        // The CLI already explained the failure on stderr.
        Ok(status) => return Err(status.code().unwrap_or(exit::FAILURE)),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            eprintln!(
                "jdk-shim: cannot auto-install: jdk.exe not found at {} or on PATH",
                packaged.display()
            );
            eprintln!(
                "  → install the jdk CLI, then: jdk install {}",
                pin.selector
            );
            return Err(exit::FAILURE);
        }
        Err(err) => {
            return Err(fail(
                &format!("cannot run {}: {err}", jdk_exe.display()),
                exit::FAILURE,
            ));
        }
    }

    match find_candidate(root, pin, vendor)? {
        Some(candidate) => Ok(candidate),
        None => Err(not_installed(pin)),
    }
}

fn not_installed(pin: &Pin) -> i32 {
    eprintln!(
        "jdk-shim: {} (pinned by {}) is not installed",
        pin.selector,
        pin.file.display()
    );
    eprintln!("  → jdk install {}", pin.selector);
    exit::NOT_INSTALLED
}

/// The tool this shim stands in for is read from its own filename: a copy
/// named `java.exe` runs java. The stem is lowercased because Windows matches
/// filenames case-insensitively, so `JAVA.EXE` names the very same tool.
fn tool_name() -> Result<String, String> {
    let Some(arg0) = env::args_os().next() else {
        return Err("argv[0] is missing".to_string());
    };
    match Path::new(&arg0).file_stem().and_then(|stem| stem.to_str()) {
        Some(name) => Ok(name.to_ascii_lowercase()),
        None => Err("argv[0] is not valid UTF-8".to_string()),
    }
}

fn fail(message: &str, code: i32) -> i32 {
    eprintln!("jdk-shim: {message}");
    code
}

#[cfg(test)]
mod tests {
    use super::{accepts, keep_waiting};
    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    #[test]
    fn keep_waiting_swallows_ctrl_c_and_break_only() {
        assert_eq!(unsafe { keep_waiting(CTRL_C_EVENT) }, TRUE);
        assert_eq!(unsafe { keep_waiting(CTRL_BREAK_EVENT) }, TRUE);
        for other in [CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT] {
            assert_eq!(unsafe { keep_waiting(other) }, FALSE, "ctrl_type {other}");
        }
    }

    #[test]
    fn plain_enter_takes_the_default_yes() {
        assert!(accepts("\n"));
        assert!(accepts("\r\n"));
    }

    #[test]
    fn y_and_yes_accept_case_insensitively() {
        for answer in ["y\n", "Y\n", "yes\r\n", "YES\n", "  y  \n"] {
            assert!(accepts(answer), "{answer:?}");
        }
    }

    #[test]
    fn anything_else_refuses() {
        for answer in ["n\n", "N\n", "no\n", "nope\n", "j\n", "quit\n"] {
            assert!(!accepts(answer), "{answer:?}");
        }
    }
}
