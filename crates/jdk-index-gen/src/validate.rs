//! Post-generation gate, run before any output is accepted:
//!
//! 1. [`tree`] — the staged directory deserializes with the CLIENT's own
//!    types (schema conformance for free), every `index.json` entry matches
//!    the file on disk (size and sha256), every package is well-formed, and
//!    every required vendor has at least one windows-x64 package.
//! 2. [`shrink_guard`]: against the published index, fail on a >`max_shrink`%
//!    package-count drop, warn above [`WARN_SHRINK`]%. A missing published
//!    index (404 or unreachable — the first run ever) skips the guard with a
//!    warning.

use jdk_core::error::{Error, Result};
use jdk_core::http::Http;
use jdk_core::index::{IndexFile, Package, safe_path_segments};
use jdk_resolve::version::Version;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

pub const WARN_SHRINK: f64 = 5.0;

/// Package counts keyed by `windows-<arch>/<vendor>.json` path.
pub struct Counts {
    pub by_file: BTreeMap<String, usize>,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.by_file.values().sum()
    }
}

pub fn tree(out_dir: &Path, required_vendors: &[&str]) -> Result<Counts> {
    let index_path = out_dir.join("index.json");
    let bytes = fs::read(&index_path).map_err(Error::io("read", &index_path))?;
    let index = IndexFile::parse(&bytes)?;

    let mut by_file = BTreeMap::new();
    for entry in &index.files {
        let segments = safe_path_segments(&entry.path)?;
        let file_path = segments
            .iter()
            .fold(out_dir.to_path_buf(), |path, segment| path.join(segment));
        let body = fs::read(&file_path).map_err(Error::io("read", &file_path))?;
        if body.len() as u64 != entry.size {
            return Err(Error::Catalog(format!(
                "{}: size mismatch: index says {}, file is {}",
                entry.path,
                entry.size,
                body.len()
            )));
        }
        let actual = jdk_core::download::sha256_hex(&body);
        if actual != entry.sha256 {
            return Err(Error::Checksum {
                subject: entry.path.clone(),
                expected: entry.sha256.clone(),
                actual,
            });
        }
        let packages: Vec<Package> = serde_json::from_slice(&body).map_err(|err| {
            Error::Catalog(format!("{}: unparseable platform file: {err}", entry.path))
        })?;
        for package in &packages {
            well_formed(&entry.path, entry, package)?;
        }
        by_file.insert(entry.path.clone(), packages.len());
    }

    for vendor in required_vendors {
        let path = format!("windows-x64/{vendor}.json");
        if !by_file.get(&path).is_some_and(|count| *count > 0) {
            return Err(Error::Catalog(format!(
                "no windows-x64 packages for required vendor {vendor}"
            )));
        }
    }
    Ok(Counts { by_file })
}

fn well_formed(path: &str, entry: &jdk_core::index::IndexEntry, package: &Package) -> Result<()> {
    let complain = |what: &str| {
        Err(Error::Catalog(format!(
            "{path}: package {}@{}: {what}",
            package.vendor, package.version
        )))
    };
    if package.tool != "java" {
        return complain("tool is not java");
    }
    if package.vendor != entry.vendor || package.os != entry.os || package.arch != entry.arch {
        return complain("vendor/os/arch disagree with the index entry");
    }
    if package.version.parse::<Version>().is_err() {
        return complain("unparseable version");
    }
    if package.sha256.len() != 64 || !package.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return complain("sha256 is not 64 hex chars");
    }
    if package.url.is_empty() {
        return complain("empty url");
    }
    Ok(())
}

/// Where `--compare-to` points: the published index (URL), a local tree, or
/// nowhere (`none` — guard off, e.g. hermetic runs that assert on it).
pub enum CompareTo {
    Url(String),
    Dir(std::path::PathBuf),
    None,
}

impl CompareTo {
    /// `none` → off; anything with `://` → URL; otherwise a local path.
    pub fn parse(text: &str) -> CompareTo {
        if text.eq_ignore_ascii_case("none") {
            CompareTo::None
        } else if text.contains("://") {
            CompareTo::Url(text.trim_end_matches('/').to_string())
        } else {
            CompareTo::Dir(std::path::PathBuf::from(text))
        }
    }
}

/// Global AND per-file: a vendor collapsing inside an otherwise healthy
/// total is exactly the failure a coarse global count waves through.
pub fn shrink_guard(published: Option<&Published>, counts: &Counts, max_shrink: f64) -> Result<()> {
    let Some(published) = published else {
        return Ok(());
    };
    check_shrink("index", published.total(), counts.total(), max_shrink)?;
    for (path, old) in &published.count_by_file {
        let new = counts.by_file.get(path).copied().unwrap_or(0);
        check_shrink(path, *old, new, max_shrink)?;
    }
    Ok(())
}

