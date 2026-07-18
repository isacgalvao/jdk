//! `jdk-index-gen`: generates the tree the `jdk-index` repository publishes —
//! `index.json` plus `windows-<arch>/<vendor>.json` — from the foojay Disco
//! API, then validates it (schema, required-vendor floor, shrink guard)
//! before the caller may publish. The daily Action in `.github/workflows/
//! index.yml` is its only production caller.
//!
//! windows-x64 is mandatory for every vendor in [`VENDORS`]; windows-aarch64
//! is best-effort — published for whichever vendors have data, never fatal.

mod fetch;
mod output;
mod validate;

use clap::Parser;
use jdk_core::error::Result;
use jdk_core::foojay;
use jdk_core::http::{Http, UrlPolicy};
use output::PlatformFile;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use validate::CompareTo;

/// The vendors the v0.1 index must cover (decision 6): each must yield at
/// least one windows-x64 package or the run fails. Foojay distribution ids;
/// `graalvm` is Oracle GraalVM (`graalvm_community` is a different id).
const VENDORS: [&str; 6] = [
    "temurin",
    "zulu",
    "corretto",
    "liberica",
    "graalvm",
    "microsoft",
];
const ARCHES: [&str; 2] = ["x64", "aarch64"];

#[derive(Parser)]
#[command(
    name = "jdk-index-gen",
    version,
    about = "Generate the jdk-index tree from the foojay Disco API"
)]
struct Cli {
    /// Output directory (created if missing)
    #[arg(long)]
    out: PathBuf,

    /// Published index to guard against shrinkage: URL, local directory,
    /// or `none`
    #[arg(long, default_value = jdk_core::catalog::DEFAULT_INDEX_URL)]
    compare_to: String,

    /// Package-count shrink (%) beyond which the run fails
    #[arg(long, default_value_t = 15.0)]
    max_shrink: f64,

    /// foojay Disco API base URL; overriding it admits plain-http loopback
    /// (hermetic tests)
    #[arg(long, default_value = foojay::DEFAULT_URL)]
    foojay: String,

    /// Recorded verbatim as `updated` in index.json (default: now, UTC);
    /// injecting it makes runs byte-reproducible
    #[arg(long)]
    updated: Option<String>,

    /// Parallel ids/<id> requests
    #[arg(long, default_value_t = 8)]
    jobs: usize,

    /// Max trust-on-first-use archive downloads per vendor+arch, for
    /// packages foojay has no sha256 for (default: unlimited; dropped
    /// packages backfill on later runs)
    #[arg(long)]
    hash_budget: Option<u32>,
}

fn main() -> ExitCode {
    match run(&Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jdk-index-gen: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    // Same override rule as the jdk CLI's JDK_INDEX/JDK_FOOJAY: only a
    // non-default source may be plain-http loopback; the real API and the
    // published URLs stay strictly https.
    let policy = if cli.foojay == foojay::DEFAULT_URL {
        UrlPolicy::Strict
    } else {
        UrlPolicy::AllowInsecureLoopback
    };
    let http = Http::new(policy)?;
    let updated = cli.updated.clone().unwrap_or_else(rfc3339_now);

    // Loaded up front: the shrink-guard baseline doubles as the sha256-reuse
    // table that keeps trust-on-first-use downloads a one-time cost.
    let published = validate::published(&http, &CompareTo::parse(&cli.compare_to))?;

    let mut platforms = Vec::new();
    let mut dropped = 0;
    for arch in ARCHES {
        for vendor in VENDORS {
            let fetched = fetch::vendor_packages(
                &http,
                &cli.foojay,
                vendor,
                arch,
                cli.jobs,
                published.as_ref(),
                cli.hash_budget,
            )?;
            dropped += fetched.dropped;
            if fetched.packages.is_empty() {
                // x64 emptiness becomes a hard error in validate::tree; the
                // aarch64 platform is best-effort by design.
                println!("windows-{arch}/{vendor}: no packages");
                continue;
            }
            platforms.push(PlatformFile {
                vendor: vendor.to_string(),
                arch: arch.to_string(),
                packages: fetched.packages,
            });
        }
    }

    let files = output::build(&platforms)?;
    // An unchanged catalog keeps the published `updated`, so index.json
    // comes out byte-identical and the workflow's commit-only-on-diff
    // genuinely publishes nothing.
    let updated = match &published {
        Some(published) if published.same_catalog(files.iter().map(|(entry, _)| entry)) => {
            println!(
                "catalog unchanged since {}; keeping that stamp",
                published.updated
            );
            published.updated.clone()
        }
        _ => updated,
    };
    output::write(&cli.out, &updated, files)?;
    let counts = validate::tree(&cli.out, &VENDORS)?;
    validate::report(&counts, dropped);
    validate::shrink_guard(published.as_ref(), &counts, cli.max_shrink)?;
    println!("index staged at {} (updated {updated})", cli.out.display());
    Ok(())
}

fn rfc3339_now() -> String {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    rfc3339_utc(epoch)
}

/// RFC 3339 UTC from epoch seconds; date part via Howard Hinnant's
/// civil-from-days algorithm.
fn rfc3339_utc(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let rem = epoch_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        // date -u -d "2024-02-29 12:30:45" +%s
        assert_eq!(rfc3339_utc(1_709_209_845), "2024-02-29T12:30:45Z");
        // date -u -d "2026-07-17 00:00:00" +%s
        assert_eq!(rfc3339_utc(1_784_246_400), "2026-07-17T00:00:00Z");
        assert_eq!(rfc3339_utc(946_684_799), "1999-12-31T23:59:59Z");
    }
}
