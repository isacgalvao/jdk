//! Catalog resolution chain: fresh cache → remote index (ETag-revalidated) →
//! stale cache (offline tolerance, inside [`crate::cache`]) → live foojay
//! fallback.

use crate::cache::Cache;
use crate::download::sha256_hex;
use crate::error::{Error, Result};
use crate::foojay;
use crate::http::Http;
use crate::index::{IndexEntry, IndexFile, Package, ReleaseStatus, current_platform, is_stable};
use jdk_resolve::selector::{Selector, normalize_vendor};
use jdk_resolve::version::Version;
use std::path::Path;

/// Where the M5 `jdk-index` repository publishes to.
pub const DEFAULT_INDEX_URL: &str = "https://raw.githubusercontent.com/isacgalvao/jdk-index/main";

/// One catalog entry as `jdk available` shows it — enough to pick a version,
/// not enough to download (foojay's listing endpoint has no checksum).
#[derive(Debug, Clone)]
pub struct Available {
    pub vendor: String,
    pub version: String,
    pub lts: bool,
    pub release_status: ReleaseStatus,
}

/// Where [`Catalog::find`] resolved a package from — surfaced so the CLI can
/// tell the user when an install came from the live API rather than the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The static, checksum-verified index.
    Index,
    /// The live foojay Disco API, queried because the index could not answer.
    Foojay,
}

pub struct Catalog {
    cache: Cache,
    index_url: String,
    foojay_url: String,
}

impl Catalog {
    pub fn new(jdk_root: &Path) -> Self {
        Self::with_urls(jdk_root, DEFAULT_INDEX_URL, foojay::DEFAULT_URL)
    }

    pub fn with_urls(jdk_root: &Path, index_url: &str, foojay_url: &str) -> Self {
        Catalog {
            cache: Cache::new(jdk_root),
            index_url: index_url.trim_end_matches('/').to_string(),
            foojay_url: foojay_url.trim_end_matches('/').to_string(),
        }
    }

    /// Best package for `selector` on this platform; selectors without vendor
    /// use `default_vendor`. Ranking is the store's: GA preferred over EA,
    /// then the highest version. Any index failure — unreachable, unknown
    /// vendor, or simply no matching version (the live API may know a release
    /// a day-old index does not) — falls through to foojay, and a total miss
    /// reports both causes. Returns the package with its [`Origin`] so the
    /// caller can tell the user when it came from the live API.
    pub fn find(
        &self,
        http: &Http,
        selector: &Selector,
        default_vendor: &str,
    ) -> Result<(Package, Origin)> {
        let vendor = normalize_vendor(selector.vendor.as_deref().unwrap_or(default_vendor));
        let (os, arch) = current_platform();
        let pattern = &selector.version;

        let from_index = self.find_in_index(http, &vendor, pattern, os, arch);
        match from_index {
            Ok(package) => Ok((package, Origin::Index)),
            Err(index_err) => {
                match foojay::find(http, &self.foojay_url, &vendor, pattern, os, arch) {
                    Ok(package) => Ok((package, Origin::Foojay)),
                    Err(foojay_err) => Err(Error::Catalog(format!(
                        "no installable package for {vendor}@{pattern}\n  index: {index_err}\n  foojay fallback: {foojay_err}"
                    ))),
                }
            }
        }
    }

    fn find_in_index(
        &self,
        http: &Http,
        vendor: &str,
        pattern: &Version,
        os: &str,
        arch: &str,
    ) -> Result<Package> {
        let packages = self.vendor_packages(http, vendor, os, arch)?;
        let candidates = packages
            .into_iter()
            .filter(|p| p.tool == "java" && p.os == os && p.arch == arch)
            .filter_map(|p| {
                // An unparseable version is a contract violation; skip the
                // entry rather than fail every other package in the file.
                let version: Version = p.version.parse().ok()?;
                if !version.matches(pattern) {
                    return None;
                }
                let stable = is_stable(p.release_status, &version);
                Some((version, stable, p))
            })
            .collect();
        pick_best(candidates).ok_or_else(|| {
            Error::Catalog(format!(
                "index has no {vendor} package matching {pattern} for {os}-{arch}"
            ))
        })
    }

