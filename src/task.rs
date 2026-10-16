//! Turning task text like `Pay rent @Home .Urgent ~30m :tomorrow n: notes` into a request.

use chrono::{Duration, Local, NaiveDate};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct TaskRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeless_due_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub label_ids: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

/// A parsed task. The list and label names still have to be turned into ids.
#[derive(Debug)]
pub struct ParsedTask {
    pub request: TaskRequest,
    /// Every `@list` in the task, not counting the notes. Only one is allowed.
    pub list_names: Vec<String>,
    pub label_names: Vec<String>,
}

/// Parses a `~30m` / `~1.5h` / `~45` duration word into whole minutes. Anything that isn't
/// exactly `~` plus a positive number and an optional `m` or `h` is not a duration, so
/// words like "~roughly" stay in the title.
fn parse_duration_token(word: &str) -> Option<i32> {
    let rest = word.strip_prefix('~')?;
    let (number, per_unit) = if let Some(n) = rest.strip_suffix('h') {
        (n, 60.0)
    } else if let Some(n) = rest.strip_suffix('m') {
        (n, 1.0)
    } else {
        (rest, 1.0)
    };
    let well_formed = number.chars().all(|c| c.is_ascii_digit() || c == '.')
        && number.chars().any(|c| c.is_ascii_digit())
        && number.matches('.').count() <= 1;
    if !well_formed {
        return None;
    }
    let minutes = (number.parse::<f64>().ok()? * per_unit).round();
    if minutes >= 1.0 && minutes <= i32::MAX as f64 {
        Some(minutes as i32)
    } else {
        None
    }
}

/// Parses a `:today` / `:tomorrow` / `:next-week` / `:YYYYMMDD` / `:YYYY-MM-DD` word into a
/// `YYYY-MM-DD` date. Short numbers like `:45` are durations, not dates.
fn parse_due_token(word: &str, today: NaiveDate) -> Option<String> {
    let value = word.strip_prefix(':')?.to_ascii_lowercase();
    let date = match value.as_str() {
        "today" => today,
        "tomorrow" => today + Duration::days(1),
        "next-week" => today + Duration::weeks(1),
        v if v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit()) => {
            NaiveDate::parse_from_str(v, "%Y%m%d").ok()?
        }
        v => NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?,
    };
    Some(date.format("%Y-%m-%d").to_string())
}

/// The original `:45` duration syntax, in minutes. Digit runs of 8 or more are malformed dates
/// (such as `:20261345`), not durations, so they are left in the title.
fn parse_legacy_minutes(word: &str) -> Option<i32> {
    let value = word.trim_start_matches(':');
    if value.len() >= 8 {
        return None;
    }
    value.parse().ok()
}

/// Rewrites relative due dates (`:today`, `:tomorrow`, `:next-week`) to absolute ones, leaving
/// everything else, including the notes and the whitespace, untouched. A task that is queued
/// offline is retried on a later day, and must keep the date it was written for.
pub fn resolve_relative_dates(input: &str, today: NaiveDate) -> String {
    let (main, notes) = input.split_at(input.find(" n:").unwrap_or(input.len()));
    let mut out = String::with_capacity(input.len());
    for piece in main.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        match parse_due_token(word, today) {
            Some(date) => {
                out.push(':');
                out.push_str(&date);
                out.push_str(&piece[word.len()..]);
            }
            None => out.push_str(piece),
        }
    }
    out.push_str(notes);
    out
}

fn push_title_word(title: &mut String, word: &str) {
    if !title.is_empty() {
        title.push(' ');
    }
    title.push_str(word);
}

pub fn parse_task(input: &str) -> ParsedTask {
    parse_task_on(input, Local::now().date_naive())
}

