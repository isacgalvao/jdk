//! Parsers for the four pin-file formats.
//!
//! Every parser tolerates a UTF-8 BOM, CRLF and stray spaces; `#` starts a
//! comment anywhere (no legitimate pin value contains it). `Ok(None)` means the
//! file declares nothing about java (e.g. a `.jdkrc` with only other tools);
//! a declared but malformed java entry is an error, never silently skipped.

use crate::selector::{Selector, normalize_vendor};
use crate::text::meaningful_lines as lines;
use crate::version::ParseError;

pub type Parser = fn(&str) -> Result<Option<Selector>, ParseError>;

/// Pin files in precedence order; the cascade stops at the first directory
/// containing any of them.
pub const SOURCES: [(&str, Parser); 4] = [
    (".jdkrc", jdkrc),
    (".sdkmanrc", sdkmanrc),
    (".java-version", java_version),
    (".tool-versions", tool_versions),
];

/// SDKMAN candidate suffix → our vendor id. SDKMAN tokens are accepted only
/// inside `.sdkmanrc`.
const SDKMAN_VENDORS: [(&str, &str); 11] = [
    ("tem", "temurin"),
    ("zulu", "zulu"),
    ("amzn", "corretto"),
    ("librca", "liberica"),
    ("ms", "microsoft"),
    ("graalce", "graalvm_community"),
    ("graal", "graalvm"),
    ("oracle", "oracle"),
    ("open", "openjdk"),
    ("sem", "semeru"),
    ("sapmchn", "sapmachine"),
];

/// `.jdkrc`: key=value, only the `java` key is read (`java=temurin@21`).
fn jdkrc(content: &str) -> Result<Option<Selector>, ParseError> {
    java_value(content).map(str::parse).transpose()
}

/// `.sdkmanrc`: key=value with SDKMAN tokens (`java=21.0.4-tem`).
fn sdkmanrc(content: &str) -> Result<Option<Selector>, ParseError> {
    java_value(content).map(sdkman_selector).transpose()
}

/// `.java-version`: bare version (`21`, `1.8.0_392`) or `vendor-version`
/// (`temurin-21`). Only the first meaningful line counts.
fn java_version(content: &str) -> Result<Option<Selector>, ParseError> {
    lines(content).next().map(vendor_dash_version).transpose()
}

/// `.tool-versions` (asdf): first `java <version>` line; further values on the
/// line are asdf fallbacks and are ignored.
fn tool_versions(content: &str) -> Result<Option<Selector>, ParseError> {
    for line in lines(content) {
        let mut tokens = line.split_whitespace();
        if tokens.next() == Some("java") {
            return match tokens.next() {
                Some(value) => vendor_dash_version(value).map(Some),
                None => Err(ParseError(line.to_string())),
            };
        }
    }
    Ok(None)
}

/// Value of the first `java=<value>` entry, tolerating spaces around `=`.
fn java_value(content: &str) -> Option<&str> {
    lines(content).find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "java").then(|| value.trim())
    })
}

/// `21.0.4-tem` → `temurin@21.0.4`. An unknown suffix becomes the vendor
/// verbatim, so matching against installed JDKs later fails with an error
/// naming it. No suffix → default vendor.
fn sdkman_selector(token: &str) -> Result<Selector, ParseError> {
    match token.rsplit_once('-') {
        Some((_, "")) => Err(ParseError(token.to_string())),
        Some((version, suffix)) => {
            let vendor = SDKMAN_VENDORS
                .iter()
                .find(|(sdkman, _)| *sdkman == suffix)
                .map_or_else(
                    || normalize_vendor(suffix),
                    |(_, vendor)| (*vendor).to_string(),
                );
            Ok(Selector {
                vendor: Some(vendor),
                version: version.parse()?,
            })
        }
        None => Ok(Selector {
            vendor: None,
            version: token.parse()?,
        }),
    }
}

