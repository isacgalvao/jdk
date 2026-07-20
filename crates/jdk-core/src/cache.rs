//! Catalog cache under `<JDK_ROOT>\cache\index`: body plus sidecar meta per
//! remote file, revalidated with ETag/If-None-Match once a TTL expires.
//!
//! Concurrency rule: ONE inter-process file lock covers read, write AND
//! invalidate — unlocked invalidation was the bug — and a post-refresh grace
//! window keeps a forced refresh from throwing away a cache another process
//! just filled.

use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use crate::http::{Http, MAX_BODY};
use crate::index::safe_path_segments;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const REFRESH_GRACE: Duration = Duration::from_secs(5 * 60);

pub struct Cache {
    dir: PathBuf,
    lock_path: PathBuf,
    ttl: Duration,
    grace: Duration,
}

/// Sidecar (`<file>.meta`) recording how a body was fetched.
#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    etag: Option<String>,
    fetched_secs: u64,
}

impl Cache {
    pub fn new(jdk_root: &Path) -> Self {
        Self::with_ttl(jdk_root, TTL, REFRESH_GRACE)
    }

    /// Custom TTL and grace window; `ttl` of zero revalidates on every read
    /// (tests and, in M3, `--refresh`).
    pub fn with_ttl(jdk_root: &Path, ttl: Duration, grace: Duration) -> Self {
        let cache = jdk_resolve::store::cache(jdk_root);
        Cache {
            dir: cache.join("index"),
            lock_path: cache.join("index.lock"),
            ttl,
            grace,
        }
    }

    /// Body of `<base_url>/<relpath>`, cached: fresh (younger than TTL) →
    /// disk without touching the network; stale → conditional GET, where a
    /// 304 refreshes the clock and reuses the disk copy; network failure with
    /// a stale copy on disk → the stale copy (offline tolerance).
    pub fn get(&self, http: &Http, base_url: &str, relpath: &str) -> Result<Vec<u8>> {
        let (body_path, meta_path) = self.entry_paths(relpath)?;
        let _lock = self.hold_lock()?;

        let cached = read_entry(&body_path, &meta_path);
        if let Some((body, meta)) = &cached
            && age(meta.fetched_secs) < self.ttl
        {
            return Ok(body.clone());
        }

        let url = format!("{}/{relpath}", base_url.trim_end_matches('/'));
        let mut headers = Vec::new();
        if let Some((_, meta)) = &cached
            && let Some(etag) = &meta.etag
        {
            headers.push(("If-None-Match", etag.clone()));
        }

        let reply = match http.get(&url, "catalog", &headers) {
            Ok(reply) => reply,
            Err(err) => {
                return match cached {
                    Some((body, _)) => Ok(body),
                    None => Err(err),
                };
            }
        };

        match reply.status() {
            304 if cached.is_some() => {
                let (body, meta) = cached.expect("guarded by the match arm");
                self.write_meta(
                    &meta_path,
                    &Meta {
                        etag: meta.etag,
                        fetched_secs: now(),
                    },
                )?;
                Ok(body)
            }
            200 => {
                let etag = reply.header("etag").map(str::to_string);
                let body = reply.bytes(MAX_BODY)?;
                if let Some(parent) = body_path.parent() {
                    fs::create_dir_all(parent).map_err(Error::io("create", parent))?;
                }
                self.write_atomic(&body_path, &body)?;
                self.write_meta(
                    &meta_path,
                    &Meta {
                        etag,
                        fetched_secs: now(),
                    },
                )?;
                Ok(body)
            }
            status => Err(Error::Http(format!("GET {url} returned {status}"))),
        }
    }

