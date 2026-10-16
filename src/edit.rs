//! `godspeed-cli edit`: change a task's title, notes, due date, duration, labels, list or snooze,
//! complete it, or delete it.
//!
//! The task is given by its id (`search --id` shows them) or by words from its title. With no
//! option a menu asks what to do; options (`--title`, `--complete`, `--due`, ...) do it straight
//! away, for scripts. With options and no task, the ids are read from stdin, one per line, so
//! `godspeed-cli search 'due:today' --ids-only | godspeed-cli edit --complete` works. Prompts go
//! to stderr and results to stdout.
//!
//! A change that couldn't be sent because Godspeed was unreachable or failing is saved in the
//! retry queue, and sent by the next `add` or `edit`.

use crate::listing::Kind;
use crate::names::{self, NameMap};
use crate::store::{Store, StoredTask};
use crate::{api, dates, exit, paths, pending, queue, token};
use chrono::{Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::process::ExitCode;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Changes {
    title: Option<String>,
    notes: Option<String>,
    complete: Option<bool>,
    /// What was typed: a date or a time.
    snooze: Option<String>,
    due: Option<String>,
    /// What was typed: `45`, `45m`, `1.5h`.
    duration: Option<String>,
    /// What was typed: part of a list's name.
    move_to: Option<String>,
    add_labels: Vec<String>,
    remove_labels: Vec<String>,
    delete: bool,
}

impl Changes {
    fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    /// Whether anything other than a delete is asked for.
    fn has_update(&self) -> bool {
        Changes { delete: false, ..self.clone() } != Changes::default()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    /// The id, or the words to find the task by, joined with spaces. `-` means ids on stdin.
    target: String,
    changes: Changes,
    yes: bool,
    json: bool,
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
        let changes = &mut parsed.changes;
        if let Some(value) = value_of(arg, "--title", &mut iter)? {
            changes.title = Some(value);
        } else if let Some(value) = value_of(arg, "--notes", &mut iter)? {
            changes.notes = Some(value);
        } else if let Some(value) = value_of(arg, "--snooze", &mut iter)? {
            changes.snooze = Some(value);
        } else if let Some(value) = value_of(arg, "--due", &mut iter)? {
            changes.due = Some(value);
        } else if let Some(value) = value_of(arg, "--duration", &mut iter)? {
            changes.duration = Some(value);
        } else if let Some(value) = value_of(arg, "--move", &mut iter)? {
            changes.move_to = Some(value);
        } else if let Some(value) = value_of(arg, "--add-label", &mut iter)? {
            changes.add_labels.push(value);
        } else if let Some(value) = value_of(arg, "--remove-label", &mut iter)? {
            changes.remove_labels.push(value);
        } else {
            match arg.as_str() {
                "--" => options_done = true,
                "--complete" => changes.complete = Some(true),
                "--incomplete" | "--reopen" => changes.complete = Some(false),
                "--delete" => changes.delete = true,
                "--yes" | "-y" => parsed.yes = true,
                "--json" => parsed.json = true,
                other => return Err(format!("unknown option '{other}' (see `godspeed-cli edit --help`)")),
            }
        }
    }
    parsed.target = words.join(" ").trim().to_string();
    let changes = &parsed.changes;
    if changes.delete && changes.has_update() {
        return Err("--delete can't be combined with other changes".to_string());
    }
    if changes.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
        return Err("--title can't be empty".to_string());
    }
    for label in changes.add_labels.iter().chain(&changes.remove_labels) {
        if label.trim().is_empty() {
            return Err("a label name can't be empty".to_string());
        }
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

/// A change waiting to be sent, as kept in the retry queue (one line of JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingEdit {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The body of the `PATCH` request. Names are already ids and dates are already absolute.
    #[serde(default)]
    pub patch: Option<Value>,
    #[serde(default)]
    pub delete: bool,
}

#[derive(Serialize, Deserialize)]
struct QueueEntry {
    queued_edit: PendingEdit,
}

impl PendingEdit {
    /// The line for the retry queue.
    pub fn to_entry(&self) -> String {
        serde_json::to_string(&QueueEntry { queued_edit: self.clone() }).expect("an edit serialises")
    }

    /// The change a retry queue entry holds, or None if it is something else (a task to add).
    pub fn parse(entry: &str) -> Option<PendingEdit> {
        if !entry.starts_with("{\"queued_edit\":") {
            return None;
        }
        serde_json::from_str::<QueueEntry>(entry).ok().map(|parsed| parsed.queued_edit)
    }

    /// The task's title if known, otherwise its id.
    pub fn name(&self) -> &str {
        self.title.as_deref().filter(|title| !title.trim().is_empty()).unwrap_or(&self.id)
    }

    pub fn describe(&self) -> String {
        if self.delete {
            return format!("delete '{}'", self.name());
        }
        let fields: Vec<String> = self.patch.as_ref().and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default();
        format!("change {} of '{}'", fields.join(", "), self.name())
    }

    /// Sends the change to Godspeed.
    pub fn send(&self, api_key: &str) -> Result<(), api::ApiError> {
        if self.delete {
            api::delete_task(&self.id, api_key)
        } else {
            let patch = self.patch.clone().unwrap_or_else(|| json!({}));
            api::update_task(&self.id, &patch, api_key).map(|_| ())
        }
    }

    /// Records the change in the local copy, if there is one, so a search sees it at once.
    pub fn record_locally(&self) {
        let path = paths::store_path();
        if !path.exists() {
            return;
        }
        let result = Store::open(&path).and_then(|store| match &self.patch {
            _ if self.delete => store.remove_task(&self.id).map(|_| ()),
            Some(patch) => store.apply_edit(&self.id, patch, patch.get("is_complete").and_then(Value::as_bool)),
            None => Ok(()),
        });
        match result {
            Ok(()) => debug!("recorded the change to {} in the local copy", self.id),
            Err(e) => eprintln!("Note: the local copy could not be updated ({e}); run `godspeed-cli refresh`."),
        }
    }

    /// Sends a change from the retry queue, and records it locally if that worked.
    pub fn send_queued(&self, api_key: &str) -> Result<(), api::ApiError> {
        self.send(api_key)?;
        self.record_locally();
        Ok(())
    }
}

/// The tasks and lists of the local copy, if there is one.
#[derive(Default)]
struct Copy {
    tasks: Vec<StoredTask>,
    lists: Vec<Value>,
}

fn load_copy() -> Copy {
    let path = paths::store_path();
    if !path.exists() {
        debug!("there is no local copy to look in");
        return Copy::default();
    }
    debug!("reading the local copy {}", path.display());
    match Store::open(&path).and_then(|store| Ok(Copy { tasks: store.tasks(true)?, lists: store.lists()? })) {
        Ok(copy) => copy,
        Err(e) => {
            debug!("the local copy could not be read: {e}");
            Copy::default()
        }
    }
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

/// Works out which task is meant. An id that isn't in the copy is still used as it is, since
/// the copy may be out of date.
fn find_target(
    copy: &Copy,
    text: &str,
    interactive: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Target, String> {
    let found = candidates(&copy.tasks, text);
    debug!("{} task(s) in the local copy match '{text}'", found.len());
    match found.as_slice() {
        [one] => Ok(Target::from_copy(&copy.tasks[*one], &copy.lists)),
        [] if looks_like_id(text) => {
            debug!("'{text}' is not in the local copy; using it as a task id");
            Ok(Target::bare(text))
        }
        [] => Err(format!("no task in the local copy matches '{text}'. Use `godspeed-cli search --id` to find its id.")),
        many => {
            let targets: Vec<Target> = many.iter().map(|&index| Target::from_copy(&copy.tasks[index], &copy.lists)).collect();
            if interactive {
                pick(&targets, input, out)
            } else {
                let listed: Vec<String> = targets.iter().map(|target| format!("  {}", target.summary())).collect();
                Err(format!("{} tasks match '{text}'; give the id of one:\n{}", targets.len(), listed.join("\n")))
            }
        }
    }
}

/// The labels to add and remove typed as `+Urgent -Low`. A word with neither sign adds.
fn parse_label_changes(text: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let (mut add, mut remove) = (Vec::new(), Vec::new());
    for word in text.split_whitespace() {
        match word.strip_prefix('-') {
            Some("") => return Err("a '-' needs a label name after it".to_string()),
            Some(name) => remove.push(name.to_string()),
            None => match word.strip_prefix('+').unwrap_or(word) {
                "" => return Err("a '+' needs a label name after it".to_string()),
                name => add.push(name.to_string()),
            },
        }
    }
    Ok((add, remove))
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
    let _ = writeln!(out, "  7) Set the due date");
    let _ = writeln!(out, "  8) Set the duration");
    let _ = writeln!(out, "  9) Add or remove labels");
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
        "7" | "due" => {
            let when = ask("Due (tomorrow, friday 9am, 2026-10-31): ");
            if when.is_empty() {
                return Err("no date given".to_string());
            }
            changes.due = Some(when);
        }
        "8" | "duration" => {
            let length = ask("Duration (45, 45m, 1.5h): ");
            if length.is_empty() {
                return Err("no duration given".to_string());
            }
            changes.duration = Some(length);
        }
        "9" | "labels" | "l" => {
            let text = ask("Labels to add or remove, such as +Urgent -Low: ");
            let (add, remove) = parse_label_changes(&text)?;
            if add.is_empty() && remove.is_empty() {
                return Err("no labels given".to_string());
            }
            changes.add_labels = add;
            changes.remove_labels = remove;
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

/// The API field and value for a date typed by the person: the `timeless` field if it names a
/// whole day, or the `moment` field if it names a time. A moment in the past is only accepted
/// if `allow_past`.
fn when_field(
    text: &str,
    now: NaiveDateTime,
    timeless: &'static str,
    moment: &'static str,
    allow_past: bool,
) -> Result<(&'static str, String), String> {
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
            if !allow_past && parsed.at.date() < now.date() {
                return Err(format!("{} is in the past", parsed.at.date()));
            }
            Ok((timeless, parsed.at.date().format("%Y-%m-%d").to_string()))
        }
        _ => {
            if !allow_past && parsed.at < now {
                return Err(format!("{} is in the past", parsed.at.format("%Y-%m-%d %H:%M")));
            }
            let moment_text = to_utc(parsed.at).ok_or_else(|| format!("{} doesn't exist in your time zone", parsed.at))?;
            Ok((moment, moment_text))
        }
    }
}

fn snooze_field(text: &str, now: NaiveDateTime) -> Result<(&'static str, String), String> {
    when_field(text, now, "timeless_snoozed_until", "snoozed_until", false)
}

fn due_field(text: &str, now: NaiveDateTime) -> Result<(&'static str, String), String> {
    when_field(text, now, "timeless_due_at", "due_at", true)
}

/// The names in the changes, as ids.
#[derive(Debug, Default, PartialEq, Eq)]
struct Resolved {
    /// The list to move to: its id and name.
    list: Option<(String, String)>,
    add_labels: Vec<(String, String)>,
    remove_labels: Vec<(String, String)>,
}

/// The body of the PATCH request for these changes.
fn build_patch(changes: &Changes, resolved: &Resolved, now: NaiveDateTime) -> Result<Value, String> {
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
    if let Some(when) = &changes.due {
        let (field, value) = due_field(when, now)?;
        body.insert(field.to_string(), json!(value));
    }
    if let Some(length) = &changes.duration {
        let minutes = dates::parse_minutes(length)
            .filter(|minutes| *minutes >= 0)
            .ok_or_else(|| format!("'{length}' isn't a duration (try 45, 45m or 1.5h)"))?;
        body.insert("duration_minutes".to_string(), json!(minutes));
    }
    if let Some((id, _)) = &resolved.list {
        body.insert("list_id".to_string(), json!(id));
    }
    if !resolved.add_labels.is_empty() {
        let ids: Vec<&str> = resolved.add_labels.iter().map(|(id, _)| id.as_str()).collect();
        body.insert("add_label_ids".to_string(), json!(ids));
    }
    if !resolved.remove_labels.is_empty() {
        let ids: Vec<&str> = resolved.remove_labels.iter().map(|(id, _)| id.as_str()).collect();
        body.insert("remove_label_ids".to_string(), json!(ids));
    }
    Ok(Value::Object(body))
}

/// The items of the local copy that `kind` names.
fn known_items(kind: Kind) -> Vec<Value> {
    let path = paths::store_path();
    if !path.exists() {
        return Vec::new();
    }
    let items = Store::open(&path).and_then(|store| match kind {
        Kind::Lists => store.lists(),
        Kind::Labels => store.labels(),
    });
    items.unwrap_or_default()
}

fn name_map(items: &[Value]) -> NameMap {
    items
        .iter()
        .filter_map(|item| Some((str_field(item, "name").to_lowercase(), item.get("id")?.as_str()?.to_string())))
        .collect()
}

/// The id that `text` names. Lists are matched by exact name, prefix and part of the name,
/// labels by exact name and prefix, as in `add`.
fn find_id(kind: Kind, map: &NameMap, text: &str) -> Option<String> {
    match kind {
        Kind::Lists => names::find_list_key(map, text),
        Kind::Labels => names::find_matching_key(map, text),
    }
}

/// The list or label that `text` names, as (id, name). The local copy and the names `add` has
/// cached are looked in first; Godspeed is asked only if they have no match.
fn lookup(kind: Kind, text: &str, api_key: &str) -> Result<(String, String), String> {
    let cache_path = match kind {
        Kind::Lists => paths::lists_path(),
        Kind::Labels => paths::labels_path(),
    };
    let items = known_items(kind);
    let mut map = names::load(&cache_path);
    map.extend(name_map(&items));
    let display = |id: &str, items: &[Value]| {
        items.iter().find(|item| str_field(item, "id") == id).map(|item| str_field(item, "name").to_string())
    };
    if let Some(id) = find_id(kind, &map, text) {
        let name = display(&id, &items).unwrap_or_else(|| text.to_string());
        debug!("'{text}' is {} '{name}' ({id}), from what is already known", kind.singular());
        return Ok((id, name));
    }

    debug!("'{text}' matches no known {}; fetching them", kind.noun());
    let fetched = match kind {
        Kind::Lists => api::fetch_list_items(api_key),
        Kind::Labels => api::fetch_label_items(api_key),
    }
    .map_err(|e| format!("could not fetch your {}: {e}", kind.noun()))?;
    let fetched_map = name_map(&fetched);
    if paths::ensure_data_dir().is_ok() {
        let _ = names::save(&cache_path, &fetched_map);
    }
    match find_id(kind, &fetched_map, text) {
        Some(id) => {
            let name = display(&id, &fetched).unwrap_or_else(|| text.to_string());
            debug!("'{text}' is {} '{name}' ({id})", kind.singular());
            Ok((id, name))
        }
        None => {
            let available: Vec<&str> = fetched.iter().map(|item| str_field(item, "name")).collect();
            Err(format!("no {} matches '{text}'. Your {}: {}", kind.singular(), kind.noun(), available.join(", ")))
        }
    }
}

fn resolve(changes: &Changes, api_key: &str) -> Result<Resolved, String> {
    let mut resolved = Resolved::default();
    if let Some(text) = &changes.move_to {
        resolved.list = Some(lookup(Kind::Lists, text, api_key)?);
    }
    for text in &changes.add_labels {
        resolved.add_labels.push(lookup(Kind::Labels, text, api_key)?);
    }
    for text in &changes.remove_labels {
        resolved.remove_labels.push(lookup(Kind::Labels, text, api_key)?);
    }
    Ok(resolved)
}

fn confirm_delete(target: &Target, input: &mut dyn BufRead, out: &mut dyn Write) -> bool {
    let _ = write!(out, "Delete '{}'? This can't be undone. [y/N]: ", target.name());
    let _ = out.flush();
    matches!(read_line(input).unwrap_or_default().to_lowercase().as_str(), "y" | "yes")
}

/// What to say once the changes are made.
fn describe(changes: &Changes, name: &str, resolved: &Resolved) -> String {
    let name = changes.title.as_deref().unwrap_or(name);
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
    if let Some(when) = &changes.due {
        done.push(format!("due {when}"));
    }
    if let Some(length) = &changes.duration {
        done.push(format!("duration {length}"));
    }
    if let Some((_, list)) = &resolved.list {
        done.push(format!("moved to {list}"));
    }
    for (_, label) in &resolved.add_labels {
        done.push(format!("label {label} added"));
    }
    for (_, label) in &resolved.remove_labels {
        done.push(format!("label {label} removed"));
    }
    format!("Updated '{name}': {}.", done.join(", "))
}

fn fail(message: &str) -> ExitCode {
    eprintln!("Error: {message}");
    exit::usage()
}

/// How one task's change went.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Done,
    /// Godspeed couldn't be reached; the change is saved and will be sent later.
    Queued(String),
    Failed(String),
}

impl Outcome {
    fn word(&self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Queued(_) => "queued",
            Outcome::Failed(_) => "failed",
        }
    }
}

/// Sends the change. If Godspeed can't be reached or is failing, it is queued instead; the local
/// copy is updated either way, so searches agree with what you asked for.
fn execute(edit: &PendingEdit, api_key: &str) -> Outcome {
    match edit.send(api_key) {
        Ok(()) => {
            edit.record_locally();
            Outcome::Done
        }
        Err(e) if e.is_retryable() => {
            debug!("the change can be retried, so it is queued: {e}");
            match queue::add(&edit.to_entry()) {
                Ok(()) => {
                    edit.record_locally();
                    Outcome::Queued(e.message)
                }
                Err(queue_error) => Outcome::Failed(format!("{e}; and it could not be saved for a retry: {queue_error}")),
            }
        }
        Err(e) => Outcome::Failed(e.message),
    }
}

/// The ids on stdin, one on each line (blank lines are skipped, and an id twice counts once).
fn read_ids(input: &mut dyn Read) -> Result<Vec<String>, String> {
    let mut text = String::new();
    input.read_to_string(&mut text).map_err(|e| format!("cannot read the ids from stdin: {e}"))?;
    let mut ids: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !looks_like_id(line) {
            return Err(format!("'{line}' on stdin isn't a task id (use `godspeed-cli search --ids-only`)"));
        }
        if !ids.iter().any(|id| id == line) {
            ids.push(line.to_string());
        }
    }
    if ids.is_empty() {
        return Err("no task ids on stdin".to_string());
    }
    Ok(ids)
}

