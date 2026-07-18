use crate::pin;
use crate::selector::Selector;
use crate::version::ParseError;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A java pin found by the cascade: the selector and the file that declared it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub selector: Selector,
    pub file: PathBuf,
}

/// Outcome of the cascade. `pin: None` means no directory pinned java and the
/// caller falls back to the global JDK. `searched` lists every directory
/// visited, for rich "nothing found" messages.
#[derive(Debug)]
pub struct Resolution {
    pub pin: Option<Pin>,
    pub searched: Vec<PathBuf>,
}

/// Why the cascade failed; `exit_code` maps to the shared contract in
/// [`crate::exit`].
#[derive(Debug)]
pub enum ResolveError {
    /// A pin file declared a java entry that does not parse.
    Pin { file: PathBuf, source: ParseError },
    /// A pin file exists but could not be read.
    Read { file: PathBuf, source: io::Error },
}

impl ResolveError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ResolveError::Pin { .. } => crate::exit::CONFIG,
            ResolveError::Read { .. } => crate::exit::FAILURE,
        }
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::Pin { file, source } => write!(f, "{}: {source}", file.display()),
            ResolveError::Read { file, source } => {
                write!(f, "cannot read {}: {source}", file.display())
            }
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ResolveError::Pin { source, .. } => Some(source),
            ResolveError::Read { source, .. } => Some(source),
        }
    }
}

/// Walks from `start` up to the drive root. The FIRST directory containing any
/// pin file decides: its files are tried in precedence order and the walk stops
/// there even when none of them pins java (levels never mix). No pin file all
/// the way up → `pin: None`.
pub fn resolve(start: &Path) -> Result<Resolution, ResolveError> {
    let mut searched = Vec::new();

    for dir in start.ancestors() {
        searched.push(dir.to_path_buf());

        // A directory holding any recognized source file is the boundary: its
        // sources are read in precedence order, and the walk ends here whether
        // or not one names a java version — a level never inherits from above.
        let mut found_source = false;
        for (filename, parse) in pin::SOURCES {
            let file = dir.join(filename);
            if !file.exists() {
                continue;
            }
            found_source = true;

            let text = fs::read_to_string(&file).map_err(|source| ResolveError::Read {
                file: file.clone(),
                source,
            })?;
            let declared = parse(&text).map_err(|source| ResolveError::Pin {
                file: file.clone(),
                source,
            })?;
            if let Some(selector) = declared {
                return Ok(Resolution {
                    pin: Some(Pin { selector, file }),
                    searched,
                });
            }
        }

        if found_source {
            return Ok(Resolution {
                pin: None,
                searched,
            });
        }
    }

    Ok(Resolution {
        pin: None,
        searched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    fn pin_of(resolution: &Resolution) -> &Pin {
        resolution.pin.as_ref().expect("expected a pin")
    }

    #[test]
    fn finds_pin_from_deep_subdirectory() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("proj");
        let deep = project
            .join("src")
            .join("main")
            .join("java")
            .join("com")
            .join("acme");
        fs::create_dir_all(&deep).unwrap();
        write(&project, ".sdkmanrc", "java=21.0.4-tem\n");

        let resolution = resolve(&deep).unwrap();
        let pin = pin_of(&resolution);
        assert_eq!(pin.selector, "temurin@21.0.4".parse().unwrap());
        assert_eq!(pin.file, project.join(".sdkmanrc"));
        // Every directory from `deep` down to `project` was visited.
        assert_eq!(resolution.searched.len(), 6);
        assert_eq!(resolution.searched.first().unwrap(), &deep);
        assert_eq!(resolution.searched.last().unwrap(), &project);
    }

    #[test]
    fn applies_precedence_within_one_directory() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), ".jdkrc", "java=temurin@21\n");
        write(temp.path(), ".sdkmanrc", "java=17.0.9-amzn\n");
        write(temp.path(), ".java-version", "11\n");
        write(temp.path(), ".tool-versions", "java zulu-8\n");

        let resolution = resolve(temp.path()).unwrap();
        assert_eq!(pin_of(&resolution).selector, "temurin@21".parse().unwrap());

        fs::remove_file(temp.path().join(".jdkrc")).unwrap();
        let resolution = resolve(temp.path()).unwrap();
        assert_eq!(
            pin_of(&resolution).selector,
            "corretto@17.0.9".parse().unwrap()
        );

        fs::remove_file(temp.path().join(".sdkmanrc")).unwrap();
        let resolution = resolve(temp.path()).unwrap();
        assert_eq!(pin_of(&resolution).selector, "11".parse().unwrap());

        fs::remove_file(temp.path().join(".java-version")).unwrap();
        let resolution = resolve(temp.path()).unwrap();
        assert_eq!(pin_of(&resolution).selector, "zulu@8".parse().unwrap());
    }

    #[test]
    fn stops_at_first_directory_with_any_source() {
        let temp = TempDir::new().unwrap();
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        write(temp.path(), ".jdkrc", "java=temurin@21\n");
        write(&child, ".java-version", "17\n");

        let resolution = resolve(&child).unwrap();
        assert_eq!(pin_of(&resolution).selector, "17".parse().unwrap());
    }

    #[test]
    fn boundary_without_java_pin_never_mixes_levels() {
        let temp = TempDir::new().unwrap();
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        write(temp.path(), ".jdkrc", "java=temurin@21\n");
        // The child directory IS a boundary (it has a source file), but that
        // file says nothing about java: resolution goes global, not to the
        // parent's pin.
        write(&child, ".tool-versions", "nodejs 20.10.0\n");

        let resolution = resolve(&child).unwrap();
        assert!(resolution.pin.is_none());
    }

    #[test]
    fn source_without_java_falls_to_next_source_in_same_directory() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), ".jdkrc", "maven=3.9\n");
        write(temp.path(), ".sdkmanrc", "java=21.0.4-tem\n");

        let resolution = resolve(temp.path()).unwrap();
        assert_eq!(
            pin_of(&resolution).selector,
            "temurin@21.0.4".parse().unwrap()
        );
    }

    #[test]
    fn malformed_pin_is_a_config_error() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), ".sdkmanrc", "java=banana\n");

        let err = resolve(temp.path()).unwrap_err();
        assert!(matches!(&err, ResolveError::Pin { file, .. } if file.ends_with(".sdkmanrc")));
        assert_eq!(err.exit_code(), crate::exit::CONFIG);
    }

    #[test]
    fn walks_past_pinless_directories_to_the_root() {
        // Two pinless levels inside the temp dir: the walk must pass through
        // both (a pinless directory is never a boundary). A pin in an ancestor
        // OUTSIDE the temp dir is machine noise this test tolerates — it can
        // never come from inside the temp tree. On a clean machine (always on
        // CI) no pin exists anywhere and the walk reaches the drive root.
        let temp = TempDir::new().unwrap();
        let start = temp.path().join("a").join("b");
        fs::create_dir_all(&start).unwrap();

        let resolution = resolve(&start).unwrap();

        assert_eq!(resolution.searched.first(), Some(&start));
        assert!(resolution.searched.contains(&temp.path().to_path_buf()));
        match &resolution.pin {
            Some(pin) => assert!(
                !pin.file.starts_with(temp.path()),
                "phantom pin inside the temp dir: {}",
                pin.file.display()
            ),
            None => {
                let root = start.ancestors().last().unwrap();
                assert_eq!(resolution.searched.last().map(PathBuf::as_path), Some(root));
            }
        }
    }
}