pub fn parse_task_on(input: &str, today: NaiveDate) -> ParsedTask {
    let mut title = String::new();
    let mut list_names: Vec<String> = Vec::new();
    let mut duration_minutes: Option<i32> = None;
    let mut timeless_due_at: Option<String> = None;
    let mut label_names: Vec<String> = Vec::new();

    // Check for notes separator
    let (main_part, notes_part) = if let Some(pos) = input.find(" n:") {
        let (main, note) = input.split_at(pos);
        (main, note.trim_start_matches(" n:").trim())
    } else {
        (input, "")
    };

    let notes = notes_part.to_string();

    for word in main_part.split_whitespace() {
        if word.starts_with('.') {
            // Extract label
            let label = word.trim_start_matches('.');
            if !label.is_empty() {
                label_names.push(label.to_string());
            }
        } else if word.starts_with('@') {
            // Extract list name
            let list = word.trim_start_matches('@');
            if !list.is_empty() {
                list_names.push(list.to_string());
            }
        } else if word.starts_with(':') {
            if let Some(date) = parse_due_token(word, today) {
                timeless_due_at = Some(date);
            } else if let Some(duration) = parse_legacy_minutes(word) {
                duration_minutes = Some(duration);
            } else {
                // If parsing fails, include it in the title
                push_title_word(&mut title, word);
            }
        } else if word.starts_with('~') {
            if let Some(duration) = parse_duration_token(word) {
                duration_minutes = Some(duration);
            } else {
                push_title_word(&mut title, word);
            }
        } else {
            push_title_word(&mut title, word);
        }
    }

    ParsedTask {
        request: TaskRequest {
            title,
            list_id: None, // Will be resolved later
            duration_minutes,
            timeless_due_at,
            due_at: None,
            label_ids: Vec::new(), // Will be resolved later
            notes,
        },
        list_names,
        label_names,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()
    }

    fn parsed(input: &str) -> TaskRequest {
        parse_task_on(input, today()).request
    }

    #[test]
    fn tilde_durations_accept_minutes_hours_and_fractions() {
        assert_eq!(parsed("Run ~30m").duration_minutes, Some(30));
        assert_eq!(parsed("Run ~90").duration_minutes, Some(90));
        assert_eq!(parsed("Run ~2h").duration_minutes, Some(120));
        assert_eq!(parsed("Run ~1.5h").duration_minutes, Some(90));
        assert_eq!(parsed("Run ~0.5h").duration_minutes, Some(30));
        assert_eq!(parsed("Run ~30m").title, "Run");
    }

    #[test]
    fn words_that_only_look_like_durations_stay_in_the_title() {
        for word in ["~roughly", "~", "~0", "~0m", "~1.2.3", "~m", "~-5", "~30x"] {
            let task = parsed(&format!("Fix {word} thing"));
            assert_eq!(task.duration_minutes, None, "{word}");
            assert_eq!(task.title, format!("Fix {word} thing"), "{word}");
        }
    }

    #[test]
    fn due_dates_accept_keywords_and_both_numeric_forms() {
        assert_eq!(parsed("A :today").timeless_due_at.as_deref(), Some("2026-10-15"));
        assert_eq!(parsed("A :tomorrow").timeless_due_at.as_deref(), Some("2026-10-16"));
        assert_eq!(parsed("A :Next-Week").timeless_due_at.as_deref(), Some("2026-10-22"));
        assert_eq!(parsed("A :20261031").timeless_due_at.as_deref(), Some("2026-10-31"));
        assert_eq!(parsed("A :2026-10-31").timeless_due_at.as_deref(), Some("2026-10-31"));
        assert_eq!(parsed("A :tomorrow").title, "A");
        assert_eq!(parsed("A :tomorrow").duration_minutes, None);
    }

    #[test]
    fn colon_followed_by_a_short_number_is_still_minutes() {
        let task = parsed("Workout :45");
        assert_eq!(task.duration_minutes, Some(45));
        assert_eq!(task.timeless_due_at, None);
        assert_eq!(task.title, "Workout");
    }

    #[test]
    fn malformed_dates_stay_in_the_title() {
        for word in [":20261345", ":2026-13-01", ":someday", ":"] {
            let task = parsed(&format!("Do {word}"));
            assert_eq!(task.timeless_due_at, None, "{word}");
            assert_eq!(task.duration_minutes, None, "{word}");
            assert_eq!(task.title, format!("Do {word}"), "{word}");
        }
    }

    #[test]
    fn all_syntax_combines_and_notes_are_not_parsed() {
        let task = parse_task_on("Write post @Work .Writing ~2h :tomorrow n: call ~5m :today @Home", today());
        assert_eq!(task.request.title, "Write post");
        assert_eq!(task.list_names, vec!["Work"]);
        assert_eq!(task.label_names, vec!["Writing"]);
        assert_eq!(task.request.duration_minutes, Some(120));
        assert_eq!(task.request.timeless_due_at.as_deref(), Some("2026-10-16"));
        assert_eq!(task.request.notes, "call ~5m :today @Home");
    }

    #[test]
    fn every_list_in_the_task_is_reported_but_not_those_in_the_notes() {
        let task = parse_task_on("Plan @Work @Home n: ask @bob", today());
        assert_eq!(task.list_names, vec!["Work", "Home"]);
        let task = parse_task_on("Plan @Work n: ask @bob and @alice", today());
        assert_eq!(task.list_names, vec!["Work"]);
    }

    #[test]
    fn a_task_made_only_of_syntax_has_an_empty_title() {
        assert_eq!(parsed("@Work .Urgent ~10m").title, "");
        assert_eq!(parsed("   ").title, "");
    }

    #[test]
    fn the_request_serialises_the_due_date_and_label_ids() {
        let mut task = parsed("Pay rent :20261101");
        task.label_ids.push("label-1".to_string());
        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["timeless_due_at"], "2026-11-01");
        assert_eq!(json["label_ids"], serde_json::json!(["label-1"]));
        assert!(json.get("label_names").is_none());
        assert!(serde_json::to_value(parsed("Plain")).unwrap().get("timeless_due_at").is_none());
    }

    #[test]
    fn relative_dates_are_made_absolute_before_a_task_is_queued() {
        assert_eq!(
            resolve_relative_dates("Buy milk :tomorrow @Home ~10m", today()),
            "Buy milk :2026-10-16 @Home ~10m"
        );
        assert_eq!(
            resolve_relative_dates("Line one\nPlan :today  .Work\n:next-week", today()),
            "Line one\nPlan :2026-10-15  .Work\n:2026-10-22"
        );
        // Notes and tasks without relative dates are untouched.
        assert_eq!(
            resolve_relative_dates("Call n: ask about :tomorrow", today()),
            "Call n: ask about :tomorrow"
        );
        assert_eq!(resolve_relative_dates("Do :45 later", today()), "Do :45 later");
    }
}
