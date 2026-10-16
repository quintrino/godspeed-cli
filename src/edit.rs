//! `godspeed-cli edit`: change a task, complete it, snooze it, delete it or move it to another
//! list.
//!
//! The task is given by its id (`search --id` shows them) or by words from its title. With no
//! option a menu asks what to do; options (`--title`, `--complete`, `--snooze`, ...) do it
//! straight away, for scripts. Prompts go to stderr and results to stdout.

use crate::names::{self, NameMap};
use crate::store::{Store, StoredTask};
use crate::{api, dates, exit, paths, token};
use chrono::{Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde_json::{json, Map, Value};
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

#[derive(Debug, Default, PartialEq, Eq)]
struct Changes {
    title: Option<String>,
    notes: Option<String>,
    complete: Option<bool>,
    /// What was typed: a date or a time.
    snooze: Option<String>,
    /// What was typed: part of a list's name.
    move_to: Option<String>,
    delete: bool,
}

impl Changes {
    fn is_empty(&self) -> bool {
        *self == Changes::default()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    /// The id, or the words to find the task by, joined with spaces.
    target: String,
    changes: Changes,
    yes: bool,
}

/// The value of an option that takes one: the next argument, or the part after `=`.
fn value_of<'a>(
    arg: &'a str,
    name: &str,
    iter: &mut std::slice::Iter<'a, String>,
) -> Result<Option<String>, String> {
    if arg == name {
        let value = iter.next().ok_or_else(|| format!("{name} needs a value"))?;
        return Ok(Some(value.clone()));
    }
    Ok(arg.strip_prefix(&format!("{name}=")).map(str::to_string))
}

/// Options can go anywhere; `--` ends them. The other words say which task.
fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut words: Vec<&str> = Vec::new();
    let mut options_done = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if options_done || (!arg.starts_with("--") && arg != "-y") {
            words.push(arg);
            continue;
        }
        if let Some(value) = value_of(arg, "--title", &mut iter)? {
            parsed.changes.title = Some(value);
        } else if let Some(value) = value_of(arg, "--notes", &mut iter)? {
            parsed.changes.notes = Some(value);
        } else if let Some(value) = value_of(arg, "--snooze", &mut iter)? {
            parsed.changes.snooze = Some(value);
        } else if let Some(value) = value_of(arg, "--move", &mut iter)? {
            parsed.changes.move_to = Some(value);
        } else {
            match arg.as_str() {
                "--" => options_done = true,
                "--complete" => parsed.changes.complete = Some(true),
                "--incomplete" | "--reopen" => parsed.changes.complete = Some(false),
                "--delete" => parsed.changes.delete = true,
                "--yes" | "-y" => parsed.yes = true,
                other => return Err(format!("unknown option '{other}' (see `godspeed-cli edit --help`)")),
            }
        }
    }
    parsed.target = words.join(" ").trim().to_string();
    let c = &parsed.changes;
    if c.delete && (c.title.is_some() || c.notes.is_some() || c.complete.is_some() || c.snooze.is_some() || c.move_to.is_some()) {
        return Err("--delete can't be combined with other changes".to_string());
    }
    if c.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
        return Err("--title can't be empty".to_string());
    }
    Ok(parsed)
}

/// Whether `text` can be a task id, which goes into the URL: letters, digits, `-` and `_`.
fn looks_like_id(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn str_field<'a>(task: &'a Value, key: &str) -> &'a str {
    task.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The task as it is known: from the local copy if it is there.
#[derive(Debug, Clone)]
struct Target {
    id: String,
    title: Option<String>,
    complete: Option<bool>,
    list: Option<String>,
    due: Option<String>,
}

impl Target {
    fn bare(id: &str) -> Target {
        Target { id: id.to_string(), title: None, complete: None, list: None, due: None }
    }

    fn from_copy(task: &StoredTask, lists: &[Value]) -> Target {
        let list_id = str_field(&task.json, "list_id");
        let due = ["due_at", "timeless_due_at"].iter().map(|key| str_field(&task.json, key)).find(|due| !due.is_empty());
        Target {
            id: str_field(&task.json, "id").to_string(),
            title: Some(str_field(&task.json, "title").to_string()),
            complete: Some(task.complete),
            list: lists.iter().find(|list| str_field(list, "id") == list_id).map(|list| str_field(list, "name").to_string()),
            due: due.map(|due| due.chars().take(10).collect()),
        }
    }

