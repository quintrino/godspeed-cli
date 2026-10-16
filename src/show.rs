//! `godspeed-cli show`: everything the local copy knows about one task.
//!
//! The task is its id, or words from its title. It is read from the local copy made by
//! `refresh`; nothing is sent to Godspeed.

use crate::edit::{candidates, load_copy, looks_like_id, str_field, Target};
use crate::eval::{Context, DueStatus};
use crate::store::StoredTask;
use crate::{exit, paths, present, queue};
use chrono::{DateTime, Local};
use serde_json::Value;
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    json: bool,
    /// The id, or words from the title, joined with spaces.
    target: String,
}

/// `--json` can go anywhere; `--` ends the options. Everything else says which task.
fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut words: Vec<&str> = Vec::new();
    let mut options_done = false;
    for arg in args {
        if options_done {
            words.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => options_done = true,
            "--json" => parsed.json = true,
            other if other.starts_with("--") => return Err(format!("unknown option '{other}' (see `godspeed-cli show --help`)")),
            _ => words.push(arg),
        }
    }
    parsed.target = words.join(" ").trim().to_string();
    Ok(parsed)
}

/// A date or time from the task, for people: a date with no time as it is, a time as local
/// `YYYY-MM-DD HH:MM`. `timeless` and `timed` are the two fields it may be in.
fn when(task: &Value, timeless: &str, timed: &str) -> Option<String> {
    let plain = str_field(task, timeless);
    if !plain.is_empty() {
        return Some(plain.to_string());
    }
    let raw = str_field(task, timed);
    if raw.is_empty() {
        return None;
    }
    Some(match DateTime::parse_from_rfc3339(raw) {
        Ok(time) => time.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => raw.to_string(),
    })
}

