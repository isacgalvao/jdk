//! Hardened zip extraction: magic-byte check, per-entry path validation that
//! treats `/` and `\` both as separators, a total-decompression ceiling
//! (zip bombs — enforced on actual bytes, not just declared sizes), and
//! symlink entries materialized as copies or skipped — never real symlinks
//! (they need privileges Windows users usually lack) and never a failed
//! install.

use crate::error::{Error, Result};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Ceiling for the total uncompressed size of one archive.
pub const MAX_UNCOMPRESSED: u64 = 4 * 1024 * 1024 * 1024;
/// Ceiling for the entry count of one archive (real JDKs ship 15–30k
/// entries; a central directory bomb declares millions).
pub const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub files: u64,
    pub symlinks_copied: u64,
    pub symlinks_skipped: u64,
}

pub fn extract_zip(archive: &Path, dest: &Path) -> Result<ExtractReport> {
    extract_zip_capped(archive, dest, MAX_UNCOMPRESSED, MAX_ENTRIES)
}

/// `max_bytes` bounds the total uncompressed output and `max_entries` the
/// entry count; the public wrapper passes [`MAX_UNCOMPRESSED`] and
/// [`MAX_ENTRIES`], tests pass tiny values.
pub fn extract_zip_capped(
    archive: &Path,
    dest: &Path,
    max_bytes: u64,
    max_entries: usize,
) -> Result<ExtractReport> {
    check_magic(archive)?;
    let file = File::open(archive).map_err(Error::io("open", archive))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|err| Error::Extract(format!("{}: {err}", archive.display())))?;
    if zip.is_empty() {
        return Err(Error::Extract(format!(
            "{}: archive is empty",
            archive.display()
        )));
    }
    if zip.len() > max_entries {
        return Err(Error::Extract(format!(
            "{}: {} entries exceed the {max_entries}-entry ceiling (zip bomb?)",
            archive.display(),
            zip.len()
        )));
    }
    fs::create_dir_all(dest).map_err(Error::io("create", dest))?;

    let mut report = ExtractReport::default();
    let mut written = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|err| Error::Extract(format!("{}: entry {i}: {err}", archive.display())))?;
        let name = entry.name().to_string();
        let rel = entry_rel_path(&name)?;
        let out = dest.join(&rel);

        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(Error::io("create", &out))?;
            continue;
        }

        // Declared-size early abort; the copy below enforces on actual bytes
        // too, because declared sizes can lie.
        if entry.size() > max_bytes.saturating_sub(written) {
            return Err(zip_bomb(archive, max_bytes));
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(Error::io("create", parent))?;
        }

        if is_symlink_entry(entry.unix_mode()) {
            let mut target = String::new();
            (&mut entry)
                .take(4096)
                .read_to_string(&mut target)
                .map_err(|err| Error::Extract(format!("{name}: unreadable symlink: {err}")))?;
            match symlink_source(&rel, &target) {
                Some(source_rel) if dest.join(&source_rel).is_file() => {
                    fs::copy(dest.join(&source_rel), &out).map_err(Error::io("copy", &out))?;
                    report.symlinks_copied += 1;
                }
                _ => report.symlinks_skipped += 1,
            }
            continue;
        }

        let mut out_file = File::create(&out).map_err(Error::io("create", &out))?;
        let budget = max_bytes.saturating_sub(written).saturating_add(1);
        let copied = io::copy(&mut (&mut entry).take(budget), &mut out_file)
            .map_err(|err| Error::Extract(format!("{name}: {err}")))?;
        written += copied;
        if written > max_bytes {
            drop(out_file);
            let _ = fs::remove_file(&out);
            return Err(zip_bomb(archive, max_bytes));
        }
        report.files += 1;
    }
    Ok(report)
}

fn zip_bomb(archive: &Path, cap: u64) -> Error {
    Error::Extract(format!(
        "{}: uncompressed content exceeds the {cap}-byte ceiling (zip bomb?)",
        archive.display()
    ))
}