    /// Every package the index publishes for `vendor` on `os`/`arch`. The
    /// platform file is verified against the sha256 recorded in `index.json`;
    /// a mismatch evicts and refetches once (index/file skew right after a
    /// generator run heals itself), then fails hard.
    pub fn vendor_packages(
        &self,
        http: &Http,
        vendor: &str,
        os: &str,
        arch: &str,
    ) -> Result<Vec<Package>> {
        let index = self.index(http)?;
        let entry = index
            .files
            .iter()
            .find(|e| e.vendor == vendor && e.os == os && e.arch == arch)
            .ok_or_else(|| {
                Error::Catalog(format!(
                    "index does not cover vendor {vendor} on {os}-{arch}"
                ))
            })?;
        let body = self.verified_platform_file(http, entry)?;
        serde_json::from_slice(&body).map_err(|err| {
            Error::Catalog(format!("{}: unparseable platform file: {err}", entry.path))
        })
    }

    /// Vendors the index publishes for `os`/`arch`, sorted. Index-only: the
    /// live fallback cannot enumerate distributions, so an unreachable index
    /// means no vendor list (callers suggest a vendor-filtered query instead).
    pub fn vendors(&self, http: &Http, os: &str, arch: &str) -> Result<Vec<String>> {
        let index = self.index(http)?;
        let mut vendors: Vec<String> = index
            .files
            .iter()
            .filter(|e| e.os == os && e.arch == arch)
            .map(|e| e.vendor.clone())
            .collect();
        vendors.sort();
        vendors.dedup();
        Ok(vendors)
    }

    /// Everything installable for `vendor` on `os`/`arch` as display data:
    /// index first; an index miss (or an index empty for this platform)
    /// falls through to the live foojay listing, like [`Catalog::find`].
    pub fn available(
        &self,
        http: &Http,
        vendor: &str,
        os: &str,
        arch: &str,
    ) -> Result<Vec<Available>> {
        let from_index = self
            .vendor_packages(http, vendor, os, arch)
            .map(|packages| {
                packages
                    .into_iter()
                    .filter(|p| p.tool == "java" && p.os == os && p.arch == arch)
                    .map(|p| Available {
                        vendor: p.vendor,
                        version: p.version,
                        lts: p.lts,
                        release_status: p.release_status,
                    })
                    .collect::<Vec<Available>>()
            });
        match from_index {
            Ok(list) if !list.is_empty() => Ok(list),
            Ok(_) => foojay::available(http, &self.foojay_url, vendor, os, arch),
            Err(index_err) => match foojay::available(http, &self.foojay_url, vendor, os, arch) {
                Ok(list) => Ok(list),
                Err(foojay_err) => Err(Error::Catalog(format!(
                    "no catalog listing for {vendor}\n  index: {index_err}\n  foojay fallback: {foojay_err}"
                ))),
            },
        }
    }

    fn index(&self, http: &Http) -> Result<IndexFile> {
        let body = self.cache.get(http, &self.index_url, "index.json")?;
        IndexFile::parse(&body)
    }

    fn verified_platform_file(&self, http: &Http, entry: &IndexEntry) -> Result<Vec<u8>> {
        let expected = entry.sha256.trim().to_ascii_lowercase();
        let body = self.cache.get(http, &self.index_url, &entry.path)?;
        if sha256_hex(&body) == expected {
            return Ok(body);
        }
        self.cache.evict(&entry.path)?;
        let body = self.cache.get(http, &self.index_url, &entry.path)?;
        let actual = sha256_hex(&body);
        if actual == expected {
            return Ok(body);
        }
        Err(Error::Checksum {
            subject: format!("{}/{}", self.index_url, entry.path),
            expected,
            actual,
        })
    }
}

