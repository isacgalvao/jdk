//! The index contract: what the `jdk-index` repository publishes and this
//! client consumes. **This module is the source of truth for the M5
//! generator** — field doc comments are normative.
//!
//! Published layout, all files UTF-8 JSON:
//!
//! ```text
//! index.json                   IndexFile — table of contents
//! windows-x64/temurin.json     Vec<Package> — one file per (os-arch, vendor)
//! windows-x64/zulu.json
//! windows-aarch64/temurin.json
//! ```
//!
//! The client caches every file with ETag + TTL and verifies each platform
//! file against the sha256 its index entry records, so the generator must
//! recompute `size`/`sha256` on every publish.

use crate::error::{Error, Result};
use jdk_resolve::version::Version;
use serde::{Deserialize, Serialize};

/// Schema version this client understands. Bumped only for breaking changes;
/// adding optional fields does not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// `index.json` — table of contents of the index repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFile {
    /// Schema version; readers reject values above [`SCHEMA_VERSION`].
    pub version: u32,
    /// RFC 3339 instant of the generator run that produced this index.
    pub updated: String,
    /// One entry per published platform file.
    pub files: Vec<IndexEntry>,
}

impl IndexFile {
    pub fn parse(bytes: &[u8]) -> Result<IndexFile> {
        let index: IndexFile = serde_json::from_slice(bytes)
            .map_err(|err| Error::Catalog(format!("unparseable index.json: {err}")))?;
        if index.version > SCHEMA_VERSION {
            return Err(Error::Catalog(format!(
                "index schema version {} is newer than this client understands ({SCHEMA_VERSION}); update jdk",
                index.version
            )));
        }
        Ok(index)
    }
}

/// One platform file listed by `index.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Path relative to `index.json`, `/`-separated, no `..`, no absolute or
    /// drive prefix: `windows-x64/temurin.json`.
    pub path: String,
    /// Canonical vendor id, lowercase with `_` (the output of
    /// `jdk_resolve::selector::normalize_vendor`): `temurin`, `sap_machine`.
    pub vendor: String,
    /// Operating system: `windows` is the only value in v0.1.
    pub os: String,
    /// CPU architecture: `x64` or `aarch64`.
    pub arch: String,
    /// Size in bytes of the platform file.
    pub size: u64,
    /// Lowercase hex sha256 of the platform file. Mandatory — the client
    /// refuses platform files that do not match it.
    pub sha256: String,
}

/// One installable package inside a platform file (the file is a
/// `Vec<Package>` on the wire).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    /// Tool dimension, multi-tool-ready: always `java` in v0.1.
    pub tool: String,
    /// Canonical vendor id (see [`IndexEntry::vendor`]).
    pub vendor: String,
    /// Java version as `jdk-resolve` parses it (JEP 223 plus vendor
    /// extensions): `21.0.5+11`, `24-ea+2`. NOT the distribution version
    /// (Zulu's `21.38.21`).
    pub version: String,
    /// `windows`.
    pub os: String,
    /// `x64` or `aarch64`.
    pub arch: String,
    /// `ga` or `ea`; the client prefers `ga` unless the selector names a
    /// pre-release explicitly.
    pub release_status: ReleaseStatus,
    /// Long-term-support line (foojay `term_of_support == "lts"`).
    pub lts: bool,
    /// Archive size in bytes as published by the vendor.
    pub size: u64,
    /// Lowercase hex sha256 of the archive. Mandatory: a package without a
    /// checksum must not be published, and this client refuses to download
    /// without one (deserialization fails when the field is missing).
    pub sha256: String,
    /// Direct vendor download URL (foojay `direct_download_uri`), https-only.
    /// Never the ephemeral `ids/<id>/redirect` form — those rotate and would
    /// rot the index between generator runs.
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseStatus {
    Ga,
    Ea,
}

/// Whether a catalog entry is a stable release: GA status *and* no pre-release
/// component in its version. Both axes are checked so an entry mislabeled on
/// either one ranks below a true stable. This is the shared ranking predicate
/// for catalog data (index and foojay); the installed store cannot use it —
/// it records only the version, never the release status — and relies on
/// `version.pre_release.is_none()` alone, which coincides because every EA
/// build carries a `-ea` pre-release (see [`crate::catalog::pick_best`]).
pub fn is_stable(status: ReleaseStatus, version: &Version) -> bool {
    status == ReleaseStatus::Ga && version.pre_release.is_none()
}

