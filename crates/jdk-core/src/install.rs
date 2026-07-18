//! Install pipeline: verified download → hardened extraction → layout
//! normalization → move into `candidates\java\<vendor@version>`.
//!
//! Concurrency: one inter-process lock file per `vendor@version` plus an
//! orphan sweep — deliberately lean, since a heavier locking subsystem would
//! be over-engineering here. Installing something already installed is an
//! idempotent no-op, checked again after the lock is acquired so a waiting
//! process sees the winner's work.

use crate::download::{Progress, fetch_archive};
use crate::error::{Error, Result};
use crate::extract::extract_zip;
use crate::http::Http;
use crate::index::Package;
use crate::layout::find_jdk_root;
use jdk_resolve::store;
use jdk_resolve::version::Version;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Installed {
    pub vendor: String,
    pub version: Version,
    pub dir: PathBuf,
    /// false when the JDK was already in the store (idempotent no-op).
    pub fresh: bool,
}

pub fn install(
    jdk_root: &Path,
    http: &Http,
    package: &Package,
    progress: Option<Progress<'_>>,
) -> Result<Installed> {
    let version: Version = package.version.parse().map_err(|_| {
        Error::Catalog(format!(
            "catalog served an unparseable version {:?} for {}",
            package.version, package.vendor
        ))
    })?;
    let name = format!("{}@{version}", package.vendor);
    let dest = store::java_candidates(jdk_root).join(&name);
    let done = |fresh| Installed {
        vendor: package.vendor.clone(),
        version: version.clone(),
        dir: dest.clone(),
        fresh,
    };

    if dest.exists() {
        return Ok(done(false));
    }

    let cache = store::cache(jdk_root);
    let locks = cache.join("locks");
    fs::create_dir_all(&locks).map_err(Error::io("create", &locks))?;
    sweep_orphan_locks(&locks);
    let _lock = InstallLock::acquire(&locks.join(format!("{name}.lock")))?;

    // Another process may have installed it while we waited on the lock.
    if dest.exists() {
        return Ok(done(false));
    }

    let archive = cache.join("downloads").join(format!("{name}.zip"));
    fetch_archive(http, package, &archive, progress)?;

    let staging = Staging::create(cache.join("staging").join(&name))?;
    extract_zip(&archive, staging.path())?;
    let jdk_dir = find_jdk_root(staging.path())?;

    let candidates = store::java_candidates(jdk_root);
    fs::create_dir_all(&candidates).map_err(Error::io("create", &candidates))?;
    match fs::rename(&jdk_dir, &dest) {
        Ok(()) => {}
        // Raced by an out-of-band install; theirs wins, staging is dropped.
        Err(err) if err.kind() == ErrorKind::AlreadyExists => return Ok(done(false)),
        Err(err) => return Err(Error::io("move into", &dest)(err)),
    }
    drop(staging);
    let _ = fs::remove_file(&archive); // rebuildable; keep the store lean

    Ok(done(true))
}

/// Held for the whole install of one `vendor@version`; a second process
/// blocks on `File::lock` and then finds the store populated. The handle is
/// opened WITHOUT `FILE_SHARE_DELETE` so [`sweep_orphan_locks`] in another
/// process cannot delete a lock file that is actually held.
struct InstallLock {
    _file: File,
}

impl InstallLock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = open_lock_file(path)?;
        file.lock().map_err(Error::io("lock", path))?;
        Ok(InstallLock { _file: file })
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        // No FILE_SHARE_DELETE: deleting a held lock file fails instead of
        // silently succeeding, which is what makes the orphan sweep safe.
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    options.open(path).map_err(Error::io("open", path))
}

/// Lock hygiene: deletes `.lock` files nobody holds. A file whose lock is
/// held (or that a concurrent process reopens before our delete lands)
/// survives, because every holder opens without `FILE_SHARE_DELETE`.
fn sweep_orphan_locks(locks: &Path) {
    let Ok(entries) = fs::read_dir(locks) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "lock") {
            continue;
        }
        let Ok(file) = open_lock_file(&path) else {
            continue;
        };
        if file.try_lock().is_ok() {
            drop(file);
            let _ = fs::remove_file(&path);
        }
    }
}

/// Extraction workspace, removed on drop whatever happens to the install.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(path: PathBuf) -> Result<Self> {
        if path.exists() {
            // Leftovers of a crashed run.
            fs::remove_dir_all(&path).map_err(Error::io("clear", &path))?;
        }
        fs::create_dir_all(&path).map_err(Error::io("create", &path))?;
        Ok(Staging { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sweep_removes_orphans_and_spares_held_locks() {
        let temp = TempDir::new().unwrap();
        let orphan = temp.path().join("temurin@17.lock");
        let held_path = temp.path().join("temurin@21.lock");
        let unrelated = temp.path().join("notes.txt");
        fs::write(&orphan, b"").unwrap();
        fs::write(&unrelated, b"keep").unwrap();

        let held = InstallLock::acquire(&held_path).unwrap();
        sweep_orphan_locks(temp.path());

        assert!(!orphan.exists(), "orphan lock should be swept");
        assert!(held_path.exists(), "held lock must survive the sweep");
        assert!(unrelated.exists(), "non-lock files are untouched");
        drop(held);

        sweep_orphan_locks(temp.path());
        assert!(!held_path.exists(), "released lock becomes an orphan");
    }

    #[test]
    fn lock_can_be_reacquired_after_release() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("zulu@21.lock");
        drop(InstallLock::acquire(&path).unwrap());
        drop(InstallLock::acquire(&path).unwrap());
    }

    #[test]
    fn staging_cleans_up_on_drop() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("staging").join("x");
        {
            let staging = Staging::create(dir.clone()).unwrap();
            fs::write(staging.path().join("f"), b"x").unwrap();
        }
        assert!(!dir.exists());
    }

    #[test]
    fn staging_clears_crash_leftovers() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("staging").join("x");
        fs::create_dir_all(dir.join("old")).unwrap();

        let staging = Staging::create(dir.clone()).unwrap();
        assert!(!staging.path().join("old").exists());
    }
}
