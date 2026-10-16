//! Deciding whether a cached task matches a search expression, and reading the fields that
//! searching, sorting and grouping need.
//!
//! The field names read from a task's JSON (`due_at`, `timeless_due_at`, `snoozed_until`,
//! `starts_at`, `label_ids`, ...) are gathered here so they can be corrected in one place if
//! the API turns out to name something differently.

use crate::query::{Cmp, DateField, DateTest, Expr, NumField, State, TextField, Trait};
use crate::store::StoredTask;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use serde_json::Value;
use std::collections::HashMap;

/// A list, label, label group or folder: what a task points at, by id, with its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub id: String,
    pub name: String,
}

impl Named {
    /// Whether a name or id given in a query (already lowercase) picks this out.
    pub fn is(&self, wanted: &str) -> bool {
        self.name.to_lowercase() == wanted || (!self.id.is_empty() && self.id.to_lowercase() == wanted)
    }

    /// Whether a name or id given in a query matches: part of the name, part of the name ignoring
    /// spaces, slashes and dashes (so `workprojects` finds "Work Projects", the way shell
    /// completion offers it), or the whole id.
    fn contains(&self, wanted: &str) -> bool {
        let name = self.name.to_lowercase();
        let squashed_wanted = squash(wanted);
        name.contains(wanted)
            || (!squashed_wanted.is_empty() && squash(&name).contains(&squashed_wanted))
            || (!self.id.is_empty() && self.id.to_lowercase() == wanted)
    }
}

pub struct Context {
    lists: HashMap<String, Value>,
    /// Where each list comes in the order the API gave them, which is the sidebar order.
    list_order: HashMap<String, usize>,
    labels: HashMap<String, Value>,
    pub now: NaiveDateTime,
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

fn first_text(value: &Value, keys: &[&str]) -> String {
    keys.iter().map(|key| text(value, key)).find(|s| !s.is_empty()).unwrap_or_default()
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

/// A moment held either as `YYYY-MM-DD` (a date with no time, taken as the start of that day)
/// or as an RFC 3339 timestamp, shown in the local time zone.
fn moment(task: &Value, key: &str) -> Option<NaiveDateTime> {
    let raw = task.get(key)?.as_str()?;
    if raw.len() == 10 {
        if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            return date.and_hms_opt(0, 0, 0);
        }
    }
    DateTime::parse_from_rfc3339(raw).ok().map(|time| time.with_timezone(&Local).naive_local())
}

fn latest(task: &Value, keys: &[&str]) -> Option<NaiveDateTime> {
    keys.iter().filter_map(|key| moment(task, key)).max()
}

fn squash(value: &str) -> String {
    value.chars().filter(|c| !matches!(c, ' ' | '/' | '-')).collect()
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

fn any_contains(items: &[Named], wanted: &str) -> bool {
    items.iter().any(|item| item.contains(wanted))
}

impl Context {
    pub fn new(lists: &[Value], labels: &[Value], now: NaiveDateTime) -> Self {
        let list_order = lists
            .iter()
            .filter_map(|list| list.get("id")?.as_str())
            .enumerate()
            .map(|(position, id)| (id.to_string(), position))
            .collect();
        Context { lists: index_by_id(lists), list_order, labels: index_by_id(labels), now }
    }

    fn list_ids(&self, task: &Value) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        if let Some(id) = task.get("list_id").and_then(Value::as_str) {
            ids.push(id.to_string());
        }
        if let Some(ordinals) = task.get("list_ordinals").and_then(Value::as_object) {
            let mut extra: Vec<String> = ordinals.keys().filter(|id| !ids.contains(id)).cloned().collect();
            extra.sort();
            ids.extend(extra);
        }
        ids
    }

    /// The lists the task is in.
    pub fn lists_of(&self, task: &Value) -> Vec<Named> {
        self.list_ids(task)
            .into_iter()
            .filter_map(|id| {
                let name = text(self.lists.get(&id)?, "name");
                (!name.is_empty()).then_some(Named { id, name })
            })
            .collect()
    }

    /// The folders the task's lists sit in, read from each list's `path` (`Work/Engineering`) or
    /// its chain of parents.
    pub fn folders_of(&self, task: &Value) -> Vec<Named> {
        let mut folders: Vec<Named> = Vec::new();
        let mut add = |folder: Named| {
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        };
        for id in self.list_ids(task) {
            let Some(list) = self.lists.get(&id) else { continue };
            let path = text(list, "path");
            let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if segments.len() > 1 {
                for name in &segments[..segments.len() - 1] {
                    add(Named { id: String::new(), name: name.to_string() });
                }
            }
            let mut current = list;
            for _ in 0..10 {
                let parent_id = first_text(current, &["parent_id", "folder_id", "parent_list_id"]);
                let Some(parent) = self.lists.get(&parent_id) else { break };
                add(Named { id: parent_id, name: text(parent, "name") });
                current = parent;
            }
        }
        folders
    }

    pub fn labels_of(&self, task: &Value) -> Vec<Named> {
        task.get("label_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|id| {
                let name = text(self.labels.get(id)?, "name");
                (!name.is_empty()).then_some(Named { id: id.to_string(), name })
            })
            .collect()
    }