pub fn run_edit(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };
    let from_stdin = args.target == "-" || (args.target.is_empty() && !args.changes.is_empty() && !io::stdin().is_terminal());
    if args.target.is_empty() && !from_stdin {
        return fail("say which task to edit: godspeed-cli edit <id> (`godspeed-cli search --id` shows ids)");
    }
    if from_stdin && args.changes.is_empty() {
        return fail("editing the ids on stdin needs something to do, such as --complete");
    }
    let Some(api_key) = token::load_token() else {
        return fail(&format!(
            "GODSPEED_TOKEN is not set (environment variable, or an `export GODSPEED_TOKEN=...` line in {})",
            paths::token_file_path().display()
        ));
    };
    debug!("edit: {args:?}");

    let copy = load_copy();
    let stdin = io::stdin();
    let mut out = io::stderr();
    let mut changes = args.changes;
    let interactive = changes.is_empty();

    let targets: Vec<Target> = if from_stdin {
        let ids = match read_ids(&mut stdin.lock()) {
            Ok(ids) => ids,
            Err(message) => return fail(&message),
        };
        debug!("{} task id(s) on stdin", ids.len());
        ids.iter()
            .map(|id| match copy.tasks.iter().find(|task| str_field(&task.json, "id") == id) {
                Some(task) => Target::from_copy(task, &copy.lists),
                None => Target::bare(id),
            })
            .collect()
    } else {
        let mut input = stdin.lock();
        let target = match find_target(&copy, &args.target, interactive, &mut input, &mut out) {
            Ok(target) => target,
            Err(message) => return fail(&message),
        };
        debug!("editing task {} ({:?})", target.id, target.title);
        if interactive {
            changes = match ask_changes(&target, &mut input, &mut out) {
                Ok(changes) => changes,
                Err(message) => return fail(&message),
            };
            if changes.is_empty() {
                eprintln!("Nothing changed.");
                return exit::SUCCESS;
            }
        }
        if changes.delete && !args.yes && !confirm_delete(&target, &mut input, &mut out) {
            eprintln!("Not deleted.");
            return exit::SUCCESS;
        }
        vec![target]
    };
    if from_stdin && changes.delete && !args.yes {
        return fail("deleting the tasks whose ids are on stdin needs --yes");
    }
    debug!("changes: {changes:?}");

    // What is waiting from earlier goes first, so an older change can't overwrite this one.
    pending::process(&api_key);

    let resolved = match resolve(&changes, &api_key) {
        Ok(resolved) => resolved,
        Err(message) => return fail(&message),
    };
    let patch = if changes.delete {
        None
    } else {
        match build_patch(&changes, &resolved, Local::now().naive_local()) {
            Ok(patch) => Some(patch),
            Err(message) => return fail(&message),
        }
    };

    let mut results: Vec<(Target, Outcome)> = Vec::new();
    for target in targets {
        let edit = PendingEdit { id: target.id.clone(), title: target.title.clone(), patch: patch.clone(), delete: changes.delete };
        let outcome = execute(&edit, &api_key);
        if !args.json {
            report(&target, &outcome, &changes, &resolved);
        }
        results.push((target, outcome));
    }

    if args.json {
        let entries: Vec<Value> = results
            .iter()
            .map(|(target, outcome)| {
                let mut entry = json!({
                    "id": target.id,
                    "title": target.title,
                    "action": if changes.delete { "delete" } else { "update" },
                    "status": outcome.word(),
                });
                if let Outcome::Queued(reason) | Outcome::Failed(reason) = outcome {
                    entry["error"] = json!(reason);
                }
                entry
            })
            .collect();
        // Ignore a closed pipe.
        let _ = writeln!(io::stdout(), "{}", serde_json::to_string_pretty(&entries).expect("the results serialise"));
    }
    if results.len() > 1 {
        let count = |word: &str| results.iter().filter(|(_, outcome)| outcome.word() == word).count();
        eprintln!("{} done, {} queued, {} failed.", count("done"), count("queued"), count("failed"));
    }
    if results.iter().all(|(_, outcome)| *outcome == Outcome::Done) {
        exit::SUCCESS
    } else {
        exit::failure()
    }
}