/// PK\x03\x04 (local file), PK\x05\x06 (empty central directory, rejected
/// later as empty), PK\x07\x08 (spanned).
fn check_magic(archive: &Path) -> Result<()> {
    let mut file = File::open(archive).map_err(Error::io("open", archive))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(|_| {
        Error::Extract(format!(
            "{}: too short to be a zip archive",
            archive.display()
        ))
    })?;
    if magic[0] == 0x50 && magic[1] == 0x4b && matches!(magic[2], 0x03 | 0x05 | 0x07) {
        Ok(())
    } else {
        Err(Error::Extract(format!(
            "{}: not a zip archive (bad magic bytes)",
            archive.display()
        )))
    }
}

/// Validates one entry name into a safe relative path. Zip entries use `/`;
/// hostile ones may use `\` — both are separators here, so `..\..\evil`
/// cannot slip through a `/`-only check. Rejects absolute paths, drive or
/// alternate-stream colons, traversal, and empty paths.
fn entry_rel_path(name: &str) -> Result<PathBuf> {
    let normalized = name.replace('\\', "/");
    let reject = |why: &str| {
        Err(Error::Security(format!(
            "archive entry {name:?} rejected: {why}"
        )))
    };
    if normalized.starts_with('/') {
        return reject("absolute path");
    }
    if normalized.contains(':') {
        return reject("drive or stream prefix");
    }
    let mut path = PathBuf::new();
    for segment in normalized.split('/') {
        match segment {
            "" | "." => continue,
            ".." => return reject("path traversal"),
            other => path.push(other),
        }
    }
    if path.as_os_str().is_empty() {
        return reject("empty path");
    }
    Ok(path)
}

fn is_symlink_entry(unix_mode: Option<u32>) -> bool {
    unix_mode.is_some_and(|mode| mode & 0o170000 == 0o120000)
}

