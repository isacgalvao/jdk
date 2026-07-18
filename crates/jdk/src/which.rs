//! `jdk which [tool]`: the full path of the executable the shim would run
//! for this directory — pin → store candidate, no pin → the `current`
//! junction — printed for IDE/toolchain configuration. Same jdk-resolve
//! calls as the shim, same not-installed/not-found outcomes.

use crate::fail::Fail;
use crate::resolve::{self, Resolved};
use jdk_resolve::exit;
use std::path::Path;

pub fn run(root: &Path, tool: Option<&str>) -> Result<(), Fail> {
    let tool = tool.unwrap_or("java");
    let tool = tool.strip_suffix(".exe").unwrap_or(tool);
    if tool.is_empty() || tool.contains(['\\', '/', '.']) {
        return Err(
            Fail::new(exit::CONFIG, format!("invalid tool name `{tool}`"))
                .hint("tools are bare names: java, javac, jar, jshell, ..."),
        );
    }

    let config = crate::config(root)?;
    let (cwd, resolved) = resolve::from_cwd(root, &config)?;
    let exe = match resolved {
        Resolved::Pinned {
            candidate: Some(candidate),
            ..
        } => candidate.dir.join("bin").join(format!("{tool}.exe")),
        Resolved::Pinned {
            pin,
            candidate: None,
        } => {
            return Err(Fail::new(
                exit::NOT_INSTALLED,
                format!(
                    "{} (pinned by {}) is not installed",
                    pin.selector,
                    pin.file.display()
                ),
            )
            .hint(format!("jdk install {}", pin.selector)));
        }
        Resolved::Global {
            current,
            exists: true,
            ..
        } => current.join("bin").join(format!("{tool}.exe")),
        Resolved::Global { exists: false, .. } => {
            return Err(Fail::new(
                exit::NOT_INSTALLED,
                format!(
                    "no java pin found from {} and no global JDK is set",
                    cwd.display()
                ),
            )
            .hint("`jdk use <version>` sets a global default; `jdk pin <version>` pins this directory"));
        }
    };

    if !exe.exists() {
        return Err(Fail::new(
            exit::TOOL_NOT_FOUND,
            format!(
                "{tool}.exe not found in the resolved JDK ({})",
                exe.display()
            ),
        ));
    }
    println!("{}", exe.display());
    Ok(())
}
