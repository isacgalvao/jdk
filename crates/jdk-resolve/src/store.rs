//! Store layout under `%USERPROFILE%\.jdk` (override: env `JDK_ROOT`).
//!
//! ```text
//! <root>\candidates\java\<vendor@version>\   installed JDKs
//! <root>\shims\                              copies of jdk-shim.exe (M4)
//! <root>\current                             junction JAVA_HOME points at (M4)
//! <root>\config.toml                         CLI config (M3)
//! <root>\cache\                              catalog cache (M2)
//! ```

use crate::selector::{Selector, normalize_vendor};
use crate::version::Version;
use std::env;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Store root: `JDK_ROOT` when set and non-empty, else `<home>\.jdk`.
/// `None` only when the home directory cannot be determined.
pub fn root() -> Option<PathBuf> {
    root_from(env::var_os("JDK_ROOT"), env::home_dir())
}

fn root_from(jdk_root: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    match jdk_root {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|home| home.join(".jdk")),
    }
}

pub fn java_candidates(root: &Path) -> PathBuf {
    root.join("candidates").join("java")
}

pub fn shims(root: &Path) -> PathBuf {
    root.join("shims")
}

pub fn current(root: &Path) -> PathBuf {
    root.join("current")
}

pub fn config(root: &Path) -> PathBuf {
    root.join("config.toml")
}

pub fn cache(root: &Path) -> PathBuf {
    root.join("cache")
}

/// An installed JDK, parsed from a `candidates\java\<vendor@version>` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub vendor: String,
    pub version: Version,
    pub dir: PathBuf,
}

/// Every installed JDK under `candidates\java`, sorted by vendor then
/// version. Entries that are not directories named `vendor@version` are
/// skipped silently — `jdk doctor` reports them (M4). A missing candidates
/// directory means nothing is installed.
pub fn installed(root: &Path) -> io::Result<Vec<Candidate>> {
    let entries = match std::fs::read_dir(java_candidates(root)) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((vendor, version)) = name.split_once('@') else {
            continue;
        };
        let Ok(version) = version.parse::<Version>() else {
            continue;
        };
        found.push(Candidate {
            vendor: normalize_vendor(vendor),
            version,
            dir: entry.path(),
        });
    }
    found.sort_by(|a, b| (&a.vendor, &a.version).cmp(&(&b.vendor, &b.version)));
    Ok(found)
}

/// Highest installed candidate matching `selector` (whose version is a prefix
/// pattern), preferring stable over pre-release: `21` with `21.0.5` and
/// `21.0.6-ea` installed picks `21.0.5`; a pre-release wins only when nothing
/// stable matches or the selector names one explicitly (`21.0.5-ea`). A
/// selector without vendor matches `default_vendor`.
pub fn best_candidate(
    root: &Path,
    selector: &Selector,
    default_vendor: &str,
) -> io::Result<Option<Candidate>> {
    let vendor = normalize_vendor(selector.vendor.as_deref().unwrap_or(default_vendor));
    let pattern = &selector.version;
    Ok(installed(root)?
        .into_iter()
        .filter(|candidate| candidate.vendor == vendor && candidate.version.matches(pattern))
        .max_by(|a, b| rank(&a.version).cmp(&rank(&b.version))))
}