/// 45 minutes as `45m`, 90 as `1h 30m`, 120 as `2h`.
fn minutes_text(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// How many changes to the task are waiting in the retry queue.
fn waiting_changes(id: &str) -> usize {
    queue::tasks().iter().filter_map(|entry| crate::edit::PendingEdit::parse(entry)).filter(|edit| edit.id == id).count()
}

/// The task as lines of `name  value`, with its title first and its notes at the end.
fn render(task: &StoredTask, ctx: &Context, waiting: usize) -> String {
    let json = &task.json;
    let title = str_field(json, "title");
    let mut out = vec![if title.trim().is_empty() { "(untitled)".to_string() } else { title.to_string() }];
    let mut row = |name: &str, value: String| out.push(format!("  {name:<10}{value}"));

    row("id", str_field(json, "id").to_string());
    row("status", if task.complete { "complete" } else { "incomplete" }.to_string());
    let lists = ctx.list_names(json);
    if !lists.is_empty() {
        row("list", lists.join(", "));
    }
    let labels = ctx.label_names(json);
    if !labels.is_empty() {
        row("labels", labels.join(", "));
    }
    if let Some(due) = when(json, "timeless_due_at", "due_at") {
        let note = match ctx.due_status(task) {
            Some(DueStatus::Overdue) => " (overdue)",
            Some(DueStatus::Today) => " (today)",
            None => "",
        };
        row("due", format!("{due}{note}"));
    }
    if let Some(snoozed) = when(json, "timeless_snoozed_until", "snoozed_until") {
        row("snoozed", format!("until {snoozed}"));
    }
    if let Some(start) = when(json, "timeless_starts_at", "starts_at") {
        row("starts", start);
    }
    if let Some(minutes) = json.get("duration_minutes").and_then(Value::as_i64).filter(|m| *m > 0) {
        row("duration", minutes_text(minutes));
    }
    if let Some(created) = when(json, "", "created_at") {
        row("created", created);
    }
    if let Some(updated) = when(json, "", "updated_at") {
        row("updated", updated);
    }
    if waiting > 0 {
        let plural = if waiting == 1 { "" } else { "s" };
        row("waiting", format!("{waiting} change{plural} not yet sent (see `godspeed-cli queue`)"));
    }
    let notes = str_field(json, "notes").trim_end();
    if !notes.is_empty() {
        out.push(String::new());
        out.push("Notes:".to_string());
        out.extend(notes.lines().map(|line| format!("  {line}")));
    }
    out.join("\n")
}

pub fn run(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };
    if args.target.is_empty() {
        eprintln!("Error: say which task to show: godspeed-cli show <id | words> (`godspeed-cli search --id` shows ids)");
        return exit::usage();
    }
    if !paths::store_path().exists() {
        eprintln!("Error: there is no local copy of your tasks yet. Run `godspeed-cli refresh` first.");
        return exit::usage();
    }
    debug!("show: looking for '{}' in the local copy", args.target);
    let copy = load_copy();
    let found = candidates(&copy.tasks, &args.target);
    debug!("show: {} task(s) match", found.len());
    let task = match found.as_slice() {
        [one] => &copy.tasks[*one],
        [] => {
            let hint = if looks_like_id(&args.target) { "no task has that id" } else { "no title matches" };
            eprintln!("Error: {hint} in the local copy ('{}'). `godspeed-cli refresh` brings it up to date.", args.target);
            return exit::usage();
        }
        many => {
            let listed: Vec<String> =
                many.iter().map(|&index| format!("  {}", Target::from_copy(&copy.tasks[index], &copy.lists).summary())).collect();
            eprintln!("Error: {} tasks match '{}'; give the id of one:\n{}", many.len(), args.target, listed.join("\n"));
            return exit::usage();
        }
    };

    let context = Context::new(&copy.lists, &copy.labels, Local::now().naive_local());
    let id = str_field(&task.json, "id");
    let output = if args.json {
        serde_json::to_string_pretty(&present::task_json(task, &context)).expect("the task serialises")
    } else {
        render(task, &context, waiting_changes(id))
    };
    // Ignore a closed pipe.
    let _ = writeln!(io::stdout(), "{output}");
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use serde_json::json;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn context() -> Context {
        Context::new(
            &[json!({"id": "l-home", "name": "Home"})],
            &[json!({"id": "x-urgent", "name": "Urgent"}), json!({"id": "x-low", "name": "Low"})],
            NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(10, 30, 0).unwrap(),
        )
    }

    #[test]
    fn the_words_say_which_task_and_json_can_go_anywhere() {
        assert_eq!(parse_args(&args(&["buy", "--json", "milk"])).unwrap(), Args { json: true, target: "buy milk".into() });
        assert_eq!(parse_args(&args(&["--", "--json"])).unwrap().target, "--json");
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert_eq!(parse_args(&args(&[])).unwrap().target, "");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(minutes_text(45), "45m");
        assert_eq!(minutes_text(60), "1h");
        assert_eq!(minutes_text(90), "1h 30m");
        assert_eq!(minutes_text(120), "2h");
    }

    #[test]
    fn a_date_with_no_time_is_shown_as_it_is_and_a_time_in_local_time() {
        let task = json!({"timeless_due_at": "2026-10-31", "snoozed_until": "bad", "starts_at": "2026-10-20T12:00:00Z"});
        assert_eq!(when(&task, "timeless_due_at", "due_at").as_deref(), Some("2026-10-31"));
        assert_eq!(when(&task, "", "snoozed_until").as_deref(), Some("bad"));
        let start = when(&task, "timeless_starts_at", "starts_at").unwrap();
        assert!(start.starts_with("2026-10-20 ") || start.starts_with("2026-10-21 ") || start.starts_with("2026-10-19 "), "{start}");
        assert_eq!(when(&task, "timeless_x", "nothing"), None);
    }

    #[test]
    fn the_task_is_laid_out_with_what_it_has_and_its_notes_last() {
        let task = StoredTask {
            json: json!({"id": "t1", "title": "Pay rent", "list_id": "l-home", "label_ids": ["x-urgent", "x-low"],
                          "timeless_due_at": "2026-10-10", "duration_minutes": 90, "notes": "ring the landlord\nask about the heating\n",
                          "created_at": "bad", "updated_at": "bad"}),
            complete: false,
        };

        let text = render(&task, &context(), 2);

        assert_eq!(
            text,
            "Pay rent\n  id        t1\n  status    incomplete\n  list      Home\n  labels    Urgent, Low\n  due       2026-10-10 (overdue)\n  duration  1h 30m\n  created   bad\n  updated   bad\n  waiting   2 changes not yet sent (see `godspeed-cli queue`)\n\nNotes:\n  ring the landlord\n  ask about the heating"
        );
    }

    #[test]
    fn a_bare_task_shows_only_what_it_has() {
        let task = StoredTask { json: json!({"id": "t2", "title": ""}), complete: true };

        let text = render(&task, &context(), 0);

        assert_eq!(text, "(untitled)\n  id        t2\n  status    complete");
    }
}
