use std::fmt;
use std::str::FromStr;

/// A version or selector string that cannot be parsed. Carries the offending
/// text; maps to exit code [`crate::exit::CONFIG`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid version or selector `{}`", self.0)
    }
}

impl std::error::Error for ParseError {}

/// Component-based version (JEP 223 plus vendor extensions): `21`, `21.0.4`,
/// `21.0.4+7`, Corretto `21.0.7.6.1`, legacy `1.8.0_392` (the underscore reads
/// as one more component separator).
///
/// The derived ordering compares components lexicographically, then build
/// (absent sorts lowest), then pre-release. A pre-release therefore sorts
/// after its release; [`crate::store::best_candidate`]
/// corrects for it by ranking stable above pre-release.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub components: Vec<u32>,
    pub build: Option<Vec<u32>>,
    pub pre_release: Option<String>,
}

impl Version {
    /// Whether this installed version is accepted by `pattern`.
    ///
    /// The pattern acts as a prefix: `17` accepts `17.0.9`, while the more
    /// specific `17.0.0` rejects a bare `17`. A lone build number is reconciled
    /// with the same number spelled as a trailing component, in either
    /// direction — pattern `11.0.2+9` accepts `11.0.2.9` (and `11.0.2.9.1`),
    /// and pattern `11.0.2.9` accepts `11.0.2+9`.
    pub fn matches(&self, pattern: &Version) -> bool {
        if self.matches_directly(pattern) {
            return true;
        }

        // A one-number build and that number written as a trailing component
        // name the same release. Fold the lone build digit onto the component
        // list and compare the flattened spellings, whichever side carries it.
        if let Some(build) = &pattern.build
            && let &[digit] = build.as_slice()
        {
            let mut folded = pattern.components.clone();
            folded.push(digit);
            return self.components.starts_with(&folded);
        }
        if let Some(build) = &self.build
            && let &[digit] = build.as_slice()
            && pattern.build.is_none()
        {
            let mut folded = self.components.clone();
            folded.push(digit);
            return pattern.components == folded;
        }

        false
    }

    /// Direct satisfaction: the pattern's components are a leading slice of
    /// ours, and any build or pre-release the pattern pins equals ours exactly.
    fn matches_directly(&self, pattern: &Version) -> bool {
        if !self.components.starts_with(&pattern.components) {
            return false;
        }
        let build_ok = pattern.build.is_none() || pattern.build == self.build;
        let pre_release_ok =
            pattern.pre_release.is_none() || pattern.pre_release == self.pre_release;
        build_ok && pre_release_ok
    }
}

impl FromStr for Version {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let err = || ParseError(s.to_string());
        if s.is_empty() {
            return Err(err());
        }

        // Whichever of '+' or '-' comes first ends the numeric part: '+' starts
        // a build when purely numeric (otherwise a pre-release, e.g. GraalVM's
        // "+11-jvmci-24.1"); '-' always starts a pre-release.
        let mut build = None;
        let mut pre_release = None;
        let head = match s.find(['+', '-']) {
            Some(at) => {
                let text = &s[at + 1..];
                if text.is_empty() {
                    return Err(err());
                }
                if s.as_bytes()[at] == b'+' {
                    match text.split('.').map(|part| part.parse().ok()).collect() {
                        Some(parts) => build = Some(parts),
                        None => pre_release = Some(text.to_string()),
                    }
                } else {
                    pre_release = Some(text.to_string());
                }
                &s[..at]
            }
            None => s,
        };

        // Legacy scheme (1.8.0_392): the underscore separates the update number.
        let components = head
            .replace('_', ".")
            .split('.')
            .map(|part| part.parse::<u32>().map_err(|_| err()))
            .collect::<Result<Vec<u32>, _>>()?;

