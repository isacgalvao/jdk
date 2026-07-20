//! Live foojay Disco API fallback, used only when the static-index chain
//! cannot answer. The queries are the exact Windows foojay filters: GA-only,
//! zip, `c_std_lib`, with the `amd64,x64` / `arm64,aarch64` architecture
//! aliasing. Checksums exist only in the `ids/<id>` detail endpoint — never
//! in the listing — and the URL used is `direct_download_uri`; the
//! `ids/<id>/redirect` form is ephemeral.

use crate::catalog::{Available, pick_best};
use crate::error::{Error, Result};
use crate::http::{Http, MAX_BODY};
use crate::index::{Package, ReleaseStatus};
use jdk_resolve::version::Version;
use serde::Deserialize;

pub const DEFAULT_URL: &str = "https://api.foojay.io/disco/v3.0";

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    result: Vec<T>,
}

/// `packages` listing item — no checksum here by API design.
#[derive(Debug, Deserialize)]
struct Listing {
    id: String,
    java_version: String,
    #[serde(default)]
    term_of_support: Option<String>,
    #[serde(default)]
    release_status: Option<String>,
    #[serde(default)]
    size: u64,
}

/// `ids/<id>` detail — the only place foojay serves a checksum.
#[derive(Debug, Deserialize)]
struct Details {
    direct_download_uri: String,
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    checksum_type: String,
}

/// Display listing for `vendor` — version/LTS/EA only, GA-only by query
/// design. The listing endpoint carries no checksum, so nothing returned
/// here can be downloaded; resolution for install stays in [`find`].
pub fn available(
    http: &Http,
    base_url: &str,
    vendor: &str,
    os: &str,
    arch: &str,
) -> Result<Vec<Available>> {
    let url = packages_url(base_url, vendor, os, arch);
    let listing: Envelope<Listing> = fetch_json(http, &url)?;
    Ok(listing
        .result
        .into_iter()
        .map(|pkg| Available {
            vendor: vendor.to_string(),
            version: pkg.java_version,
            lts: pkg.term_of_support.as_deref() == Some("lts"),
            release_status: release_status(pkg.release_status.as_deref()),
        })
        .collect())
}

/// Best GA package for `vendor` matching `pattern`, fully resolved (direct
/// URL + sha256). A missing or non-sha256 checksum is a hard error — an
/// unverifiable archive is never downloaded.
pub fn find(
    http: &Http,
    base_url: &str,
    vendor: &str,
    pattern: &Version,
    os: &str,
    arch: &str,
) -> Result<Package> {
    let url = packages_url(base_url, vendor, os, arch);
    let listing: Envelope<Listing> = fetch_json(http, &url)?;

    let mut candidates = Vec::new();
    for pkg in listing.result {
        let Ok(version) = pkg.java_version.parse::<Version>() else {
            continue;
        };
        if !version.matches(pattern) {
            continue;
        }
        let stable = pkg.release_status.as_deref() != Some("ea") && version.pre_release.is_none();
        candidates.push((version, stable, pkg));
    }
    let Some(chosen) = pick_best(candidates) else {
        return Err(Error::Catalog(format!(
            "foojay has no {vendor} package matching {pattern} for {os}/{arch}"
        )));
    };

    let details_url = format!("{}/ids/{}", base_url.trim_end_matches('/'), chosen.id);
    let details: Envelope<Details> = fetch_json(http, &details_url)?;
    let Some(details) = details.result.into_iter().next() else {
        return Err(Error::Catalog(format!(
            "foojay returned no details for package id {}",
            chosen.id
        )));
    };
    if !details.checksum_type.eq_ignore_ascii_case("sha256") || details.checksum.trim().is_empty() {
        return Err(Error::Security(format!(
            "foojay provided no sha256 for {vendor}@{} (checksum_type {:?}); refusing an unverifiable download",
            chosen.java_version, details.checksum_type
        )));
    }

    Ok(Package {
        tool: "java".to_string(),
        vendor: vendor.to_string(),
        version: chosen.java_version,
        os: os.to_string(),
        arch: arch.to_string(),
        release_status: release_status(chosen.release_status.as_deref()),
        lts: chosen.term_of_support.as_deref() == Some("lts"),
        size: chosen.size,
        sha256: details.checksum.trim().to_ascii_lowercase(),
        url: details.direct_download_uri,
    })
}

fn fetch_json<T: serde::de::DeserializeOwned>(http: &Http, url: &str) -> Result<T> {
    let reply = http.get(url, "foojay", &[])?;
    if reply.status() != 200 {
        return Err(Error::Http(format!(
            "GET {url} returned {}",
            reply.status()
        )));
    }
    serde_json::from_slice(&reply.bytes(MAX_BODY)?)
        .map_err(|err| Error::Catalog(format!("unparseable foojay response from {url}: {err}")))
}

/// Maps foojay's `release_status` field to [`ReleaseStatus`]: only the
/// explicit `"ea"` marker is early-access; everything else is treated as GA.
fn release_status(raw: Option<&str>) -> ReleaseStatus {
    match raw {
        Some("ea") => ReleaseStatus::Ea,
        _ => ReleaseStatus::Ga,
    }
}

/// The exact Windows foojay query, parameterized by vendor.
fn packages_url(base_url: &str, vendor: &str, os: &str, arch: &str) -> String {
    let arch_param = match arch {
        "x64" => "amd64,x64",
        "aarch64" => "arm64,aarch64",
        other => other,
    };
    format!(
        "{}/packages?operating_system={os}&architecture={arch_param}&archive_type=zip&lib_c_type=c_std_lib&package_type=jdk&release_status=ga&distribution={vendor}",
        base_url.trim_end_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_query_is_exact() {
        let url = packages_url(DEFAULT_URL, "temurin", "windows", "x64");
        assert_eq!(
            url,
            "https://api.foojay.io/disco/v3.0/packages?operating_system=windows&architecture=amd64,x64&archive_type=zip&lib_c_type=c_std_lib&package_type=jdk&release_status=ga&distribution=temurin"
        );
    }

    #[test]
    fn aarch64_query_uses_the_arm_alias() {
        let url = packages_url(DEFAULT_URL, "zulu", "windows", "aarch64");
        assert!(url.contains("architecture=arm64,aarch64"), "{url}");
        assert!(url.contains("distribution=zulu"), "{url}");
    }
}
