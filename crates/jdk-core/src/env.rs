//! Persistent user environment via the registry API — never `setx`
//! (anti-model 1: silent truncation at 1024 chars, `REG_EXPAND_SZ` collapsed
//! to literal `REG_SZ`, user PATH leaked into the machine scope).
//!
//! Every operation takes the Environment key as a parameter: production
//! passes `HKCU\Environment` ([`user_key`]), tests pass a disposable
//! `HKCU\Software\jdk-test-*` subkey — there is no test-only switch in any
//! production path, and nothing here ever writes machine scope
//! ([`machine_key`] opens it read-only, for `jdk doctor`).

use crate::error::{Error, Result};
use std::path::Path;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE};

pub use winreg::enums::RegType;
pub use winreg::{RegKey, RegValue};

pub const JAVA_HOME: &str = "JAVA_HOME";
pub const PATH: &str = "Path";

/// A `REG_SZ` / `REG_EXPAND_SZ` value: decoded text plus its registry form.
/// The decode is for inspection only — writes that must preserve an existing
/// value reuse its original bytes, so a lossy decode never round-trips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvValue {
    pub text: String,
    pub expandable: bool,
}

impl EnvValue {
    /// The registry type name, as config.toml and doctor messages show it.
    pub fn kind(&self) -> &'static str {
        if self.expandable {
            "REG_EXPAND_SZ"
        } else {
            "REG_SZ"
        }
    }
}

/// `HKCU\Environment`, read+write — the production target of `jdk setup`.
pub fn user_key() -> Result<RegKey> {
    open(
        RegKey::predef(HKEY_CURRENT_USER),
        "Environment",
        KEY_QUERY_VALUE | KEY_SET_VALUE,
    )
}

/// The machine-wide environment key, READ-ONLY: `jdk doctor` inspects it for
/// a conflicting machine JAVA_HOME; jdk never writes machine scope.
pub fn machine_key() -> Result<RegKey> {
    open(
        RegKey::predef(HKEY_LOCAL_MACHINE),
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        KEY_QUERY_VALUE,
    )
}

/// An `HKCU` subkey by path, read+write — the `JDK_ENV_KEY` /
/// `JDK_MACHINE_ENV_KEY` injection point hermetic tests use (a disposable
/// `Software\jdk-test-*` subkey standing in for the real environment keys).
pub fn hkcu_subkey(path: &str) -> Result<RegKey> {
    open(
        RegKey::predef(HKEY_CURRENT_USER),
        path,
        KEY_QUERY_VALUE | KEY_SET_VALUE,
    )
}

/// `HKCU` itself — test support builds its disposable subkeys from here.
pub fn hkcu() -> RegKey {
    RegKey::predef(HKEY_CURRENT_USER)
}

fn open(root: RegKey, path: &str, access: u32) -> Result<RegKey> {
    root.open_subkey_with_flags(path, access)
        .map_err(|err| Error::Env(format!("cannot open registry key {path}: {err}")))
}

/// Reads a string value. `Ok(None)` when absent; a non-string registry type
/// is an error naming the type (doctor shows it verbatim).
pub fn read(key: &RegKey, name: &str) -> Result<Option<EnvValue>> {
    let raw = match key.get_raw_value(name) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::Env(format!("cannot read {name}: {err}"))),
    };
    let expandable = match raw.vtype {
        RegType::REG_SZ => false,
        RegType::REG_EXPAND_SZ => true,
        other => {
            return Err(Error::Env(format!(
                "{name} has registry type {other:?}; expected REG_SZ or REG_EXPAND_SZ"
            )));
        }
    };
    Ok(Some(EnvValue {
        text: decode(&raw.bytes),
        expandable,
    }))
}

/// How the persisted JAVA_HOME relates to the junction jdk manages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaHomeState {
    /// Already the junction path — nothing to do.
    Ours,
    Absent,
    /// Set by someone else; `jdk setup` asks before replacing it.
    Foreign(EnvValue),
}

pub fn java_home_state(key: &RegKey, junction: &Path) -> Result<JavaHomeState> {
    Ok(match read(key, JAVA_HOME)? {
        None => JavaHomeState::Absent,
        Some(value) if same_path(&value.text, junction) => JavaHomeState::Ours,
        Some(value) => JavaHomeState::Foreign(value),
    })
}

/// Writes `JAVA_HOME = <junction>` as `REG_SZ` (decision 8, written once).
/// The absolute resolved path, not a literal `%USERPROFILE%` expression: it
/// stays correct under a `JDK_ROOT` override and is unambiguous for the
/// tools that read the registry raw without expanding (anti-model 1 is
/// exactly such a literal left where expansion never happens).
pub fn set_java_home(key: &RegKey, junction: &Path) -> Result<()> {
    let value = string_value(&junction.to_string_lossy(), RegType::REG_SZ);
    key.set_raw_value(JAVA_HOME, &value)
        .map_err(|err| Error::Env(format!("cannot write JAVA_HOME: {err}")))
}

