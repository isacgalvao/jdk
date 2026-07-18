//! `jdk available [filter] [--latest]`: what the catalog can install for
//! this platform, newest first, LTS/EA flagged. The filter is a vendor
//! (`temurin`), a version pattern (`21`) or both (`temurin@21`). Without a
//! vendor the listing needs the index — the live foojay fallback cannot
//! enumerate distributions.

use crate::fail::Fail;
use crate::remote;
use jdk_core::catalog::Available;
use jdk_core::index::{ReleaseStatus, current_platform};
use jdk_resolve::exit;
use jdk_resolve::selector::normalize_vendor;
use jdk_resolve::version::Version;
use std::path::Path;

pub fn run(root: &Path, filter: Option<&str>, latest: bool) -> Result<(), Fail> {
    let filter = Filter::parse(filter)?;
    let (http, catalog) = remote::client(root)?;
    let (os, arch) = current_platform();

    let vendors = match &filter.vendor {
        Some(vendor) => vec![vendor.clone()],
        None => catalog.vendors(&http, os, arch).map_err(|err| {
            Fail::engine(err).hint(
                "the vendor list needs the index; a vendor filter (e.g. `jdk available temurin`) can query the live API instead",
            )
        })?,
    };

    // One broken vendor must not take the listing down: warn and continue.
    // Only when EVERY vendor fails does the first failure become the error.
    let mut rows: Vec<(Version, Available)> = Vec::new();
    let mut first_failure: Option<Fail> = None;
    let mut failed = 0usize;
    for vendor in &vendors {
        let listing = match catalog.available(&http, vendor, os, arch) {
            Ok(listing) => listing,
            Err(err) => {
                eprintln!("jdk: warning: skipping {vendor}: {}", one_line(&err));
                if first_failure.is_none() {
                    first_failure = Some(Fail::engine(err));
                }
                failed += 1;
                continue;
            }
        };
        rows.extend(listing.into_iter().filter_map(|entry| {
            // An unparseable version is a catalog contract violation; skip
            // the entry rather than fail the whole listing.
            let version: Version = entry.version.parse().ok()?;
            match &filter.version {
                Some(pattern) if !version.matches(pattern) => None,
                _ => Some((version, entry)),
            }
        }));
    }
    if failed == vendors.len()
        && let Some(fail) = first_failure
    {
        return Err(fail);
    }

    if latest {
        rows = trim_to_latest(rows);
    }
    // Vendor A→Z, then newest first within a vendor.
    rows.sort_by(|(va, a), (vb, b)| a.vendor.cmp(&b.vendor).then(vb.cmp(va)));

    if rows.is_empty() {
        eprintln!("jdk: nothing in the catalog matches the filter");
        eprintln!("  → `jdk available` (no filter) lists every vendor");
        return Ok(());
    }
    let width = rows
        .iter()
        .map(|(_, entry)| entry.vendor.len() + entry.version.len() + 1)
        .max()
        .unwrap_or(0);
    for (_, entry) in rows {
        let selector = format!("{}@{}", entry.vendor, entry.version);
        let mut flags = Vec::new();
        if entry.lts {
            flags.push("LTS");
        }
        if entry.release_status == ReleaseStatus::Ea {
            flags.push("EA");
        }
        if flags.is_empty() {
            println!("{selector}");
        } else {
            println!("{selector:width$}  {}", flags.join(" "));
        }
    }
    Ok(())
}