fn check_shrink(what: &str, old: usize, new: usize, max_shrink: f64) -> Result<()> {
    if old == 0 || new >= old {
        println!("shrink guard: {what}: {old} -> {new} packages, ok");
        return Ok(());
    }
    let shrink = ((old - new) as f64 / old as f64) * 100.0;
    if shrink > max_shrink {
        return Err(Error::Catalog(format!(
            "{what} shrank {shrink:.1}% ({old} -> {new} packages), over the {max_shrink}% limit — refusing to publish (foojay outage or filter regression?)"
        )));
    }
    if shrink > WARN_SHRINK {
        eprintln!(
            "warning: {what} shrank {shrink:.1}% ({old} -> {new} packages), within the {max_shrink}% limit"
        );
    } else {
        println!("shrink guard: {what}: {old} -> {new} packages, ok ({shrink:.1}% smaller)");
    }
    Ok(())
}

/// What the currently published index knows: its `updated` stamp (reused
/// when the catalog has not changed, keeping publishes diff-driven),
/// per-file package counts and sha256 (shrink guard, change detection), and
/// the sha256 of every URL it vouches for (checksum reuse — the key that
/// keeps trust-on-first-use downloads a one-time cost).
pub struct Published {
    pub updated: String,
    pub count_by_file: BTreeMap<String, usize>,
    pub sha256_by_file: BTreeMap<String, String>,
    pub sha256_by_url: HashMap<String, String>,
}

impl Published {
    pub fn total(&self) -> usize {
        self.count_by_file.values().sum()
    }

    /// True when `entries` name exactly the published platform files with
    /// byte-identical content — the "nothing really changed" signal that
    /// makes reusing the published `updated` correct.
    pub fn same_catalog<'a>(
        &self,
        entries: impl Iterator<Item = &'a jdk_core::index::IndexEntry>,
    ) -> bool {
        let mut seen = 0;
        for entry in entries {
            seen += 1;
            if self.sha256_by_file.get(&entry.path) != Some(&entry.sha256) {
                return false;
            }
        }
        seen == self.sha256_by_file.len()
    }
}

/// Loads the published index named by `--compare-to`, or `None` when there is
/// nothing to compare against. Only a MISSING published index.json
/// (404/unreachable) is tolerated; once it loads, its platform files must all
/// load too — a partial read would make the comparison base a lie.
pub fn published(http: &Http, compare_to: &CompareTo) -> Result<Option<Published>> {
    match compare_to {
        CompareTo::None => {
            println!("shrink guard: off (--compare-to none)");
            Ok(None)
        }
        CompareTo::Url(base) => {
            let bytes = match fetch_bytes(http, &format!("{base}/index.json")) {
                Ok(bytes) => bytes,
                Err(err) => {
                    eprintln!(
                        "warning: no published index to compare against ({err}); skipping the shrink guard (expected on the very first run)"
                    );
                    return Ok(None);
                }
            };
            load_published(&bytes, |path| fetch_bytes(http, &format!("{base}/{path}"))).map(Some)
        }
        CompareTo::Dir(dir) => {
            let index_path = dir.join("index.json");
            let bytes = match fs::read(&index_path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    eprintln!(
                        "warning: no index at {} ({err}); skipping the shrink guard",
                        index_path.display()
                    );
                    return Ok(None);
                }
            };
            load_published(&bytes, |path| {
                let file = dir.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
                fs::read(&file).map_err(Error::io("read", &file))
            })
            .map(Some)
        }
    }
}

fn load_published(
    index_bytes: &[u8],
    read_file: impl Fn(&str) -> Result<Vec<u8>>,
) -> Result<Published> {
    let index = IndexFile::parse(index_bytes)?;
    let mut count_by_file = BTreeMap::new();
    let mut sha256_by_file = BTreeMap::new();
    let mut sha256_by_url = HashMap::new();
    for entry in &index.files {
        safe_path_segments(&entry.path)?;
        let body = read_file(&entry.path).map_err(|err| {
            Error::Catalog(format!(
                "published index lists {} but it cannot be read ({err}); refusing a partial comparison base",
                entry.path
            ))
        })?;
        let packages: Vec<Package> = serde_json::from_slice(&body).map_err(|err| {
            Error::Catalog(format!(
                "published {}: unparseable platform file: {err}",
                entry.path
            ))
        })?;
        count_by_file.insert(entry.path.clone(), packages.len());
        sha256_by_file.insert(entry.path.clone(), entry.sha256.clone());
        for package in packages {
            sha256_by_url.insert(package.url, package.sha256);
        }
    }
    Ok(Published {
        updated: index.updated,
        count_by_file,
        sha256_by_file,
        sha256_by_url,
    })
}

fn fetch_bytes(http: &Http, url: &str) -> Result<Vec<u8>> {
    let reply = http.get(url, "jdk-index-gen", &[])?;
    if reply.status() != 200 {
        return Err(Error::Http(format!(
            "GET {url} returned {}",
            reply.status()
        )));
    }
    reply.bytes(32 * 1024 * 1024)
}

/// Per-vendor/arch summary lines plus totals, for the Action log.
pub fn report(counts: &Counts, dropped: usize) {
    for (path, count) in &counts.by_file {
        println!("{path}: {count} packages");
    }
    println!("total: {} packages, {dropped} dropped", counts.total());
}
