//! `jdk` CLI: install/uninstall/list/available/pin/current/which (M3) plus
//! the Windows pillar (M4) — `setup` (persistent JAVA_HOME/PATH/shims),
//! `use` (atomic junction retarget) and `doctor` (named health checks).
//!
//! Output contract: stdout carries data, stderr carries messages, prompts and
//! progress. Errors state what failed, what to do about it, and context —
//! and exit with the shared contract in [`jdk_resolve::exit`].

mod available;
mod current;
mod doctor;
mod fail;
mod install;
mod list;
mod pin;
mod remote;
mod resolve;
mod setup;
mod uninstall;
mod r#use;
mod which;

use clap::{Parser, Subcommand};
use fail::Fail;
use jdk_resolve::config::Config;
use jdk_resolve::selector::Selector;
use jdk_resolve::version::ParseError;
use jdk_resolve::{exit, store};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "jdk", version, about = "Windows-first Java version manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install a JDK (a bare version like `21` uses the config vendor)
    Install {
        /// `vendor@version` or bare version: temurin@21, 21.0.5, zulu@17
        selector: String,
        /// Shim auto-install mode: lean output, no next-step hints
        #[arg(long, hide = true)]
        from_shim: bool,
    },
    /// Remove an installed JDK
    Uninstall {
        /// Selector of an installed JDK (see `jdk list`)
        selector: String,
    },
    /// List installed JDKs
    List,
    /// List JDKs available to install
    Available {
        /// Vendor (`temurin`), version (`21`) or both (`temurin@21`)
        filter: Option<String>,
        /// Only the best version of each major line
        #[arg(long)]
        latest: bool,
        /// Include early-access (preview) builds
        #[arg(long)]
        ea: bool,
    },
    /// Pin a Java version for this directory (writes .jdkrc)
    Pin {
        /// `vendor@version` or bare version
        selector: String,
    },
    /// Show which Java the current directory resolves to, and why
    Current,
    /// Print the resolved path of a JDK tool (for IDE setup)
    Which {
        /// Tool name (default: java)
        tool: Option<String>,
    },
    /// Set the global JDK by retargeting the `current` junction
    Use {
        /// Selector of an installed JDK (see `jdk list`)
        selector: String,
    },
    /// Prepare Windows once: JAVA_HOME, PATH, shims (idempotent)
    Setup {
        /// Replace a JAVA_HOME set by another tool without asking
        #[arg(long)]
        yes: bool,
        /// Copy shims from this jdk-shim.exe instead of the one next to jdk.exe
        #[arg(long, value_name = "PATH", hide = true)]
        shim_source: Option<PathBuf>,
    },
    /// Check the store, junction, registry and PATH; explain every problem
    Doctor,
}

fn main() {
    match run(Cli::parse()) {
        Ok(()) => {}
        Err(fail) => {
            eprint!("{fail}");
            std::process::exit(fail.code);
        }
    }
}

fn run(cli: Cli) -> Result<(), Fail> {
    let root = jdk_root()?;
    match cli.command {
        Command::Install {
            selector,
            from_shim,
        } => install::run(&root, &selector, from_shim),
        Command::Uninstall { selector } => uninstall::run(&root, &selector),
        Command::List => list::run(&root),
        Command::Available {
            filter,
            latest,
            ea,
        } => available::run(&root, filter.as_deref(), latest, ea),
        Command::Pin { selector } => pin::run(&root, &selector),
        Command::Current => current::run(&root),
        Command::Which { tool } => which::run(&root, tool.as_deref()),
        Command::Use { selector } => r#use::run(&root, &selector),
        Command::Setup { yes, shim_source } => setup::run(&root, yes, shim_source.as_deref()),
        Command::Doctor => doctor::run(&root),
    }
}

fn jdk_root() -> Result<PathBuf, Fail> {
    store::root().ok_or_else(|| {
        Fail::new(
            exit::FAILURE,
            "cannot determine the home directory for the JDK store",
        )
        .hint("set JDK_ROOT to choose a store location")
    })
}

/// `<root>\config.toml`, defaults when absent; malformed is a config error.
fn config(root: &Path) -> Result<Config, Fail> {
    jdk_resolve::config::load(root).map_err(|err| {
        Fail::new(exit::CONFIG, err.to_string())
            .hint(format!("fix or delete {}", store::config(root).display()))
    })
}

fn parse_selector(text: &str) -> Result<Selector, Fail> {
    text.parse().map_err(|err: ParseError| {
        Fail::new(exit::CONFIG, err.to_string())
            .hint("selectors are vendor@version or a bare version: temurin@21, 21.0.5, zulu@17")
    })
}

/// The user Environment registry key `setup`/`doctor` operate on:
/// `HKCU\Environment`, or the disposable HKCU subkey `JDK_ENV_KEY` names —
/// the hermetic-test injection point, same pattern as `JDK_INDEX`. Hermetic
/// runs (`true` in the pair) also suppress the WM_SETTINGCHANGE broadcast:
/// the values under test are not the real environment.
fn user_env() -> Result<(jdk_core::env::RegKey, bool), Fail> {
    let fail = |err: jdk_core::Error| Fail::new(exit::FAILURE, err.to_string());
    match std::env::var("JDK_ENV_KEY") {
        Ok(path) if !path.trim().is_empty() => {
            Ok((jdk_core::env::hkcu_subkey(path.trim()).map_err(fail)?, true))
        }
        _ => Ok((jdk_core::env::user_key().map_err(fail)?, false)),
    }
}
