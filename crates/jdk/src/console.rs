//! The y/N prompt every consent-bearing command asks: license acceptance,
//! replacing a foreign JAVA_HOME, purging the store. One shape, one default —
//! anything but an explicit `y`/`yes` refuses, so silence, EOF and garbage all
//! leave the machine as it was.
//!
//! Off-console (CI, a pipe, the shim path) nothing is printed and nothing is
//! read: a prompt no one can answer would hang someone's build waiting for a
//! keystroke that never comes.

use std::io::{self, IsTerminal};

/// Whether a question can be asked at all: stdin AND stderr on a terminal —
/// stdin because the answer has to come from somewhere, stderr because that
/// is where the question goes.
pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// Asks `prompt` on stderr and reads one line, but only when `interactive`;
/// otherwise it refuses without printing or reading. Callers pass their own
/// interactivity so a command can rule the prompt out for other reasons too
/// (`jdk install --from-shim` never asks).
pub fn confirm(interactive: bool, prompt: std::fmt::Arguments<'_>) -> bool {
    if interactive {
        eprint!("{prompt}");
    }
    decide(interactive, read_line)
}

fn read_line() -> Option<String> {
    let mut answer = String::new();
    match io::stdin().read_line(&mut answer) {
        // Ok(0) is EOF: no answer to honor, and the default is no.
        Ok(read) if read > 0 => Some(answer),
        _ => None,
    }
}

/// The decision inside [`confirm`], split off so it can be exercised without
/// a real TTY or stdin. The `&&` short-circuit IS the off-console refusal:
/// `read_answer` is never consulted there.
fn decide(interactive: bool, read_answer: impl FnOnce() -> Option<String>) -> bool {
    interactive
        && read_answer().is_some_and(|answer| {
            let answer = answer.trim();
            answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_yes_agrees() {
        for answer in [
            "y",
            "Y",
            "yes",
            "YES",
            "y\n",
            "Y\n",
            "yes\r\n",
            "YES\n",
            " yes \r\n",
            "  y  \n",
        ] {
            assert!(decide(true, || Some(answer.to_string())), "{answer:?}");
        }
    }

    #[test]
    fn no_empty_eof_or_garbage_all_refuse() {
        for answer in [
            "n", "N", "n\n", "N\n", "no", "no\n", "\n", "\r\n", " ", "banana\n", "sure", "yep",
        ] {
            assert!(!decide(true, || Some(answer.to_string())), "{answer:?}");
        }
        assert!(!decide(true, || None), "EOF (0 bytes read) refuses");
    }

    /// No console (CI, a pipe, the shim path): refuse without reading — a
    /// hang here would block someone's build for an answer they cannot give.
    #[test]
    fn off_console_refuses_without_reading() {
        assert!(!decide(false, || panic!(
            "an off-console prompt must not read stdin"
        )));
    }
}
