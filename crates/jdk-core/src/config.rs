//! Writer for `<JDK_ROOT>\config.toml` — the only writer of that file. The
//! reader is `jdk_resolve::config` (std-only, shim-safe); this side emits
//! exactly the flat subset that reader documents. v0.1 rewrites the known
//! keys only: unknown keys a hand-edit added are NOT preserved.
//!
//! Known keys beyond the resolve-visible ones: `java-home-before` /
//! `java-home-before-kind`, the JAVA_HOME backup `jdk setup` takes before
//! replacing a foreign value and `jdk setup --undo` puts back. The resolve
//! reader skips them as unknown — the shim never consumes them — but this
//! writer owns them and preserves them across rewrites. Reading them still
//! goes through that reader's `entries`: the file means one thing, not one
//! thing per binary.

use crate::error::{Error, Result};
use crate::file_ops::atomic_rename;
use jdk_resolve::config::{self, Config, ConfigError};
use jdk_resolve::store;
use std::fs;
use std::io;
use std::path::Path;

/// Characters no value may carry into `config.toml`. A quote or a line break
/// ends the value outright; `#` is legal in a Windows path and the current
/// reader keeps it, but a shim copy deployed before that fix cuts the line
/// there and rejects the whole file — so a mixed-version store stays readable
/// only if we never write one.
const UNWRITABLE: [char; 4] = ['"', '#', '\n', '\r'];

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

/// Records the pre-setup JAVA_HOME alongside `config`. Refuses a value
/// `config.toml` cannot carry (see `UNWRITABLE`) — the caller surfaces the
/// old value instead of silently losing it.
pub fn save_java_home_before(root: &Path, config: &Config, backup: &JavaHomeBefore) -> Result<()> {
    if backup.value.contains(UNWRITABLE) {
        return Err(Error::Env(format!(
            "previous JAVA_HOME {:?} contains characters config.toml cannot hold",
            backup.value
        )));
    }
    emit(root, config, Some(backup))
}

/// Drops the JAVA_HOME backup once `setup --undo` has put it back in the
/// registry, keeping the rest of `config` — the vendor and the license consent
/// are the user's, not setup's to revoke. Consuming it is what makes a second
/// undo a no-op instead of a restore of a value that is already restored.
pub fn clear_java_home_before(root: &Path, config: &Config) -> Result<()> {
    emit(root, config, None)
}

/// The saved pre-setup JAVA_HOME, if any. A missing kind key reads as
/// `REG_SZ`; an unknown kind is an error naming it. A file the shim's reader
/// rejects is an error here too — the backup is destined for the registry, so
/// half-read is worse than not read.
pub fn java_home_before(root: &Path) -> Result<Option<JavaHomeBefore>> {
    let path = store::config(root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::io("read", &path)(err)),
    };

    let mut value = None;
    let mut expandable = false;
    for entry in config::entries(&text) {
        let entry = entry.map_err(unreadable)?;
        match entry.key {
            "java-home-before" => value = Some(entry.string().map_err(unreadable)?.to_string()),
            "java-home-before-kind" => {
                expandable = match entry.string().map_err(unreadable)? {
                    "REG_SZ" => false,
                    "REG_EXPAND_SZ" => true,
                    other => {
                        return Err(Error::Env(format!(
                            "config.toml java-home-before-kind {other:?} is not REG_SZ or REG_EXPAND_SZ"
                        )));
                    }
                };
            }
            // Keys this side does not own, the shim's two included.
            _ => {}
        }
    }
    Ok(value.map(|value| JavaHomeBefore { value, expandable }))
}

/// A config the shared reader turned down. Not a disagreement to paper over:
/// the shim reads the same file by the same rules and is failing too.
fn unreadable(err: ConfigError) -> Error {
    Error::Env(err.to_string())
}