/// Candidate ranking: stable (no pre-release) beats pre-release, then the
/// higher version wins. The raw `Version` ordering alone would rank
/// `21.0.5-ea` above `21.0.5`. jdk-core ranks remote packages by the same
/// rule (`catalog::pick_best` there) — keep the two aligned.
fn rank(version: &Version) -> (bool, &Version) {
    (version.pre_release.is_none(), version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn store_with(candidates: &[&str]) -> TempDir {
        let temp = TempDir::new().unwrap();
        for name in candidates {
            fs::create_dir_all(java_candidates(temp.path()).join(name)).unwrap();
        }
        temp
    }

    fn best(root: &Path, selector: &str) -> Option<Candidate> {
        best_candidate(root, &selector.parse().unwrap(), "temurin").unwrap()
    }

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn resolves_root_override_and_home_default() {
        assert_eq!(
            root_from(
                Some("D:\\store".into()),
                Some(PathBuf::from("C:\\Users\\x"))
            ),
            Some(PathBuf::from("D:\\store"))
        );
        // Empty JDK_ROOT counts as unset.
        assert_eq!(
            root_from(Some(OsString::new()), Some(PathBuf::from("C:\\Users\\x"))),
            Some(PathBuf::from("C:\\Users\\x").join(".jdk"))
        );
        assert_eq!(root_from(None, None), None);
    }

    #[test]
    fn lays_out_the_store() {
        let root = Path::new("R:\\store");
        assert_eq!(java_candidates(root), root.join("candidates").join("java"));
        assert_eq!(shims(root), root.join("shims"));
        assert_eq!(current(root), root.join("current"));
        assert_eq!(config(root), root.join("config.toml"));
        assert_eq!(cache(root), root.join("cache"));
    }

    #[test]
    fn installed_lists_sorted_by_vendor_then_version() {
        let temp = store_with(&["zulu@8", "temurin@21.0.4", "temurin@17.0.9", "notajdk"]);
        let names: Vec<String> = installed(temp.path())
            .unwrap()
            .into_iter()
            .map(|c| format!("{}@{}", c.vendor, c.version))
            .collect();
        assert_eq!(names, ["temurin@17.0.9", "temurin@21.0.4", "zulu@8"]);
        assert!(
            installed(TempDir::new().unwrap().path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn picks_the_highest_matching_version() {
        let temp = store_with(&["temurin@21.0.3", "temurin@21.0.4", "temurin@17.0.9"]);
        let found = best(temp.path(), "21").unwrap();
        assert_eq!(found.version, "21.0.4".parse().unwrap());
        assert_eq!(
            found.dir,
            java_candidates(temp.path()).join("temurin@21.0.4")
        );
    }

    #[test]
    fn selector_without_vendor_uses_the_default_parameter() {
        let temp = store_with(&["temurin@21.0.4", "zulu@21.0.5"]);
        assert_eq!(best(temp.path(), "21").unwrap().vendor, "temurin");

        let via_zulu = best_candidate(temp.path(), &"21".parse().unwrap(), "zulu")
            .unwrap()
            .unwrap();
        assert_eq!(via_zulu.vendor, "zulu");
        assert_eq!(via_zulu.version, "21.0.5".parse().unwrap());
    }

    #[test]
    fn explicit_vendor_filters() {
        let temp = store_with(&["temurin@21.0.4", "zulu@21.0.5"]);
        assert_eq!(best(temp.path(), "zulu@21").unwrap().vendor, "zulu");
        assert!(best(temp.path(), "corretto@21").is_none());
    }

    #[test]
    fn prefers_stable_over_pre_release() {
        // Even a HIGHER pre-release loses to a stable match.
        let temp = store_with(&["temurin@21.0.5", "temurin@21.0.5-ea", "temurin@21.0.6-ea"]);
        assert_eq!(best(temp.path(), "21").unwrap().version, v("21.0.5"));
        assert_eq!(best(temp.path(), "21.0.5").unwrap().version, v("21.0.5"));
    }

    #[test]
    fn pre_release_wins_only_without_a_stable_match() {
        let temp = store_with(&["temurin@22-ea", "temurin@21.0.5"]);
        assert_eq!(best(temp.path(), "22").unwrap().version, v("22-ea"));
    }

    #[test]
    fn explicit_pre_release_selector_picks_the_pre_release() {
        let temp = store_with(&["temurin@21.0.5", "temurin@21.0.5-ea"]);
        assert_eq!(
            best(temp.path(), "21.0.5-ea").unwrap().version,
            v("21.0.5-ea")
        );
    }

    #[test]
    fn matches_build_patterns_in_directory_names() {
        let temp = store_with(&["temurin@21.0.4+7"]);
        assert!(best(temp.path(), "21.0.4+7").is_some());
        assert!(best(temp.path(), "21.0.4.7").is_some());
        assert!(best(temp.path(), "21").is_some());
        assert!(best(temp.path(), "21.0.4+8").is_none());
    }

    #[test]
    fn skips_unparseable_entries_and_plain_files() {
        let temp = store_with(&["notajdk", "temurin@banana", "temurin@21.0.4"]);
        fs::write(java_candidates(temp.path()).join("temurin@22"), "not a dir").unwrap();
        let found = best(temp.path(), "21").unwrap();
        assert_eq!(found.version, "21.0.4".parse().unwrap());
        assert!(best(temp.path(), "22").is_none());
    }

    #[test]
    fn missing_candidates_directory_means_nothing_installed() {
        let temp = TempDir::new().unwrap();
        assert!(best(temp.path(), "21").is_none());
    }
}
