//! Failure rendering: the error, then `→` action lines.
//! Renders to stderr as:
//!
//! ```text
//! jdk: no installed JDK matches zulu@21
//!   installed: temurin@21.0.5+11
//!   → jdk install zulu@21
//! ```

use jdk_core::Error;
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
        let fail = Fail::new(code, err.to_string());
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
