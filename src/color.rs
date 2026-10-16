//! Colour in the output: the escape codes, and the decision whether to use them.

use std::env;
use std::io::{self, IsTerminal};

/// ANSI escape codes: red, yellow, magenta and bold cyan text, and going back to normal.
pub const RED: &str = "\x1b[31m";
pub const YELLOW: &str = "\x1b[33m";
pub const MAGENTA: &str = "\x1b[35m";
pub const HEADER: &str = "\x1b[1;36m";
pub const RESET: &str = "\x1b[0m";

/// When to colour the output: `--color auto|always|never`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Only when output goes to a terminal, and not if `NO_COLOR` is set.
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub fn parse(word: &str) -> Option<ColorMode> {
        match word {
            "auto" => Some(ColorMode::Auto),
            "always" => Some(ColorMode::Always),
            "never" => Some(ColorMode::Never),
            _ => None,
        }
    }
}

/// Whether to colour the output. `--color always` and `never` are absolute. Otherwise colour is
/// used for a terminal, unless `NO_COLOR` is set to something (see no-color.org) or the terminal
/// is `dumb`.
pub fn color_enabled(mode: ColorMode, is_terminal: bool, no_color: Option<&str>, term: Option<&str>) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => is_terminal && no_color.is_none_or(str::is_empty) && term != Some("dumb"),
    }
}

/// Whether to colour what is about to be written to stdout, looking at the real terminal and
/// environment.
pub fn stdout_color(mode: ColorMode) -> bool {
    let is_terminal = io::stdout().is_terminal();
    let no_color = env::var("NO_COLOR").ok();
    let term = env::var("TERM").ok();
    let enabled = color_enabled(mode, is_terminal, no_color.as_deref(), term.as_deref());
    debug!(
        "colour: --color {mode:?}, stdout is a terminal: {is_terminal}, NO_COLOR: {}, TERM: {} -> {}",
        no_color.as_deref().map_or("unset".to_string(), |v| format!("{v:?}")),
        term.as_deref().unwrap_or("unset"),
        if enabled { "on" } else { "off" }
    );
    enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_is_one_of_three_words() {
        assert_eq!(ColorMode::parse("auto"), Some(ColorMode::Auto));
        assert_eq!(ColorMode::parse("always"), Some(ColorMode::Always));
        assert_eq!(ColorMode::parse("never"), Some(ColorMode::Never));
        for bad in ["", "Auto", "red", "yes", "1"] {
            assert_eq!(ColorMode::parse(bad), None, "{bad:?}");
        }
        assert_eq!(ColorMode::default(), ColorMode::Auto);
    }

    #[test]
    fn colour_is_used_for_a_terminal_unless_no_color_or_a_dumb_terminal_say_otherwise() {
        let on = |mode, tty, no_color: Option<&str>, term: Option<&str>| color_enabled(mode, tty, no_color, term);
        // Auto: only for a terminal.
        assert!(on(ColorMode::Auto, true, None, Some("xterm-256color")));
        assert!(on(ColorMode::Auto, true, None, None));
        assert!(!on(ColorMode::Auto, false, None, Some("xterm")), "not for a pipe or a file");
        // NO_COLOR, when set to anything non-empty, turns it off; empty counts as unset.
        assert!(!on(ColorMode::Auto, true, Some("1"), Some("xterm")));
        assert!(!on(ColorMode::Auto, true, Some("anything"), None));
        assert!(on(ColorMode::Auto, true, Some(""), Some("xterm")));
        // A dumb terminal can't show colour.
        assert!(!on(ColorMode::Auto, true, None, Some("dumb")));
        // The flag overrides all of that.
        assert!(on(ColorMode::Always, false, Some("1"), Some("dumb")));
        assert!(!on(ColorMode::Never, true, None, Some("xterm")));
    }

    #[test]
    fn the_four_colours_and_the_reset_are_distinct_escape_codes() {
        let all = [RED, YELLOW, MAGENTA, HEADER, RESET];
        for (i, a) in all.iter().enumerate() {
            assert!(a.starts_with("\x1b[") && a.ends_with('m'), "{a:?}");
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
