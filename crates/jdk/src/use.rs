//! `jdk use <selector>`: the global switch. One atomic junction retarget —
//! JAVA_HOME keeps its value (decision 8), so every console and IDE already
//! open resolves the new JDK on its next `java` invocation. No auto-install
//! in v0.1: a missing candidate is an actionable error, not a download.

use crate::fail::Fail;
use crate::uninstall;
use jdk_core::current;
use jdk_resolve::{exit, store};
use std::path::Path;

pub fn run(root: &Path, selector: &str) -> Result<(), Fail> {
    uninstall::sweep_orphans(root);
    let selector = crate::parse_selector(selector)?;
    let config = crate::config(root)?;

    let scan_fail = |err| Fail::new(exit::FAILURE, format!("cannot scan the store: {err}"));
    let candidate = store::best_candidate(root, &selector, &config.vendor).map_err(scan_fail)?;
    let Some(candidate) = candidate else {
        let installed = store::installed(root).map_err(scan_fail)?;
        let mut message = format!("no installed JDK matches {selector}");
        if !installed.is_empty() {
            let names: Vec<String> = installed
                .iter()
                .map(|c| format!("{}@{}", c.vendor, c.version))
                .collect();
            message.push_str(&format!("\n  installed: {}", names.join(", ")));
        }
        return Err(Fail::new(exit::NOT_INSTALLED, message)
            .hint(format!("jdk install {selector}"))
            .hint("`jdk list` shows what is installed"));
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
