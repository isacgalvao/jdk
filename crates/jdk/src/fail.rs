//! Failure rendering: the error, then `→` action lines.
//! Renders to stderr as:
//!
//! ```text
//! jdk: no installed JDK matches zulu@21
//!   installed: temurin@21.0.5+11
//!   → jdk install zulu@21
//! ```

use jdk_core::Error;
use jdk_core::file_ops;
use jdk_core::shims::TOOLS;
use jdk_resolve::exit;
use jdk_resolve::selector::Selector;
use jdk_resolve::store::Candidate;
use std::fmt;

#[derive(Debug)]
pub struct Fail {
    pub code: i32,
    message: String,
    hints: Vec<String>,
}

impl Fail {
    pub fn new(code: i32, message: impl Into<String>) -> Fail {
        Fail {
            code,
            message: message.into(),
            hints: Vec::new(),
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Fail {
        self.hints.push(hint.into());
        self
    }

    /// Engine errors: network failures past the retry schedule map to the
    /// NETWORK exit code; everything else (catalog miss, checksum, security,
    /// extract, I/O) is a plain failure. The messages already carry their
    /// own context.
    pub fn engine(err: Error) -> Fail {
        let code = match &err {
            Error::Http(_) => exit::NETWORK,
            _ => exit::FAILURE,
        };
        let mut fail = Fail::new(code, err.to_string());
        if let Some(hint) = io_hint(&err) {
            fail = fail.hint(hint);
        }
        match err {
            Error::Checksum { .. } => {
                fail.hint("retry; a persistent mismatch means the source is corrupt or tampered")
            }
            Error::Http(_) => fail.hint("check the network/proxy and retry"),
            _ => fail,
        }
    }

    /// Store-scan failures (`store::installed` / `store::best_candidate`): a
    /// plain FAILURE with the I/O cause appended.
    pub fn scan(err: impl fmt::Display) -> Fail {
        Fail::new(exit::FAILURE, format!("cannot scan the store: {err}"))
    }
}

/// The two Windows refusals a hint can act on, for every failure that reaches
/// here as an [`Error::Io`] (BUG-11): the executable swaps and their staging,
/// the download's writes, and the moves into the store — the `.exe` renames
/// real-time antivirus opens handles on, and the big writes a full volume
/// stops. `uninstall` said as much and the paths that meet it most often did
/// not. The extraction's own writes are NOT among them: they surface as
/// [`Error::Extract`], which carries no `io::Error` to read a cause from.
fn io_hint(err: &Error) -> Option<&'static str> {
    let Error::Io { source, .. } = err else {
        return None;
    };
    if file_ops::in_use(source) {
        // Deliberately two causes, in the shape `uninstall` already uses: the
        // codes are ambiguous (5 is both a mapped image and an ACL denial),
        // and naming only the antivirus would send someone with a permissions
        // problem chasing a scanner.
        Some(
            "the file is in use by a running process (antivirus holds a fresh .exe briefly), or you lack permission to replace it",
        )
    } else if file_ops::disk_full(source) {
        Some("free space on the store's volume and retry")
    } else {
        None
    }
}

/// Every shim that could not be written, as ONE failure naming all of them
/// (BUG-07). The swap phase attempts all six, so reporting only the first
/// would cost a run per tool to discover the rest — and they usually share a
/// single cause, which the hints then name once.
pub fn shims_failed(failed: &[(&'static str, Error)]) -> Fail {
    let mut message = format!(
        "{} of {} shims could not be written",
        failed.len(),
        TOOLS.len()
    );
    let mut hints: Vec<&'static str> = Vec::new();
    for (tool, err) in failed {
        message.push_str(&format!("\n  {tool}: {err}"));
        if let Some(hint) = io_hint(err)
            && !hints.contains(&hint)
        {
            hints.push(hint);
        }
    }
    hints
        .into_iter()
        .fold(Fail::new(exit::FAILURE, message), |fail, hint| {
            fail.hint(hint)
        })
}

/// The "no installed JDK matches {selector}" failure shared by `jdk use` and
/// `jdk uninstall`: NOT_INSTALLED, the installed list appended when non-empty,
/// and the `jdk list` hint. `offer_install` adds the `jdk install` hint that
/// `use` shows and `uninstall` does not.
pub fn not_installed(selector: &Selector, installed: &[Candidate], offer_install: bool) -> Fail {
    let mut message = format!("no installed JDK matches {selector}");
    if !installed.is_empty() {
        let names: Vec<String> = installed
            .iter()
            .map(|c| format!("{}@{}", c.vendor, c.version))
            .collect();
        message.push_str(&format!("\n  installed: {}", names.join(", ")));
    }
    let mut fail = Fail::new(exit::NOT_INSTALLED, message);
    if offer_install {
        fail = fail.hint(format!("jdk install {selector}"));
    }
    fail.hint("`jdk list` shows what is installed")
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "jdk: {}", self.message)?;
        for hint in &self.hints {
            writeln!(f, "  → {hint}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::path::Path;

    /// BUG-11: install and update spend their time renaming `.exe` files and
    /// writing hundreds of megabytes, and used to say nothing about the two
    /// things that stop both on Windows — while `uninstall` did. The in-use
    /// hint names BOTH causes its code can mean, because os error 5 does not
    /// tell a mapped image from an ACL denial.
    #[test]
    fn io_failures_name_what_holds_the_file_and_the_full_volume() {
        let path = Path::new("C:\\store\\bin\\jdk.exe");
        let held = Fail::engine(Error::io("place", path)(io::Error::from(
            io::ErrorKind::PermissionDenied,
        )));
        assert!(held.to_string().contains("in use by a running"), "{held}");
        assert!(held.to_string().contains("lack permission"), "{held}");

        let full = Fail::engine(Error::io("write", path)(io::Error::from(
            io::ErrorKind::StorageFull,
        )));
        assert!(full.to_string().contains("free space"), "{full}");

        // Anything else has nothing actionable to add, and a hint that fits
        // every failure teaches the reader to skip them.
        let other = Fail::engine(Error::io("read", path)(io::Error::from(
            io::ErrorKind::NotFound,
        )));
        assert!(!other.to_string().contains('→'), "{other}");
    }

    /// BUG-07: the swap phase attempts every tool, so the report names every
    /// one that failed — and the cause they share once, not once per tool.
    #[test]
    fn every_failed_shim_is_named_and_a_shared_cause_hinted_once() {
        let dir = Path::new("C:\\store\\shims");
        let failed = vec![
            (
                "java",
                Error::io("place", &dir.join("java.exe"))(io::Error::from(
                    io::ErrorKind::PermissionDenied,
                )),
            ),
            (
                "javac",
                Error::io("place", &dir.join("javac.exe"))(io::Error::from(
                    io::ErrorKind::PermissionDenied,
                )),
            ),
        ];

        let fail = shims_failed(&failed);

        let rendered = fail.to_string();
        assert_eq!(fail.code, exit::FAILURE);
        assert!(
            rendered.contains(&format!("2 of {} shims", TOOLS.len())),
            "{rendered}"
        );
        assert!(rendered.contains("java: cannot place"), "{rendered}");
        assert!(rendered.contains("javac: cannot place"), "{rendered}");
        assert_eq!(
            rendered.matches("in use by a running").count(),
            1,
            "one cause, one hint: {rendered}"
        );
    }

    #[test]
    fn renders_message_then_arrow_hints() {
        let fail = Fail::new(exit::NOT_INSTALLED, "temurin@22 is not installed")
            .hint("jdk install temurin@22");
        assert_eq!(
            fail.to_string(),
            "jdk: temurin@22 is not installed\n  → jdk install temurin@22\n"
        );
    }

    #[test]
    fn maps_http_errors_to_the_network_exit_code() {
        assert_eq!(
            Fail::engine(Error::Http("timed out".into())).code,
            exit::NETWORK
        );
        assert_eq!(
            Fail::engine(Error::Catalog("no package".into())).code,
            exit::FAILURE
        );
    }
}
