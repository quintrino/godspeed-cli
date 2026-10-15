//! `godspeed-cli search`: find tasks in the local copy made by `refresh`. Works offline.

use crate::eval::Context;
use crate::store::{Store, StoredTask};
use crate::{exit, paths, present, query};
use chrono::{DateTime, Local, Utc};
use serde_json::Value;
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Debug, Default, PartialEq, Eq)]
struct SearchArgs {
    json: bool,
    query: Vec<String>,
}

/// `--json` can go anywhere; `--` ends the options, so a query can contain `--json` itself.
/// Everything else is the query. A lone leading `-` is a negation, not an option.
fn parse_args(args: &[String]) -> SearchArgs {
    let mut parsed = SearchArgs::default();
    let mut options_done = false;
    for arg in args {
        match arg.as_str() {
            "--" if !options_done => options_done = true,
            "--json" if !options_done => parsed.json = true,
            _ => parsed.query.push(arg.clone()),
        }
    }
    parsed
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

pub fn run_search(args: &[String]) -> ExitCode {
    let args = parse_args(args);
    let query = match query::parse(&args.query, Local::now().date_naive()) {
        Ok(query) => query,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };

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

    let context = Context::new(&loaded.lists, &loaded.labels, Local::now().date_naive());
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

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn json_can_appear_anywhere_and_the_rest_is_the_query() {
        assert_eq!(parse_args(&args(&[])), SearchArgs { json: false, query: vec![] });
        assert_eq!(
            parse_args(&args(&["label:Low", "--json", "due<=3d"])),
            SearchArgs { json: true, query: args(&["label:Low", "due<=3d"]) }
        );
        assert_eq!(parse_args(&args(&["--json"])), SearchArgs { json: true, query: vec![] });
    }

    #[test]
    fn a_leading_dash_is_a_negation_not_an_option() {
        assert_eq!(parse_args(&args(&["-label:Low"])), SearchArgs { json: false, query: args(&["-label:Low"]) });
        assert_eq!(parse_args(&args(&["-x", "-y"])).query, args(&["-x", "-y"]));
    }

    #[test]
    fn double_dash_ends_the_options() {
        assert_eq!(
            parse_args(&args(&["--", "--json", "milk"])),
            SearchArgs { json: false, query: args(&["--json", "milk"]) }
        );
        assert_eq!(
            parse_args(&args(&["--json", "--", "--", "x"])),
            SearchArgs { json: true, query: args(&["--", "x"]) }
        );
    }

    #[test]
    fn a_note_appears_once_the_copy_is_a_day_old() {
        let now = DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(age_note("2026-10-15T00:00:00Z", now), None);
        assert_eq!(age_note("2026-10-14T13:00:00Z", now), None);
        assert!(age_note("2026-10-14T12:00:00Z", now).unwrap().contains("1 day old"));
        assert!(age_note("2026-10-12T12:00:00Z", now).unwrap().contains("3 days old"));
        assert_eq!(age_note("not a date", now), None);
    }
}
