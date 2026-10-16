//! Creating a task: parse it, resolve its list and labels, send it, and queue it if that fails.
//!
//! Besides the task syntax (`.label @list ~30m :tomorrow n: notes`), options straight after
//! `add` set the same things `edit` can change: `--due`, `--duration`, `--list`, `--label`,
//! `--notes`, `--snooze`, `--start` and `--complete`. `--each` makes every line (or argument) a
//! separate task, for piping a list in, and `--dry-run` shows what would be created without
//! sending anything.

use crate::edit::{self, PendingEdit};
use crate::names::{find_list_key, find_matching_key, NameMap};
use crate::notify::send_notification;
use crate::task::{parse_task, resolve_relative_dates, ParsedTask, TaskRequest};
use crate::{api, dates, exit, names, paths, pending, queue, token};
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::io::{self, Read, Write};
use std::process::ExitCode;

pub enum TaskError {
    /// The task itself is wrong, so sending it again can never work and it must not be queued.
    Invalid(String),
    /// The task is fine but couldn't be sent, so it is queued and retried.
    Failed(Box<dyn std::error::Error>),
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Invalid(message) => f.write_str(message),
            TaskError::Failed(error) => write!(f, "{error}"),
        }
    }
}

/// What the options after `add` ask for, with every date already made absolute, so that a task
/// kept for a retry means the same thing whenever it is sent.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// A list name, to be matched when the task is sent. It replaces an `@list` in the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
    /// Label names, added to any `.label`s in the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeless_due_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeless_snoozed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeless_starts_at: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub complete: bool,
}

impl AddOptions {
    pub fn is_empty(&self) -> bool {
        *self == AddOptions::default()
    }

    /// What can't be set when a task is created, and is sent as a change to the new task right
    /// after: the snooze, the start date and being complete.
    fn follow_up(&self) -> Option<Value> {
        let mut patch = serde_json::Map::new();
        for (key, value) in [
            ("snoozed_until", &self.snoozed_until),
            ("timeless_snoozed_until", &self.timeless_snoozed_until),
            ("starts_at", &self.starts_at),
            ("timeless_starts_at", &self.timeless_starts_at),
        ] {
            if let Some(value) = value {
                patch.insert(key.to_string(), json!(value));
            }
        }
        if self.complete {
            patch.insert("is_complete".to_string(), json!(true));
        }
        (!patch.is_empty()).then_some(Value::Object(patch))
    }

    /// The options as `name value` words, for showing what a waiting task will do.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut put = |name: &str, value: Option<String>| {
            if let Some(value) = value {
                parts.push(format!("{name} {value}"));
            }
        };
        put("list", self.list.clone());
        if !self.labels.is_empty() {
            put("labels", Some(self.labels.join(",")));
        }
        put("due", self.timeless_due_at.clone().or_else(|| self.due_at.clone()));
        put("duration", self.duration_minutes.map(|m| format!("{m}m")));
        put("snooze", self.timeless_snoozed_until.clone().or_else(|| self.snoozed_until.clone()));
        put("start", self.timeless_starts_at.clone().or_else(|| self.starts_at.clone()));
        if self.notes.is_some() {
            parts.push("notes".to_string());
        }
        if self.complete {
            parts.push("complete".to_string());
        }
        parts.join(", ")
    }
}

/// A task in the retry queue that was added with options: one line of JSON. A task without
/// options is kept as its plain text, as it always was.
#[derive(Debug, Serialize, Deserialize)]
struct QueuedAdd {
    text: String,
    #[serde(default)]
    options: AddOptions,
}

#[derive(Serialize, Deserialize)]
struct QueuedAddEntry {
    queued_add: QueuedAdd,
}

fn queue_entry(text: &str, options: &AddOptions) -> String {
    if options.is_empty() {
        return text.to_string();
    }
    serde_json::to_string(&QueuedAddEntry { queued_add: QueuedAdd { text: text.to_string(), options: options.clone() } })
        .expect("a task serialises")
}

/// The task text and options a retry queue entry holds.
pub fn split_entry(entry: &str) -> (String, AddOptions) {
    if entry.starts_with("{\"queued_add\":") {
        if let Ok(parsed) = serde_json::from_str::<QueuedAddEntry>(entry) {
            return (parsed.queued_add.text, parsed.queued_add.options);
        }
    }
    (entry.to_string(), AddOptions::default())
}