/// Engine errors span lines (index cause + fallback cause); a warning wants
/// one scannable line.
fn one_line(err: &jdk_core::Error) -> String {
    err.to_string()
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Best entry of each `vendor` + major line, ranked like everywhere else:
/// stable beats pre-release/EA, then the higher version.
fn trim_to_latest(rows: Vec<(Version, Available)>) -> Vec<(Version, Available)> {
    let mut best: Vec<(Version, Available)> = Vec::new();
    for (version, entry) in rows {
        let major = version.components.first().copied().unwrap_or(0);
        let stable = entry.release_status == ReleaseStatus::Ga && version.pre_release.is_none();
        match best.iter_mut().find(|(v, e)| {
            e.vendor == entry.vendor && v.components.first().copied().unwrap_or(0) == major
        }) {
            Some(slot) => {
                let slot_stable =
                    slot.1.release_status == ReleaseStatus::Ga && slot.0.pre_release.is_none();
                if (stable, &version) > (slot_stable, &slot.0) {
                    *slot = (version, entry);
                }
            }
            None => best.push((version, entry)),
        }
    }
    best
}

struct Filter {
    vendor: Option<String>,
    version: Option<Version>,
}

impl Filter {
    /// `temurin` → vendor; `21` / `21.0.5` → version pattern; `temurin@21`
    /// → both. Anything else is a config error naming the accepted shapes.
    fn parse(text: Option<&str>) -> Result<Filter, Fail> {
        let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
            return Ok(Filter {
                vendor: None,
                version: None,
            });
        };
        match text.split_once('@') {
            Some((vendor, version)) if !vendor.is_empty() && !version.is_empty() => Ok(Filter {
                vendor: Some(normalize_vendor(vendor)),
                version: Some(version.parse().map_err(|_| bad_filter(text))?),
            }),
            Some(_) => Err(bad_filter(text)),
            None => match text.parse::<Version>() {
                Ok(version) => Ok(Filter {
                    vendor: None,
                    version: Some(version),
                }),
                // Not a version: a vendor name, as long as it looks like one.
                Err(_)
                    if text
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
                {
                    Ok(Filter {
                        vendor: Some(normalize_vendor(text)),
                        version: None,
                    })
                }
                Err(_) => Err(bad_filter(text)),
            },
        }
    }
}

fn bad_filter(text: &str) -> Fail {
    Fail::new(exit::CONFIG, format!("invalid filter `{text}`"))
        .hint("filters are a vendor (`temurin`), a version (`21`) or both (`temurin@21`)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(text: &str) -> Filter {
        Filter::parse(Some(text)).unwrap()
    }

    #[test]
    fn parses_the_three_filter_shapes() {
        assert!(Filter::parse(None).unwrap().vendor.is_none());

        let vendor_only = filter("Temurin");
        assert_eq!(vendor_only.vendor.as_deref(), Some("temurin"));
        assert!(vendor_only.version.is_none());

        let version_only = filter("21");
        assert!(version_only.vendor.is_none());
        assert_eq!(version_only.version, Some("21".parse().unwrap()));

        let both = filter("zulu@17.0");
        assert_eq!(both.vendor.as_deref(), Some("zulu"));
        assert_eq!(both.version, Some("17.0".parse().unwrap()));
    }

    #[test]
    fn rejects_garbage_filters() {
        for text in ["@21", "temurin@", "a b", "temurin@banana"] {
            assert!(Filter::parse(Some(text)).is_err(), "{text:?}");
        }
    }

    #[test]
    fn latest_keeps_the_best_of_each_major_preferring_stable() {
        let entry = |vendor: &str, version: &str, status: ReleaseStatus| {
            (
                version.parse::<Version>().unwrap(),
                Available {
                    vendor: vendor.to_string(),
                    version: version.to_string(),
                    lts: false,
                    release_status: status,
                },
            )
        };
        let trimmed = trim_to_latest(vec![
            entry("temurin", "21.0.4", ReleaseStatus::Ga),
            entry("temurin", "21.0.5", ReleaseStatus::Ga),
            entry("temurin", "21.0.6-ea", ReleaseStatus::Ea),
            entry("temurin", "17.0.9", ReleaseStatus::Ga),
            entry("zulu", "21.0.3", ReleaseStatus::Ga),
        ]);
        let mut names: Vec<String> = trimmed
            .iter()
            .map(|(_, e)| format!("{}@{}", e.vendor, e.version))
            .collect();
        names.sort();
        assert_eq!(names, ["temurin@17.0.9", "temurin@21.0.5", "zulu@21.0.3"]);
    }
}