/// A registry string value built from text (UTF-16LE + trailing NUL).
pub fn string_value(text: &str, vtype: RegType) -> RegValue<'static> {
    RegValue {
        bytes: encode(text).into(),
        vtype,
    }
}

/// Prepends `shims` to the user PATH exactly once. The existing value keeps
/// its BYTES (the new entry is prepended to the original byte sequence, so
/// nothing is re-encoded or re-quoted) and its TYPE (`REG_EXPAND_SZ` stays
/// `REG_EXPAND_SZ`). No length ceiling — this is the registry API, not
/// setx. Returns whether the registry changed (false = already present).
pub fn prepend_path(key: &RegKey, shims: &Path) -> Result<bool> {
    let write = |value: &RegValue| {
        key.set_raw_value(PATH, value)
            .map_err(|err| Error::Env(format!("cannot write Path: {err}")))
    };
    let entry = shims.to_string_lossy();

    let raw = match key.get_raw_value(PATH) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Fresh hives lack a user Path; REG_EXPAND_SZ is the Windows
            // convention for it.
            write(&string_value(&entry, RegType::REG_EXPAND_SZ))?;
            return Ok(true);
        }
        Err(err) => return Err(Error::Env(format!("cannot read Path: {err}"))),
        Ok(raw) => raw,
    };
    if !matches!(raw.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
        return Err(Error::Env(format!(
            "Path has registry type {:?}; expected REG_SZ or REG_EXPAND_SZ",
            raw.vtype
        )));
    }

    let text = decode(&raw.bytes);
    if path_count(&text, shims) > 0 {
        return Ok(false);
    }
    if text.trim().is_empty() {
        write(&string_value(&entry, raw.vtype))?;
        return Ok(true);
    }

    let mut bytes = utf16_bytes(&format!("{entry};"));
    bytes.extend_from_slice(&raw.bytes);
    write(&RegValue {
        bytes: bytes.into(),
        vtype: raw.vtype,
    })?;
    Ok(true)
}

/// How many PATH entries of `path_text` name `dir` — 1 is healthy, 0 means
/// setup never ran, more means duplication (anti-model 5). Comparison is
/// Windows-flavored: case-insensitive, surrounding quotes and trailing
/// separators ignored.
pub fn path_count(path_text: &str, dir: &Path) -> usize {
    let wanted = normalize_entry(&dir.to_string_lossy());
    path_text
        .split(';')
        .filter(|entry| normalize_entry(entry) == wanted)
        .count()
}

/// Same normalization as [`path_count`], for single-value comparisons
/// (JAVA_HOME vs the junction path).
fn same_path(text: &str, path: &Path) -> bool {
    normalize_entry(text) == normalize_entry(&path.to_string_lossy())
}

fn normalize_entry(entry: &str) -> String {
    entry
        .trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

/// Tells every open window the environment changed:
/// `SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, "Environment")`.
/// Explorer re-reads HKCU\Environment on it, so consoles opened from the
/// shell afterwards see the new values without a logoff. Fire-and-forget:
/// SMTO_ABORTIFHUNG skips hung windows and a failure is not actionable.
/// Callers only broadcast after a real registry mutation, never in hermetic
/// runs (the values under test are not the real environment).
pub fn broadcast_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let environment: Vec<u16> = "Environment\0".encode_utf16().collect();
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}