        Ok(Version {
            components,
            build,
            pre_release,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dotted = |parts: &[u32]| {
            parts
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        };
        f.write_str(&dotted(&self.components))?;
        if let Some(build) = &self.build {
            write!(f, "+{}", dotted(build))?;
        }
        if let Some(pre_release) = &self.pre_release {
            write!(f, "-{pre_release}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn parses_plain_versions() {
        assert_eq!(v("21").components, vec![21]);
        assert_eq!(v("21.0").components, vec![21, 0]);
        assert_eq!(v("17.0.9").components, vec![17, 0, 9]);
        assert_eq!(v("21.0.7.6.1").components, vec![21, 0, 7, 6, 1]);
        assert_eq!(v("21.0.7.0.7.6").components, vec![21, 0, 7, 0, 7, 6]);
    }

    #[test]
    fn parses_build() {
        assert_eq!(v("11.0.2+9").build, Some(vec![9]));
        assert_eq!(v("21.0.7+9.1").build, Some(vec![9, 1]));
        assert_eq!(v("21.0.5+13.674.11").build, Some(vec![13, 674, 11]));
    }

    #[test]
    fn parses_pre_release() {
        let ea = v("21.0.7-ea");
        assert_eq!(ea.components, vec![21, 0, 7]);
        assert_eq!(ea.pre_release, Some("ea".to_string()));

        // '-' first: everything after it is pre-release, '+' included.
        assert_eq!(v("21.0.5-ea+11").pre_release, Some("ea+11".to_string()));
        // '+' first but not numeric: pre-release (GraalVM identifiers).
        let graal = v("21.0.5+11-jvmci-24.1-b01");
        assert_eq!(graal.build, None);
        assert_eq!(graal.pre_release, Some("11-jvmci-24.1-b01".to_string()));
    }

    #[test]
    fn parses_legacy_underscore() {
        let legacy = v("1.8.0_392");
        assert_eq!(legacy.components, vec![1, 8, 0, 392]);
        assert_eq!(legacy.build, None);
    }

    #[test]
    fn build_overflow_becomes_pre_release() {
        // All-digit but out of u32 range: falls back to pre-release instead of
        // panicking.
        let huge = v("21+99999999999999999999");
        assert_eq!(huge.build, None);
        assert_eq!(huge.pre_release, Some("99999999999999999999".to_string()));
    }

    #[test]
    fn rejects_invalid() {
        for text in [
            "", "invalid", "21.x.0", "21_a", "21..0", ".21", "21.", "21.0.7+", "21.0.7-",
        ] {
            assert!(
                text.parse::<Version>().is_err(),
                "{text:?} should not parse"
            );
        }
    }

    #[test]
    fn displays_canonically() {
        for text in [
            "21",
            "21.0",
            "17.0.9",
            "11.0.2+9",
            "21.0.7.6.1",
            "21.0.7+9.1.3",
            "21.0.7-ea",
        ] {
            assert_eq!(v(text).to_string(), text);
        }
        assert_eq!(v("1.8.0_392").to_string(), "1.8.0.392");
    }

    #[test]
    fn matches_prefix() {
        assert!(v("21.0.1").matches(&v("21")));
        assert!(!v("21.0.1").matches(&v("17")));
        assert!(v("17.0.9").matches(&v("17.0")));
        assert!(v("17.0.9").matches(&v("17.0.9")));
        assert!(!v("17.0.9").matches(&v("17.0.8")));
        // Pattern more specific than the installed version: no match.
        assert!(!v("21").matches(&v("21.0")));
        assert!(!v("21.0").matches(&v("21.0.0")));
    }

    #[test]
    fn matches_build() {
        let installed = v("21.0.0+37");
        assert!(installed.matches(&v("21")));
        assert!(installed.matches(&v("21.0.0")));
        assert!(installed.matches(&v("21.0.0+37")));
        assert!(!installed.matches(&v("21.0.0+38")));

        // Installed without build never satisfies a build pattern.
        assert!(!v("21.0.4").matches(&v("21.0.4+7")));
    }

    #[test]
    fn matches_build_flexibly() {
        // Pattern with build vs build incorporated into components.
        assert!(v("24.0.2.12.1").matches(&v("24.0.2+12")));
        assert!(v("24.0.2.12").matches(&v("24.0.2+12")));
        assert!(v("21.0.5.11.0.25").matches(&v("21.0.5+11")));
        assert!(!v("24.0.2.13.1").matches(&v("24.0.2+12")));
        assert!(!v("24.0.3.12.1").matches(&v("24.0.2+12")));

        // The reverse: pattern with the build spelled as a component.
        assert!(v("21.0.5+11").matches(&v("21.0.5.11")));
        assert!(!v("21.0.5+12").matches(&v("21.0.5.11")));
        assert!(!v("21.0.4+11").matches(&v("21.0.5.11")));
    }

    #[test]
    fn matches_pre_release() {
        let ea = v("21.0.5-ea");
        assert!(ea.matches(&v("21.0.5")));
        assert!(ea.matches(&v("21.0.5-ea")));
        assert!(!ea.matches(&v("21.0.5-beta")));
        assert!(!v("21.0.5").matches(&v("21.0.5-ea")));
    }

    #[test]
    fn orders_versions() {
        assert!(v("21") < v("22"));
        assert!(v("21.0") < v("21.1"));
        assert!(v("21") < v("21.0"));
        assert!(v("21.0.7.5.9") < v("21.0.7.6.1"));
        assert!(v("21.0.5") < v("21.0.5+1"));
        assert!(v("21.0.5+9") < v("21.0.5+10"));
    }
}