/// Splits at the first `-` immediately followed by a digit, so `temurin-21`
/// and `graalvm-community-21.0.2` both split right before the version.
/// No such boundary → the whole text is the version.
fn vendor_dash_version(text: &str) -> Result<Selector, ParseError> {
    let boundary = text
        .match_indices('-')
        .find(|(at, _)| *at > 0 && text.as_bytes().get(at + 1).is_some_and(u8::is_ascii_digit));
    match boundary {
        Some((at, _)) => Ok(Selector {
            vendor: Some(normalize_vendor(&text[..at])),
            version: text[at + 1..].parse()?,
        }),
        None => Ok(Selector {
            vendor: None,
            version: text.parse()?,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(vendor: Option<&str>, version: &str) -> Option<Selector> {
        Some(Selector {
            vendor: vendor.map(str::to_string),
            version: version.parse().unwrap(),
        })
    }

    #[test]
    fn jdkrc_reads_only_the_java_key() {
        let content = "# project toolchain\nmaven=3.9\njava=temurin@21\nkotlin=2.0\n";
        assert_eq!(jdkrc(content).unwrap(), some(Some("temurin"), "21"));
    }

    #[test]
    fn jdkrc_without_java_key_declares_nothing() {
        assert_eq!(jdkrc("maven=3.9\n").unwrap(), None);
        assert_eq!(jdkrc("").unwrap(), None);
        assert_eq!(jdkrc("# only a comment\n").unwrap(), None);
    }

    #[test]
    fn jdkrc_tolerates_bom_crlf_spaces_and_comments() {
        let content = "\u{feff}# header\r\n  java = temurin@21.0.4  # LTS\r\n";
        assert_eq!(jdkrc(content).unwrap(), some(Some("temurin"), "21.0.4"));
    }

    #[test]
    fn jdkrc_malformed_java_value_errors() {
        assert!(jdkrc("java=@21\n").is_err());
        assert!(jdkrc("java=temurin@banana\n").is_err());
        assert!(jdkrc("java=\n").is_err());
    }

    #[test]
    fn sdkmanrc_translates_every_known_suffix() {
        for (suffix, vendor) in SDKMAN_VENDORS {
            let content = format!("java=21.0.4-{suffix}\n");
            assert_eq!(
                sdkmanrc(&content).unwrap(),
                some(Some(vendor), "21.0.4"),
                "suffix {suffix} should map to {vendor}"
            );
        }
    }

    #[test]
    fn sdkmanrc_unknown_suffix_becomes_vendor_verbatim() {
        assert_eq!(
            sdkmanrc("java=22.3-grl\n").unwrap(),
            some(Some("grl"), "22.3")
        );
    }

    #[test]
    fn sdkmanrc_without_suffix_uses_default_vendor() {
        assert_eq!(sdkmanrc("java=21.0.4\n").unwrap(), some(None, "21.0.4"));
    }

    #[test]
    fn sdkmanrc_malformed_tokens_error() {
        assert!(sdkmanrc("java=21.0.4-\n").is_err());
        assert!(sdkmanrc("java=-tem\n").is_err());
        assert!(sdkmanrc("java=21.0.4.crac-librca\n").is_err());
    }

    #[test]
    fn sdkmanrc_ignores_other_tools() {
        let content = "maven=3.9.6\njava=17.0.9-amzn\n";
        assert_eq!(sdkmanrc(content).unwrap(), some(Some("corretto"), "17.0.9"));
    }

    #[test]
    fn java_version_bare() {
        assert_eq!(java_version("21\n").unwrap(), some(None, "21"));
        assert_eq!(java_version("21.0.4\r\n").unwrap(), some(None, "21.0.4"));
        assert_eq!(
            java_version("\u{feff}  17.0.9  \n").unwrap(),
            some(None, "17.0.9")
        );
        assert_eq!(
            java_version("1.8.0_392\n").unwrap(),
            some(None, "1.8.0_392")
        );
    }

    #[test]
    fn java_version_with_vendor_prefix() {
        assert_eq!(
            java_version("temurin-21\n").unwrap(),
            some(Some("temurin"), "21")
        );
        assert_eq!(
            java_version("zulu-8.0.392\n").unwrap(),
            some(Some("zulu"), "8.0.392")
        );
    }

    #[test]
    fn java_version_empty_declares_nothing() {
        assert_eq!(java_version("").unwrap(), None);
        assert_eq!(java_version("# comment only\n\n").unwrap(), None);
    }

    #[test]
    fn java_version_takes_first_line_and_rejects_garbage() {
        assert_eq!(java_version("21\n17\n").unwrap(), some(None, "21"));
        assert!(java_version("banana\n").is_err());
    }

    #[test]
    fn tool_versions_reads_the_java_line() {
        let content = "nodejs 20.10.0\njava temurin-21.0.4+7.1 system\npython 3.12\n";
        assert_eq!(
            tool_versions(content).unwrap(),
            some(Some("temurin"), "21.0.4+7.1")
        );
    }

    #[test]
    fn tool_versions_splits_vendor_at_first_dash_before_digit() {
        assert_eq!(
            tool_versions("java graalvm-community-21.0.2\n").unwrap(),
            some(Some("graalvm_community"), "21.0.2")
        );
    }

    #[test]
    fn tool_versions_without_java_declares_nothing() {
        assert_eq!(tool_versions("nodejs 20.10.0\n").unwrap(), None);
        assert_eq!(tool_versions("").unwrap(), None);
    }

    #[test]
    fn tool_versions_java_without_value_errors() {
        assert!(tool_versions("java\n").is_err());
        assert!(tool_versions("java # pending\n").is_err());
    }

    #[test]
    fn tool_versions_tolerates_comments() {
        let content = "# runtimes\r\njava temurin-21 # LTS\r\n";
        assert_eq!(tool_versions(content).unwrap(), some(Some("temurin"), "21"));
    }
}
