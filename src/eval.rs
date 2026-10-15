//! Deciding whether a cached task matches a search expression, and reading the fields that
//! searching, sorting and grouping need.
//!
//! The field names read from a task's JSON (`due_at`, `timeless_due_at`, `snoozed_until`,
//! `starts_at`, `label_ids`, ...) are gathered here so they can be corrected in one place if
//! the API turns out to name something differently.

use crate::query::{Cmp, DateField, DateTest, Expr, NumField, State, TextField, Trait};
use crate::store::StoredTask;
use chrono::{DateTime, Local, NaiveDate};
use serde_json::Value;
use std::collections::HashMap;

pub struct Context {
    lists: HashMap<String, Value>,
    labels: HashMap<String, Value>,
    pub today: NaiveDate,
}

fn index_by_id(values: &[Value]) -> HashMap<String, Value> {
    values
        .iter()
        .filter_map(|value| Some((value.get("id")?.as_str()?.to_string(), value.clone())))
        .collect()
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Whether the field is there with something in it: not missing, null, "" or [].
fn present(task: &Value, key: &str) -> bool {
    match task.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(_) => true,
    }
}

fn any_present(task: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| present(task, key))
}

/// A date held either as `YYYY-MM-DD` (a date with no time) or as an RFC 3339 timestamp,
/// which is read as a day in the local time zone.
fn date_value(task: &Value, key: &str) -> Option<NaiveDate> {
    let raw = task.get(key)?.as_str()?;
    if raw.len() == 10 {
        if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            return Some(date);
        }
    }
    DateTime::parse_from_rfc3339(raw).ok().map(|time| time.with_timezone(&Local).date_naive())
}

fn latest_date(task: &Value, keys: &[&str]) -> Option<NaiveDate> {
    keys.iter().filter_map(|key| date_value(task, key)).max()
}

fn compare<T: Ord>(left: T, cmp: Cmp, right: T) -> bool {
    match cmp {
        Cmp::Lt => left < right,
        Cmp::Lte => left <= right,
        Cmp::Gt => left > right,
        Cmp::Gte => left >= right,
        Cmp::Eq => left == right,
    }
}

impl Context {
    pub fn new(lists: &[Value], labels: &[Value], today: NaiveDate) -> Self {
        Context { lists: index_by_id(lists), labels: index_by_id(labels), today }
    }

