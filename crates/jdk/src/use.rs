//! `jdk use <selector>`: the global switch. One atomic junction retarget —
//! JAVA_HOME keeps its value (decision 8), so every console and IDE already
//! open resolves the new JDK on its next `java` invocation. No auto-install
//! in v0.1: a missing candidate is an actionable error, not a download.

use crate::fail::{self, Fail};
use crate::uninstall;
use jdk_core::current;
use jdk_resolve::store;
use std::path::Path;

pub fn run(root: &Path, selector: &str) -> Result<(), Fail> {
    uninstall::sweep_orphans(root);
    let selector = crate::parse_selector(selector)?;
    let config = crate::config(root)?;

    let candidate = store::best_candidate(root, &selector, &config.vendor).map_err(Fail::scan)?;
    let Some(candidate) = candidate else {
        let installed = store::installed(root).map_err(Fail::scan)?;
        return Err(fail::not_installed(&selector, &installed, true));
    };

    current::retarget(root, &candidate.dir).map_err(Fail::engine)?;
    eprintln!(
        "jdk: global JDK → {}@{} (open consoles switch too: the junction moved, JAVA_HOME did not)",
        candidate.vendor, candidate.version
    );
    if !store::shims(root).join("java.exe").exists() {
        eprintln!("  → run `jdk setup` once so JAVA_HOME and PATH point at the store");
    }
    Ok(())
}