    /// The label groups the task's labels belong to.
    pub fn label_groups_of(&self, task: &Value) -> Vec<Named> {
        let mut groups: Vec<Named> = Vec::new();
        for label in self.labels_of(task) {
            let Some(label) = self.labels.get(&label.id) else { continue };
            let group = Named {
                id: first_text(label, &["group_id", "label_group_id"]),
                name: first_text(label, &["group_name", "label_group_name", "group"]),
            };
            if (!group.name.is_empty() || !group.id.is_empty()) && !groups.contains(&group) {
                groups.push(group);
            }
        }
        groups
    }

    pub fn list_names(&self, task: &Value) -> Vec<String> {
        self.lists_of(task).into_iter().map(|list| list.name).collect()
    }

    pub fn label_names(&self, task: &Value) -> Vec<String> {
        self.labels_of(task).into_iter().map(|label| label.name).collect()
    }

    /// The earliest position, in sidebar order, of the lists the task is in.
    pub fn list_position(&self, task: &Value) -> Option<usize> {
        self.list_ids(task).iter().filter_map(|id| self.list_order.get(id)).min().copied()
    }

    /// The moment for a date operator. `due` is the later of the due date and any snooze, since
    /// a snoozed task isn't due until it wakes; `first-due` ignores snoozes.
    pub fn date(&self, task: &Value, field: DateField) -> Option<NaiveDateTime> {
        match field {
            DateField::Due => latest(task, &["snoozed_until", "timeless_snoozed_until", "due_at", "timeless_due_at"]),
            DateField::FirstDue => latest(task, &["due_at", "timeless_due_at"]),
            DateField::Snoozed => latest(task, &["snoozed_until", "timeless_snoozed_until"]),
            DateField::Start => latest(task, &["starts_at", "timeless_starts_at"]),
            DateField::Created => latest(task, &["created_at"]),
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
                None => first_text(item, &["name", "title", "filename"]).to_lowercase(),
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
            Trait::Any => true,
        }
    }

    fn state(&self, task: &StoredTask, state: State) -> bool {
        match state {
            State::Complete => task.complete,
            // Available: no start date, or one that has passed. Snoozes don't count.
            State::Available => self.date(&task.json, DateField::Start).is_none_or(|start| start <= self.now),
            // Completed but not yet cleared. Tasks that aren't completed never match.
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
            TextField::List => any_contains(&self.lists_of(task), value),
            TextField::Folder => any_contains(&self.folders_of(task), value),
            TextField::In => any_contains(&self.lists_of(task), value) || any_contains(&self.folders_of(task), value),
            TextField::Label => any_contains(&self.labels_of(task), value),
            TextField::LabelGroup => any_contains(&self.label_groups_of(task), value),
        }
    }

