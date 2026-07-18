//! `vendor@version` selector: `temurin@21`, or plain `21` for the default vendor.

use crate::version::{ParseError, Version};
use std::fmt;
use std::str::FromStr;

/// What a user or pin file asks for. `vendor: None` means "the default vendor".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub vendor: Option<String>,
    pub version: Version,
}

impl FromStr for Selector {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let text = s.trim();
        match text.split_once('@') {
            Some((vendor, version)) => {
                if vendor.is_empty() || version.is_empty() {
                    return Err(ParseError(text.to_string()));
                }
                Ok(Selector {
                    vendor: Some(normalize_vendor(vendor)),
                    version: version.parse()?,
                })
            }
            None => Ok(Selector {
                vendor: None,
                version: text.parse()?,
            }),
        }
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.vendor {
            Some(vendor) => write!(f, "{vendor}@{}", self.version),
            None => write!(f, "{}", self.version),
        }
    }
}

/// Lowercases and maps `-` to `_`, so the asdf spelling `graalvm-community`
/// meets the store spelling `graalvm_community`.
pub fn normalize_vendor(vendor: &str) -> String {
    vendor.trim().to_ascii_lowercase().replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str) -> Selector {
        s.parse().unwrap()
    }

    #[test]
    fn parses_vendor_and_version() {
        let s = sel("temurin@21");
        assert_eq!(s.vendor.as_deref(), Some("temurin"));
        assert_eq!(s.version, "21".parse().unwrap());

        let s = sel("corretto@17.0.8");
        assert_eq!(s.vendor.as_deref(), Some("corretto"));
        assert_eq!(s.version, "17.0.8".parse().unwrap());
    }

    #[test]
    fn parses_bare_version() {
        assert_eq!(sel("21").vendor, None);
        assert_eq!(sel("21.0.4+7").version, "21.0.4+7".parse().unwrap());
        assert_eq!(sel("1.8.0_392").version.components, vec![1, 8, 0, 392]);
    }

    #[test]
    fn trims_and_normalizes() {
        assert_eq!(sel("  temurin@21  ").vendor.as_deref(), Some("temurin"));
        assert_eq!(
            sel("GraalVM-Community@21").vendor.as_deref(),
            Some("graalvm_community")
        );
    }

    #[test]
    fn rejects_malformed() {
        for text in ["", "@21", "temurin@", "temurin@banana", "temurin@21@x"] {
            assert!(
                text.parse::<Selector>().is_err(),
                "{text:?} should not parse"
            );
        }
    }

    #[test]
    fn displays_canonically() {
        assert_eq!(sel("temurin@21").to_string(), "temurin@21");
        assert_eq!(sel("21.0.4").to_string(), "21.0.4");
    }
}