/// Shared candidate ranking (index and foojay): stable beats pre-release/EA,
/// ties go to the higher version — the same rule
/// `jdk_resolve::store::best_candidate` applies to installed JDKs.
pub(crate) fn pick_best<T>(candidates: Vec<(Version, bool, T)>) -> Option<T> {
    candidates
        .into_iter()
        .max_by(|(version_a, stable_a, _), (version_b, stable_b, _)| {
            (stable_a, version_a).cmp(&(stable_b, version_b))
        })
        .map(|(_, _, chosen)| chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn pick_best_prefers_stable_over_newer_pre_release() {
        let chosen = pick_best(vec![
            (v("21.0.6-ea"), false, "ea"),
            (v("21.0.5"), true, "ga"),
        ]);
        assert_eq!(chosen, Some("ga"));
    }

    #[test]
    fn pick_best_takes_the_highest_stable() {
        let chosen = pick_best(vec![(v("21.0.4"), true, "old"), (v("21.0.5"), true, "new")]);
        assert_eq!(chosen, Some("new"));
    }

    #[test]
    fn pick_best_accepts_pre_release_when_nothing_stable_matches() {
        let chosen = pick_best(vec![
            (v("22-ea"), false, "ea22"),
            (v("22.0.1-ea"), false, "ea2201"),
        ]);
        assert_eq!(chosen, Some("ea2201"));
    }

    #[test]
    fn pick_best_of_nothing_is_none() {
        assert_eq!(pick_best::<&str>(Vec::new()), None);
    }

    /// Cross-checks that [`pick_best`]'s ranking selects the same version the
    /// installed store's `jdk_resolve::store::best_candidate` would, for
    /// realistic catalog data. The catalog side derives `stable` with the SAME
    /// expression production uses (`release_status == Ga && pre_release.is_none()`
    /// — see [`Catalog::find_in_index`]), NOT a `pre_release`-only shortcut, so a
    /// drift in either ranker surfaces here. The two `stable` rules coincide for
    /// well-formed data because every EA build carries a `-ea` pre-release
    /// component in its version, which is what lets the store — which sees only
    /// the version, never the release status — agree. A small fixed dataset
    /// stands in for a property test.
    #[test]
    fn pick_best_agrees_with_store_best_candidate_across_scenarios() {
        use jdk_resolve::store;
        use std::fs;
        use tempfile::TempDir;

        // (version, release_status) per candidate. EA builds carry a `-ea`
        // component, matching what foojay serves.
        type Ver = (&'static str, ReleaseStatus);
        let scenarios: &[(&[Ver], &str)] = &[
            (
                &[
                    ("21.0.5", ReleaseStatus::Ga),
                    ("21.0.6-ea", ReleaseStatus::Ea),
                ],
                "21",
            ), // stable beats a newer EA
            (
                &[("21.0.4", ReleaseStatus::Ga), ("21.0.5", ReleaseStatus::Ga)],
                "21",
            ), // higher stable wins
            (
                &[
                    ("22-ea", ReleaseStatus::Ea),
                    ("22.0.1-ea", ReleaseStatus::Ea),
                ],
                "22",
            ), // both EA: the higher one wins
            (
                &[
                    ("21.0.4+7", ReleaseStatus::Ga),
                    ("21.0.4+8", ReleaseStatus::Ga),
                ],
                "21.0.4",
            ), // build disambiguates a tie
            (
                &[("17.0.9", ReleaseStatus::Ga), ("21.0.4", ReleaseStatus::Ga)],
                "17",
            ), // prefix selection
        ];

        for (versions, pattern) in scenarios {
            let temp = TempDir::new().unwrap();
            for (version, _) in *versions {
                fs::create_dir_all(
                    store::java_candidates(temp.path()).join(format!("temurin@{version}")),
                )
                .unwrap();
            }
            let selector: Selector = pattern.parse().unwrap();

            let from_store = store::best_candidate(temp.path(), &selector, "temurin")
                .unwrap()
                .map(|candidate| candidate.version);

            let candidates: Vec<(Version, bool, Version)> = versions
                .iter()
                .map(|(s, status)| (v(s), *status))
                .filter(|(version, _)| version.matches(&selector.version))
                .map(|(version, status)| {
                    // The production expression, verbatim (`find_in_index`).
                    let stable = status == ReleaseStatus::Ga && version.pre_release.is_none();
                    (version.clone(), stable, version)
                })
                .collect();
            let from_catalog = pick_best(candidates);

            assert_eq!(
                from_catalog, from_store,
                "pick_best vs best_candidate disagree for pattern {pattern:?} over {versions:?}"
            );
        }
    }
}
