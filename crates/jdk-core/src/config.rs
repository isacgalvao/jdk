//! Writer for `<JDK_ROOT>\config.toml` — the only writer of that file. The
//! reader is `jdk_resolve::config` (std-only, shim-safe); this side emits
//! exactly the flat subset that reader documents. v0.1 rewrites the known
//! keys only: unknown keys a hand-edit added are NOT preserved.
//!
//! Known keys beyond the resolve-visible pair: `java-home-before` /
//! `java-home-before-kind`, the pre-setup JAVA_HOME backup kept for a
//! future `setup --undo`. The resolve reader skips them as unknown — the
//! shim never consumes them — but this writer owns them and preserves
//! them across rewrites.

use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use jdk_resolve::config::Config;
use jdk_resolve::store;
use std::fs;
use std::io;
use std::path::Path;

/// The JAVA_HOME value `jdk setup` replaced: exact text plus whether it was
/// `REG_EXPAND_SZ` (an undo must restore the registry type too).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaHomeBefore {
    pub value: String,
    pub expandable: bool,
}

/// Serializes `config` to `<root>\config.toml` atomically (tmp + rename, so
/// a concurrent shim never reads a half-written file). An existing
/// JAVA_HOME backup is preserved.
pub fn write(root: &Path, config: &Config) -> Result<()> {
    emit(root, config, java_home_before(root)?.as_ref())
}

/// Records the pre-setup JAVA_HOME alongside `config`. Refuses a value the
/// flat config subset cannot represent (quotes, line breaks) — the caller
/// surfaces the old value instead of silently losing it.
pub fn save_java_home_before(root: &Path, config: &Config, backup: &JavaHomeBefore) -> Result<()> {
    if backup.value.contains(['"', '\n', '\r']) {
        return Err(Error::Env(format!(
            "previous JAVA_HOME {:?} contains characters config.toml cannot hold",
            backup.value
        )));
    }
    emit(root, config, Some(backup))
}

/// The saved pre-setup JAVA_HOME, if any. A missing kind key reads as
/// `REG_SZ`; an unknown kind is an error naming it.
pub fn java_home_before(root: &Path) -> Result<Option<JavaHomeBefore>> {
    let text = match fs::read_to_string(store::config(root)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::io("read", &store::config(root))(err)),
    };

    let mut value = None;
    let mut expandable = false;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let unquoted = raw.trim().trim_matches('"');
        match key.trim() {
            "java-home-before" => value = Some(unquoted.to_string()),
            "java-home-before-kind" => {
                expandable = match unquoted {
                    "REG_SZ" => false,
                    "REG_EXPAND_SZ" => true,
                    other => {
                        return Err(Error::Env(format!(
                            "config.toml java-home-before-kind {other:?} is not REG_SZ or REG_EXPAND_SZ"
                        )));
                    }
                };
            }
            _ => {}
        }
    }
    Ok(value.map(|value| JavaHomeBefore { value, expandable }))
}

fn emit(root: &Path, config: &Config, backup: Option<&JavaHomeBefore>) -> Result<()> {
    if config.vendor.is_empty() || config.vendor.contains('"') {
        return Err(Error::Catalog(format!(
            "vendor {:?} cannot be written to config.toml",
            config.vendor
        )));
    }
    let mut text = format!(
        "vendor = \"{}\"\nauto-install = \"{}\"\n",
        config.vendor,
        config.auto_install.as_str()
    );
    if let Some(backup) = backup {
        let kind = if backup.expandable {
            "REG_EXPAND_SZ"
        } else {
            "REG_SZ"
        };
        text.push_str(&format!(
            "java-home-before = \"{}\"\njava-home-before-kind = \"{kind}\"\n",
            backup.value
        ));
    }
    fs::create_dir_all(root).map_err(Error::io("create", root))?;
    let dest = store::config(root);
    // Pid-suffixed tmp: concurrent writers never clobber each other's
    // staging file; the atomic rename decides who wins.
    let tmp = dest.with_extension(format!("toml.{}.tmp", std::process::id()));
    fs::write(&tmp, text).map_err(Error::io("write", &tmp))?;
    atomic_rename(&tmp, &dest).map_err(Error::io("replace", &dest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdk_resolve::config::{AutoInstall, load};
    use tempfile::TempDir;

    #[test]
    fn round_trips_through_the_resolve_reader() {
        let temp = TempDir::new().unwrap();
        let config = Config {
            vendor: "zulu".to_string(),
            auto_install: AutoInstall::Always,
        };

        write(temp.path(), &config).unwrap();

        assert_eq!(load(temp.path()).unwrap(), config);
    }

    #[test]
    fn overwrites_an_existing_config() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), &Config::default()).unwrap();

        let changed = Config {
            vendor: "corretto".to_string(),
            auto_install: AutoInstall::Never,
        };
        write(temp.path(), &changed).unwrap();

        assert_eq!(load(temp.path()).unwrap(), changed);
    }

    #[test]
    fn refuses_a_vendor_the_reader_could_not_parse() {
        let temp = TempDir::new().unwrap();
        let broken = Config {
            vendor: "zu\"lu".to_string(),
            auto_install: AutoInstall::Prompt,
        };
        assert!(write(temp.path(), &broken).is_err());
    }

    #[test]
    fn java_home_backup_round_trips_and_survives_a_config_rewrite() {
        let temp = TempDir::new().unwrap();
        assert_eq!(java_home_before(temp.path()).unwrap(), None);

        let backup = JavaHomeBefore {
            value: r"%JDK17%\home".to_string(),
            expandable: true,
        };
        save_java_home_before(temp.path(), &Config::default(), &backup).unwrap();
        assert_eq!(java_home_before(temp.path()).unwrap(), Some(backup.clone()));

        // A later config rewrite must not lose the backup...
        let changed = Config {
            vendor: "zulu".to_string(),
            auto_install: AutoInstall::Never,
        };
        write(temp.path(), &changed).unwrap();
        assert_eq!(java_home_before(temp.path()).unwrap(), Some(backup));
        assert_eq!(load(temp.path()).unwrap(), changed);

        // ...and the resolve-side reader (the shim's) still parses the file,
        // skipping the backup keys as unknown.
        let plain = JavaHomeBefore {
            value: r"C:\Program Files\Java\jdk-17".to_string(),
            expandable: false,
        };
        save_java_home_before(temp.path(), &changed, &plain).unwrap();
        assert_eq!(java_home_before(temp.path()).unwrap(), Some(plain));
        assert_eq!(load(temp.path()).unwrap(), changed);
    }

    #[test]
    fn refuses_a_backup_the_subset_cannot_hold() {
        let temp = TempDir::new().unwrap();
        let hostile = JavaHomeBefore {
            value: "C:\\evil\"quote".to_string(),
            expandable: false,
        };
        assert!(save_java_home_before(temp.path(), &Config::default(), &hostile).is_err());
        assert_eq!(java_home_before(temp.path()).unwrap(), None);
    }
}
