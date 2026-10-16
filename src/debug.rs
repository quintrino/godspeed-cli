//! `--debug`: a running commentary on stderr of what the program is doing, to help find out why
//! something doesn't work. Nothing is printed unless it is turned on, and stdout is never touched,
//! so `--json` output stays clean.
//!
//! The API token is never printed. Anything logged is passed through [`redact`], which replaces
//! every secret that has been registered with [`add_secret`], and request headers are logged with
//! the token already left out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ENABLED: AtomicBool = AtomicBool::new(false);
static SECRETS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// How much of a request or response body is printed.
const MAX_BODY: usize = 2000;

pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Registers a value, such as the API token, that must never appear in the output.
pub fn add_secret(secret: &str) {
    let secret = secret.trim();
    if secret.len() >= 4 {
        let mut secrets = SECRETS.lock().expect("the secrets list is not poisoned");
        if !secrets.iter().any(|known| known == secret) {
            secrets.push(secret.to_string());
        }
    }
}

/// `text` with every registered secret replaced by `<secret>`.
pub fn redact(text: &str) -> String {
    let secrets = SECRETS.lock().expect("the secrets list is not poisoned");
    let mut redacted = text.to_string();
    for secret in secrets.iter() {
        redacted = redacted.replace(secret.as_str(), "<secret>");
    }
    redacted
}

/// At most `MAX_BODY` characters of `body`, saying how much more there was.
pub fn shorten(body: &str) -> String {
    let length = body.chars().count();
    if length <= MAX_BODY {
        return body.to_string();
    }
    let kept: String = body.chars().take(MAX_BODY).collect();
    format!("{kept}... ({} more characters not shown)", length - MAX_BODY)
}

/// Prints a debug line, or several, one for each line of `message`. Does nothing when debugging
/// is off.
pub fn log(message: std::fmt::Arguments) {
    if !enabled() {
        return;
    }
    for line in redact(&message.to_string()).lines() {
        eprintln!("[debug] {line}");
    }
}

/// `debug!("...", args)` prints a line with `--debug`, and does nothing without it.
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::debug::log(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_replaced_wherever_it_appears() {
        add_secret("super-secret-debug-token");

        let text = redact("Authorization: Bearer super-secret-debug-token and again super-secret-debug-token.");

        assert_eq!(text, "Authorization: Bearer <secret> and again <secret>.");
        assert!(!redact("x super-secret-debug-token y").contains("super-secret"));
    }

    #[test]
    fn text_without_a_secret_is_unchanged() {
        assert_eq!(redact("nothing to hide here"), "nothing to hide here");
    }

    #[test]
    fn a_secret_that_is_too_short_to_be_one_is_ignored_so_ordinary_words_are_not_blanked() {
        add_secret("ab");
        add_secret("   ");
        assert_eq!(redact("a table about ab"), "a table about ab");
    }

    #[test]
    fn a_short_body_is_shown_whole_and_a_long_one_is_cut_with_a_note() {
        assert_eq!(shorten("short"), "short");
        let long = "x".repeat(MAX_BODY + 50);
        let shown = shorten(&long);
        assert!(shown.starts_with(&"x".repeat(MAX_BODY)));
        assert!(shown.ends_with("(50 more characters not shown)"), "{shown}");
        assert_eq!(shorten(&"y".repeat(MAX_BODY)), "y".repeat(MAX_BODY), "exactly the limit is not cut");
    }

    #[test]
    fn cutting_counts_characters_not_bytes() {
        let long = "é".repeat(MAX_BODY + 3);
        let shown = shorten(&long);
        assert!(shown.starts_with(&"é".repeat(MAX_BODY)));
        assert!(shown.contains("3 more characters"));
    }
}
