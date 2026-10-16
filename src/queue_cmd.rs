//! `godspeed-cli queue`: what is waiting to be sent, and what to do about it.
//!
//! - `queue` (or `queue list`) shows what is waiting: tasks that `add` couldn't send, and
//!   changes that `edit` couldn't send.
//! - `queue retry` tries to send it all now.
//! - `queue drop N...` or `queue drop --all` throws waiting items away.

use crate::edit::{read_line, PendingEdit};
use crate::{exit, pending, queue, token};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::process::ExitCode;

/// One thing in the queue.
#[derive(Debug, PartialEq)]
enum Item {
    /// A task to create, as it was typed.
    Add(String),
    Edit(PendingEdit),
}

fn items() -> Vec<Item> {
    queue::tasks()
        .into_iter()
        .map(|entry| match PendingEdit::parse(&entry) {
            Some(edit) => Item::Edit(edit),
            None => Item::Add(entry),
        })
        .collect()
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shorten(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// What the item will do, on one line.
fn describe(item: &Item) -> (&'static str, String) {
    match item {
        Item::Add(text) => ("add", single_line(text)),
        Item::Edit(edit) => {
            let what = if edit.delete {
                "delete".to_string()
            } else {
                let fields: Vec<String> = edit
                    .patch
                    .as_ref()
                    .and_then(Value::as_object)
                    .map(|object| object.iter().map(|(key, value)| format!("{key}={value}")).collect())
                    .unwrap_or_default();
                fields.join(" ")
            };
            ("edit", format!("{} ({}): {}", single_line(edit.name()), edit.id, shorten(&what, 100)))
        }
    }
}

fn item_json(number: usize, item: &Item) -> Value {
    match item {
        Item::Add(text) => json!({"number": number, "kind": "add", "text": text}),
        Item::Edit(edit) => json!({
            "number": number,
            "kind": "edit",
            "id": edit.id,
            "title": edit.title,
            "delete": edit.delete,
            "patch": edit.patch,
        }),
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ListArgs {
    json: bool,
    count: bool,
}

fn parse_list_args(args: &[String]) -> Result<ListArgs, String> {
    let mut parsed = ListArgs::default();
    for arg in args {
        match arg.as_str() {
            "--json" => parsed.json = true,
            "--count" => parsed.count = true,
            other => return Err(format!("unknown argument '{other}' for queue (see `godspeed-cli queue --help`)")),
        }
    }
    if parsed.json && parsed.count {
        return Err("--json and --count can't be combined".to_string());
    }
    Ok(parsed)
}

fn list(args: &[String]) -> ExitCode {
    let args = match parse_list_args(args) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };
    let items = items();
    debug!("queue: {} item(s) waiting in {}", items.len(), crate::paths::queue_path().display());
    let output = if args.count {
        items.len().to_string()
    } else if args.json {
        let entries: Vec<Value> = items.iter().enumerate().map(|(index, item)| item_json(index + 1, item)).collect();
        serde_json::to_string_pretty(&entries).expect("the queue serialises")
    } else if items.is_empty() {
        eprintln!("Nothing is waiting to be sent.");
        String::new()
    } else {
        let mut lines = vec!["#   KIND  WAITING".to_string()];
        for (index, item) in items.iter().enumerate() {
            let (kind, text) = describe(item);
            lines.push(format!("{:<3} {kind:<5} {text}", index + 1));
        }
        lines.join("\n")
    };
    if !output.is_empty() {
        // Ignore a closed pipe.
        let _ = writeln!(io::stdout(), "{output}");
    }
    exit::SUCCESS
}

fn retry() -> ExitCode {
    let Some(api_key) = token::load_token() else {
        return fail("GODSPEED_TOKEN is not set, so nothing can be sent");
    };
    let before = items().len();
    if before == 0 {
        eprintln!("Nothing is waiting to be sent.");
        return exit::SUCCESS;
    }
    pending::process(&api_key);
    let after = items().len();
    println!("{} of {before} sent or dropped; {after} still waiting.", before - after.min(before));
    if after == 0 {
        exit::SUCCESS
    } else {
        exit::failure()
    }
}

/// `drop` takes the numbers `queue` shows, or `--all`; `--yes` skips the question.
#[derive(Debug, Default, PartialEq, Eq)]
struct DropArgs {
    numbers: Vec<usize>,
    all: bool,
    yes: bool,
}

fn parse_drop_args(args: &[String]) -> Result<DropArgs, String> {
    let mut parsed = DropArgs::default();
    for arg in args {
        match arg.as_str() {
            "--all" => parsed.all = true,
            "--yes" | "-y" => parsed.yes = true,
            number => match number.parse::<usize>() {
                Ok(n) if n >= 1 => parsed.numbers.push(n),
                _ => return Err(format!("'{number}' isn't an item number (see `godspeed-cli queue`)")),
            },
        }
    }
    if parsed.all != parsed.numbers.is_empty() {
        return Err("say which items to drop: their numbers from `godspeed-cli queue`, or --all".to_string());
    }
    Ok(parsed)
}

fn drop_items(args: &[String]) -> ExitCode {
    let args = match parse_drop_args(args) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };
    let items = items();
    let mut indexes: Vec<usize> = if args.all { (0..items.len()).collect() } else { args.numbers.iter().map(|n| n - 1).collect() };
    indexes.sort_unstable();
    indexes.dedup();
    if let Some(bad) = indexes.iter().find(|index| **index >= items.len()) {
        return fail(&format!("there is no item {} (the queue has {})", bad + 1, items.len()));
    }
    if indexes.is_empty() {
        eprintln!("Nothing is waiting to be sent.");
        return exit::SUCCESS;
    }
    let plural = if indexes.len() == 1 { "" } else { "s" };
    if !args.yes {
        let mut err = io::stderr();
        for &index in &indexes {
            let (kind, text) = describe(&items[index]);
            let _ = writeln!(err, "  {} {kind:<5} {text}", index + 1);
        }
        let _ = write!(err, "Drop {} item{plural}? They will never be sent. [y/N]: ", indexes.len());
        let _ = err.flush();
        let answer = read_line(&mut io::stdin().lock()).unwrap_or_default().to_lowercase();
        if !matches!(answer.as_str(), "y" | "yes") {
            eprintln!("Nothing dropped.");
            return exit::SUCCESS;
        }
    }
    if let Err(e) = queue::remove_indexes(&indexes) {
        eprintln!("Error: cannot update the queue: {e}");
        return exit::failure();
    }
    println!("Dropped {} item{plural}.", indexes.len());
    if indexes.iter().any(|index| matches!(items[*index], Item::Edit(_))) {
        eprintln!(
            "Note: the local copy still shows the changes you dropped until those tasks change in Godspeed; \
             `godspeed-cli refresh --full` makes it match Godspeed again."
        );
    }
    exit::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    eprintln!("Error: {message}");
    exit::usage()
}

pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None => list(&[]),
        Some("list") => list(&args[1..]),
        Some("retry" | "send") if args.len() == 1 => retry(),
        Some("retry" | "send") => fail("retry takes no arguments"),
        Some("drop") => drop_items(&args[1..]),
        Some(flag) if flag.starts_with("--") => list(args),
        Some(other) => fail(&format!("'{other}' isn't a queue command (list, retry or drop; see `godspeed-cli queue --help`)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_added_task_and_a_change_are_described_on_one_line() {
        assert_eq!(describe(&Item::Add("Buy  milk\n@Home".into())), ("add", "Buy milk @Home".to_string()));
        let edit = PendingEdit {
            id: "t3".into(),
            title: Some("Buy milk".into()),
            patch: Some(json!({"is_complete": true, "title": "Tea"})),
            delete: false,
        };
        assert_eq!(describe(&Item::Edit(edit)), ("edit", "Buy milk (t3): is_complete=true title=\"Tea\"".to_string()));
        let delete = PendingEdit { id: "t4".into(), title: None, patch: None, delete: true };
        assert_eq!(describe(&Item::Edit(delete)), ("edit", "t4 (t4): delete".to_string()));
    }

    #[test]
    fn a_long_change_is_cut() {
        let long = "x".repeat(300);
        let edit = PendingEdit { id: "t".into(), title: None, patch: Some(json!({ "notes": long })), delete: false };
        let (_, text) = describe(&Item::Edit(edit));
        assert!(text.chars().count() < 140 && text.ends_with('…'), "{text}");
    }

    #[test]
    fn list_flags_are_json_or_count_but_not_both() {
        assert_eq!(parse_list_args(&args(&["--count"])).unwrap(), ListArgs { count: true, json: false });
        assert!(parse_list_args(&args(&["--json", "--count"])).is_err());
        assert!(parse_list_args(&args(&["x"])).is_err());
    }

    #[test]
    fn drop_takes_numbers_or_all() {
        assert_eq!(parse_drop_args(&args(&["2", "1", "--yes"])).unwrap(), DropArgs { numbers: vec![2, 1], all: false, yes: true });
        assert!(parse_drop_args(&args(&["--all"])).unwrap().all);
        assert!(parse_drop_args(&args(&[])).is_err(), "nothing named");
        assert!(parse_drop_args(&args(&["--all", "1"])).is_err(), "both");
        assert!(parse_drop_args(&args(&["0"])).is_err());
        assert!(parse_drop_args(&args(&["two"])).is_err());
    }
}
