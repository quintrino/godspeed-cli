//! `godspeed-cli search`: find tasks in the local copy made by `refresh`.
//!
//! It works offline unless asked to refresh first (`--refresh`, `--max-age`, or `max_age` in
//! the config file); only then does it need a token and the network.

use crate::config::{self, Age};
use crate::eval::Context;
use crate::refresh;
use crate::store::{Facts, Store, StoredTask};
use crate::{exit, paths, present, query, token};
use chrono::{DateTime, Local, Utc};
use serde_json::Value;
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Debug, Default, PartialEq, Eq)]
struct SearchArgs {
    json: bool,
    /// Refresh the copy before searching.
    refresh: bool,
    /// Refresh first if the copy is older than this. Overrides the config file.
    max_age: Option<Age>,
    query: Vec<String>,
}

/// `--json`, `--refresh` and `--max-age AGE` (or `--max-age=AGE`) can go anywhere; `--` ends the
/// options, so a query can contain them. Everything else is the query. A lone leading `-` is a
/// negation, not an option.
fn parse_args(args: &[String]) -> Result<SearchArgs, String> {
    let mut parsed = SearchArgs::default();
    let mut options_done = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if options_done {
            parsed.query.push(arg.clone());
            continue;
        }
        let max_age_value = if arg == "--max-age" {
            Some(iter.next().ok_or("--max-age needs a duration, such as --max-age 6h")?.as_str())
        } else {
            arg.strip_prefix("--max-age=")
        };
        if let Some(value) = max_age_value {
            parsed.max_age = Some(
                Age::parse(value)
                    .ok_or_else(|| format!("--max-age: '{value}' is not a duration (use something like 30m, 6h, \"2 days\" or never)"))?,
            );
            continue;
        }
        match arg.as_str() {
            "--" => options_done = true,
            "--json" => parsed.json = true,
            "--refresh" => parsed.refresh = true,
            _ => parsed.query.push(arg.clone()),
        }
    }
    Ok(parsed)
}

/// Whether a copy last refreshed at `refreshed_at` is too old. A copy with no usable time is
/// always too old.
fn is_stale(max_age: Age, refreshed_at: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(limit) = max_age.limit() else { return false };
    match refreshed_at.and_then(|raw| DateTime::parse_from_rfc3339(raw).ok()) {
        Some(refreshed) => now - refreshed.with_timezone(&Utc) >= limit,
        None => true,
    }
}

/// Whether to refresh before searching: when asked to, or when the copy is older than the age
/// from `--max-age`, or else from the config file.
fn refresh_wanted(args: &SearchArgs, config_max_age: Option<Age>, refreshed_at: Option<&str>, now: DateTime<Utc>) -> bool {
    if args.refresh {
        return true;
    }
    args.max_age.or(config_max_age).is_some_and(|age| is_stale(age, refreshed_at, now))
}

/// A note when the local copy is a day or more old.
fn age_note(refreshed_at: &str, now: DateTime<Utc>) -> Option<String> {
    let refreshed = DateTime::parse_from_rfc3339(refreshed_at).ok()?;
    let days = (now - refreshed.with_timezone(&Utc)).num_days();
    (days >= 1).then(|| {
        format!(
            "Note: the local copy is {days} day{} old; run `godspeed-cli refresh` to update it.",
            if days == 1 { "" } else { "s" }
        )
    })
}

struct Loaded {
    tasks: Vec<StoredTask>,
    lists: Vec<Value>,
    labels: Vec<Value>,
    complete_count: usize,
    refreshed_at: Option<String>,
}

fn load(store: &Store, include_completed: bool) -> Result<Loaded, Box<dyn std::error::Error>> {
    Ok(Loaded {
        tasks: store.tasks(include_completed)?,
        lists: store.lists()?,
        labels: store.labels()?,
        complete_count: store.complete_count()?,
        refreshed_at: store.refreshed_at()?,
    })
}

/// What is known about the existing copy, or the defaults if there is none.
fn existing_facts() -> Option<Facts> {
    let path = paths::store_path();
    if !path.exists() {
        return None;
    }
    Store::open(&path).and_then(|store| store.facts()).ok()
}