    pub fn matches(&self, expr: &Expr, task: &StoredTask) -> bool {
        match expr {
            Expr::All => true,
            Expr::Text(word) | Expr::Phrase(word) => Self::searchable_text(&task.json).contains(word.as_str()),
            Expr::Field(field, value) => self.text_field(*field, value, &task.json),
            Expr::Date(field, test) => match (self.date(&task.json, *field), test) {
                (None, _) => false,
                (Some(moment), DateTest::Within(from, to)) => *from <= moment && moment <= *to,
                (Some(moment), DateTest::Cmp(cmp, other)) => compare(moment, *cmp, *other),
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

    /// Thursday 15 October 2026, 10:30.
    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(10, 30, 0).unwrap()
    }

    fn context() -> Context {
        Context::new(
            &[
                json!({"id": "l-eng", "name": "Engineering", "path": "Work/Engineering"}),
                json!({"id": "l-home", "name": "Home"}),
                json!({"id": "l-sub", "name": "Backend", "parent_id": "f-work"}),
                json!({"id": "f-work", "name": "Work Folder"}),
            ],
            &[
                json!({"id": "x-low", "name": "Low", "group_name": "Priority", "group_id": "g-pri"}),
                json!({"id": "x-high", "name": "High", "group_name": "Priority", "group_id": "g-pri"}),
                json!({"id": "x-err", "name": "Errands"}),
            ],
            now(),
        )
    }

    fn task(json: Value) -> StoredTask {
        StoredTask { json, complete: false }
    }

    /// Whether `query` matches `task`.
    fn hit(query: &str, task: &StoredTask) -> bool {
        let query = parse(&[query.to_string()], now()).unwrap_or_else(|e| panic!("{query}: {e}"));
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
    fn words_match_title_notes_and_attachment_names_ignoring_case_and_order() {
        let t = budget();
        assert!(hit("BUDGET", &t));
        assert!(hit("tickets", &t));
        assert!(hit("pdf", &t));
        assert!(hit("bug budget", &t), "word order doesn't matter");
        assert!(hit("budget bug", &t));
        assert!(!hit("budget zebra", &t));
    }

    #[test]
    fn quotes_match_the_exact_string() {
        // The guide's example: "create tickets" matches "create tickets for feature" but not
        // "create feature tickets".
        let a = task(json!({"id": "a", "title": "create tickets for feature"}));
        let b = task(json!({"id": "b", "title": "create feature tickets"}));
        assert!(hit("\"create tickets\"", &a));
        assert!(!hit("\"create tickets\"", &b));
        assert!(hit("create tickets", &b), "without quotes the words can be anywhere");
        assert!(hit("\"fix budget\"", &budget()));
        assert!(!hit("\"budget fix\"", &budget()));
    }

    #[test]
    fn negation_of_words_and_phrases() {
        let t = task(json!({"id": "n", "title": "research the budget meeting"}));
        assert!(!hit("research -meeting", &t));
        assert!(hit("research -zebra", &t));
        assert!(!hit("research -\"budget meeting\"", &t));
        assert!(hit("research -\"meeting budget\"", &t));
    }

    #[test]
    fn plain_words_do_not_match_the_list_or_label_names() {
        let t = budget();
        assert!(!hit("engineering", &t));
        assert!(!hit("low", &t));
    }

    #[test]
    fn title_notes_and_attachment_operators() {
        let t = budget();
        assert!(hit("title:budget", &t));
        assert!(!hit("title:tickets", &t));
        assert!(hit("notes:q4", &t));
        assert!(!hit("notes:budget", &t));
        assert!(hit("attachment:budget.pdf", &t));
        assert!(!hit("attachment:tickets", &t));
        assert!(hit("title:\"fix budget\"", &t));
    }

    #[test]
    fn list_matches_the_list_name_or_id() {
        let t = budget();
        assert!(hit("list:engineering", &t));
        assert!(hit("list:eng", &t));
        assert!(hit("list:l-eng", &t));
        assert!(!hit("list:l-en", &t), "an id must match whole");
        assert!(!hit("list:home", &t));
        assert!(!hit("list:work", &t), "list: looks at the list, not its folder");
    }

    #[test]
    fn folder_matches_the_folders_a_list_sits_in() {
        let t = budget();
        assert!(hit("folder:work", &t), "from the list's path");
        assert!(!hit("folder:engineering", &t), "the list itself is not a folder");
        let sub = task(json!({"id": "s", "title": "x", "list_id": "l-sub"}));
        assert!(hit("folder:\"work folder\"", &sub), "from the chain of parents");
        assert!(hit("folder:f-work", &sub), "by id");
        assert!(!hit("folder:backend", &sub));
        let home = task(json!({"id": "h", "title": "x", "list_id": "l-home"}));
        assert!(!hit("folder:home", &home), "a list outside any folder matches no folder");
    }

    #[test]
    fn in_matches_a_list_or_a_folder() {
        let t = budget();
        assert!(hit("in:engineering", &t));
        assert!(hit("in:work", &t));
        assert!(hit("in:l-eng", &t));
        assert!(!hit("in:home", &t));
        let sub = task(json!({"id": "s", "title": "x", "list_id": "l-sub"}));
        assert!(hit("in:backend", &sub));
        assert!(hit("in:\"work folder\"", &sub));
        assert!(hit("research in:engineering", &task(json!({"id": "r", "title": "research", "list_id": "l-eng"}))));
    }

    #[test]
    fn label_matches_the_name_or_id() {
        let t = budget();
        assert!(hit("label:low", &t));
        assert!(hit("label:lo", &t));
        assert!(hit("label:x-low", &t));
        assert!(!hit("label:x-lo", &t));
        assert!(!hit("label:high", &t));
    }

    #[test]
    fn names_match_without_their_spaces_too() {
        let ctx = Context::new(
            &[json!({"id": "l1", "name": "Work Projects"})],
            &[json!({"id": "x1", "name": "High-Priority"})],
            now(),
        );
        let t = task(json!({"id": "w", "title": "x", "list_id": "l1", "label_ids": ["x1"]}));
        let hit_in = |query: &str| {
            let q = parse(&[query.to_string()], now()).unwrap();
            ctx.matches(&q.expr, &t)
        };
        assert!(hit_in("list:workprojects"));
        assert!(hit_in("in:work-projects"));
        assert!(hit_in("label:highpriority"));
        assert!(hit_in("label:high-priority"));
        assert!(hit_in("label:priority"));
        assert!(!hit_in("list:projectswork"));
        assert!(!hit_in("list:-"), "a query that is only separators matches nothing special");
    }

    #[test]
    fn label_group_matches_the_group_name_or_id() {
        let t = budget();
        assert!(hit("label-group:priority", &t));
        assert!(hit("label-group:g-pri", &t));
        assert!(!hit("label-group:errands", &t));
        let errand = task(json!({"id": "e", "title": "x", "label_ids": ["x-err"]}));
        assert!(!hit("label-group:priority", &errand), "a label with no group is in no group");
    }

    #[test]
    fn tasks_with_no_list_or_label_match_no_list_or_label_operator() {
        let t = task(json!({"id": "3", "title": "loose"}));
        for query in ["list:a", "folder:a", "in:a", "label:a", "label-group:a", "has:label"] {
            assert!(!hit(query, &t), "{query}");
        }
        assert!(hit("!label:low", &t));
    }

    #[test]
    fn date_shorthands_cover_the_period() {
        let t = budget();
        assert!(hit("due:20261031", &t));
        assert!(hit("due:2026-10-31", &t));
        assert!(hit("due:16d", &t));
        assert!(hit("due:month", &t));
        assert!(hit("due:year", &t));
        assert!(!hit("due:week", &t));
        assert!(!hit("due:today", &t));
        assert!(!hit("due:tomorrow", &t));
        assert!(hit("created:2026-10-01", &t));
        assert!(hit("first-due:month", &t));
    }

    #[test]
    fn a_timed_task_is_due_today_whatever_the_time_of_day() {
        // Noon UTC falls on the same calendar day in every zone from UTC-11 to UTC+11.
        let noon = task(json!({"id": "t", "title": "x", "due_at": "2026-10-15T12:00:00Z"}));
        assert!(hit("due:today", &noon));
        assert!(!hit("due:tomorrow", &noon));
        assert!(hit("due:week", &noon));
    }

    #[test]
    fn date_comparisons_on_whole_days() {
        let t = budget();
        assert!(hit("due>today", &t));
        assert!(hit("due>=2026-10-31", &t));
        assert!(!hit("due>2026-10-31", &t), "a task due on the 31st is not after the 31st");
        assert!(hit("due<=2026-10-31", &t), "a task due on the 31st is on or before the 31st");
        assert!(!hit("due<2026-10-31", &t));
        assert!(hit("due<2026-11-01", &t));
        assert!(hit("created<today", &t));
        assert!(hit("created>-30d", &t));
    }

    #[test]
    fn date_comparisons_use_exact_moments_for_times_and_relative_amounts() {
        // A date with no time starts at midnight, which is before now (10:30) + 3h.
        let today = task(json!({"id": "t", "title": "x", "timeless_due_at": "2026-10-15"}));
        assert!(hit("due<=3h", &today));
        assert!(hit("due<=0h", &today), "midnight today is before now");
        assert!(!hit("due>3h", &today));
        // Five days away is beyond 3 hours but within 10 days.
        let later = task(json!({"id": "l", "title": "x", "timeless_due_at": "2026-10-20"}));
        assert!(!hit("due<=3h", &later));
        assert!(hit("due>3h", &later));
        assert!(hit("due<=10d", &later));
        assert!(!hit("due<=2d", &later));
        // Overdue tasks are "due in the next 3 hours" too, as in Godspeed's example.
        let overdue = task(json!({"id": "o", "title": "x", "timeless_due_at": "2026-10-01"}));
        assert!(hit("due<=3h", &overdue));
    }

    #[test]
    fn the_guides_snoozed_until_oct_14th_example() {
        let snoozed = task(json!({"id": "s", "title": "x", "snoozed_until": "2026-10-14T12:00:00Z"}));
        let other = task(json!({"id": "o", "title": "x", "snoozed_until": "2026-10-20T12:00:00Z"}));
        let query = "snoozed>=\"Oct 14th 12am\" & snoozed<=\"Oct 14th 1159pm\"";
        assert!(hit(query, &snoozed));
        assert!(!hit(query, &other));
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
    fn timestamps_with_a_time_count_for_their_day() {
        let t = task(json!({"id": "6", "title": "timed", "due_at": "2026-10-20T12:00:00Z"}));
        assert!(hit("due:2026-10-20", &t));
        assert!(hit("has:due-time", &t));
        assert!(!hit("has:due-time", &budget()));
    }

    #[test]
    fn duration_and_number_operators() {
        let t = budget();
        assert!(hit("duration<=30m", &t));
        assert!(hit("duration=30", &t));
        assert!(hit("duration:30", &t));
        assert!(hit("duration>=0.5h", &t));
        assert!(hit("duration<\"1 hour and 30 minutes\"", &t));
        assert!(!hit("duration>30m", &t));
        assert!(hit("snooze-count=2", &t));
        assert!(hit("snooze-count>=2", &t));
        assert!(!hit("snooze-count>=10", &t));
        assert!(hit("indent-level=0", &t), "top-level tasks have indent level 0");
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
        for query in ["has:notes", "has:attachment", "has:label"] {
            assert!(!hit(query, &bare), "{query}");
        }
    }

    #[test]
    fn has_any_matches_every_task() {
        let bare = task(json!({"id": "8", "title": "bare"}));
        assert!(hit("has:any", &bare));
        assert!(hit("has:any", &budget()));
        assert!(!hit("!has:any", &budget()));
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
    fn uncleared_means_completed_but_not_yet_cleared() {
        let open = budget();
        let done = StoredTask { json: json!({"id": "d", "title": "done"}), complete: true };
        let cleared = StoredTask {
            json: json!({"id": "c", "title": "cleared", "cleared_at": "2026-10-01T00:00:00Z"}),
            complete: true,
        };
        assert!(hit("is:uncleared", &done));
        assert!(!hit("is:uncleared", &cleared));
        assert!(!hit("is:uncleared", &open), "an incomplete task is never uncleared");
        assert!(hit("!is:uncleared", &open));
        assert!(hit("!is:uncleared", &cleared));
        assert!(!hit("!is:uncleared", &done));
    }

    #[test]
    fn complete_is_for_the_local_copy() {
        let done = StoredTask { json: json!({"id": "d", "title": "done"}), complete: true };
        assert!(hit("is:complete", &done));
        assert!(!hit("is:complete", &budget()));
    }

    #[test]
    fn available_means_no_start_date_or_one_in_the_past_and_ignores_snoozes() {
        assert!(hit("is:available", &budget()));
        let later = task(json!({"id": "a", "title": "t", "timeless_starts_at": "2026-10-20"}));
        assert!(!hit("is:available", &later));
        let started_today = task(json!({"id": "b", "title": "t", "timeless_starts_at": "2026-10-15"}));
        assert!(hit("is:available", &started_today));
        let past = task(json!({"id": "p", "title": "t", "timeless_starts_at": "2026-09-01"}));
        assert!(hit("is:available", &past));
        let snoozed = task(json!({"id": "c", "title": "t", "timeless_snoozed_until": "2026-10-16"}));
        assert!(hit("is:available", &snoozed), "a snooze doesn't make a task unavailable");
        let started_later_today = task(json!({"id": "d", "title": "t", "starts_at": "2099-01-01T00:00:00Z"}));
        assert!(!hit("is:available", &started_later_today));
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
        // From the guide.
        assert!(hit("budget & (bug | error)", &t));
        assert!(hit("budget & !zebra", &t));
        assert!(hit("in:features | (budget & !(zebra | giraffe))", &t));
        assert!(!hit("budget & !(bug | error)", &t));
    }

    #[test]
    fn a_task_in_several_lists_matches_each() {
        let t = task(json!({"id": "m", "title": "multi", "list_id": "l-eng", "list_ordinals": {"l-home": 1, "l-eng": 2}}));
        assert!(hit("list:engineering", &t));
        assert!(hit("list:home", &t));
        assert_eq!(context().list_names(&t.json), vec!["Engineering", "Home"]);
    }

    #[test]
    fn names_are_returned_as_written_and_list_position_follows_the_api_order() {
        let t = task(json!({"id": "n", "title": "x", "list_id": "l-home", "label_ids": ["x-high", "x-err", "unknown"]}));
        assert_eq!(context().list_names(&t.json), vec!["Home"]);
        assert_eq!(context().label_names(&t.json), vec!["High", "Errands"]);
        assert_eq!(context().list_position(&t.json), Some(1));
        assert_eq!(context().list_position(&budget().json), Some(0));
        assert_eq!(context().list_position(&json!({"id": "z"})), None);
    }

    #[test]
    fn named_items_are_picked_by_whole_name_or_id() {
        let item = Named { id: "ABC-123".to_string(), name: "Morning".to_string() };
        assert!(item.is("morning"));
        assert!(item.is("abc-123"));
        assert!(!item.is("morn"));
        let without_id = Named { id: String::new(), name: "x".to_string() };
        assert!(without_id.is("x"));
        assert!(!without_id.is(""), "an empty id must not match an empty query");
    }
}