/// The options as typed after `add`, before dates are made absolute.
#[derive(Debug, Default, PartialEq, Eq)]
struct Flags {
    due: Option<String>,
    duration: Option<String>,
    snooze: Option<String>,
    start: Option<String>,
    notes: Option<String>,
    list: Option<String>,
    labels: Vec<String>,
    complete: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AddArgs {
    json: bool,
    dry_run: bool,
    each: bool,
    flags: Flags,
    /// What is left after the options: the task, as separate arguments.
    text: Vec<String>,
}

const VALUE_FLAGS: [&str; 8] = ["--due", "--duration", "--snooze", "--start", "--notes", "--list", "--label", "--add-label"];
const SWITCHES: [&str; 6] = ["--json", "--dry-run", "--each", "--lines", "--complete", "--debug"];

/// How many arguments an option straight after `add` takes in all, counting itself: 1 for a
/// switch or `--due=...`, 2 for `--due ...`, or None if it isn't an option of `add`.
pub fn option_width(arg: &str) -> Option<usize> {
    if SWITCHES.contains(&arg) {
        return Some(1);
    }
    if VALUE_FLAGS.contains(&arg) {
        return Some(2);
    }
    let name = arg.split_once('=')?.0;
    VALUE_FLAGS.contains(&name).then_some(1)
}

/// The options straight after `add` are read; the first word that isn't one starts the task, and
/// `--` ends the options. So `add Buy --json` is a task, and a task that starts with an option
/// is written `add -- --json ...`.
fn parse_add_args(args: &[String]) -> Result<AddArgs, String> {
    let mut parsed = AddArgs::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            index += 1;
            break;
        }
        let Some(width) = option_width(arg) else { break };
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if width == 1 && VALUE_FLAGS.contains(&name) => (name, Some(value.to_string())),
            _ => (arg, None),
        };
        let value = if VALUE_FLAGS.contains(&name) {
            match inline {
                Some(value) => Some(value),
                None => Some(args.get(index + 1).ok_or_else(|| format!("{name} needs a value"))?.clone()),
            }
        } else {
            None
        };
        let flags = &mut parsed.flags;
        match name {
            "--json" => parsed.json = true,
            "--dry-run" => parsed.dry_run = true,
            "--each" | "--lines" => parsed.each = true,
            "--complete" => flags.complete = true,
            "--debug" => {}
            "--due" => flags.due = value,
            "--duration" => flags.duration = value,
            "--snooze" => flags.snooze = value,
            "--start" => flags.start = value,
            "--notes" => flags.notes = value,
            "--list" => flags.list = value,
            _ => flags.labels.extend(value),
        }
        index += width;
    }
    parsed.text = args[index..].to_vec();
    Ok(parsed)
}

/// Turns the options as typed into [`AddOptions`], reading the dates as of `now`.
fn resolve_flags(flags: &Flags, now: chrono::NaiveDateTime) -> Result<AddOptions, String> {
    let mut options = AddOptions { notes: flags.notes.clone(), complete: flags.complete, ..AddOptions::default() };
    if let Some(list) = &flags.list {
        if list.trim().is_empty() {
            return Err("--list needs a list name".to_string());
        }
        options.list = Some(list.trim().to_string());
    }
    for label in &flags.labels {
        if label.trim().is_empty() {
            return Err("--label needs a label name".to_string());
        }
        options.labels.push(label.trim().to_string());
    }
    if let Some(text) = &flags.due {
        match edit::due_field(text, now)? {
            ("timeless_due_at", value) => options.timeless_due_at = Some(value),
            (_, value) => options.due_at = Some(value),
        }
    }
    if let Some(text) = &flags.snooze {
        match edit::snooze_field(text, now)? {
            ("timeless_snoozed_until", value) => options.timeless_snoozed_until = Some(value),
            (_, value) => options.snoozed_until = Some(value),
        }
    }
    if let Some(text) = &flags.start {
        match edit::start_field(text, now)? {
            ("timeless_starts_at", value) => options.timeless_starts_at = Some(value),
            (_, value) => options.starts_at = Some(value),
        }
    }
    if let Some(text) = &flags.duration {
        let minutes = dates::parse_minutes(text)
            .filter(|minutes| (1..=i64::from(i32::MAX)).contains(minutes))
            .ok_or_else(|| format!("'{text}' isn't a duration (try 45, 45m or 1.5h)"))?;
        options.duration_minutes = Some(minutes as i32);
    }
    Ok(options)
}

