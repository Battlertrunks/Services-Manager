//! Small text helpers for cleaning up child-process output before it reaches the TUI.

use std::sync::LazyLock;

static ANSI_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\)|[@-_])")
        .expect("valid ANSI regex")
});

/// Removes ANSI escape sequences and control characters from a log line.
pub fn clean_line(input: &str) -> String {
    let without_ansi = ANSI_RE.replace_all(input, "");
    let mut out = String::with_capacity(without_ansi.len());
    for ch in without_ansi.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\r' | '\n' => {}
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

/// Keeps an individual log line to a sane size, without splitting a UTF-8 character.
pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc_sequences() {
        let line = "\x1b[32mready\x1b[0m in \x1b[1m300ms\x1b[0m";
        assert_eq!(clean_line(line), "ready in 300ms");

        let link = "\x1b]8;;https://example.com\x07click\x1b]8;;\x07";
        assert_eq!(clean_line(link), "click");
    }

    #[test]
    fn expands_tabs_and_drops_carriage_returns() {
        assert_eq!(clean_line("one\ttwo\r"), "one    two");
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("héllo", 2), "hé…");
    }
}
