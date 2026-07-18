//! Post-extraction layout normalization. Vendors wrap the JDK in arbitrary
//! directory levels (`jdk-21.0.5+11\`, sometimes none at all); the real root
//! is found by breadth-first search for the `bin\javac.exe` marker — javac
//! distinguishes a JDK from a JRE — and derived structurally as the parent
//! of `bin`, never by string surgery on the match path.

use crate::error::{Error, Result};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory containing `bin\javac.exe`, searched breadth-first so the
/// shallowest (outermost) JDK wins; sibling directories are visited in name
/// order for determinism.
pub fn find_jdk_root(dir: &Path) -> Result<PathBuf> {
    let mut queue = VecDeque::from([dir.to_path_buf()]);
    while let Some(current) = queue.pop_front() {
        let entries = fs::read_dir(&current).map_err(Error::io("scan", &current))?;
        let mut subdirs = Vec::new();
        let mut has_javac = false;
        for entry in entries {
            let entry = entry.map_err(Error::io("scan", &current))?;
            let kind = entry.file_type().map_err(Error::io("scan", &current))?;
            if kind.is_dir() {
                subdirs.push(entry.path());
            } else if kind.is_file() && entry.file_name().eq_ignore_ascii_case("javac.exe") {
                has_javac = true;
            }
        }
        if has_javac
            && current
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
            && let Some(root) = current.parent()
        {
            return Ok(root.to_path_buf());
        }
        subdirs.sort();
        queue.extend(subdirs);
    }
    Err(Error::Extract(format!(
        "no bin\\javac.exe under {} — the archive does not contain a JDK (a JRE?)",
        dir.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn plant(root: &Path, rel: &[&str]) {
        let mut path = root.to_path_buf();
        for part in &rel[..rel.len() - 1] {
            path.push(part);
        }
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(rel[rel.len() - 1]), b"x").unwrap();
    }

    #[test]
    fn finds_the_root_one_level_deep() {
        let temp = TempDir::new().unwrap();
        plant(temp.path(), &["jdk-21.0.5+11", "bin", "javac.exe"]);

        assert_eq!(
            find_jdk_root(temp.path()).unwrap(),
            temp.path().join("jdk-21.0.5+11")
        );
    }

    #[test]
    fn finds_a_bare_root() {
        let temp = TempDir::new().unwrap();
        plant(temp.path(), &["bin", "javac.exe"]);

        assert_eq!(find_jdk_root(temp.path()).unwrap(), temp.path());
    }

    #[test]
    fn digs_through_extra_wrapper_levels() {
        let temp = TempDir::new().unwrap();
        plant(
            temp.path(),
            &["wrapper", "another", "jdk-17", "bin", "javac.exe"],
        );

        assert_eq!(
            find_jdk_root(temp.path()).unwrap(),
            temp.path().join("wrapper").join("another").join("jdk-17")
        );
    }

    #[test]
    fn a_jre_is_not_a_jdk() {
        let temp = TempDir::new().unwrap();
        plant(temp.path(), &["jre-21", "bin", "java.exe"]);

        let err = find_jdk_root(temp.path()).unwrap_err();
        assert!(err.to_string().contains("JRE"), "{err}");
    }

    #[test]
    fn javac_outside_a_bin_directory_is_not_a_marker() {
        let temp = TempDir::new().unwrap();
        plant(temp.path(), &["docs", "javac.exe"]);

        assert!(find_jdk_root(temp.path()).is_err());
    }

    #[test]
    fn prefers_the_shallowest_jdk() {
        let temp = TempDir::new().unwrap();
        plant(temp.path(), &["outer", "bin", "javac.exe"]);
        plant(temp.path(), &["outer", "nested", "jdk", "bin", "javac.exe"]);

        assert_eq!(
            find_jdk_root(temp.path()).unwrap(),
            temp.path().join("outer")
        );
    }

    #[test]
    fn marker_match_is_case_insensitive() {
        let temp = TempDir::new().unwrap();
        plant(temp.path(), &["jdk", "BIN", "JAVAC.EXE"]);

        assert_eq!(find_jdk_root(temp.path()).unwrap(), temp.path().join("jdk"));
    }
}
