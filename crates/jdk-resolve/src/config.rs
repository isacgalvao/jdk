//! Reader for `<JDK_ROOT>\config.toml`, shared by the CLI and the shim.
//!
//! Deliberately NOT a TOML library (this crate is the shim's std-only
//! firewall). The file is a flat SUBSET of TOML that the CLI — its only
//! writer (`jdk-core::config`) — guarantees: `key = "string"` or
//! `key = true|false` lines, `#` comments, blank lines. No tables, arrays,
//! escapes or multi-line values. The reader tolerates a UTF-8 BOM and CRLF,
//! ignores unknown keys (a newer jdk may know more), and rejects anything
//! outside the subset with an error naming the line.
//!
//! v0.1 keys: `vendor` (default vendor for versions without one) and
//! `auto-install` (shim behavior for a pinned-but-missing JDK).

use crate::selector::normalize_vendor;
use crate::text::meaningful_lines;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

/// Vendor used when no config file sets one.
pub const DEFAULT_VENDOR: &str = "temurin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Vendor a bare version (`21`) resolves to. Normalized lowercase.
    pub vendor: String,
    /// What the shim does when the pinned version is not installed.
    pub auto_install: AutoInstall,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            vendor: DEFAULT_VENDOR.to_string(),
            auto_install: AutoInstall::Prompt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoInstall {
    /// Install without asking, TTY or not.
    Always,
    /// Ask inline when stdin and stderr are both a TTY; otherwise fail
    /// actionably (CI never hangs).
    #[default]
    Prompt,
    /// Never install from the shim; fail actionably.
    Never,
}

impl AutoInstall {
    pub fn as_str(self) -> &'static str {
        match self {
            AutoInstall::Always => "always",
            AutoInstall::Prompt => "prompt",
            AutoInstall::Never => "never",
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    /// The file exists but could not be read.
    Read(io::Error),
    /// A line falls outside the written subset or a known key has an invalid
    /// value. Carries the offending line and the reason.
    Parse { line: String, reason: String },
}

impl ConfigError {
    fn parse(line: &str, reason: impl Into<String>) -> ConfigError {
        ConfigError::Parse {
            line: line.to_string(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read(err) => write!(f, "cannot read config.toml: {err}"),
            ConfigError::Parse { line, reason } => {
                write!(f, "invalid config.toml line `{line}`: {reason}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Config at `<root>\config.toml`; a missing file means all defaults.
pub fn load(root: &Path) -> Result<Config, ConfigError> {
    match fs::read_to_string(crate::store::config(root)) {
        Ok(text) => parse(&text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(err) => Err(ConfigError::Read(err)),
    }
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let mut config = Config::default();
    for line in meaningful_lines(text) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(ConfigError::parse(line, "expected `key = \"value\"`"));
        };
        match key.trim() {
            "vendor" => config.vendor = normalize_vendor(quoted(line, value)?),
            "auto-install" => {
                config.auto_install = match quoted(line, value)? {
                    "always" => AutoInstall::Always,
                    "prompt" => AutoInstall::Prompt,
                    "never" => AutoInstall::Never,
                    other => {
                        return Err(ConfigError::parse(
                            line,
                            format!("unknown auto-install value `{other}` (always|prompt|never)"),
                        ));
                    }
                };
            }
            // Unknown keys are ignored: a config written by a newer jdk must
            // not break an older shim.
            _ => {
                if !is_subset_value(value.trim()) {
                    return Err(ConfigError::parse(
                        line,
                        "value must be a \"quoted string\" or true|false",
                    ));
                }
            }
        }
    }
    Ok(config)
}

/// The value of a known string key, unquoted; anything else is an error.
fn quoted<'a>(line: &str, value: &'a str) -> Result<&'a str, ConfigError> {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|inner| !inner.is_empty() && !inner.contains('"'))
        .ok_or_else(|| ConfigError::parse(line, "value must be a \"quoted string\""))
}

/// Whether an ignored value still fits the written subset (quoted string or
/// bare boolean) — a malformed line is an error even on an unknown key.
fn is_subset_value(value: &str) -> bool {
    value == "true"
        || value == "false"
        || (value.len() >= 2
            && value.starts_with('"')
            && value.ends_with('"')
            && !value[1..value.len() - 1].contains('"'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn missing_file_is_all_defaults() {
        let temp = TempDir::new().unwrap();
        let config = load(temp.path()).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.vendor, "temurin");
        assert_eq!(config.auto_install, AutoInstall::Prompt);
    }

    #[test]
    fn loads_both_keys() {
        let temp = TempDir::new().unwrap();
        fs::write(
            crate::store::config(temp.path()),
            "vendor = \"zulu\"\nauto-install = \"never\"\n",
        )
        .unwrap();
        let config = load(temp.path()).unwrap();
        assert_eq!(config.vendor, "zulu");
        assert_eq!(config.auto_install, AutoInstall::Never);
    }

    #[test]
    fn tolerates_bom_crlf_comments_and_spacing() {
        let config =
            parse("\u{feff}# jdk config\r\n  vendor=\"corretto\"  # team default\r\n\r\n").unwrap();
        assert_eq!(config.vendor, "corretto");
        assert_eq!(config.auto_install, AutoInstall::Prompt);
    }

    #[test]
    fn normalizes_the_vendor() {
        assert_eq!(
            parse("vendor = \"GraalVM-Community\"").unwrap().vendor,
            "graalvm_community"
        );
    }

    #[test]
    fn parses_every_auto_install_value() {
        for (text, expected) in [
            ("always", AutoInstall::Always),
            ("prompt", AutoInstall::Prompt),
            ("never", AutoInstall::Never),
        ] {
            let config = parse(&format!("auto-install = \"{text}\"")).unwrap();
            assert_eq!(config.auto_install, expected, "{text}");
        }
    }

    #[test]
    fn ignores_unknown_keys_inside_the_subset() {
        let config = parse("future-key = \"x\"\nflag = true\nvendor = \"zulu\"\n").unwrap();
        assert_eq!(config.vendor, "zulu");
    }

    #[test]
    fn malformed_lines_error_clearly() {
        for text in [
            "vendor",                   // no `=`
            "vendor = zulu",            // unquoted string
            "vendor = \"\"",            // empty
            "vendor = \"zu\"lu\"",      // stray quote
            "auto-install = \"maybe\"", // unknown enum value
            "auto-install = true",      // wrong type
            "future = [1, 2]",          // outside the subset, even if unknown
            "[table]",                  // TOML we do not speak
        ] {
            let err = parse(text).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Parse { .. }),
                "{text:?} should be a parse error, got {err:?}"
            );
            let message = err.to_string();
            assert!(message.contains("config.toml"), "{text:?} → {message}");
        }
    }
}