/// Says how a change went.
fn report(target: &Target, outcome: &Outcome, changes: &Changes, resolved: &Resolved) {
    match outcome {
        Outcome::Done if changes.delete => println!("Deleted '{}'.", target.name()),
        Outcome::Done => println!("{}", describe(changes, target.name(), resolved)),
        Outcome::Queued(reason) => eprintln!(
            "Could not reach Godspeed ({reason}); the change to '{}' is saved and will be sent the next time you run add or edit.",
            target.name()
        ),
        Outcome::Failed(reason) => {
            eprintln!("Failed to {} '{}': {reason}", if changes.delete { "delete" } else { "update" }, target.name());
            if reason.contains("404") {
                eprintln!("There may be no task with that id; `godspeed-cli search --id` shows them.");
            }
        }
    }
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
        assert_eq!(parse_args(&args(&["-", "--complete"])).unwrap().target, "-");
    }

    #[test]
    fn due_duration_labels_and_json_are_options() {
        let parsed = parse_args(&args(&[
            "x", "--due", "friday", "--duration=1.5h", "--add-label", "Urgent", "--add-label", "Home", "--remove-label=Low", "--json",
        ]))
        .unwrap();
        assert_eq!(parsed.changes.due.as_deref(), Some("friday"));
        assert_eq!(parsed.changes.duration.as_deref(), Some("1.5h"));
        assert_eq!(parsed.changes.add_labels, vec!["Urgent", "Home"]);
        assert_eq!(parsed.changes.remove_labels, vec!["Low"]);
        assert!(parsed.json);
    }

    #[test]
    fn bad_options_are_refused() {
        assert!(parse_args(&args(&["x", "--nope"])).unwrap_err().contains("unknown option"));
        assert!(parse_args(&args(&["x", "--title"])).unwrap_err().contains("needs a value"));
        assert!(parse_args(&args(&["x", "--delete", "--complete"])).unwrap_err().contains("can't be combined"));
        assert!(parse_args(&args(&["x", "--delete", "--add-label", "a"])).unwrap_err().contains("can't be combined"));
        assert!(parse_args(&args(&["x", "--delete", "--yes"])).is_ok());
        assert!(parse_args(&args(&["x", "--title", "  "])).is_err());
        assert!(parse_args(&args(&["x", "--add-label", " "])).is_err());
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
    fn ids_are_read_from_stdin_once_each_and_anything_else_is_refused() {
        let read = |text: &str| read_ids(&mut Cursor::new(text.as_bytes().to_vec()));
        assert_eq!(read("a1\n  b2 \n\nc3\na1\n").unwrap(), vec!["a1", "b2", "c3"]);
        assert!(read("a1 b2\n").unwrap_err().contains("isn't a task id"), "two on a line");
        assert!(read("").unwrap_err().contains("no task ids"));
        assert!(read("  \n").unwrap_err().contains("no task ids"));
        assert!(read("a1\nBuy milk\n").unwrap_err().contains("isn't a task id"));
        assert!(read("../etc\n").is_err());
    }

    #[test]
    fn the_patch_has_only_what_changed() {
        let changes = Changes { title: Some("New".into()), complete: Some(true), ..Default::default() };
        let patch = build_patch(&changes, &Resolved::default(), now()).unwrap();
        assert_eq!(patch, json!({"title": "New", "is_complete": true}));
        let moved = Resolved { list: Some(("l-home".into(), "Home".into())), ..Default::default() };
        assert_eq!(build_patch(&Changes::default(), &moved, now()).unwrap(), json!({"list_id": "l-home"}));
        let notes = build_patch(&Changes { notes: Some(String::new()), ..Default::default() }, &Resolved::default(), now()).unwrap();
        assert_eq!(notes, json!({"notes": ""}), "empty notes clear them");
    }

    #[test]
    fn the_patch_carries_due_duration_and_label_ids() {
        let changes = Changes {
            due: Some("2026-10-31".into()),
            duration: Some("1.5h".into()),
            add_labels: vec!["urgent".into()],
            remove_labels: vec!["low".into()],
            ..Default::default()
        };
        let resolved = Resolved {
            add_labels: vec![("x-urgent".into(), "Urgent".into())],
            remove_labels: vec![("x-low".into(), "Low".into())],
            ..Default::default()
        };
        let patch = build_patch(&changes, &resolved, now()).unwrap();
        assert_eq!(
            patch,
            json!({"timeless_due_at": "2026-10-31", "duration_minutes": 90, "add_label_ids": ["x-urgent"], "remove_label_ids": ["x-low"]})
        );
        let timed = build_patch(&Changes { due: Some("tomorrow 3pm".into()), ..Default::default() }, &Resolved::default(), now()).unwrap();
        assert!(timed["due_at"].as_str().unwrap().ends_with('Z'), "{timed}");
    }

    #[test]
    fn a_bad_duration_is_refused_and_a_due_date_may_be_in_the_past() {
        let bad = Changes { duration: Some("soon".into()), ..Default::default() };
        assert!(build_patch(&bad, &Resolved::default(), now()).unwrap_err().contains("isn't a duration"));
        let past = Changes { due: Some("2026-10-01".into()), ..Default::default() };
        assert_eq!(build_patch(&past, &Resolved::default(), now()).unwrap(), json!({"timeless_due_at": "2026-10-01"}));
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

    #[test]
    fn labels_are_typed_with_plus_and_minus() {
        assert_eq!(parse_label_changes("+Urgent -Low High").unwrap(), (vec!["Urgent".to_string(), "High".to_string()], vec!["Low".to_string()]));
        assert_eq!(parse_label_changes("").unwrap(), (vec![], vec![]));
        assert!(parse_label_changes("-").is_err());
        assert!(parse_label_changes("+").is_err());
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
        assert_eq!(menu("7\nfriday\n", &target).unwrap().due.as_deref(), Some("friday"));
        assert_eq!(menu("8\n45m\n", &target).unwrap().duration.as_deref(), Some("45m"));
        let labels = menu("9\n+Urgent -Low\n", &target).unwrap();
        assert_eq!((labels.add_labels, labels.remove_labels), (vec!["Urgent".to_string()], vec!["Low".to_string()]));
        assert!(menu("q\n", &target).unwrap().is_empty());
        assert!(menu("", &target).unwrap().is_empty(), "the end of the input quits");
        assert!(menu("99\n", &target).is_err());
        assert!(menu("1\n\n", &target).is_err(), "an empty title is refused");
        assert!(menu("9\n\n", &target).is_err(), "no labels is refused");
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

    #[test]
    fn a_queued_edit_is_one_line_that_reads_back_and_is_not_mistaken_for_a_task() {
        let edit = PendingEdit {
            id: "t1".into(),
            title: Some("Buy\nmilk".into()),
            patch: Some(json!({"is_complete": true, "add_label_ids": ["x"]})),
            delete: false,
        };
        let entry = edit.to_entry();
        assert!(!entry.contains('\n'), "{entry}");
        assert_eq!(PendingEdit::parse(&entry), Some(edit.clone()));
        assert_eq!(PendingEdit::parse("Buy milk @Home"), None);
        assert_eq!(PendingEdit::parse("{\"queued_edit\": nonsense"), None);
        assert_eq!(edit.describe(), "change add_label_ids, is_complete of 'Buy\nmilk'");
        let delete = PendingEdit { id: "t2".into(), title: None, patch: None, delete: true };
        assert_eq!(PendingEdit::parse(&delete.to_entry()), Some(delete.clone()));
        assert_eq!(delete.describe(), "delete 't2'");
    }
}
