//! `jdk pin <selector>`: writes/updates `java=<selector>` in the cwd's
//! `.jdkrc`, preserving every other line and comment byte-for-byte. Pinning
//! something not installed is allowed (the file may be committed for
//! teammates) — it warns and hints at install instead of failing.

use crate::fail::Fail;
use jdk_resolve::selector::Selector;
use jdk_resolve::{exit, store};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

pub fn run(root: &Path, selector: &str) -> Result<(), Fail> {
    let selector = crate::parse_selector(selector)?;
    let config = crate::config(root)?;
    let cwd = env::current_dir().map_err(|err| {
        Fail::new(
            exit::FAILURE,
            format!("cannot read the current directory: {err}"),
        )
    })?;
    let path = cwd.join(".jdkrc");

    let existing = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(Fail::new(
                exit::FAILURE,
                format!("cannot read {}: {err}", path.display()),
            ));
        }
    };
    let updated = upsert_java(&existing, &selector);
    fs::write(&path, updated).map_err(|err| {
        Fail::new(
            exit::FAILURE,
            format!("cannot write {}: {err}", path.display()),
        )
    })?;
    eprintln!("jdk: pinned java={selector} in {}", path.display());

    let installed = store::best_candidate(root, &selector, &config.vendor).map_err(Fail::scan)?;
    if installed.is_none() {
        eprintln!("jdk: note: {selector} is not installed yet");
        eprintln!("  → jdk install {selector}");
    }
    Ok(())
}

/// Replaces the first `java=` entry in place (keeping its line ending and
/// any inline comment) or appends one; every other byte passes through.
fn upsert_java(content: &str, selector: &Selector) -> String {
    let mut out = String::with_capacity(content.len() + 32);
    let mut replaced = false;
    for line in content.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        if !replaced && is_java_line(body) {
            let ending = &line[body.len()..];
            match body.find('#') {
                Some(at) => out.push_str(&format!("java={selector} {}{ending}", &body[at..])),
                None => out.push_str(&format!("java={selector}{ending}")),
            }
            replaced = true;
        } else {
            out.push_str(line);
        }
    }
    if !replaced {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("java={selector}\n"));
    }
    out
}

fn is_java_line(body: &str) -> bool {
    let code = body.split('#').next().unwrap_or_default();
    code.split_once('=')
        .is_some_and(|(key, _)| key.trim() == "java")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str) -> Selector {
        s.parse().unwrap()
    }

    #[test]
    fn creates_the_entry_in_an_empty_file() {
        assert_eq!(upsert_java("", &sel("temurin@21")), "java=temurin@21\n");
    }

    #[test]
    fn replaces_only_the_java_line_preserving_the_rest() {
        let content = "# toolchain\nmaven=3.9\njava=zulu@17\nkotlin=2.0\n";
        assert_eq!(
            upsert_java(content, &sel("temurin@21")),
            "# toolchain\nmaven=3.9\njava=temurin@21\nkotlin=2.0\n"
        );
    }

    #[test]
    fn preserves_crlf_endings_and_inline_comments() {
        let content = "# header\r\njava=zulu@17 # LTS for now\r\nmaven=3.9\r\n";
        assert_eq!(
            upsert_java(content, &sel("temurin@21")),
            "# header\r\njava=temurin@21 # LTS for now\r\nmaven=3.9\r\n"
        );
    }

    #[test]
    fn appends_when_no_java_entry_exists() {
        assert_eq!(upsert_java("maven=3.9", &sel("21")), "maven=3.9\njava=21\n");
    }

    #[test]
    fn a_commented_out_java_line_is_not_the_entry() {
        let content = "# java=zulu@17\n";
        assert_eq!(
            upsert_java(content, &sel("21")),
            "# java=zulu@17\njava=21\n"
        );
    }

    #[test]
    fn replaces_only_the_first_java_entry() {
        let content = "java=zulu@17\njava=zulu@8\n";
        assert_eq!(upsert_java(content, &sel("21")), "java=21\njava=zulu@8\n");
    }
}
