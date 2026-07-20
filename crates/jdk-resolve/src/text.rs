//! Shared text scanning for the flat line-oriented pin and config formats.

/// Meaningful lines: BOM stripped, `#` comments removed, trimmed, blanks
/// skipped. Every pin-file and config parser reads through this so they treat
/// whitespace, CRLF and comments identically.
pub(crate) fn meaningful_lines(text: &str) -> impl Iterator<Item = &str> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|line| match line.find('#') {
            Some(at) => line[..at].trim(),
            None => line.trim(),
        })
        .filter(|line| !line.is_empty())
}
