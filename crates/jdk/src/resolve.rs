//! Shared cwd resolution for `current` and `which`: the exact jdk-resolve
//! calls the shim makes (cascade, then store match or global junction), so
//! the CLI's answer provably matches what a shim invocation would run.

use crate::fail::Fail;
use jdk_resolve::cascade::{self, Pin};
use jdk_resolve::config::Config;
use jdk_resolve::exit;
use jdk_resolve::store::{self, Candidate};
use std::env;
use std::path::{Path, PathBuf};

pub enum Resolved {
    /// A pin file decided; `candidate` is None when it is not installed.
    /// Boxed to keep the variants size-balanced (clippy: large_enum_variant).
    Pinned {
        pin: Pin,
        candidate: Option<Box<Candidate>>,
    },
    /// No directory up the tree pins java: the global junction decides.
    Global {
        current: PathBuf,
        exists: bool,
        searched: usize,
    },
}

pub fn from_cwd(root: &Path, config: &Config) -> Result<(PathBuf, Resolved), Fail> {
    let cwd = env::current_dir().map_err(|err| {
        Fail::new(
            exit::FAILURE,
            format!("cannot read the current directory: {err}"),
        )
    })?;
    let resolution = cascade::resolve(&cwd).map_err(|err| {
        Fail::new(err.exit_code(), err.to_string())
            .hint("fix that pin file (or remove its java entry)")
    })?;
    let resolved = match resolution.pin {
        Some(pin) => {
            let candidate =
                store::best_candidate(root, &pin.selector, &config.vendor).map_err(|err| {
                    Fail::new(
                        exit::FAILURE,
                        format!(
                            "cannot scan {}: {err}",
                            store::java_candidates(root).display()
                        ),
                    )
                })?;
            Resolved::Pinned {
                pin,
                candidate: candidate.map(Box::new),
            }
        }
        None => {
            let current = store::current(root);
            Resolved::Global {
                exists: current.exists(),
                current,
                searched: resolution.searched.len(),
            }
        }
    };
    Ok((cwd, resolved))
}