/// A task as the text says it, then as the options say it: an option replaces what the text
/// set (the list, the due date, the duration, the notes) or adds to it (the labels).
fn apply_options(parsed: &mut ParsedTask, options: &AddOptions) {
    if let Some(notes) = &options.notes {
        parsed.request.notes = notes.clone();
    }
    if let Some(list) = &options.list {
        parsed.list_names = vec![list.clone()];
    }
    parsed.label_names.extend(options.labels.iter().cloned());
    if options.timeless_due_at.is_some() || options.due_at.is_some() {
        parsed.request.timeless_due_at = options.timeless_due_at.clone();
        parsed.request.due_at = options.due_at.clone();
    }
    if let Some(minutes) = options.duration_minutes {
        parsed.request.duration_minutes = Some(minutes);
    }
}

/// Checks a parsed task is one that can be sent.
fn validate(parsed: &ParsedTask) -> Result<(), TaskError> {
    if parsed.list_names.len() > 1 {
        let listed: Vec<String> = parsed.list_names.iter().map(|name| format!("@{name}")).collect();
        return Err(TaskError::Invalid(format!(
            "Multiple lists specified ({}); a task can be in only one list",
            listed.join(", ")
        )));
    }
    if parsed.request.title.is_empty() {
        return Err(TaskError::Invalid("The task has no title".to_string()));
    }
    Ok(())
}

fn resolve_names(
    request: &mut TaskRequest,
    list_names: &[String],
    label_names: &[String],
    api_key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(list_name) = list_names.first() {
        let mut lists_cache = names::load(&paths::lists_path());

        debug!("looking for the list '{list_name}' among {} cached list names", lists_cache.len());
        if let Some(list_id) = find_list_key(&lists_cache, list_name) {
            request.list_id = Some(list_id);
        } else {
            debug!("'{list_name}' is not in the cached lists; fetching them");
            // Fetch lists from API
            lists_cache = api::fetch_lists(api_key)?;
            names::save(&paths::lists_path(), &lists_cache)?;
            request.list_id = find_list_key(&lists_cache, list_name);
        }
    }

    if !label_names.is_empty() {
        let mut labels_cache: NameMap = names::load(&paths::labels_path());

        let need_fetch = label_names
            .iter()
            .any(|label_name| find_matching_key(&labels_cache, label_name).is_none());
        if need_fetch {
            labels_cache = api::fetch_labels(api_key)?;
            names::save(&paths::labels_path(), &labels_cache)?;
        }

        for label_name in label_names {
            debug!("looking for the label '{label_name}'");
            if let Some(label_id) = find_matching_key(&labels_cache, label_name) {
                request.label_ids.push(label_id);
            }
        }
    }

    Ok(())
}

/// Parses the task, applies the options, resolves the names and sends it. Then sends the part
/// the API only takes as a change to an existing task (snooze, start date, complete), or, if
/// that can't be sent now, saves it for a retry.
pub fn process_task_with(task_str: &str, options: &AddOptions, api_key: &str) -> Result<Value, TaskError> {
    let mut parsed = parse_task(task_str);
    apply_options(&mut parsed, options);
    validate(&parsed)?;
    let ParsedTask { mut request, list_names, label_names } = parsed;
    debug!("parsed task: title {:?}, list names {list_names:?}, label names {label_names:?}", request.title);
    resolve_names(&mut request, &list_names, &label_names, api_key).map_err(TaskError::Failed)?;
    let response = api::send_task(&request, api_key).map_err(TaskError::Failed)?;
    if let Some(patch) = options.follow_up() {
        send_follow_up(&response, &request.title, patch, api_key);
    }
    Ok(response)
}

/// The id of a task the API has just created.
fn created_id(response: &Value) -> Option<&str> {
    response.pointer("/todo_item/id").or_else(|| response.get("id")).and_then(Value::as_str)
}