/// UTF-16LE with the trailing NUL the registry stores for string values.
fn encode(text: &str) -> Vec<u8> {
    utf16_bytes(&format!("{text}\0"))
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

/// Registry string bytes → text, dropping the trailing NUL(s).
fn decode(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use test_support::reg::TestKey;

    fn shims() -> PathBuf {
        PathBuf::from(r"C:\Users\x\.jdk\shims")
    }

    fn junction() -> PathBuf {
        PathBuf::from(r"C:\Users\x\.jdk\current")
    }

    #[test]
    fn read_reports_absent_text_and_type() {
        let test = TestKey::create();
        assert_eq!(read(&test.key, "NOTHING").unwrap(), None);

        test.key
            .set_raw_value("PLAIN", &string_value("hello", RegType::REG_SZ))
            .unwrap();
        let value = read(&test.key, "PLAIN").unwrap().unwrap();
        assert_eq!(value.text, "hello");
        assert!(!value.expandable);
        assert_eq!(value.kind(), "REG_SZ");

        test.key
            .set_raw_value("EXPAND", &string_value("%TEMP%\\x", RegType::REG_EXPAND_SZ))
            .unwrap();
        let value = read(&test.key, "EXPAND").unwrap().unwrap();
        assert!(value.expandable);
        assert_eq!(value.kind(), "REG_EXPAND_SZ");

        test.key.set_value("DWORD", &7u32).unwrap();
        let err = read(&test.key, "DWORD").unwrap_err().to_string();
        assert!(err.contains("REG_DWORD"), "{err}");
    }

    #[test]
    fn java_home_state_distinguishes_ours_absent_foreign() {
        let test = TestKey::create();
        assert_eq!(
            java_home_state(&test.key, &junction()).unwrap(),
            JavaHomeState::Absent
        );

        set_java_home(&test.key, &junction()).unwrap();
        assert_eq!(
            java_home_state(&test.key, &junction()).unwrap(),
            JavaHomeState::Ours
        );
        // The write is REG_SZ with the absolute path (decision 8).
        let written = read(&test.key, JAVA_HOME).unwrap().unwrap();
        assert_eq!(written.text, junction().to_string_lossy());
        assert!(!written.expandable);

        // Case and a trailing separator do not make it foreign.
        test.key
            .set_raw_value(
                JAVA_HOME,
                &string_value(r"c:\users\X\.JDK\current\", RegType::REG_SZ),
            )
            .unwrap();
        assert_eq!(
            java_home_state(&test.key, &junction()).unwrap(),
            JavaHomeState::Ours
        );

        test.key
            .set_raw_value(
                JAVA_HOME,
                &string_value(r"C:\Program Files\Java\jdk-17", RegType::REG_SZ),
            )
            .unwrap();
        match java_home_state(&test.key, &junction()).unwrap() {
            JavaHomeState::Foreign(value) => {
                assert_eq!(value.text, r"C:\Program Files\Java\jdk-17");
            }
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    #[test]
    fn prepend_path_creates_a_missing_path_as_expand_sz() {
        let test = TestKey::create();

        assert!(prepend_path(&test.key, &shims()).unwrap());

        let value = read(&test.key, PATH).unwrap().unwrap();
        assert_eq!(value.text, shims().to_string_lossy());
        assert!(value.expandable);
    }

    #[test]
    fn prepend_path_preserves_existing_bytes_and_type() {
        let test = TestKey::create();
        // A realistic user PATH: REG_EXPAND_SZ with an unexpanded variable.
        let original = string_value(r"%USERPROFILE%\bin;C:\tools", RegType::REG_EXPAND_SZ);
        test.key.set_raw_value(PATH, &original).unwrap();

        assert!(prepend_path(&test.key, &shims()).unwrap());

        let raw = test.key.get_raw_value(PATH).unwrap();
        assert_eq!(raw.vtype, RegType::REG_EXPAND_SZ, "type preserved");
        let prefix = utf16_bytes(&format!("{};", shims().to_string_lossy()));
        assert_eq!(
            &raw.bytes[prefix.len()..],
            &original.bytes[..],
            "original value bytes preserved verbatim after the new entry"
        );
        assert!(raw.bytes.starts_with(&prefix));
    }

    #[test]
    fn prepend_path_is_idempotent() {
        let test = TestKey::create();
        test.key
            .set_raw_value(
                PATH,
                &string_value(
                    &format!(r"C:\other;{}", shims().to_string_lossy()),
                    RegType::REG_SZ,
                ),
            )
            .unwrap();

        // Already present (even mid-PATH): no write, no duplication.
        assert!(!prepend_path(&test.key, &shims()).unwrap());
        let value = read(&test.key, PATH).unwrap().unwrap();
        assert_eq!(path_count(&value.text, &shims()), 1);
        assert_eq!(value.kind(), "REG_SZ", "type still untouched");
    }

    #[test]
    fn prepend_path_replaces_an_empty_value() {
        let test = TestKey::create();
        test.key
            .set_raw_value(PATH, &string_value("", RegType::REG_SZ))
            .unwrap();

        assert!(prepend_path(&test.key, &shims()).unwrap());

        let value = read(&test.key, PATH).unwrap().unwrap();
        assert_eq!(value.text, shims().to_string_lossy());
        assert!(!value.text.contains(';'), "no dangling separator");
    }

    #[test]
    fn path_count_normalizes_windows_style() {
        let text = r#"C:\a;"C:\USERS\X\.jdk\shims\";c:\users\x\.jdk\shims;C:\b"#;
        assert_eq!(path_count(text, &shims()), 2);
        assert_eq!(path_count(r"C:\a;C:\b", &shims()), 0);
        assert_eq!(path_count("", &shims()), 0);
    }
}