/// The `(os, arch)` pair the running binary installs for, in index vocabulary.
pub fn current_platform() -> (&'static str, &'static str) {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x64"
    };
    ("windows", arch)
}

/// Validates an [`IndexEntry::path`]-style relative path and returns its
/// segments. Rejects traversal, absolute paths and drive/stream colons — a
/// hostile index must not be able to write outside the cache directory.
pub fn safe_path_segments(path: &str) -> Result<Vec<&str>> {
    let reject = |why: &str| {
        Err(Error::Security(format!(
            "unsafe path {path:?} in index data: {why}"
        )))
    };
    if path.contains(['\\', ':']) {
        return reject("backslash or colon");
    }
    if path.starts_with('/') {
        return reject("absolute");
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.contains(&"..") {
        return reject("path traversal");
    }
    if segments.iter().any(|s| s.is_empty() || *s == ".") {
        return reject("empty or dot segment");
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wire-format fixture: field names and shapes here are the M5 contract.
    /// Changing this test means changing the generator.
    const INDEX_JSON: &str = r#"{
        "version": 1,
        "updated": "2026-07-17T00:00:00Z",
        "files": [
            {
                "path": "windows-x64/temurin.json",
                "vendor": "temurin",
                "os": "windows",
                "arch": "x64",
                "size": 123,
                "sha256": "aa11"
            }
        ]
    }"#;

    const PACKAGES_JSON: &str = r#"[
        {
            "tool": "java",
            "vendor": "temurin",
            "version": "21.0.5+11",
            "os": "windows",
            "arch": "x64",
            "release_status": "ga",
            "lts": true,
            "size": 200000000,
            "sha256": "cafebabe",
            "url": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/x.zip"
        }
    ]"#;

    #[test]
    fn parses_the_index_fixture() {
        let index = IndexFile::parse(INDEX_JSON.as_bytes()).unwrap();
        assert_eq!(index.version, 1);
        let entry = &index.files[0];
        assert_eq!(entry.path, "windows-x64/temurin.json");
        assert_eq!(entry.vendor, "temurin");
        assert_eq!(entry.sha256, "aa11");
    }

    #[test]
    fn parses_the_packages_fixture() {
        let packages: Vec<Package> = serde_json::from_slice(PACKAGES_JSON.as_bytes()).unwrap();
        let p = &packages[0];
        assert_eq!(p.tool, "java");
        assert_eq!(p.version, "21.0.5+11");
        assert_eq!(p.release_status, ReleaseStatus::Ga);
        assert!(p.lts);
    }

    #[test]
    fn release_status_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&ReleaseStatus::Ga).unwrap(), "\"ga\"");
        assert_eq!(
            serde_json::from_str::<ReleaseStatus>("\"ea\"").unwrap(),
            ReleaseStatus::Ea
        );
    }

    #[test]
    fn package_without_sha256_does_not_parse() {
        // The mandatory-checksum rule is enforced by the schema itself.
        let json = r#"[{
            "tool": "java", "vendor": "temurin", "version": "21",
            "os": "windows", "arch": "x64", "release_status": "ga",
            "lts": true, "size": 1,
            "url": "https://example.com/x.zip"
        }]"#;
        assert!(serde_json::from_slice::<Vec<Package>>(json.as_bytes()).is_err());
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let json = r#"{"version": 2, "updated": "x", "files": []}"#;
        let err = IndexFile::parse(json.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("update jdk"), "{err}");
    }

    #[test]
    fn safe_path_accepts_the_contract_shape() {
        assert_eq!(
            safe_path_segments("windows-x64/temurin.json").unwrap(),
            vec!["windows-x64", "temurin.json"]
        );
        assert_eq!(
            safe_path_segments("index.json").unwrap(),
            vec!["index.json"]
        );
    }

    #[test]
    fn safe_path_rejects_escapes() {
        for path in [
            "../evil",
            "a/../evil",
            "/abs",
            "a\\b",
            "C:/x",
            "a//b",
            "./a",
            "",
        ] {
            assert!(
                safe_path_segments(path).is_err(),
                "{path:?} should be rejected"
            );
        }
    }

    #[test]
    fn current_platform_is_windows() {
        let (os, arch) = current_platform();
        assert_eq!(os, "windows");
        assert!(arch == "x64" || arch == "aarch64");
    }
}
