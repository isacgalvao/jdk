//! HTTP client + catalog construction. `JDK_INDEX` / `JDK_FOOJAY` override
//! the source URLs (local mirrors, hermetic tests); when an override is
//! present the URL policy admits plain-http loopback — the same injection
//! point jdk-core's tests use, with no test-only switch in this binary. The
//! no-override default is strict https-only against the published index.

use crate::fail::Fail;
use jdk_core::catalog::{Catalog, DEFAULT_INDEX_URL};
use jdk_core::foojay;
use jdk_core::http::{Http, UrlPolicy};
use std::env;
use std::path::Path;

pub fn client(root: &Path) -> Result<(Http, Catalog), Fail> {
    let index = env_url("JDK_INDEX");
    let foojay = env_url("JDK_FOOJAY");
    let policy = if index.is_some() || foojay.is_some() {
        UrlPolicy::AllowInsecureLoopback
    } else {
        UrlPolicy::Strict
    };
    let catalog = Catalog::with_urls(
        root,
        index.as_deref().unwrap_or(DEFAULT_INDEX_URL),
        foojay.as_deref().unwrap_or(foojay::DEFAULT_URL),
    );
    let http = Http::new(policy).map_err(Fail::engine)?;
    Ok((http, catalog))
}

/// A URL-override env var (`JDK_INDEX` / `JDK_FOOJAY`), trimmed; empty
/// counts as unset. Shared with doctor's reachability probe.
pub fn env_url(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
