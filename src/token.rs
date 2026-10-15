use crate::paths::token_file_path;
use std::env;
use std::fs;
use std::io;

/// The API token: `GODSPEED_TOKEN` if it is set and non-empty, otherwise the
/// `GODSPEED_TOKEN=...` line of the token file. The token is never printed.
pub fn load_token() -> Option<String> {
    if let Ok(token) = env::var("GODSPEED_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }

    let path = token_file_path();
    match fs::read_to_string(&path) {
        Ok(text) => parse_token_assignment(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("Warning: cannot read {}: {}", path.display(), e);
            None
        }
    }
}

/// Finds `GODSPEED_TOKEN=value` in shell-style text, allowing `export`, comments, other
/// variables and single or double quotes around the value.
fn parse_token_assignment(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != "GODSPEED_TOKEN" {
            continue;
        }
        let value = value.trim();
        let value = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q).and_then(|v| v.strip_suffix(*q)))
            .unwrap_or(value)
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_token_assignment;

    #[test]
    fn the_token_is_found_in_shell_style_text() {
        let token = |text: &str| parse_token_assignment(text);
        assert_eq!(token("export GODSPEED_TOKEN=\"abc123\"\n").as_deref(), Some("abc123"));
        assert_eq!(token("GODSPEED_TOKEN=abc123").as_deref(), Some("abc123"));
        assert_eq!(token("export GODSPEED_TOKEN='abc123'").as_deref(), Some("abc123"));
        assert_eq!(token("  export   GODSPEED_TOKEN = \"abc123\"  ").as_deref(), Some("abc123"));
        assert_eq!(
            token("# comment\nOTHER=x\n\nexport GODSPEED_TOKEN=\"abc123\"\nexport LATER=y").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn text_without_a_usable_token_assignment_gives_none() {
        assert_eq!(parse_token_assignment(""), None);
        assert_eq!(parse_token_assignment("# GODSPEED_TOKEN=commented-out"), None);
        assert_eq!(parse_token_assignment("OTHER_GODSPEED_TOKEN=x\nGODSPEED_TOKENS=y"), None);
        assert_eq!(parse_token_assignment("GODSPEED_TOKEN="), None);
        assert_eq!(parse_token_assignment("GODSPEED_TOKEN=\"\""), None);
        assert_eq!(parse_token_assignment("just some text"), None);
    }

    #[test]
    fn the_first_non_empty_assignment_wins() {
        assert_eq!(
            parse_token_assignment("GODSPEED_TOKEN=\nGODSPEED_TOKEN=second\nGODSPEED_TOKEN=third").as_deref(),
            Some("second")
        );
    }
}