fn send_follow_up(response: &Value, title: &str, patch: Value, api_key: &str) {
    let Some(id) = created_id(response) else {
        eprintln!("Warning: the task was created, but Godspeed didn't say its id, so its snooze, start date or completion couldn't be set.");
        return;
    };
    let edit = PendingEdit { id: id.to_string(), title: Some(title.to_string()), patch: Some(patch), delete: false };
    debug!("sending what a new task can't be created with: {}", edit.describe());
    match edit.send(api_key) {
        Ok(()) => {}
        Err(e) if e.is_retryable() => match queue::add(&edit.to_entry()) {
            Ok(()) => eprintln!("Note: the task was created, but setting its snooze, start date or completion failed ({e}); it is saved and will be sent the next time you run add or edit."),
            Err(queue_error) => eprintln!("Warning: the task was created, but its snooze, start date or completion couldn't be set ({e}) or saved ({queue_error})."),
        },
        Err(e) => eprintln!("Warning: the task was created, but its snooze, start date or completion was refused: {e}"),
    }
}

/// Reads the whole of stdin.
fn read_stdin() -> String {
    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer).unwrap_or_default();
    buffer
}

/// The text of the tasks to add. Normally one: the arguments joined with spaces, or all of
/// stdin (so a task of several lines can be piped in). With `--each`, one for each argument, or
/// for each line of stdin that isn't blank.
fn task_texts(args: &AddArgs) -> Vec<String> {
    if args.each {
        let texts: Vec<String> = if args.text.is_empty() {
            read_stdin().lines().map(str::trim).map(str::to_string).collect()
        } else {
            args.text.iter().map(|text| text.trim().to_string()).collect()
        };
        return texts.into_iter().filter(|text| !text.is_empty()).collect();
    }
    if !args.text.is_empty() {
        // Join all arguments with spaces to handle multi-word input
        return vec![args.text.join(" ")];
    }
    vec![read_stdin().trim_end().to_string()]
}

/// What happened to one task.
enum Outcome {
    Created(Value),
    /// Couldn't be sent now; it is saved and will be retried.
    Queued(String),
    /// Can never be sent.
    Rejected(String),
}

fn add_one(input: &str, options: &AddOptions, api_key: &str) -> Outcome {
    match process_task_with(input, options, api_key) {
        Ok(response) => Outcome::Created(response),
        Err(TaskError::Invalid(reason)) => {
            eprintln!("Error: {reason}");
            send_notification(&format!("Error: {reason}"));
            Outcome::Rejected(reason)
        }
        Err(TaskError::Failed(e)) => {
            eprintln!("Failed to send task: {}", e);
            let entry = queue_entry(&resolve_relative_dates(input, Local::now().date_naive()), options);
            if let Err(queue_error) = queue::add(&entry) {
                eprintln!("Could not save the task to the retry queue: {queue_error}");
            }
            send_notification("Failed to send task");
            Outcome::Queued(e.to_string())
        }
    }
}

/// What a task would be, as the API would be asked for it, except that the list and labels are
/// still names. For `--dry-run`.
fn preview_json(parsed: &ParsedTask, options: &AddOptions) -> Value {
    let request = &parsed.request;
    let mut out = serde_json::Map::new();
    out.insert("title".to_string(), json!(request.title));
    if !request.notes.is_empty() {
        out.insert("notes".to_string(), json!(request.notes));
    }
    if let Some(list) = parsed.list_names.first() {
        out.insert("list".to_string(), json!(list));
    }
    if !parsed.label_names.is_empty() {
        out.insert("labels".to_string(), json!(parsed.label_names));
    }
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(value) = value {
            out.insert(key.to_string(), json!(value));
        }
    };
    put("timeless_due_at", &request.timeless_due_at);
    put("due_at", &request.due_at);
    put("snoozed_until", &options.snoozed_until);
    put("timeless_snoozed_until", &options.timeless_snoozed_until);
    put("starts_at", &options.starts_at);
    put("timeless_starts_at", &options.timeless_starts_at);
    if let Some(minutes) = request.duration_minutes {
        out.insert("duration_minutes".to_string(), json!(minutes));
    }
    if options.complete {
        out.insert("is_complete".to_string(), json!(true));
    }
    Value::Object(out)
}

