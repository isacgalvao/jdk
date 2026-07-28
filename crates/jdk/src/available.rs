//! `jdk available [filter] [--latest] [--ea]`: what the catalog can install
//! for this platform, newest first, LTS/EA flagged. The filter is a vendor
//! (`temurin`), a version pattern (`21`) or both (`temurin@21`). Early-access
//! builds are hidden unless `--ea` — or unless the filter itself asks for
//! them, so that what can be listed and what can be installed stay the same
//! set. Without a vendor the listing needs the index — the live foojay
//! fallback cannot enumerate distributions.

use crate::fail::Fail;
use crate::remote;
use jdk_core::catalog::{Available, Catalog};
use jdk_core::http::Http;
use jdk_core::index::{ReleaseStatus, current_platform, is_stable};
use jdk_resolve::exit;
use jdk_resolve::selector::normalize_vendor;
use jdk_resolve::version::Version;
use std::path::Path;

pub fn run(root: &Path, filter: Option<&str>, latest: bool, ea: bool) -> Result<(), Fail> {
    let requested = filter.unwrap_or("").trim().to_string();
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

    // A filter that names a pre-release IS the request for early access, and
    // `jdk install temurin@27-ea` needs no flag either — requiring one here
    // would only guard the display of something already installable.
    let mut ea = ea || filter.names_pre_release();
    let mut listing = collect(&catalog, &http, &vendors, &filter, ea, true)?;

    // An explicit version with no stable match may still exist as early
    // access: `jdk install` resolves it and names its selector, so refusing
    // to show it would leave the listing claiming it does not exist. The
    // retry stays quiet — the first pass already warned about broken vendors.
    if listing.rows.is_empty() && !ea && filter.version.is_some() {
        let widened = collect(&catalog, &http, &vendors, &filter, true, false)?;
        if !widened.rows.is_empty() {
            eprintln!("jdk: only early-access builds match {requested}");
            listing = widened;
            ea = true;
        }
    }

    let mut rows = listing.rows;
    if latest {
        rows = trim_to_latest(rows, ea);
    }
    // Vendor A→Z, then newest first within a vendor.
    rows.sort_by(|(va, a), (vb, b)| a.vendor.cmp(&b.vendor).then(vb.cmp(va)));

    if rows.is_empty() {
        eprintln!("jdk: nothing in the catalog matches the filter");
        // Most vendors publish no early access at all, so `--ea` adds
        // nothing for them; unexplained, that reads as a broken flag.
        if ea && !listing.saw_ea {
            let subject = filter.vendor.as_deref().unwrap_or("the catalog");
            eprintln!("  → {subject} has no early-access builds for {os}-{arch}");
        }
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

/// One pass over `vendors` with the version filter applied, plus whether the
/// catalog carried ANY early-access build for them — which is what lets an
/// empty result explain itself instead of blaming the filter.
struct Listing {
    rows: Vec<(Version, Available)>,
    saw_ea: bool,
}

/// Reads every vendor's listing. One broken vendor must not take the listing
/// down: it is warned about (when `warn`) and skipped, and only when EVERY
/// vendor fails does the first failure become the error.
fn collect(
    catalog: &Catalog,
    http: &Http,
    vendors: &[String],
    filter: &Filter,
    include_ea: bool,
    warn: bool,
) -> Result<Listing, Fail> {
    let (os, arch) = current_platform();
    let mut listing = Listing {
        rows: Vec::new(),
        saw_ea: false,
    };
    let mut first_failure: Option<Fail> = None;
    let mut failed = 0usize;
    for vendor in vendors {
        let entries = match catalog.available(http, vendor, os, arch, include_ea) {
            Ok(entries) => entries,
            Err(err) => {
                if warn {
                    eprintln!("jdk: warning: skipping {vendor}: {}", one_line(&err));
                }
                if first_failure.is_none() {
                    first_failure = Some(Fail::engine(err));
                }
                failed += 1;
                continue;
            }
        };
        listing.saw_ea |= entries
            .iter()
            .any(|entry| entry.release_status == ReleaseStatus::Ea);
        listing.rows.extend(entries.into_iter().filter_map(|entry| {
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
    Ok(listing)
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
/// stable beats pre-release/EA, then the higher version. `split_status` —
/// set while early access is on display — adds the release status to the
/// grouping key, so each line keeps its best GA *and* its best EA. Sharing
/// one slot instead lets every stable line swallow its own preview, which
/// is what left `--ea --latest` showing early access only for the majors
/// that have no stable build at all.
fn trim_to_latest(
    rows: Vec<(Version, Available)>,
    split_status: bool,
) -> Vec<(Version, Available)> {
    let mut best: Vec<(Version, Available)> = Vec::new();
    for (version, entry) in rows {
        let major = version.components.first().copied().unwrap_or(0);
        let stable = is_stable(entry.release_status, &version);
        match best.iter_mut().find(|(v, e)| {
            e.vendor == entry.vendor
                && v.components.first().copied().unwrap_or(0) == major
                && (!split_status || e.release_status == entry.release_status)
        }) {
            Some(slot) => {
                let slot_stable = is_stable(slot.1.release_status, &slot.0);
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
    /// Whether the filter asks for a pre-release by name (`temurin@27-ea`),
    /// which the listing reads as `--ea` — see [`run`].
    fn names_pre_release(&self) -> bool {
        self.version
            .as_ref()
            .is_some_and(|version| version.pre_release.is_some())
    }

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

    fn entry(vendor: &str, version: &str, status: ReleaseStatus) -> (Version, Available) {
        (
            version.parse::<Version>().unwrap(),
            Available {
                vendor: vendor.to_string(),
                version: version.to_string(),
                lts: false,
                release_status: status,
            },
        )
    }

    fn sorted_names(rows: &[(Version, Available)]) -> Vec<String> {
        let mut names: Vec<String> = rows
            .iter()
            .map(|(_, e)| format!("{}@{}", e.vendor, e.version))
            .collect();
        names.sort();
        names
    }

    fn sample_rows() -> Vec<(Version, Available)> {
        vec![
            entry("temurin", "21.0.4", ReleaseStatus::Ga),
            entry("temurin", "21.0.5", ReleaseStatus::Ga),
            entry("temurin", "21.0.6-ea", ReleaseStatus::Ea),
            entry("temurin", "17.0.9", ReleaseStatus::Ga),
            entry("zulu", "21.0.3", ReleaseStatus::Ga),
        ]
    }

    #[test]
    fn latest_keeps_the_best_of_each_major_preferring_stable() {
        let trimmed = trim_to_latest(sample_rows(), false);
        assert_eq!(
            sorted_names(&trimmed),
            ["temurin@17.0.9", "temurin@21.0.5", "zulu@21.0.3"]
        );
    }

    /// With early access on display, `--latest` keeps the best of each
    /// status: the 21 line shows its newest GA *and* its newest preview,
    /// instead of the GA swallowing the very builds `--ea` asked for.
    #[test]
    fn latest_with_early_access_keeps_one_of_each_status() {
        let trimmed = trim_to_latest(sample_rows(), true);
        assert_eq!(
            sorted_names(&trimmed),
            [
                "temurin@17.0.9",
                "temurin@21.0.5",
                "temurin@21.0.6-ea",
                "zulu@21.0.3"
            ]
        );
    }

    #[test]
    fn a_pre_release_filter_asks_for_early_access_by_itself() {
        assert!(filter("temurin@27-ea").names_pre_release());
        assert!(filter("27-ea").names_pre_release());
        assert!(!filter("temurin@27").names_pre_release());
        assert!(!filter("temurin").names_pre_release());
        assert!(!Filter::parse(None).unwrap().names_pre_release());
    }
}
