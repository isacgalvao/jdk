//! One error enum for the whole engine, message-first; the CLI (M3) maps
//! variants onto the exit-code contract in `jdk_resolve::exit`.

use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    /// Network or protocol failure, already past the retry schedule.
    #[error("{0}")]
    Http(String),
    /// A security policy said no: plain-http URL, untrusted download host,
    /// path traversal, missing checksum.
    #[error("{0}")]
    Security(String),
    /// Content does not match its mandatory sha256. Always blocking.
    #[error("sha256 mismatch for {subject}: expected {expected}, got {actual}")]
    Checksum {
        subject: String,
        expected: String,
        actual: String,
    },
    /// The catalog cannot answer: unknown vendor, no matching package,
    /// malformed index, index and fallback both unreachable.
    #[error("{0}")]
    Catalog(String),
    /// An archive cannot be safely extracted or does not contain a JDK.
    #[error("{0}")]
    Extract(String),
    /// A user-environment registry operation failed or met an unusable value.
    #[error("{0}")]
    Env(String),
    #[error("cannot {action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl Error {
    /// `map_err` adapter attaching the action and path an I/O error hit.
    pub fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Error {
        let path = path.to_path_buf();
        move |source| Error::Io {
            action,
            path,
            source,
        }
    }
}