fn emit(root: &Path, config: &Config, backup: Option<&JavaHomeBefore>) -> Result<()> {
    if config.vendor.is_empty() || config.vendor.contains(UNWRITABLE) {
        return Err(Error::Catalog(format!(
            "vendor {:?} cannot be written to config.toml",
            config.vendor
        )));
    }
    // `accept-license` is written on every rewrite, `false` included. It is
    // standing consent to proprietary vendor terms, so it must survive a
    // `jdk setup` — dropping the key when it is false would be a rewrite that
    // silently revokes it, and writing it only when true would leave "absent"
    // meaning two different things across versions.
    let mut text = format!(
        "vendor = \"{}\"\nauto-install = \"{}\"\naccept-license = {}\n",
        config.vendor,
        config.auto_install.as_str(),
        config.accept_license
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
            accept_license: true,
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
            accept_license: false,
        };
        write(temp.path(), &changed).unwrap();

        assert_eq!(load(temp.path()).unwrap(), changed);
    }

    /// A rewrite that dropped `accept-license = true` would revoke consent
    /// the user granted, without ever saying so — `jdk setup` rewrites this
    /// file for reasons that have nothing to do with licenses.
    #[test]
    fn license_consent_survives_a_config_rewrite() {
        let temp = TempDir::new().unwrap();
        let consented = Config {
            accept_license: true,
            ..Config::default()
        };
        write(temp.path(), &consented).unwrap();

        // The round trip a `jdk setup` performs: load, act, write back.
        let loaded = load(temp.path()).unwrap();
        assert!(loaded.accept_license);
        save_java_home_before(
            temp.path(),
            &loaded,
            &JavaHomeBefore {
                value: r"C:\Program Files\Java\jdk-17".to_string(),
                expandable: false,
            },
        )
        .unwrap();
        assert!(load(temp.path()).unwrap().accept_license);

        // And `false` is written too, so revoking consent by hand takes.
        write(temp.path(), &Config::default()).unwrap();
        assert!(!load(temp.path()).unwrap().accept_license);
        let text = fs::read_to_string(store::config(temp.path())).unwrap();
        assert!(text.contains("accept-license = false"), "{text}");
    }

    #[test]
    fn refuses_a_vendor_the_reader_could_not_parse() {
        let temp = TempDir::new().unwrap();
        let broken = Config {
            vendor: "zu\"lu".to_string(),
            auto_install: AutoInstall::Prompt,
            accept_license: false,
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
            accept_license: false,
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

    /// What `setup --undo` does after handing the value back: the backup goes,
    /// the user's own settings stay.
    #[test]
    fn clearing_the_backup_keeps_the_rest_of_the_config() {
        let temp = TempDir::new().unwrap();
        let config = Config {
            vendor: "zulu".to_string(),
            auto_install: AutoInstall::Never,
            accept_license: true,
        };
        save_java_home_before(
            temp.path(),
            &config,
            &JavaHomeBefore {
                value: r"C:\Program Files\Java\jdk-17".to_string(),
                expandable: false,
            },
        )
        .unwrap();

        clear_java_home_before(temp.path(), &config).unwrap();

        assert_eq!(java_home_before(temp.path()).unwrap(), None);
        assert_eq!(load(temp.path()).unwrap(), config);
    }

    #[test]
    fn refuses_a_backup_the_subset_cannot_hold() {
        let temp = TempDir::new().unwrap();
        for hostile in [
            "C:\\evil\"quote",
            // Readable by the current reader, but a shim deployed before the
            // quote-aware scanner cuts the line at the `#` and fails the file.
            "C:\\Tools\\jdk#17",
            "C:\\evil\nnewline",
            "C:\\evil\rreturn",
        ] {
            let backup = JavaHomeBefore {
                value: hostile.to_string(),
                expandable: false,
            };
            assert!(
                save_java_home_before(temp.path(), &Config::default(), &backup).is_err(),
                "{hostile:?} must not reach config.toml"
            );
            assert_eq!(java_home_before(temp.path()).unwrap(), None);
        }
    }

    #[test]
    fn awkward_paths_survive_the_round_trip_on_both_sides() {
        // `#` is refused going in now, but a config written before that rule
        // (or by hand) still has to read back whole on both sides.
        for value in [
            r"C:\Tools\jdk#17",
            r"C:\Program Files\Java\jdk-17",
            r"%JDK17%\home",
            r"C:\Usuários\José\jdk#21",
            r"C:\Program Files (x86)\Java\jdk-8",
        ] {
            let temp = TempDir::new().unwrap();
            let backup = JavaHomeBefore {
                value: value.to_string(),
                expandable: true,
            };
            emit(temp.path(), &Config::default(), Some(&backup)).unwrap();

            assert_eq!(
                java_home_before(temp.path()).unwrap(),
                Some(backup),
                "{value}"
            );
            assert_eq!(load(temp.path()).unwrap(), Config::default(), "{value}");
        }
    }

    #[test]
    fn both_readers_agree_on_the_same_text() {
        // One reader erroring while the other returns a quietly truncated
        // backup is the whole defect: the file says one thing, not one thing
        // per binary asking.
        for text in [
            "vendor = \"zulu\"\njava-home-before = \"C:\\Tools\\jdk#17\"\n",
            "java-home-before = \"C:\\Tools\\jdk#17\"  # replaced by setup\n",
            "\u{feff}vendor = \"zulu\"\r\n\r\n# trailing note\r\n",
            "",
            "java-home-before = C:\\Tools\\jdk\n", // unquoted
            "java-home-before\n",                  // no `=`
            "vendor = \"zu\"lu\"\n",               // stray quote
            "[table]\n",                           // TOML neither speaks
        ] {
            let temp = TempDir::new().unwrap();
            fs::create_dir_all(temp.path()).unwrap();
            fs::write(store::config(temp.path()), text).unwrap();
            assert_eq!(
                load(temp.path()).is_err(),
                java_home_before(temp.path()).is_err(),
                "the two readers disagree about {text:?}"
            );
        }
    }
}