/// Where a relative symlink points, staying inside the extraction root; None
/// when absolute or escaping — those entries are skipped, never an error (a
/// Windows JDK install must survive stray symlink entries).
fn symlink_source(link_rel: &Path, target: &str) -> Option<PathBuf> {
    let target = target.trim().replace('\\', "/");
    if target.starts_with('/') || target.contains(':') {
        return None;
    }
    let mut segments: Vec<_> = link_rel
        .parent()
        .map(|parent| parent.components().collect())
        .unwrap_or_default();
    for segment in target.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                segments.pop()?;
            }
            other => segments.push(std::path::Component::Normal(other.as_ref())),
        }
    }
    Some(segments.iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    fn write_zip(path: &Path, build: impl FnOnce(&mut zip::ZipWriter<File>)) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        build(&mut writer);
        writer.finish().unwrap();
    }

    fn add_file(writer: &mut zip::ZipWriter<File>, name: &str, content: &[u8]) {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content).unwrap();
    }

    #[test]
    fn extracts_nested_entries() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("a.zip");
        write_zip(&archive, |writer| {
            add_file(writer, "jdk-21/bin/java.exe", b"java");
            add_file(writer, "jdk-21/release", b"JAVA_VERSION=21");
        });

        let dest = temp.path().join("out");
        let report = extract_zip(&archive, &dest).unwrap();

        assert_eq!(report.files, 2);
        assert_eq!(
            fs::read(dest.join("jdk-21").join("bin").join("java.exe")).unwrap(),
            b"java"
        );
    }

    #[test]
    fn traversal_entries_abort_the_extraction() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("evil.zip");
        write_zip(&archive, |writer| {
            add_file(writer, "../evil.txt", b"boom");
        });

        let dest = temp.path().join("inner").join("out");
        fs::create_dir_all(&dest).unwrap();
        let err = extract_zip(&archive, &dest).unwrap_err();

        assert!(matches!(err, Error::Security(_)), "{err}");
        assert!(!temp.path().join("inner").join("evil.txt").exists());
    }

    #[test]
    fn rejects_hostile_entry_names() {
        for name in [
            "../x",
            "..\\x",
            "a/../../x",
            "a\\..\\..\\x",
            "/abs",
            "\\abs",
            "C:\\evil",
            "c:/evil",
            "ads:stream",
            "",
            ".",
        ] {
            assert!(entry_rel_path(name).is_err(), "{name:?} should be rejected");
        }
    }

    #[test]
    fn accepts_normal_entry_names() {
        assert_eq!(
            entry_rel_path("jdk-21/bin/java.exe").unwrap(),
            Path::new("jdk-21").join("bin").join("java.exe")
        );
        // Trailing slash (directory entries) and redundant dots are tolerated.
        assert_eq!(entry_rel_path("jdk-21/").unwrap(), Path::new("jdk-21"));
        assert_eq!(entry_rel_path("./a/b").unwrap(), Path::new("a").join("b"));
    }

    #[test]
    fn caps_total_uncompressed_size() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("bomb.zip");
        write_zip(&archive, |writer| {
            add_file(writer, "a.bin", &[0u8; 600]);
            add_file(writer, "b.bin", &[0u8; 600]);
        });

        let err =
            extract_zip_capped(&archive, &temp.path().join("out"), 1000, MAX_ENTRIES).unwrap_err();
        assert!(err.to_string().contains("zip bomb"), "{err}");
    }

    #[test]
    fn caps_the_entry_count() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("many.zip");
        write_zip(&archive, |writer| {
            for i in 0..5 {
                add_file(writer, &format!("f{i}.txt"), b"x");
            }
        });

        let err = extract_zip_capped(&archive, &temp.path().join("out"), MAX_UNCOMPRESSED, 3)
            .unwrap_err();
        assert!(err.to_string().contains("entries exceed"), "{err}");

        // The same archive extracts fine under the real ceiling.
        extract_zip(&archive, &temp.path().join("ok")).unwrap();
    }

    #[test]
    fn symlink_entries_are_copied_when_the_target_exists() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("links.zip");
        write_zip(&archive, |writer| {
            add_file(writer, "jdk/bin/java.exe", b"real java");
            writer
                .add_symlink(
                    "jdk/bin/java-link",
                    "java.exe",
                    SimpleFileOptions::default(),
                )
                .unwrap();
        });

        let dest = temp.path().join("out");
        let report = extract_zip(&archive, &dest).unwrap();

        assert_eq!(report.symlinks_copied, 1);
        assert_eq!(
            fs::read(dest.join("jdk").join("bin").join("java-link")).unwrap(),
            b"real java"
        );
    }

    #[test]
    fn escaping_or_absolute_symlinks_are_skipped_not_fatal() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("links.zip");
        write_zip(&archive, |writer| {
            add_file(writer, "jdk/bin/java.exe", b"java");
            writer
                .add_symlink(
                    "jdk/escape",
                    "../../../etc/passwd",
                    SimpleFileOptions::default(),
                )
                .unwrap();
            writer
                .add_symlink("jdk/abs", "C:\\Windows\\evil", SimpleFileOptions::default())
                .unwrap();
        });

        let dest = temp.path().join("out");
        let report = extract_zip(&archive, &dest).unwrap();

        assert_eq!(report.symlinks_skipped, 2);
        assert_eq!(report.files, 1);
        assert!(!dest.join("jdk").join("escape").exists());
    }

    #[test]
    fn refuses_non_zip_files() {
        let temp = TempDir::new().unwrap();
        let fake = temp.path().join("fake.zip");
        fs::write(&fake, b"MZ this is not a zip").unwrap();

        let err = extract_zip(&fake, &temp.path().join("out")).unwrap_err();
        assert!(err.to_string().contains("magic"), "{err}");

        fs::write(&fake, b"PK").unwrap();
        let err = extract_zip(&fake, &temp.path().join("out")).unwrap_err();
        assert!(err.to_string().contains("too short"), "{err}");
    }

    #[test]
    fn symlink_source_resolves_relative_targets() {
        let link = Path::new("jdk").join("bin").join("link");
        assert_eq!(
            symlink_source(&link, "java.exe"),
            Some(Path::new("jdk").join("bin").join("java.exe"))
        );
        assert_eq!(
            symlink_source(&link, "../lib/x.dll"),
            Some(Path::new("jdk").join("lib").join("x.dll"))
        );
        assert_eq!(symlink_source(&link, "../../../escape"), None);
        assert_eq!(symlink_source(&link, "/absolute"), None);
        assert_eq!(symlink_source(&link, "C:\\x"), None);
    }
}
