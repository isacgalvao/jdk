//! `jdk list`: installed JDKs, sorted, with the global one marked when the
//! `current` junction points at it (`jdk use` owns the junction; here we
//! only read it).

use crate::fail::Fail;
use crate::uninstall;
use jdk_resolve::{exit, store};
use std::fs;
use std::path::Path;

pub fn run(root: &Path) -> Result<(), Fail> {
    uninstall::sweep_orphans(root);
    let installed = store::installed(root)
        .map_err(|err| Fail::new(exit::FAILURE, format!("cannot scan the store: {err}")))?;
    if installed.is_empty() {
        eprintln!("jdk: no JDKs installed");
        eprintln!("  → jdk install temurin@21");
        return Ok(());
    }

    let global = fs::canonicalize(store::current(root)).ok();
    for candidate in installed {
        let is_global = global
            .as_deref()
            .is_some_and(|target| fs::canonicalize(&candidate.dir).is_ok_and(|dir| dir == target));
        let marker = if is_global { "  (global)" } else { "" };
        println!("{}@{}{marker}", candidate.vendor, candidate.version);
    }
    Ok(())
}
