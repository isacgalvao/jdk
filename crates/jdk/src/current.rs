//! `jdk current`: the resolution from the cwd, explained — which file
//! pinned, what it asked for, what that resolves to and whether it is
//! installed. Without a pin, the global junction. The explanation is data
//! (stdout); the not-installed/not-configured outcome is the error path
//! (stderr + contract exit code).

use crate::fail::Fail;
use crate::resolve::{self, Resolved};
use jdk_resolve::exit;
use std::fs;
use std::path::Path;

pub fn run(root: &Path) -> Result<(), Fail> {
    let config = crate::config(root)?;
    let (cwd, resolved) = resolve::from_cwd(root, &config)?;

    match resolved {
        Resolved::Pinned { pin, candidate } => {
            println!("pinned:    {} by {}", pin.selector, pin.file.display());
            if pin.selector.vendor.is_none() {
                println!("vendor:    {} (config default)", config.vendor);
            }
            match candidate {
                Some(candidate) => {
                    println!("resolved:  {}@{}", candidate.vendor, candidate.version);
                    println!("location:  {}", candidate.dir.display());
                    Ok(())
                }
                None => {
                    println!("resolved:  not installed");
                    Err(Fail::new(
                        exit::NOT_INSTALLED,
                        format!("{} is not installed", pin.selector),
                    )
                    .hint(format!("jdk install {}", pin.selector)))
                }
            }
        }
        Resolved::Global {
            current,
            exists,
            searched,
        } => {
            println!(
                "pinned:    nothing (searched {searched} directories from {} up)",
                cwd.display()
            );
            if exists {
                println!(
                    "global:    {}{}",
                    current.display(),
                    global_target(&current)
                );
                Ok(())
            } else {
                println!("global:    none");
                Err(Fail::new(
                    exit::NOT_INSTALLED,
                    "no java pin found and no global JDK is set",
                )
                .hint("`jdk use <version>` sets a global default; `jdk pin <version>` pins this directory"))
            }
        }
    }
}

/// ` → vendor@version` when the junction target is a store candidate, ` →
/// <path>` when it points elsewhere, empty when unreadable.
fn global_target(current: &Path) -> String {
    let Ok(target) = fs::canonicalize(current) else {
        return String::new();
    };
    match target.file_name().and_then(|name| name.to_str()) {
        Some(name) if name.contains('@') => format!(" → {name}"),
        _ => format!(" → {}", target.display()),
    }
}