fn preview_text(preview: &Value) -> String {
    let mut lines = vec![format!("Would create '{}'", preview["title"].as_str().unwrap_or(""))];
    let mut row = |name: &str, value: String| lines.push(format!("  {name:<10}{value}"));
    if let Some(list) = preview["list"].as_str() {
        row("list", list.to_string());
    }
    if let Some(labels) = preview["labels"].as_array() {
        row("labels", labels.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "));
    }
    for (name, keys) in [
        ("due", ["timeless_due_at", "due_at"]),
        ("snoozed", ["timeless_snoozed_until", "snoozed_until"]),
        ("starts", ["timeless_starts_at", "starts_at"]),
    ] {
        if let Some(value) = keys.iter().find_map(|key| preview[*key].as_str()) {
            row(name, value.to_string());
        }
    }
    if let Some(minutes) = preview["duration_minutes"].as_i64() {
        row("duration", format!("{minutes}m"));
    }
    if preview["is_complete"] == json!(true) {
        row("status", "complete".to_string());
    }
    if let Some(notes) = preview["notes"].as_str() {
        row("notes", notes.replace('\n', " "));
    }
    lines.join("\n")
}

/// `godspeed-cli add [OPTIONS] <TASK>...`, or the task on stdin.
pub fn run_add(raw_args: &[String]) -> ExitCode {
    let args = match parse_add_args(raw_args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };
    let options = match resolve_flags(&args.flags, Local::now().naive_local()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };
    debug!("add: {args:?}, options {options:?}");

    if args.dry_run {
        return dry_run(&args, &options);
    }

    if let Err(e) = paths::ensure_data_dir() {
        eprintln!("Failed to create directories: {}", e);
        return exit::failure();
    }

    let Some(api_key) = token::load_token() else {
        send_notification("GODSPEED_TOKEN is not set");
        eprintln!(
            "Error: GODSPEED_TOKEN is not set (environment variable, or an `export GODSPEED_TOKEN=...` line in {})",
            paths::token_file_path().display()
        );
        return exit::usage();
    };

    let inputs = task_texts(&args);

    pending::process(&api_key);

    let inputs: Vec<String> = inputs.into_iter().filter(|input| !input.is_empty()).collect();
    if inputs.is_empty() {
        if args.each && args.json {
            let _ = writeln!(io::stdout(), "[]");
        }
        return exit::SUCCESS;
    }

    let outcomes: Vec<(String, Outcome)> = inputs
        .into_iter()
        .map(|input| {
            let outcome = add_one(&input, &options, &api_key);
            (input, outcome)
        })
        .collect();

    if args.json {
        let value = if args.each {
            Value::Array(
                outcomes
                    .iter()
                    .map(|(input, outcome)| match outcome {
                        Outcome::Created(response) => json!({"input": input, "status": "created", "response": response}),
                        Outcome::Queued(reason) => json!({"input": input, "status": "queued", "error": reason}),
                        Outcome::Rejected(reason) => json!({"input": input, "status": "rejected", "error": reason}),
                    })
                    .collect(),
            )
        } else {
            match &outcomes[0].1 {
                Outcome::Created(response) => response.clone(),
                _ => Value::Null,
            }
        };
        if !value.is_null() {
            // Ignore a closed pipe instead of panicking.
            let _ = writeln!(io::stdout(), "{}", serde_json::to_string_pretty(&value).expect("the response serialises"));
        }
    }

    let count = |wanted: fn(&Outcome) -> bool| outcomes.iter().filter(|(_, outcome)| wanted(outcome)).count();
    let created = count(|o| matches!(o, Outcome::Created(_)));
    let queued = count(|o| matches!(o, Outcome::Queued(_)));
    let rejected = count(|o| matches!(o, Outcome::Rejected(_)));
    if outcomes.len() > 1 {
        eprintln!("{created} created, {queued} queued, {rejected} rejected.");
    }
    if queued > 0 {
        exit::failure()
    } else if rejected > 0 {
        exit::usage()
    } else {
        exit::SUCCESS
    }
}

/// `--dry-run`: shows what would be created, and sends nothing. No token or network is needed;
/// the list and label names are shown as written, not looked up.
fn dry_run(args: &AddArgs, options: &AddOptions) -> ExitCode {
    let inputs = task_texts(args);
    let mut previews: Vec<Value> = Vec::new();
    let mut rejected = 0;
    for input in inputs.iter().filter(|input| !input.is_empty()) {
        let mut parsed = parse_task(input);
        apply_options(&mut parsed, options);
        match validate(&parsed) {
            Ok(()) => previews.push(preview_json(&parsed, options)),
            Err(e) => {
                eprintln!("Error: {e} (in '{input}')");
                rejected += 1;
            }
        }
    }
    let output = if args.json {
        let value = if args.each { Value::Array(previews.clone()) } else { previews.first().cloned().unwrap_or(Value::Null) };
        if value.is_null() { String::new() } else { serde_json::to_string_pretty(&value).expect("the preview serialises") }
    } else {
        previews.iter().map(preview_text).collect::<Vec<_>>().join("\n\n")
    };
    if !output.is_empty() {
        // Ignore a closed pipe.
        let _ = writeln!(io::stdout(), "{output}");
    }
    eprintln!("Dry run: nothing was sent.");
    if rejected > 0 {
        exit::usage()
    } else {
        exit::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn now() -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(10, 30, 0).unwrap()
    }

    #[test]
    fn options_straight_after_add_are_read_and_the_first_other_word_starts_the_task() {
        let parsed = parse_add_args(&args(&["--json", "--due", "friday", "--label=Urgent", "--label", "Home", "Buy", "milk", "--json"])).unwrap();
        assert!(parsed.json);
        assert_eq!(parsed.flags.due.as_deref(), Some("friday"));
        assert_eq!(parsed.flags.labels, vec!["Urgent", "Home"]);
        assert_eq!(parsed.text, args(&["Buy", "milk", "--json"]), "after the text it is just text");
    }

    #[test]
    fn double_dash_ends_the_options() {
        let parsed = parse_add_args(&args(&["--dry-run", "--", "--json", "x"])).unwrap();
        assert!(parsed.dry_run && !parsed.json);
        assert_eq!(parsed.text, args(&["--json", "x"]));
        assert_eq!(parse_add_args(&args(&["--"])).unwrap().text, Vec::<String>::new());
    }

    #[test]
    fn words_that_only_look_like_options_are_the_task() {
        let parsed = parse_add_args(&args(&["--version", "bump"])).unwrap();
        assert_eq!(parsed.text, args(&["--version", "bump"]));
        assert_eq!(parse_add_args(&args(&["search", "the", "attic"])).unwrap().text, args(&["search", "the", "attic"]));
        assert!(parse_add_args(&args(&["--each", "--lines", "--complete"])).unwrap().flags.complete);
    }

    #[test]
    fn an_option_that_needs_a_value_without_one_is_an_error() {
        assert!(parse_add_args(&args(&["--due"])).unwrap_err().contains("needs a value"));
        assert!(parse_add_args(&args(&["--notes"])).is_err());
        assert_eq!(parse_add_args(&args(&["--notes="])).unwrap().flags.notes.as_deref(), Some(""));
    }

    #[test]
    fn the_width_of_an_option_says_how_many_arguments_it_takes() {
        assert_eq!(option_width("--json"), Some(1));
        assert_eq!(option_width("--due"), Some(2));
        assert_eq!(option_width("--due=friday"), Some(1));
        assert_eq!(option_width("--nope"), None);
        assert_eq!(option_width("Buy"), None);
        assert_eq!(option_width("--nope=1"), None);
    }

    #[test]
    fn dates_and_durations_become_absolute_options() {
        let flags = Flags {
            due: Some("2026-10-31".into()),
            duration: Some("1.5h".into()),
            snooze: Some("2026-11-02".into()),
            start: Some("tomorrow 9am".into()),
            list: Some(" Work ".into()),
            labels: vec!["Urgent".into()],
            notes: Some("n".into()),
            complete: true,
        };
        let options = resolve_flags(&flags, now()).unwrap();
        assert_eq!(options.timeless_due_at.as_deref(), Some("2026-10-31"));
        assert_eq!(options.duration_minutes, Some(90));
        assert_eq!(options.timeless_snoozed_until.as_deref(), Some("2026-11-02"));
        assert!(options.starts_at.as_deref().is_some_and(|s| s.ends_with('Z')), "{options:?}");
        assert_eq!(options.list.as_deref(), Some("Work"));
        assert!(options.complete);
        let later = resolve_flags(&Flags { due: Some("friday 5pm".into()), ..Flags::default() }, now()).unwrap();
        assert!(later.due_at.is_some() && later.timeless_due_at.is_none());
    }

    #[test]
    fn bad_dates_durations_and_names_are_errors() {
        let bad = |flags: Flags| resolve_flags(&flags, now()).unwrap_err();
        assert!(bad(Flags { due: Some("someday".into()), ..Flags::default() }).contains("isn't a date"));
        assert!(bad(Flags { duration: Some("soon".into()), ..Flags::default() }).contains("isn't a duration"));
        assert!(bad(Flags { duration: Some("0".into()), ..Flags::default() }).contains("isn't a duration"));
        assert!(bad(Flags { snooze: Some("2020-01-01".into()), ..Flags::default() }).contains("in the past"));
        assert!(bad(Flags { list: Some(" ".into()), ..Flags::default() }).contains("--list"));
        assert!(bad(Flags { labels: vec![" ".into()], ..Flags::default() }).contains("--label"));
    }

    #[test]
    fn an_option_replaces_what_the_text_set_and_adds_to_the_labels() {
        let mut parsed = parse_task("Pay rent @Home .Urgent ~30m :20261101 n: from the text");
        let options = AddOptions {
            notes: Some("from the option".into()),
            list: Some("Work".into()),
            labels: vec!["Low".into()],
            due_at: Some("2026-11-02T09:00:00Z".into()),
            duration_minutes: Some(45),
            ..AddOptions::default()
        };

        apply_options(&mut parsed, &options);

        assert_eq!(parsed.request.notes, "from the option");
        assert_eq!(parsed.list_names, vec!["Work"]);
        assert_eq!(parsed.label_names, vec!["Urgent", "Low"]);
        assert_eq!(parsed.request.timeless_due_at, None, "the option's due date replaces the text's");
        assert_eq!(parsed.request.due_at.as_deref(), Some("2026-11-02T09:00:00Z"));
        assert_eq!(parsed.request.duration_minutes, Some(45));
    }

    #[test]
    fn only_what_a_new_task_cannot_be_created_with_is_a_follow_up() {
        assert_eq!(AddOptions::default().follow_up(), None);
        let options = AddOptions {
            due_at: Some("x".into()),
            notes: Some("n".into()),
            timeless_snoozed_until: Some("2026-11-02".into()),
            starts_at: Some("2026-11-03T09:00:00Z".into()),
            complete: true,
            ..AddOptions::default()
        };
        assert_eq!(
            options.follow_up(),
            Some(json!({"timeless_snoozed_until": "2026-11-02", "starts_at": "2026-11-03T09:00:00Z", "is_complete": true}))
        );
    }

    #[test]
    fn a_waiting_task_is_plain_text_without_options_and_one_line_of_json_with_them() {
        assert_eq!(queue_entry("Buy milk @Home", &AddOptions::default()), "Buy milk @Home");
        let options = AddOptions { list: Some("Home".into()), complete: true, ..AddOptions::default() };
        let entry = queue_entry("Buy\nmilk", &options);
        assert!(!entry.contains('\n') && entry.starts_with("{\"queued_add\":"), "{entry}");
        assert_eq!(split_entry(&entry), ("Buy\nmilk".to_string(), options.clone()));
        assert_eq!(split_entry("Buy milk"), ("Buy milk".to_string(), AddOptions::default()));
        assert_eq!(split_entry("{\"queued_add\": nonsense").0, "{\"queued_add\": nonsense");
        assert_eq!(options.summary(), "list Home, complete");
    }

    #[test]
    fn each_means_a_task_for_every_argument_and_blank_ones_are_skipped() {
        let each = AddArgs { each: true, text: args(&["Buy milk", "  ", " Call mom "]), ..AddArgs::default() };
        assert_eq!(task_texts(&each), vec!["Buy milk", "Call mom"]);
        let joined = AddArgs { text: args(&["Buy", "milk"]), ..AddArgs::default() };
        assert_eq!(task_texts(&joined), vec!["Buy milk"]);
    }

    #[test]
    fn the_preview_shows_the_task_as_it_would_be_sent() {
        let options = AddOptions { complete: true, timeless_snoozed_until: Some("2026-11-02".into()), ..AddOptions::default() };
        let mut parsed = parse_task("Pay rent @Home .Urgent ~30m :20261101 n: ring them");
        apply_options(&mut parsed, &options);

        let preview = preview_json(&parsed, &options);

        assert_eq!(
            preview,
            json!({"title": "Pay rent", "notes": "ring them", "list": "Home", "labels": ["Urgent"], "timeless_due_at": "2026-11-01",
                   "timeless_snoozed_until": "2026-11-02", "duration_minutes": 30, "is_complete": true})
        );
        assert_eq!(
            preview_text(&preview),
            "Would create 'Pay rent'\n  list      Home\n  labels    Urgent\n  due       2026-11-01\n  snoozed   2026-11-02\n  duration  30m\n  status    complete\n  notes     ring them"
        );
    }
}