    /// How to name the task to the person: its title, or else its id.
    fn name(&self) -> &str {
        self.title.as_deref().filter(|title| !title.trim().is_empty()).unwrap_or(&self.id)
    }

    fn summary(&self) -> String {
        let mut parts = vec![self.name().to_string()];
        if let Some(list) = &self.list {
            parts.push(format!("[{list}]"));
        }
        if let Some(due) = &self.due {
            parts.push(format!("due {due}"));
        }
        if self.complete == Some(true) {
            parts.push("(complete)".to_string());
        }
        parts.push(format!("id {}", self.id));
        parts.join("  ")
    }
}

/// The tasks in the copy that `text` names: the one with that id, or else those whose titles
/// contain every word of it, ignoring case.
fn candidates(tasks: &[StoredTask], text: &str) -> Vec<usize> {
    if let Some(index) = tasks.iter().position(|task| str_field(&task.json, "id") == text) {
        return vec![index];
    }
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            let title = str_field(&task.json, "title").to_lowercase();
            words.iter().all(|word| title.contains(word))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Reads one line, or None at the end of the input.
fn read_line(input: &mut dyn BufRead) -> Option<String> {
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

/// Asks which of several tasks is meant.
fn pick(options: &[Target], input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Target, String> {
    let _ = writeln!(out, "{} tasks match:", options.len());
    for (number, option) in options.iter().enumerate() {
        let _ = writeln!(out, "  {}) {}", number + 1, option.summary());
    }
    let _ = write!(out, "Which one? [1-{}, or q to quit]: ", options.len());
    let _ = out.flush();
    let answer = read_line(input).unwrap_or_default();
    match answer.parse::<usize>() {
        Ok(number) if (1..=options.len()).contains(&number) => Ok(options[number - 1].clone()),
        _ => Err("no task chosen".to_string()),
    }
}

/// Works out which task is meant. The copy is looked at if there is one; an id that isn't in it
/// is still used as it is, since the copy may be out of date.
fn find_target(
    text: &str,
    interactive: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Target, String> {
    let path = paths::store_path();
    let (tasks, lists) = if path.exists() {
        debug!("looking for '{text}' in the local copy {}", path.display());
        match Store::open(&path).and_then(|store| Ok((store.tasks(true)?, store.lists()?))) {
            Ok(found) => found,
            Err(e) => {
                debug!("the local copy could not be read: {e}");
                (Vec::new(), Vec::new())
            }
        }
    } else {
        debug!("there is no local copy to look in");
        (Vec::new(), Vec::new())
    };
    let found = candidates(&tasks, text);
    debug!("{} task(s) in the local copy match '{text}'", found.len());
    match found.as_slice() {
        [one] => Ok(Target::from_copy(&tasks[*one], &lists)),
        [] if looks_like_id(text) => {
            debug!("'{text}' is not in the local copy; using it as a task id");
            Ok(Target::bare(text))
        }
        [] => Err(format!("no task in the local copy matches '{text}'. Use `godspeed-cli search --id` to find its id.")),
        many => {
            let targets: Vec<Target> = many.iter().map(|&index| Target::from_copy(&tasks[index], &lists)).collect();
            if interactive {
                pick(&targets, input, out)
            } else {
                let listed: Vec<String> = targets.iter().map(|target| format!("  {}", target.summary())).collect();
                Err(format!("{} tasks match '{text}'; give the id of one:\n{}", targets.len(), listed.join("\n")))
            }
        }
    }
}

/// The menu: asks what to do with the task, and for what the action needs. Returns no changes
/// if the person quits.
fn ask_changes(target: &Target, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Changes, String> {
    let complete_label = if target.complete == Some(true) { "Mark incomplete" } else { "Complete" };
    let _ = writeln!(out, "{}", target.summary());
    let _ = writeln!(out, "  1) Edit the title");
    let _ = writeln!(out, "  2) Edit the notes");
    let _ = writeln!(out, "  3) {complete_label}");
    let _ = writeln!(out, "  4) Snooze");
    let _ = writeln!(out, "  5) Delete");
    let _ = writeln!(out, "  6) Move to another list");
    let _ = writeln!(out, "  q) Quit");
    let _ = write!(out, "Choose: ");
    let _ = out.flush();
    let choice = read_line(input).unwrap_or_default().to_lowercase();

    let mut ask = |question: &str| -> String {
        let _ = write!(out, "{question}");
        let _ = out.flush();
        read_line(input).unwrap_or_default()
    };
    let mut changes = Changes::default();
    match choice.as_str() {
        "1" | "title" | "t" => {
            let title = ask("New title: ");
            if title.is_empty() {
                return Err("the title can't be empty".to_string());
            }
            changes.title = Some(title);
        }
        "2" | "notes" | "n" => changes.notes = Some(ask("New notes (empty clears them): ")),
        "3" | "complete" | "c" => changes.complete = Some(target.complete != Some(true)),
        "4" | "snooze" | "s" => {
            let when = ask("Snooze until (tomorrow, friday 9am, 3h, 2026-10-31): ");
            if when.is_empty() {
                return Err("no date given".to_string());
            }
            changes.snooze = Some(when);
        }
        "5" | "delete" | "d" => changes.delete = true,
        "6" | "move" | "m" => {
            let list = ask("Move to which list (part of its name): ");
            if list.is_empty() {
                return Err("no list given".to_string());
            }
            changes.move_to = Some(list);
        }
        "q" | "quit" | "" => {}
        other => return Err(format!("'{other}' isn't one of the choices")),
    }
    Ok(changes)
}

/// A local moment as an RFC 3339 timestamp in UTC.
fn to_utc(local: NaiveDateTime) -> Option<String> {
    Local.from_local_datetime(&local).earliest().map(|time| time.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// The API field and value for snoozing until what was typed: a whole day (`timeless_...`) if
/// it names a day, or a moment otherwise. Never in the past.
fn snooze_field(text: &str, now: NaiveDateTime) -> Result<(&'static str, String), String> {
    let text = text.trim();
    // 20261031, as `add` accepts.
    let compact = (text.len() == 8).then(|| NaiveDate::parse_from_str(text, "%Y%m%d").ok()).flatten();
    let parsed = match compact {
        Some(day) => dates::Parsed { at: day.and_hms_opt(0, 0, 0).expect("midnight exists"), kind: dates::Kind::Day },
        None => dates::parse(text, now).ok_or_else(|| {
            format!("'{text}' isn't a date or time I can read (try tomorrow, friday 9am, 3h or 2026-10-31)")
        })?,
    };
    match parsed.kind {
        dates::Kind::Day => {
            if parsed.at.date() < now.date() {
                return Err(format!("{} is in the past", parsed.at.date()));
            }
            Ok(("timeless_snoozed_until", parsed.at.date().format("%Y-%m-%d").to_string()))
        }
        _ => {
            if parsed.at < now {
                return Err(format!("{} is in the past", parsed.at.format("%Y-%m-%d %H:%M")));
            }
            let moment = to_utc(parsed.at).ok_or_else(|| format!("{} doesn't exist in your time zone", parsed.at))?;
            Ok(("snoozed_until", moment))
        }
    }
}

/// The body of the PATCH request for these changes. `list_id` is the list to move to.
fn build_patch(changes: &Changes, list_id: Option<&str>, now: NaiveDateTime) -> Result<Value, String> {
    let mut body = Map::new();
    if let Some(title) = &changes.title {
        body.insert("title".to_string(), json!(title));
    }
    if let Some(notes) = &changes.notes {
        body.insert("notes".to_string(), json!(notes));
    }
    if let Some(complete) = changes.complete {
        body.insert("is_complete".to_string(), json!(complete));
    }
    if let Some(when) = &changes.snooze {
        let (field, value) = snooze_field(when, now)?;
        body.insert(field.to_string(), json!(value));
    }
    if let Some(list_id) = list_id {
        body.insert("list_id".to_string(), json!(list_id));
    }
    Ok(Value::Object(body))
}

/// The list that `text` names, as (id, name). Lists are fetched from the API and the cached
/// name -> id file is refreshed from them.
fn find_list(text: &str, api_key: &str) -> Result<(String, String), String> {
    let items = api::fetch_list_items(api_key).map_err(|e| format!("could not fetch your lists: {e}"))?;
    let map: NameMap = items
        .iter()
        .filter_map(|item| Some((str_field(item, "name").to_lowercase(), item.get("id")?.as_str()?.to_string())))
        .collect();
    if paths::ensure_data_dir().is_ok() {
        let _ = names::save(&paths::lists_path(), &map);
    }
    match names::find_list_key(&map, text) {
        Some(id) => {
            let name = items.iter().find(|item| str_field(item, "id") == id).map_or(text, |item| str_field(item, "name"));
            debug!("'{text}' is the list '{name}' ({id})");
            Ok((id.clone(), name.to_string()))
        }
        None => {
            let available: Vec<&str> = items.iter().map(|item| str_field(item, "name")).collect();
            Err(format!("no list matches '{text}'. Your lists: {}", available.join(", ")))
        }
    }
}

fn confirm_delete(target: &Target, input: &mut dyn BufRead, out: &mut dyn Write) -> bool {
    let _ = write!(out, "Delete '{}'? This can't be undone. [y/N]: ", target.name());
    let _ = out.flush();
    matches!(read_line(input).unwrap_or_default().to_lowercase().as_str(), "y" | "yes")
}

/// What to say once the changes are made.
fn describe(changes: &Changes, target: &Target, list_name: Option<&str>) -> String {
    let name = changes.title.as_deref().unwrap_or_else(|| target.name());
    let mut done: Vec<String> = Vec::new();
    if changes.title.is_some() {
        done.push("renamed".to_string());
    }
    if changes.notes.is_some() {
        done.push("notes changed".to_string());
    }
    match changes.complete {
        Some(true) => done.push("completed".to_string()),
        Some(false) => done.push("marked incomplete".to_string()),
        None => {}
    }
    if let Some(when) = &changes.snooze {
        done.push(format!("snoozed until {when}"));
    }
    if let Some(list) = list_name {
        done.push(format!("moved to {list}"));
    }
    format!("Updated '{name}': {}.", done.join(", "))
}

fn fail(message: &str) -> ExitCode {
    eprintln!("Error: {message}");
    exit::usage()
}

pub fn run_edit(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };
    if args.target.is_empty() {
        return fail("say which task to edit: godspeed-cli edit <id> (`godspeed-cli search --id` shows ids)");
    }
    let Some(api_key) = token::load_token() else {
        return fail(&format!(
            "GODSPEED_TOKEN is not set (environment variable, or an `export GODSPEED_TOKEN=...` line in {})",
            paths::token_file_path().display()
        ));
    };
    debug!("edit: {args:?}");

    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stderr();
    let interactive = args.changes.is_empty();

    let target = match find_target(&args.target, interactive, &mut input, &mut out) {
        Ok(target) => target,
        Err(message) => return fail(&message),
    };
    debug!("editing task {} ({:?})", target.id, target.title);

    let changes = if interactive {
        match ask_changes(&target, &mut input, &mut out) {
            Ok(changes) => changes,
            Err(message) => return fail(&message),
        }
    } else {
        args.changes
    };
    if changes.is_empty() {
        eprintln!("Nothing changed.");
        return exit::SUCCESS;
    }
    debug!("changes: {changes:?}");

    let store = {
        let path = paths::store_path();
        path.exists().then(|| Store::open(&path)).and_then(Result::ok)
    };

    if changes.delete {
        if !args.yes && !confirm_delete(&target, &mut input, &mut out) {
            eprintln!("Not deleted.");
            return exit::SUCCESS;
        }
        if let Err(e) = api::delete_task(&target.id, &api_key) {
            return api_failure("delete the task", &e.to_string());
        }
        if let Some(store) = &store {
            debug!("removing the task from the local copy");
            if let Err(e) = store.remove_task(&target.id) {
                eprintln!("Note: the local copy could not be updated ({e}); run `godspeed-cli refresh`.");
            }
        }
        println!("Deleted '{}'.", target.name());
        return exit::SUCCESS;
    }

    let list = match &changes.move_to {
        Some(text) => match find_list(text, &api_key) {
            Ok(found) => Some(found),
            Err(message) => return fail(&message),
        },
        None => None,
    };
    let patch = match build_patch(&changes, list.as_ref().map(|(id, _)| id.as_str()), Local::now().naive_local()) {
        Ok(patch) => patch,
        Err(message) => return fail(&message),
    };
    if let Err(e) = api::update_task(&target.id, &patch, &api_key) {
        return api_failure("update the task", &e.to_string());
    }
    if let Some(store) = &store {
        debug!("recording the change in the local copy");
        if let Err(e) = store.apply_edit(&target.id, &patch, changes.complete) {
            eprintln!("Note: the local copy could not be updated ({e}); run `godspeed-cli refresh`.");
        }
    }
    println!("{}", describe(&changes, &target, list.as_ref().map(|(_, name)| name.as_str())));
    exit::SUCCESS
}

fn api_failure(what: &str, reason: &str) -> ExitCode {
    eprintln!("Failed to {what}: {reason}");
    if reason.contains("404") {
        eprintln!("There may be no task with that id; `godspeed-cli search --id` shows them.");
    }
    exit::failure()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(10, 30, 0).unwrap()
    }

    fn task(id: &str, title: &str) -> StoredTask {
        StoredTask { json: json!({"id": id, "title": title, "list_id": "l1", "due_at": "2026-10-20T09:00:00Z"}), complete: false }
    }

    #[test]
    fn the_id_and_the_options_are_told_apart() {
        let parsed = parse_args(&args(&["abc-123", "--complete", "--title", "New name", "--snooze=tomorrow"])).unwrap();
        assert_eq!(parsed.target, "abc-123");
        assert_eq!(parsed.changes.complete, Some(true));
        assert_eq!(parsed.changes.title.as_deref(), Some("New name"));
        assert_eq!(parsed.changes.snooze.as_deref(), Some("tomorrow"));
        assert_eq!(parse_args(&args(&["buy", "milk"])).unwrap().target, "buy milk");
        assert!(parse_args(&args(&["x"])).unwrap().changes.is_empty());
        assert_eq!(parse_args(&args(&["--", "--odd"])).unwrap().target, "--odd");
    }

    #[test]
    fn bad_options_are_refused() {
        assert!(parse_args(&args(&["x", "--nope"])).unwrap_err().contains("unknown option"));
        assert!(parse_args(&args(&["x", "--title"])).unwrap_err().contains("needs a value"));
        assert!(parse_args(&args(&["x", "--delete", "--complete"])).unwrap_err().contains("can't be combined"));
        assert!(parse_args(&args(&["x", "--title", "  "])).is_err());
    }

    #[test]
    fn a_task_is_found_by_id_or_by_the_words_of_its_title() {
        let tasks = vec![task("a1", "Buy whole milk"), task("b2", "Pay rent"), task("c3", "Milk the cow")];
        assert_eq!(candidates(&tasks, "b2"), vec![1]);
        assert_eq!(candidates(&tasks, "MILK"), vec![0, 2]);
        assert_eq!(candidates(&tasks, "whole milk"), vec![0]);
        assert_eq!(candidates(&tasks, "rent milk"), Vec::<usize>::new());
        assert_eq!(candidates(&tasks, ""), Vec::<usize>::new());
    }

    #[test]
    fn only_safe_characters_are_taken_as_an_id() {
        assert!(looks_like_id("abc-123_X"));
        assert!(!looks_like_id("a/b"));
        assert!(!looks_like_id("two words"));
        assert!(!looks_like_id("../x"));
        assert!(!looks_like_id(""));
    }

    #[test]
    fn the_patch_has_only_what_changed() {
        let changes = Changes { title: Some("New".into()), complete: Some(true), ..Default::default() };
        let patch = build_patch(&changes, None, now()).unwrap();
        assert_eq!(patch, json!({"title": "New", "is_complete": true}));
        let moved = build_patch(&Changes::default(), Some("l-home"), now()).unwrap();
        assert_eq!(moved, json!({"list_id": "l-home"}));
        let notes = build_patch(&Changes { notes: Some(String::new()), ..Default::default() }, None, now()).unwrap();
        assert_eq!(notes, json!({"notes": ""}), "empty notes clear them");
    }

    #[test]
    fn snoozing_to_a_day_uses_the_timeless_field_and_to_a_time_uses_a_moment() {
        assert_eq!(snooze_field("2026-10-31", now()).unwrap(), ("timeless_snoozed_until", "2026-10-31".to_string()));
        assert_eq!(snooze_field("20261031", now()).unwrap(), ("timeless_snoozed_until", "2026-10-31".to_string()));
        assert_eq!(snooze_field("tomorrow", now()).unwrap(), ("timeless_snoozed_until", "2026-10-16".to_string()));
        let (field, value) = snooze_field("3h", now()).unwrap();
        assert_eq!(field, "snoozed_until");
        assert!(value.ends_with('Z') && value.len() == 20, "{value}");
    }

    #[test]
    fn snoozing_to_the_past_or_to_nonsense_is_refused() {
        assert!(snooze_field("2026-10-01", now()).unwrap_err().contains("in the past"));
        assert!(snooze_field("whenever", now()).unwrap_err().contains("isn't a date"));
    }

    fn menu(choice: &str, target: &Target) -> Result<Changes, String> {
        ask_changes(target, &mut Cursor::new(choice.as_bytes().to_vec()), &mut Vec::new())
    }

    #[test]
    fn the_menu_turns_choices_into_changes() {
        let target = Target::bare("t1");
        assert_eq!(menu("1\nNew title\n", &target).unwrap().title.as_deref(), Some("New title"));
        assert_eq!(menu("notes\nsome notes\n", &target).unwrap().notes.as_deref(), Some("some notes"));
        assert_eq!(menu("3\n", &target).unwrap().complete, Some(true));
        assert_eq!(menu("4\nfriday\n", &target).unwrap().snooze.as_deref(), Some("friday"));
        assert!(menu("5\n", &target).unwrap().delete);
        assert_eq!(menu("6\nhome\n", &target).unwrap().move_to.as_deref(), Some("home"));
        assert!(menu("q\n", &target).unwrap().is_empty());
        assert!(menu("", &target).unwrap().is_empty(), "the end of the input quits");
        assert!(menu("9\n", &target).is_err());
        assert!(menu("1\n\n", &target).is_err(), "an empty title is refused");
    }

    #[test]
    fn completing_a_complete_task_reopens_it() {
        let mut target = Target::bare("t1");
        target.complete = Some(true);
        assert_eq!(menu("3\n", &target).unwrap().complete, Some(false));
    }

    #[test]
    fn several_matches_are_chosen_by_number() {
        let options = vec![Target::bare("a"), Target::bare("b")];
        let chosen = pick(&options, &mut Cursor::new(b"2\n".to_vec()), &mut Vec::new()).unwrap();
        assert_eq!(chosen.id, "b");
        assert!(pick(&options, &mut Cursor::new(b"3\n".to_vec()), &mut Vec::new()).is_err());
        assert!(pick(&options, &mut Cursor::new(Vec::new()), &mut Vec::new()).is_err());
    }

    #[test]
    fn deleting_needs_a_yes() {
        let target = Target::bare("t1");
        let ask = |answer: &str| confirm_delete(&target, &mut Cursor::new(answer.as_bytes().to_vec()), &mut Vec::new());
        assert!(ask("y\n"));
        assert!(ask("YES\n"));
        assert!(!ask("n\n"));
        assert!(!ask("\n"));
        assert!(!ask(""));
    }

    #[test]
    fn the_summary_names_the_task_its_list_due_date_and_id() {
        let lists = vec![json!({"id": "l1", "name": "Home"})];
        let target = Target::from_copy(&task("a1", "Buy milk"), &lists);
        assert_eq!(target.summary(), "Buy milk  [Home]  due 2026-10-20  id a1");
        assert_eq!(Target::bare("zz").summary(), "zz  id zz");
    }
}
