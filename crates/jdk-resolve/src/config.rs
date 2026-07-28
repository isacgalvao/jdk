//! Reader for `<JDK_ROOT>\config.toml`, shared by the CLI and the shim.
//!
//! Deliberately NOT a TOML library (this crate is the shim's std-only
//! firewall). The file is a flat SUBSET of TOML that the CLI — its only
//! writer (`jdk-core::config`) — guarantees: `key = "string"` or
//! `key = true|false` lines, `#` comments outside quotes, blank lines. No
//! tables, arrays, escapes or multi-line values. The reader tolerates a UTF-8
//! BOM and CRLF, ignores unknown keys (a newer jdk may know more), and rejects
//! anything outside the subset with an error naming the line.
//!
//! `entries` is the whole format's only lexer: `parse` here reads the keys
//! that make up [`Config`], and `jdk-core::config` reads the JAVA_HOME backup
//! keys through the same iterator. One file, one notion of what it says.
//!
//! Keys: `vendor` (default vendor for versions without one), `auto-install`
//! (shim behavior for a pinned-but-missing JDK) and `accept-license` (standing
//! consent to proprietary vendor terms, for CI and scripts).

use crate::selector::normalize_vendor;
use crate::text::config_lines;
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
    /// Standing consent to the terms of vendors that ship under proprietary
    /// licenses, for hosts with no console to prompt on (CI, scripts). The
    /// shim never acts on it — it declines those terms whatever the config
    /// says (see `jdk install --from-shim`); only an explicit `jdk install`
    /// reads it. Defaults to `false`: consent is never assumed.
    pub accept_license: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            vendor: DEFAULT_VENDOR.to_string(),
            auto_install: AutoInstall::Prompt,
            accept_license: false,
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

/// One `key = value` line of the subset, already checked to be inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub key: &'a str,
    pub value: Value<'a>,
    /// The comment-stripped line, kept so a caller rejecting a value it does
    /// not recognize reports it exactly like the lexer does.
    line: &'a str,
}

/// A value of the subset. The two shapes stay apart so a key that wants a
/// string can refuse `auto-install = true` instead of reading `"true"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    /// Inner text of a `"quoted string"`, never containing a quote. May be
    /// empty: whether that is meaningful belongs to the key (a vendor cannot
    /// be empty, a backed-up JAVA_HOME can).
    Str(&'a str),
    Bool(bool),
}

impl<'a> Entry<'a> {
    /// The value as a string, or a parse error naming the line.
    pub fn string(&self) -> Result<&'a str, ConfigError> {
        match self.value {
            Value::Str(text) => Ok(text),
            Value::Bool(_) => Err(self.invalid("value must be a \"quoted string\"")),
        }
    }

    /// The value as a boolean, or a parse error naming the line. Quoted
    /// `"true"` is refused rather than coerced: a flag whose only wrong
    /// spellings read as `false` would revoke consent without saying so.
    pub fn boolean(&self) -> Result<bool, ConfigError> {
        match self.value {
            Value::Bool(flag) => Ok(flag),
            Value::Str(_) => Err(self.invalid("value must be bare true or false")),
        }
    }

    /// Rejects this line for a reason only the key's owner knows (an unknown
    /// enum value, an empty vendor).
    pub fn invalid(&self, reason: impl Into<String>) -> ConfigError {
        ConfigError::parse(self.line, reason)
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
    for entry in entries(text) {
        let entry = entry?;
        match entry.key {
            "vendor" => {
                let vendor = entry.string()?;
                if vendor.is_empty() {
                    return Err(entry.invalid("vendor must not be empty"));
                }
                config.vendor = normalize_vendor(vendor);
            }
            "auto-install" => {
                config.auto_install = match entry.string()? {
                    "always" => AutoInstall::Always,
                    "prompt" => AutoInstall::Prompt,
                    "never" => AutoInstall::Never,
                    other => {
                        return Err(entry.invalid(format!(
                            "unknown auto-install value `{other}` (always|prompt|never)"
                        )));
                    }
                };
            }
            "accept-license" => config.accept_license = entry.boolean()?,
            // Unknown keys are ignored: a config written by a newer jdk must
            // not break an older shim. `entries` still held the line to the
            // subset, so a malformed one is an error whatever its key.
            _ => {}
        }
    }
    Ok(config)
}

