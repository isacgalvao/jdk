//! Catalog, download and install engine of the jdk version manager.
//!
//! - contract: [`index`] — the schema the `jdk-index` repository publishes;
//! - acquisition: [`catalog`] (resolution chain) over [`cache`] (ETag + TTL,
//!   one file lock), [`foojay`] (live fallback) and [`http`] (rustls,
//!   corporate CAs, HTTPS-only on every hop);
//! - materialization: [`download`] (resumable, sha256-mandatory),
//!   [`extract`] (hardened zip), [`layout`] (find the JDK root),
//!   [`install`] (locked, idempotent);
//! - self-update source: [`release`] (latest-version discovery and verified
//!   bundle fetch from this project's own GitHub releases).

#[cfg(windows)]
pub mod admin;
pub mod cache;
pub mod catalog;
pub mod config;
#[cfg(windows)]
pub mod current;
pub mod download;
#[cfg(windows)]
pub mod env;
pub mod error;
pub mod extract;
pub mod file_ops;
pub mod foojay;
pub mod http;
pub mod index;
pub mod install;
pub mod layout;
pub mod release;
#[cfg(windows)]
pub mod shims;

pub use error::{Error, Result};
