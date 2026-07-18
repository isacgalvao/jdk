//! Deterministic assembly and writing of the index tree: same packages plus
//! same `--updated` in, byte-identical files out, so the published repo only
//! diffs when the catalog really changed.
//!
//! Layout (the contract in `jdk_core::index`):
//!
//! ```text
//! <out>/index.json                 IndexFile
//! <out>/windows-x64/<vendor>.json  Vec<Package>, newest first
//! <out>/windows-aarch64/...        only when the vendor has aarch64 data
//! ```

use jdk_core::download::sha256_hex;
use jdk_core::error::{Error, Result};
use jdk_core::index::{IndexEntry, IndexFile, Package, SCHEMA_VERSION};
use std::fs;
use std::path::Path;

/// One `windows-<arch>/<vendor>.json` to publish. Assembly inputs arrive
/// already sorted (vendor × arch loop order + `fetch`'s package order).
pub struct PlatformFile {
    pub vendor: String,
    pub arch: String,
    pub packages: Vec<Package>,
}

/// Serializes every platform file and computes its index entry, sorted by
/// path — separate from [`write`] so the caller can compare the entries
/// against the published index (and reuse its `updated`) before anything
/// touches disk.
pub fn build(platforms: &[PlatformFile]) -> Result<Vec<(IndexEntry, Vec<u8>)>> {
    let mut files = Vec::new();
    for platform in platforms {
        let path = format!("windows-{}/{}.json", platform.arch, platform.vendor);
        let body = stable_json(&platform.packages)?;
        files.push((
            IndexEntry {
                path,
                vendor: platform.vendor.clone(),
                os: "windows".to_string(),
                arch: platform.arch.clone(),
                size: body.len() as u64,
                sha256: sha256_hex(&body),
            },
            body,
        ));
    }
    files.sort_by(|(a, _), (b, _)| a.path.cmp(&b.path));
    Ok(files)
}

pub fn write(out_dir: &Path, updated: &str, files: Vec<(IndexEntry, Vec<u8>)>) -> Result<()> {
    for (entry, body) in &files {
        let dest = out_dir.join(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(Error::io("create", parent))?;
        }
        fs::write(&dest, body).map_err(Error::io("write", &dest))?;
    }

    let index = IndexFile {
        version: SCHEMA_VERSION,
        updated: updated.to_string(),
        files: files.into_iter().map(|(entry, _)| entry).collect(),
    };
    let dest = out_dir.join("index.json");
    fs::create_dir_all(out_dir).map_err(Error::io("create", out_dir))?;
    fs::write(&dest, stable_json(&index)?).map_err(Error::io("write", &dest))
}

/// Pretty JSON + trailing newline: serde keeps struct field order, so the
/// bytes are a pure function of the data — and day-to-day git diffs stay
/// line-per-fact readable.
fn stable_json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut body =
        serde_json::to_vec_pretty(value).map_err(|err| Error::Catalog(err.to_string()))?;
    body.push(b'\n');
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdk_core::index::ReleaseStatus;

    fn package(version: &str) -> Package {
        Package {
            tool: "java".to_string(),
            vendor: "temurin".to_string(),
            version: version.to_string(),
            os: "windows".to_string(),
            arch: "x64".to_string(),
            release_status: ReleaseStatus::Ga,
            lts: true,
            size: 1,
            sha256: "aa".repeat(32),
            url: "https://vendor.example/a.zip".to_string(),
        }
    }

    #[test]
    fn writes_the_contract_layout_with_matching_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let platforms = [PlatformFile {
            vendor: "temurin".to_string(),
            arch: "x64".to_string(),
            packages: vec![package("21.0.5+11")],
        }];
        let files = build(&platforms).unwrap();
        write(dir.path(), "2026-07-17T00:00:00Z", files).unwrap();

        let index_bytes = fs::read(dir.path().join("index.json")).unwrap();
        let index = IndexFile::parse(&index_bytes).unwrap();
        assert_eq!(index.version, SCHEMA_VERSION);
        assert_eq!(index.updated, "2026-07-17T00:00:00Z");
        assert_eq!(index.files.len(), 1);

        let entry = &index.files[0];
        assert_eq!(entry.path, "windows-x64/temurin.json");
        let body = fs::read(dir.path().join("windows-x64").join("temurin.json")).unwrap();
        assert_eq!(entry.size, body.len() as u64);
        assert_eq!(entry.sha256, sha256_hex(&body));
        assert!(body.ends_with(b"\n"));
    }
}