    /// Forced invalidation (M3 `--refresh`). Honors the grace window: an entry
    /// fetched moments ago survives, so refresh storms cannot thrash the
    /// cache. Returns whether the entry was actually dropped.
    pub fn invalidate(&self, relpath: &str) -> Result<bool> {
        let (body_path, meta_path) = self.entry_paths(relpath)?;
        let _lock = self.hold_lock()?;
        if let Some((_, meta)) = read_entry(&body_path, &meta_path)
            && age(meta.fetched_secs) < self.grace
        {
            return Ok(false);
        }
        remove_entry(&body_path, &meta_path);
        Ok(true)
    }

    /// Unconditional eviction, bypassing the grace window — for integrity
    /// failures (a platform file that no longer matches its index sha256),
    /// where keeping the entry would keep serving bad data.
    pub(crate) fn evict(&self, relpath: &str) -> Result<()> {
        let (body_path, meta_path) = self.entry_paths(relpath)?;
        let _lock = self.hold_lock()?;
        remove_entry(&body_path, &meta_path);
        Ok(())
    }

    fn entry_paths(&self, relpath: &str) -> Result<(PathBuf, PathBuf)> {
        let mut body_path = self.dir.clone();
        for segment in safe_path_segments(relpath)? {
            body_path.push(segment);
        }
        let mut meta_name = body_path
            .file_name()
            .expect("safe_path_segments guarantees a file name")
            .to_owned();
        meta_name.push(".meta");
        let meta_path = body_path.with_file_name(meta_name);
        Ok((body_path, meta_path))
    }

    /// The single lock: every cache operation holds it for its whole
    /// duration, including the HTTP round-trip — a second process blocks and
    /// then reads the freshly written entry. Released when the handle drops.
    fn hold_lock(&self) -> Result<File> {
        fs::create_dir_all(&self.dir).map_err(Error::io("create", &self.dir))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .map_err(Error::io("open", &self.lock_path))?;
        file.lock().map_err(Error::io("lock", &self.lock_path))?;
        Ok(file)
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let mut tmp_name = path
            .file_name()
            .expect("cache paths always have a file name")
            .to_owned();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
        fs::write(&tmp, bytes).map_err(Error::io("write", &tmp))?;
        atomic_rename(&tmp, path).map_err(Error::io("finalize", path))
    }

    fn write_meta(&self, path: &Path, meta: &Meta) -> Result<()> {
        let json = serde_json::to_vec(meta).expect("Meta always serializes");
        self.write_atomic(path, &json)
    }
}

fn read_entry(body_path: &Path, meta_path: &Path) -> Option<(Vec<u8>, Meta)> {
    let body = fs::read(body_path).ok()?;
    let meta: Meta = serde_json::from_slice(&fs::read(meta_path).ok()?).ok()?;
    Some((body, meta))
}

fn remove_entry(body_path: &Path, meta_path: &Path) {
    let _ = fs::remove_file(body_path);
    let _ = fs::remove_file(meta_path);
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn age(fetched_secs: u64) -> Duration {
    Duration::from_secs(now().saturating_sub(fetched_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn entry_paths_mirror_the_remote_layout() {
        let temp = TempDir::new().unwrap();
        let cache = Cache::new(temp.path());
        let (body, meta) = cache.entry_paths("windows-x64/temurin.json").unwrap();
        assert_eq!(body, cache.dir.join("windows-x64").join("temurin.json"));
        assert_eq!(
            meta,
            cache.dir.join("windows-x64").join("temurin.json.meta")
        );
    }

    #[test]
    fn hostile_relpaths_are_rejected() {
        let temp = TempDir::new().unwrap();
        let cache = Cache::new(temp.path());
        for path in ["../escape", "a/../../b", "/abs", "c:\\x", "a//b"] {
            assert!(
                cache.entry_paths(path).is_err(),
                "{path:?} should be rejected"
            );
        }
    }

    #[test]
    fn invalidate_of_a_missing_entry_is_ok() {
        let temp = TempDir::new().unwrap();
        let cache = Cache::new(temp.path());
        assert!(cache.invalidate("index.json").unwrap());
    }
}
