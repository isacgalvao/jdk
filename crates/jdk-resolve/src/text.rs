//! Shared text scanning for the flat line-oriented pin and config formats.
//!
//! Both formats share the frame — BOM stripped, CRLF and stray spaces
//! tolerated, blank lines skipped, `#` starts a comment — and part ways on one
//! point: whether a quote shields a `#`. One scanner, two entry points instead
//! of a flag, so each call site names the premise it buys into.

/// Meaningful lines of a pin file, where `#` starts a comment anywhere: the
/// pin formats have no quoting and no legitimate pin value contains a `#`
/// (see `pin.rs`).
pub(crate) fn pin_lines(text: &str) -> impl Iterator<Item = &str> {
    scan(text, before_hash)
}

/// Meaningful lines of `config.toml`, where a `#` between double quotes is
/// data. The config subset quotes its string values and one of them,
/// `java-home-before`, holds a filesystem path: `C:\Tools\jdk#17` is a legal
/// Windows path, and cutting the line there stripped the closing quote and
/// failed the whole file — which is the shim's `java` for every pinned
/// directory, over a comment nobody wrote.
pub(crate) fn config_lines(text: &str) -> impl Iterator<Item = &str> {
    scan(text, before_unquoted_hash)
}

fn scan(text: &str, strip_comment: fn(&str) -> &str) -> impl Iterator<Item = &str> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(move |line| strip_comment(line).trim())
        .filter(|line| !line.is_empty())
}

fn before_hash(line: &str) -> &str {
    match line.find('#') {
        Some(at) => &line[..at],
        None => line,
    }
}

/// Up to the first `#` outside double quotes. An unbalanced quote keeps the
/// rest of the line quoted on purpose: the config parser then rejects a value
/// it cannot read, where truncating would have hidden the malformed line
/// behind a plausible-looking value.
fn before_unquoted_hash(line: &str) -> &str {
    let mut quoted = false;
    for (at, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..at],
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(text: &str) -> Vec<&str> {
        pin_lines(text).collect()
    }

    fn config(text: &str) -> Vec<&str> {
        config_lines(text).collect()
    }

    #[test]
    fn both_strip_bom_crlf_blanks_and_trailing_comments() {
        let text = "\u{feff}# header\r\n  a = \"x\"  # why\r\n\r\nb = true\n";
        assert_eq!(pin(text), ["a = \"x\"", "b = true"]);
        assert_eq!(config(text), ["a = \"x\"", "b = true"]);
    }

    #[test]
    fn only_the_config_scanner_lets_a_quoted_hash_through() {
        let line = "java-home-before = \"C:\\Tools\\jdk#17\"\n";
        assert_eq!(config(line), ["java-home-before = \"C:\\Tools\\jdk#17\""]);
        assert_eq!(pin(line), ["java-home-before = \"C:\\Tools\\jdk"]);
    }

    #[test]
    fn a_comment_after_a_quoted_value_is_still_a_comment() {
        assert_eq!(config("k = \"v#1\" # note\n"), ["k = \"v#1\""]);
        // Odd quote count: the `#` stays inside the value so the parser sees
        // (and rejects) the whole malformed line.
        assert_eq!(config("k = \"v # note\n"), ["k = \"v # note"]);
    }
}