pub fn run_search(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };
    let now = Local::now().naive_local();
    let query = match query::parse(&args.query, now) {
        Ok(query) => query,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };
    let config = match config::load() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };

    // Bring the copy up to date first, if asked to or if it is too old.
    let had_copy = paths::store_path().exists();
    let facts = existing_facts().unwrap_or_default();
    if refresh_wanted(&args, config.max_age, facts.refreshed_at.as_deref(), Utc::now()) {
        let refreshed = match token::load_token() {
            Some(api_key) => {
                let options = refresh::Options {
                    full: false,
                    // Never quietly drop completed tasks the copy already has.
                    include_completed: config.include_completed
                        || query.include_completed
                        || facts.include_completed == Some(true),
                    full_refresh_after: config.full_refresh_after,
                };
                refresh::perform(&api_key, &options).map(|_| ()).map_err(|e| format!("the refresh failed: {e}"))
            }
            None => Err("GODSPEED_TOKEN is not set, so the copy can't be refreshed".to_string()),
        };
        if let Err(reason) = refreshed {
            if !had_copy {
                eprintln!("Error: there is no local copy of your tasks yet, and {reason}.");
                return exit::failure();
            }
            eprintln!("Note: {reason}; searching the existing copy.");
        }
    }

    let path = paths::store_path();
    if !path.exists() {
        eprintln!("Error: there is no local copy of your tasks yet. Run `godspeed-cli refresh` first.");
        return exit::usage();
    }
    let loaded = match Store::open(&path).and_then(|store| load(&store, query.include_completed)) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("Error: cannot read the local copy: {e}");
            return exit::failure();
        }
    };

    if let Some(note) = loaded.refreshed_at.as_deref().and_then(|at| age_note(at, Utc::now())) {
        eprintln!("{note}");
    }
    if query.include_completed && loaded.complete_count == 0 {
        eprintln!("Note: the local copy has no completed tasks; run `godspeed-cli refresh --include-completed` to add them.");
    }

    let context = Context::new(&loaded.lists, &loaded.labels, now);
    let matched: Vec<&StoredTask> = loaded.tasks.iter().filter(|task| context.matches(&query.expr, task)).collect();
    if matched.is_empty() && !args.json {
        eprintln!("No matching tasks.");
    }

    let output = present::render(&query, matched, &context, args.json);
    if !output.is_empty() {
        // Ignore a closed pipe (`... | head`) instead of panicking.
        let _ = writeln!(io::stdout(), "{output}");
    }
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn parsed(list: &[&str]) -> SearchArgs {
        parse_args(&args(list)).unwrap()
    }

    fn hours(n: i64) -> Age {
        Age::After(Duration::hours(n))
    }

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn json_can_appear_anywhere_and_the_rest_is_the_query() {
        assert_eq!(parsed(&[]), SearchArgs::default());
        assert_eq!(
            parsed(&["label:Low", "--json", "due<=3d"]),
            SearchArgs { json: true, query: args(&["label:Low", "due<=3d"]), ..Default::default() }
        );
        assert_eq!(parsed(&["--json"]), SearchArgs { json: true, ..Default::default() });
    }

    #[test]
    fn refresh_and_max_age_flags() {
        assert!(parsed(&["--refresh", "milk"]).refresh);
        assert_eq!(parsed(&["milk", "--refresh"]).query, args(&["milk"]));
        assert_eq!(parsed(&["--max-age", "6h", "milk"]).max_age, Some(hours(6)));
        assert_eq!(parsed(&["--max-age=2 days", "milk"]).max_age, Some(Age::After(Duration::days(2))));
        assert_eq!(parsed(&["milk", "--max-age", "never"]).max_age, Some(Age::Never));
        assert_eq!(parsed(&["--max-age", "0"]).max_age, Some(Age::After(Duration::zero())));
        let both = parsed(&["--refresh", "--max-age", "1h", "--json", "-x", "y"]);
        assert!(both.refresh && both.json);
        assert_eq!(both.query, args(&["-x", "y"]));
    }

    #[test]
    fn a_bad_or_missing_max_age_is_an_error() {
        assert!(parse_args(&args(&["--max-age"])).unwrap_err().contains("needs a duration"));
        assert!(parse_args(&args(&["--max-age", "soon"])).unwrap_err().contains("'soon' is not a duration"));
        assert!(parse_args(&args(&["--max-age=soon"])).unwrap_err().contains("'soon' is not a duration"));
    }

    #[test]
    fn a_leading_dash_is_a_negation_not_an_option() {
        assert_eq!(parsed(&["-label:Low"]).query, args(&["-label:Low"]));
        assert_eq!(parsed(&["-x", "-y"]).query, args(&["-x", "-y"]));
    }

    #[test]
    fn double_dash_ends_the_options_so_the_query_can_contain_them() {
        assert_eq!(parsed(&["--", "--json", "milk"]), SearchArgs { query: args(&["--json", "milk"]), ..Default::default() });
        assert_eq!(
            parsed(&["--", "--refresh", "--max-age", "1h"]).query,
            args(&["--refresh", "--max-age", "1h"])
        );
        assert_eq!(parsed(&["--json", "--", "--", "x"]), SearchArgs { json: true, query: args(&["--", "x"]), ..Default::default() });
    }

    #[test]
    fn a_copy_is_stale_once_it_reaches_the_age_and_a_copy_with_no_time_always_is() {
        let now = at("2026-10-15T12:00:00Z");
        assert!(!is_stale(hours(6), Some("2026-10-15T07:00:00Z"), now));
        assert!(is_stale(hours(6), Some("2026-10-15T06:00:00Z"), now), "exactly the age counts as stale");
        assert!(is_stale(hours(6), Some("2026-10-14T00:00:00Z"), now));
        assert!(is_stale(hours(6), None, now));
        assert!(is_stale(hours(6), Some("garbage"), now));
        assert!(is_stale(Age::After(Duration::zero()), Some("2026-10-15T12:00:00Z"), now), "0 means always");
        assert!(!is_stale(Age::Never, None, now));
        assert!(!is_stale(Age::Never, Some("2000-01-01T00:00:00Z"), now));
    }

    #[test]
    fn refresh_wanted_follows_the_flag_then_max_age_then_the_config() {
        let now = at("2026-10-15T12:00:00Z");
        let fresh = Some("2026-10-15T11:00:00Z");
        let old = Some("2026-10-10T11:00:00Z");
        let none = SearchArgs::default();

        // Nothing asked for: never refresh by itself.
        assert!(!refresh_wanted(&none, None, old, now));
        // The flag always refreshes.
        assert!(refresh_wanted(&SearchArgs { refresh: true, ..Default::default() }, None, fresh, now));
        // The config's max_age applies when there is no flag.
        assert!(refresh_wanted(&none, Some(hours(6)), old, now));
        assert!(!refresh_wanted(&none, Some(hours(6)), fresh, now));
        // --max-age overrides the config, either way, including turning it off.
        let strict = SearchArgs { max_age: Some(hours(1)), ..Default::default() };
        assert!(refresh_wanted(&strict, Some(hours(48)), Some("2026-10-15T10:30:00Z"), now));
        let never = SearchArgs { max_age: Some(Age::Never), ..Default::default() };
        assert!(!refresh_wanted(&never, Some(hours(6)), old, now));
        let lenient = SearchArgs { max_age: Some(hours(48)), ..Default::default() };
        assert!(!refresh_wanted(&lenient, Some(hours(1)), old.map(|_| "2026-10-14T12:00:00Z"), now));
        // A copy that doesn't exist yet is always stale.
        assert!(refresh_wanted(&none, Some(hours(6)), None, now));
    }

    #[test]
    fn a_note_appears_once_the_copy_is_a_day_old() {
        let now = at("2026-10-15T12:00:00Z");
        assert_eq!(age_note("2026-10-15T00:00:00Z", now), None);
        assert_eq!(age_note("2026-10-14T13:00:00Z", now), None);
        assert!(age_note("2026-10-14T12:00:00Z", now).unwrap().contains("1 day old"));
        assert!(age_note("2026-10-12T12:00:00Z", now).unwrap().contains("3 days old"));
        assert_eq!(age_note("not a date", now), None);
    }
}