/// Every `key = value` entry, in file order, each already inside the subset.
/// This is the single lexer both readers of the file share — the shim's
/// `parse` above and the CLI's JAVA_HOME-backup reader in `jdk-core::config`.
/// Two hand-rolled readers of one format drift: the CLI's used to truncate at
/// any `#` and unquote with `trim_matches`, so a path holding a `#` came back
/// silently cut on that side and a hard parse error on this one.
pub fn entries(text: &str) -> impl Iterator<Item = Result<Entry<'_>, ConfigError>> {
    config_lines(text).map(entry)
}

fn entry(line: &str) -> Result<Entry<'_>, ConfigError> {
    let Some((key, value)) = line.split_once('=') else {
        return Err(ConfigError::parse(line, "expected `key = \"value\"`"));
    };
    let value = match value.trim() {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        quoted => Value::Str(unquote(quoted).ok_or_else(|| {
            ConfigError::parse(line, "value must be a \"quoted string\" or true|false")
        })?),
    };
    Ok(Entry {
        key: key.trim(),
        value,
        line,
    })
}

/// Inner text of a `"quoted string"`; `None` for anything else. Nothing is
/// unescaped — the writer emits no escapes, so a value is whatever sits
/// between the outer quotes, `#` and backslashes included.
fn unquote(value: &str) -> Option<&str> {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|inner| !inner.contains('"'))
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
        assert!(!config.accept_license, "license consent is never a default");
    }

    #[test]
    fn loads_every_key() {
        let temp = TempDir::new().unwrap();
        fs::write(
            crate::store::config(temp.path()),
            "vendor = \"zulu\"\nauto-install = \"never\"\naccept-license = true\n",
        )
        .unwrap();
        let config = load(temp.path()).unwrap();
        assert_eq!(config.vendor, "zulu");
        assert_eq!(config.auto_install, AutoInstall::Never);
        assert!(config.accept_license);
    }

    #[test]
    fn accept_license_reads_both_booleans() {
        assert!(parse("accept-license = true").unwrap().accept_license);
        assert!(!parse("accept-license = false").unwrap().accept_license);
        // Absent is the default, and the default is refusal.
        assert!(!parse("vendor = \"zulu\"").unwrap().accept_license);
    }

    /// Anything that is not a bare boolean is an error, never a silent
    /// `false`: this key is legal consent, so a typo must stop the run and
    /// be fixed, not be read as the opposite of what was written.
    #[test]
    fn a_non_boolean_accept_license_is_an_error() {
        for text in [
            "accept-license = \"true\"",
            "accept-license = \"yes\"",
            "accept-license = \"\"",
            "accept-license = yes",
            "accept-license = 1",
            "accept-license =",
        ] {
            let err = parse(text).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Parse { .. }),
                "{text:?} should be a parse error, got {err:?}"
            );
        }
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
    fn a_hash_inside_quotes_is_data_not_a_comment() {
        // The regression: one `#` in the backed-up JAVA_HOME used to truncate
        // the line, drop the closing quote and fail the file for the shim.
        let text = "java-home-before = \"C:\\Tools\\jdk#17\"\nvendor = \"zulu\"\n";
        assert_eq!(parse(text).unwrap().vendor, "zulu");

        let values: Vec<Value<'_>> = entries(text).map(|e| e.unwrap().value).collect();
        assert_eq!(values[0], Value::Str("C:\\Tools\\jdk#17"));
    }

    #[test]
    fn entries_report_key_value_and_type() {
        let text = "vendor = \"zulu\"  # team default\nflag = false\nempty = \"\"\n";
        let found: Vec<Entry<'_>> = entries(text).map(Result::unwrap).collect();
        assert_eq!(found[0].key, "vendor");
        assert_eq!(found[0].string().unwrap(), "zulu");
        assert_eq!(found[1].value, Value::Bool(false));
        assert!(found[1].string().is_err(), "a boolean is not a string");
        assert!(!found[1].boolean().unwrap());
        assert!(found[0].boolean().is_err(), "a string is not a boolean");
        // An empty string is inside the subset; only the key decides whether
        // it means anything (`vendor = ""` is rejected below).
        assert_eq!(found[2].string().unwrap(), "");
    }

    #[test]
    fn malformed_lines_error_clearly() {
        for text in [
            "vendor",                    // no `=`
            "vendor = zulu",             // unquoted string
            "vendor = \"\"",             // empty
            "vendor = \"zu\"lu\"",       // stray quote
            "auto-install = \"maybe\"",  // unknown enum value
            "auto-install = true",       // wrong type
            "accept-license = \"true\"", // wrong type, the other way round
            "future = [1, 2]",           // outside the subset, even if unknown
            "[table]",                   // TOML we do not speak
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