    fn list_values<'a>(&'a self, task: &Value) -> Vec<&'a Value> {
        let mut ids: Vec<String> = Vec::new();
        if let Some(id) = task.get("list_id").and_then(Value::as_str) {
            ids.push(id.to_string());
        }
        if let Some(ordinals) = task.get("list_ordinals").and_then(Value::as_object) {
            let mut extra: Vec<String> = ordinals.keys().filter(|id| !ids.contains(id)).cloned().collect();
            extra.sort();
            ids.extend(extra);
        }
        ids.iter().filter_map(|id| self.lists.get(id)).collect()
    }

    fn label_values<'a>(&'a self, task: &Value) -> Vec<&'a Value> {
        task.get("label_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|id| self.labels.get(id))
            .collect()
    }

    /// The names of the lists the task is in, as written.
    pub fn list_names(&self, task: &Value) -> Vec<String> {
        self.list_values(task).into_iter().map(|list| text(list, "name")).filter(|n| !n.is_empty()).collect()
    }

    pub fn label_names(&self, task: &Value) -> Vec<String> {
        self.label_values(task).into_iter().map(|label| text(label, "name")).filter(|n| !n.is_empty()).collect()
    }

    fn list_paths(&self, task: &Value) -> Vec<String> {
        self.list_values(task)
            .into_iter()
            .map(|list| {
                let path = text(list, "path");
                if path.is_empty() { text(list, "name") } else { path }
            })
            .collect()
    }

    fn label_groups(&self, task: &Value) -> Vec<String> {
        self.label_values(task)
            .into_iter()
            .flat_map(|label| ["group_name", "label_group_name", "group"].map(|key| text(label, key)))
            .filter(|group| !group.is_empty())
            .collect()
    }

    /// The date for a date operator. `due` is the later of the due date and any snooze, since a
    /// snoozed task isn't due until it wakes; `first-due` ignores snoozes.
    pub fn date(&self, task: &Value, field: DateField) -> Option<NaiveDate> {
        match field {
            DateField::Due => {
                latest_date(task, &["snoozed_until", "timeless_snoozed_until", "due_at", "timeless_due_at"])
            }
            DateField::FirstDue => latest_date(task, &["due_at", "timeless_due_at"]),
            DateField::Snoozed => latest_date(task, &["snoozed_until", "timeless_snoozed_until"]),
            DateField::Start => latest_date(task, &["starts_at", "timeless_starts_at"]),
            DateField::Created => latest_date(task, &["created_at"]),
        }
    }

    pub fn number(&self, task: &Value, field: NumField) -> Option<i64> {
        let key = match field {
            NumField::Duration => "duration_minutes",
            NumField::SnoozeCount => "snooze_count",
            NumField::IndentLevel => "indent_level",
        };
        task.get(key).and_then(Value::as_i64)
    }

    fn attachment_names(task: &Value) -> Vec<String> {
        task.get("attachments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|item| match item.as_str() {
                Some(name) => name.to_lowercase(),
                None => ["name", "title", "filename"]
                    .iter()
                    .map(|key| text(item, key))
                    .find(|name| !name.is_empty())
                    .unwrap_or_default()
                    .to_lowercase(),
            })
            .collect()
    }

    /// The text a plain word or phrase is looked for in: title, notes and attachment names.
    fn searchable_text(task: &Value) -> String {
        let mut parts = vec![text(task, "title"), text(task, "notes")];
        parts.extend(Self::attachment_names(task));
        parts.join("\n").to_lowercase()
    }

    fn has_trait(&self, task: &Value, wanted: Trait) -> bool {
        match wanted {
            Trait::Notes => present(task, "notes"),
            Trait::Attachment => present(task, "attachments"),
            Trait::Label => present(task, "label_ids"),
            Trait::Repeat => any_present(task, &["repeat", "recurrence", "repeat_rule"]),
            Trait::Due => any_present(task, &["due_at", "timeless_due_at"]),
            Trait::Snooze => any_present(task, &["snoozed_until", "timeless_snoozed_until"]),
            Trait::Start => any_present(task, &["starts_at", "timeless_starts_at"]),
            Trait::DueTime => present(task, "due_at"),
            Trait::StartTime => present(task, "starts_at"),
            Trait::Bookmark => present(task, "bookmarked_at"),
            Trait::Email => any_present(task, &["email", "email_id", "source_email"]),
            Trait::Duration => present(task, "duration_minutes"),
            Trait::Any => [
                Trait::Notes,
                Trait::Attachment,
                Trait::Label,
                Trait::Repeat,
                Trait::Due,
                Trait::Snooze,
                Trait::Start,
                Trait::Bookmark,
                Trait::Email,
                Trait::Duration,
            ]
            .into_iter()
            .any(|t| self.has_trait(task, t)),
        }
    }

    fn state(&self, task: &StoredTask, state: State) -> bool {
        match state {
            State::Complete => task.complete,
            // Available: not waiting for a start date or a snooze to end.
            State::Available => {
                let waiting = |field| self.date(&task.json, field).is_some_and(|date| date > self.today);
                !waiting(DateField::Start) && !waiting(DateField::Snoozed)
            }
            State::Uncleared => {
                let cleared = task.json.get("is_cleared").and_then(Value::as_bool).unwrap_or(false)
                    || present(&task.json, "cleared_at");
                task.complete && !cleared
            }
        }
    }

    fn text_field(&self, field: TextField, value: &str, task: &Value) -> bool {
        let contains = |haystack: &str| haystack.to_lowercase().contains(value);
        match field {
            TextField::Title => contains(&text(task, "title")),
            TextField::Notes => contains(&text(task, "notes")),
            TextField::Attachment => Self::attachment_names(task).iter().any(|name| name.contains(value)),
            TextField::List => self.list_names(task).iter().any(|name| contains(name)),
            TextField::Folder => self.list_paths(task).iter().any(|path| contains(path)),
            TextField::Label => self.label_names(task).iter().any(|name| contains(name)),
            TextField::LabelGroup => self.label_groups(task).iter().any(|group| contains(group)),
        }
    }

    pub fn matches(&self, expr: &Expr, task: &StoredTask) -> bool {
        match expr {
            Expr::All => true,
            Expr::Text(word) | Expr::Phrase(word) => Self::searchable_text(&task.json).contains(word.as_str()),
            Expr::Field(field, value) => self.text_field(*field, value, &task.json),
            Expr::Date(field, test) => match (self.date(&task.json, *field), test) {
                (None, _) => false,
                (Some(date), DateTest::Within(from, to)) => *from <= date && date <= *to,
                (Some(date), DateTest::Cmp(cmp, other)) => compare(date, *cmp, *other),
            },
            Expr::Number(field, cmp, value) => {
                self.number(&task.json, *field).is_some_and(|number| compare(number, *cmp, *value))
            }
            Expr::Has(wanted) => self.has_trait(&task.json, *wanted),
            Expr::Is(state) => self.state(task, *state),
            Expr::Not(inner) => !self.matches(inner, task),
            Expr::And(left, right) => self.matches(left, task) && self.matches(right, task),
            Expr::Or(left, right) => self.matches(left, task) || self.matches(right, task),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;
    use serde_json::json;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()
    }

    fn context() -> Context {
        Context::new(
            &[
                json!({"id": "l-eng", "name": "Engineering", "path": "Work/Engineering"}),
                json!({"id": "l-home", "name": "Home"}),
            ],
            &[
                json!({"id": "x-low", "name": "Low", "group_name": "Priority"}),
                json!({"id": "x-high", "name": "High", "group_name": "Priority"}),
                json!({"id": "x-err", "name": "Errands"}),
            ],
            today(),
        )
    }

    fn task(json: Value) -> StoredTask {
        StoredTask { json, complete: false }
    }

    /// Whether `query` matches `task`.
    fn hit(query: &str, task: &StoredTask) -> bool {
        let query = parse(&[query.to_string()], today()).unwrap_or_else(|e| panic!("{query}: {e}"));
        context().matches(&query.expr, task)
    }

    fn budget() -> StoredTask {
        task(json!({
            "id": "1",
            "title": "Fix budget bug",
            "notes": "Create tickets for Q4",
            "list_id": "l-eng",
            "label_ids": ["x-low"],
            "timeless_due_at": "2026-10-31",
            "duration_minutes": 30,
            "indent_level": 0,
            "snooze_count": 2,
            "attachments": [{"name": "Budget.PDF"}],
            "created_at": "2026-10-01T12:00:00Z",
        }))
    }

    #[test]
    fn words_match_title_notes_and_attachment_names_case_insensitively() {
        let t = budget();
        assert!(hit("BUDGET", &t));
        assert!(hit("tickets", &t));
        assert!(hit("pdf", &t));
        assert!(hit("budget bug", &t));
        assert!(hit("\"fix budget\"", &t));
        assert!(!hit("\"budget fix\"", &t));
        assert!(!hit("budget zebra", &t));
    }

    #[test]
    fn plain_words_do_not_match_the_list_or_label_names() {
        let t = budget();
        assert!(!hit("engineering", &t));
        assert!(!hit("low", &t));
    }

    #[test]
    fn field_operators() {
        let t = budget();
        assert!(hit("title:budget", &t));
        assert!(!hit("title:tickets", &t));
        assert!(hit("notes:q4", &t));
        assert!(hit("attachment:budget.pdf", &t));
        assert!(hit("list:engineering", &t));
        assert!(hit("list:eng", &t));
        assert!(!hit("list:home", &t));
        assert!(hit("label:low", &t));
        assert!(!hit("label:high", &t));
        assert!(hit("label-group:priority", &t));
        assert!(hit("title:\"fix budget\"", &t));
    }

    #[test]
    fn folder_and_in_match_the_list_path_as_well_as_its_name() {
        let t = budget();
        assert!(hit("folder:work", &t));
        assert!(hit("in:work/eng", &t));
        assert!(hit("in:engineering", &t));
        assert!(!hit("list:work", &t), "list: looks at the name only");
        assert!(!hit("folder:home", &t));
        let home = task(json!({"id": "2", "title": "x", "list_id": "l-home"}));
        assert!(hit("folder:home", &home), "a list without a path falls back to its name");
    }

    #[test]
    fn tasks_with_no_list_or_label_match_no_list_or_label_operator() {
        let t = task(json!({"id": "3", "title": "loose"}));
        for query in ["list:a", "folder:a", "label:a", "label-group:a", "has:label"] {
            assert!(!hit(query, &t), "{query}");
        }
        assert!(hit("!label:low", &t));
    }

    #[test]
    fn date_equality_shorthands_and_periods() {
        let t = budget();
        assert!(hit("due:20261031", &t));
        assert!(hit("due:2026-10-31", &t));
        assert!(hit("due:16d", &t));
        assert!(hit("due:month", &t));
        assert!(hit("due:year", &t));
        assert!(!hit("due:week", &t));
        assert!(!hit("due:today", &t));
        assert!(hit("created:2026-10-01", &t));
    }

    #[test]
    fn date_comparisons() {
        let t = budget();
        assert!(hit("due>today", &t));
        assert!(hit("due>=2026-10-31", &t));
        assert!(!hit("due>2026-10-31", &t));
        assert!(hit("due<=2026-10-31", &t));
        assert!(hit("due<2026-11-01", &t));
        assert!(hit("created<today", &t));
        assert!(hit("created>-30d", &t));
    }

    #[test]
    fn a_task_without_the_date_matches_no_date_test() {
        let t = task(json!({"id": "4", "title": "undated"}));
        for query in ["due:today", "due<today", "due>today", "start:today", "created:today", "snoozed<today"] {
            assert!(!hit(query, &t), "{query}");
        }
        assert!(hit("!due:today", &t));
    }

    #[test]
    fn due_is_the_later_of_the_due_date_and_the_snooze_but_first_due_ignores_snoozes() {
        let t = task(json!({
            "id": "5", "title": "snoozed",
            "timeless_due_at": "2026-10-10",
            "timeless_snoozed_until": "2026-10-20",
        }));
        assert!(hit("due:2026-10-20", &t));
        assert!(!hit("due:2026-10-10", &t));
        assert!(hit("first-due:2026-10-10", &t));
        assert!(hit("snoozed:2026-10-20", &t));
    }

    #[test]
    fn timestamps_with_a_time_are_read_as_days() {
        let t = task(json!({"id": "6", "title": "timed", "due_at": "2026-10-20T12:00:00Z"}));
        assert!(hit("due:2026-10-20", &t));
        assert!(hit("has:due-time", &t));
        assert!(!hit("has:due-time", &budget()));
    }

    #[test]
    fn duration_and_number_operators() {
        let t = budget();
        assert!(hit("duration<=30m", &t));
        assert!(hit("duration:30", &t));
        assert!(hit("duration>=0.5h", &t));
        assert!(!hit("duration>30m", &t));
        assert!(hit("snooze-count=2", &t));
        assert!(hit("snooze-count>1", &t));
        assert!(hit("indent-level=0", &t));
        assert!(!hit("indent-level>0", &t));
        let none = task(json!({"id": "7", "title": "no duration"}));
        assert!(!hit("duration<60m", &none), "a task with no duration is not shorter than 60m");
        assert!(hit("!duration<60m", &none));
    }

    #[test]
    fn traits() {
        let t = budget();
        for query in ["has:notes", "has:attachment", "has:label", "has:due", "has:duration", "has:any"] {
            assert!(hit(query, &t), "{query}");
        }
        for query in ["has:repeat", "has:snooze", "has:start", "has:due-time", "has:start-time", "has:bookmark", "has:email"] {
            assert!(!hit(query, &t), "{query}");
        }
        let bare = task(json!({"id": "8", "title": "bare", "notes": "", "attachments": [], "label_ids": []}));
        for query in ["has:notes", "has:attachment", "has:label", "has:any"] {
            assert!(!hit(query, &bare), "{query}");
        }
    }

    #[test]
    fn repeat_bookmark_and_email_traits() {
        let t = task(json!({
            "id": "9", "title": "t", "repeat": {"every": 1}, "bookmarked_at": "2026-01-01T00:00:00Z",
            "email_id": "abc", "snoozed_until": "2026-10-20T00:00:00Z", "starts_at": "2026-10-20T00:00:00Z",
        }));
        for query in ["has:repeat", "has:bookmark", "has:email", "has:snooze", "has:start", "has:start-time"] {
            assert!(hit(query, &t), "{query}");
        }
    }

    #[test]
    fn states() {
        let open = budget();
        let done = StoredTask { json: json!({"id": "d", "title": "done"}), complete: true };
        let cleared = StoredTask { json: json!({"id": "c", "title": "cleared", "cleared_at": "2026-10-01T00:00:00Z"}), complete: true };
        assert!(!hit("is:complete", &open));
        assert!(hit("is:complete", &done));
        assert!(hit("is:uncleared", &done));
        assert!(!hit("is:uncleared", &cleared));
        assert!(!hit("is:uncleared", &open));
    }

    #[test]
    fn available_means_no_future_start_date_or_snooze() {
        assert!(hit("is:available", &budget()));
        let later = task(json!({"id": "a", "title": "t", "timeless_starts_at": "2026-10-20"}));
        assert!(!hit("is:available", &later));
        let started = task(json!({"id": "b", "title": "t", "timeless_starts_at": "2026-10-15"}));
        assert!(hit("is:available", &started));
        let snoozed = task(json!({"id": "c", "title": "t", "timeless_snoozed_until": "2026-10-16"}));
        assert!(!hit("is:available", &snoozed));
    }

    #[test]
    fn boolean_combinations() {
        let t = budget();
        assert!(hit("budget & label:low", &t));
        assert!(hit("zebra | budget", &t));
        assert!(!hit("zebra | giraffe", &t));
        assert!(hit("(zebra | budget) label:low", &t));
        assert!(hit("!zebra", &t));
        assert!(hit("-zebra", &t));
        assert!(!hit("!budget", &t));
        assert!(hit("budget -label:high", &t));
        assert!(hit("!(label:high | list:home)", &t));
    }

    #[test]
    fn a_task_in_several_lists_matches_each() {
        let t = task(json!({"id": "m", "title": "multi", "list_id": "l-eng", "list_ordinals": {"l-home": 1, "l-eng": 2}}));
        assert!(hit("list:engineering", &t));
        assert!(hit("list:home", &t));
        assert_eq!(context().list_names(&t.json), vec!["Engineering", "Home"]);
    }

    #[test]
    fn names_are_returned_as_written() {
        let t = task(json!({"id": "n", "title": "x", "list_id": "l-home", "label_ids": ["x-high", "x-err", "unknown"]}));
        assert_eq!(context().list_names(&t.json), vec!["Home"]);
        assert_eq!(context().label_names(&t.json), vec!["High", "Errands"]);
    }
}
