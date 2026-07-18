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
