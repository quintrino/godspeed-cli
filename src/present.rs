//! Putting search results in order, grouping them, applying `limit:`, and printing them as a
//! fixed-width table or as JSON.

use crate::eval::Context;
use crate::query::{DateField, GroupField, NumField, Query, SortField, SortSpec};
use crate::store::StoredTask;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::cmp::Ordering;

/// Column widths of the table: title, list, due date, labels.
const WIDTHS: [usize; 4] = [80, 12, 10, 20];

/// What a task is ordered by. A value the task doesn't have sorts after every real one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Missing,
    Text(String),
    Num(i64),
    Date(NaiveDate),
    /// Position among the priority targets, then name.
    Rank(usize, String),
}

fn compare_keys(a: &Key, b: &Key, descending: bool) -> Ordering {
    match (a, b) {
        (Key::Missing, Key::Missing) => Ordering::Equal,
        (Key::Missing, _) => Ordering::Greater,
        (_, Key::Missing) => Ordering::Less,
        _ if descending => a.cmp(b).reverse(),
        _ => a.cmp(b),
    }
}

fn rank_of(name: &str, targets: &[String]) -> usize {
    let name = name.to_lowercase();
    targets.iter().position(|t| *t == name).unwrap_or(targets.len())
}

/// The sort key for a task that has several names (lists or labels): the best-ranked one.
fn key_for_names(names: &[String], targets: &[String]) -> Key {
    let best = names.iter().map(|name| (rank_of(name, targets), name.to_lowercase())).min();
    match best {
        None => Key::Missing,
        Some((_, name)) if targets.is_empty() => Key::Text(name),
        Some((rank, name)) => Key::Rank(rank, name),
    }
}

fn date_key(date: Option<NaiveDate>) -> Key {
    date.map_or(Key::Missing, Key::Date)
}

fn sort_key(spec: &SortSpec, task: &StoredTask, ctx: &Context) -> Key {
    let json = &task.json;
    let number = |field| ctx.number(json, field).map_or(Key::Missing, Key::Num);
    match spec.field {
        SortField::Title => json.get("title").and_then(Value::as_str).map_or(Key::Missing, |t| Key::Text(t.to_lowercase())),
        SortField::List => key_for_names(&ctx.list_names(json), &spec.targets),
        SortField::Label => key_for_names(&ctx.label_names(json), &spec.targets),
        SortField::Due => date_key(ctx.date(json, DateField::Due)),
        SortField::FirstDue => date_key(ctx.date(json, DateField::FirstDue)),
        SortField::Snoozed => date_key(ctx.date(json, DateField::Snoozed)),
        SortField::Start => date_key(ctx.date(json, DateField::Start)),
        SortField::Created => date_key(ctx.date(json, DateField::Created)),
        SortField::Updated => json.get("updated_at").and_then(Value::as_str).map_or(Key::Missing, |t| Key::Text(t.to_string())),
        SortField::Duration => number(NumField::Duration),
        SortField::SnoozeCount => number(NumField::SnoozeCount),
        SortField::IndentLevel => number(NumField::IndentLevel),
    }
}

/// Orders by each `sort:` in turn, the first being the main one. Tasks that tie keep the order
/// they came in, which is most recently updated first.
fn sort_tasks(tasks: &mut [&StoredTask], sorts: &[SortSpec], ctx: &Context) {
    if sorts.is_empty() {
        return;
    }
    let mut keyed: Vec<(Vec<Key>, &StoredTask)> = tasks
        .iter()
        .map(|&task| (sorts.iter().map(|spec| sort_key(spec, task, ctx)).collect(), task))
        .collect();
    // A stable sort, so tasks that tie keep their incoming order.
    keyed.sort_by(|(a, _), (b, _)| {
        for (spec, (a, b)) in sorts.iter().zip(a.iter().zip(b)) {
            let ordering = compare_keys(a, b, spec.descending);
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    });
    for (slot, (_, task)) in tasks.iter_mut().zip(keyed) {
        *slot = task;
    }
}

pub struct Group<'a> {
    pub name: String,
    pub tasks: Vec<&'a StoredTask>,
}

const NO_LIST: &str = "(no list)";
const NO_LABEL: &str = "(no label)";
const NO_DATE: &str = "(none)";

fn group_names(field: GroupField, task: &StoredTask, ctx: &Context) -> Vec<String> {
    let date = |field| {
        vec![ctx.date(&task.json, field).map_or_else(|| NO_DATE.to_string(), |d| d.format("%Y-%m-%d").to_string())]
    };
    let or_default = |names: Vec<String>, default: &str| if names.is_empty() { vec![default.to_string()] } else { names };
    match field {
        GroupField::List => or_default(ctx.list_names(&task.json), NO_LIST),
        GroupField::Label => or_default(ctx.label_names(&task.json), NO_LABEL),
        GroupField::Due => date(DateField::Due),
        GroupField::FirstDue => date(DateField::FirstDue),
        GroupField::Snoozed => date(DateField::Snoozed),
        GroupField::Start => date(DateField::Start),
        GroupField::Created => date(DateField::Created),
    }
}

/// Splits the tasks into groups, keeping their order within each. The `targets` groups come
/// first in the order given, then the rest alphabetically (dates chronologically), then the
/// group of tasks that have no value. A task with several lists or labels goes in only the
/// best-ranked one, unless `duplicates` is on.
fn group_tasks<'a>(
    tasks: &[&'a StoredTask],
    field: GroupField,
    targets: &[String],
    duplicates: bool,
    ctx: &Context,
) -> Vec<Group<'a>> {
    let mut groups: Vec<Group<'a>> = Vec::new();
    for &task in tasks {
        let mut names = group_names(field, task, ctx);
        names.sort_by_key(|name| (rank_of(name, targets), name.to_lowercase()));
        if !duplicates {
            names.truncate(1);
        }
        for name in names {
            match groups.iter_mut().find(|g| g.name == name) {
                Some(group) => group.tasks.push(task),
                None => groups.push(Group { name, tasks: vec![task] }),
            }
        }
    }
    groups.sort_by_key(|g| (g.name.starts_with('('), rank_of(&g.name, targets), g.name.to_lowercase()));
    groups
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let kept: String = value.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn row_cells(task: &StoredTask, ctx: &Context) -> [String; 4] {
    let title = task.json.get("title").and_then(Value::as_str).unwrap_or("");
    let title = if title.trim().is_empty() { "(untitled)" } else { title };
    let due = ctx.date(&task.json, DateField::Due).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    [
        single_line(title),
        single_line(&ctx.list_names(&task.json).join(", ")),
        due,
        single_line(&ctx.label_names(&task.json).join(", ")),
    ]
}

fn format_line(cells: &[String; 4]) -> String {
    let line: Vec<String> = cells
        .iter()
        .zip(WIDTHS)
        .map(|(cell, width)| format!("{:<width$}", truncate(cell, width)))
        .collect();
    line.join("  ").trim_end().to_string()
}

fn table_rows(tasks: &[&StoredTask], ctx: &Context) -> Vec<String> {
    tasks.iter().map(|task| format_line(&row_cells(task, ctx))).collect()
}

fn task_json(task: &StoredTask, ctx: &Context) -> Value {
    let mut value = task.json.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("list_names".to_string(), json!(ctx.list_names(&task.json)));
        object.insert("label_names".to_string(), json!(ctx.label_names(&task.json)));
    }
    value
}

/// Sorts, limits and groups `tasks` as the query asks, and formats the result. The table has
/// a header row; with `group:` each group gets a heading and a count instead. In JSON, the
/// result is an array of tasks, or of `{"group", "tasks"}` objects when grouping. Every task
/// also gets `list_names` and `label_names`.
pub fn render(query: &Query, mut tasks: Vec<&StoredTask>, ctx: &Context, json: bool) -> String {
    sort_tasks(&mut tasks, &query.sorts, ctx);
    if let Some(limit) = query.limit {
        tasks.truncate(limit);
    }

    let groups = query
        .group
        .as_ref()
        .map(|spec| group_tasks(&tasks, spec.field, &spec.targets, query.duplicates, ctx));

    if json {
        let value = match groups {
            Some(groups) => Value::Array(
                groups
                    .iter()
                    .map(|g| json!({"group": g.name, "tasks": g.tasks.iter().map(|t| task_json(t, ctx)).collect::<Vec<_>>()}))
                    .collect(),
            ),
            None => Value::Array(tasks.iter().map(|t| task_json(t, ctx)).collect()),
        };
        return serde_json::to_string_pretty(&value).expect("the results serialise");
    }

    match groups {
        Some(groups) => groups
            .iter()
            .map(|g| {
                let mut lines = vec![format!("{} ({})", g.name, g.tasks.len())];
                lines.extend(table_rows(&g.tasks, ctx));
                lines.join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        None if tasks.is_empty() => String::new(),
        None => {
            let header = format_line(&["TITLE", "LIST", "DUE", "LABELS"].map(String::from));
            let mut lines = vec![header];
            lines.extend(table_rows(&tasks, ctx));
            lines.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()
    }

    fn context() -> Context {
        Context::new(
            &[
                json!({"id": "l-work", "name": "Work"}),
                json!({"id": "l-home", "name": "Home"}),
                json!({"id": "l-misc", "name": "Misc"}),
            ],
            &[
                json!({"id": "x-high", "name": "High"}),
                json!({"id": "x-med", "name": "Medium"}),
                json!({"id": "x-low", "name": "Low"}),
            ],
            today(),
        )
    }

    fn task(id: &str, extra: Value) -> StoredTask {
        let mut json = json!({"id": id, "title": format!("Task {id}"), "updated_at": "2026-01-01T00:00:00Z"});
        json.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        StoredTask { json, complete: false }
    }

    fn query(text: &str) -> Query {
        parse(&[text.to_string()], today()).unwrap()
    }

    fn ids(output: &str) -> Vec<String> {
        output
            .lines()
            .filter_map(|line| line.split_whitespace().nth(1).filter(|_| line.starts_with("Task")))
            .map(str::to_string)
            .collect()
    }

    fn run(text: &str, tasks: &[StoredTask]) -> String {
        render(&query(text), tasks.iter().collect(), &context(), false)
    }

    #[test]
    fn without_sort_the_order_given_is_kept() {
        let tasks = [task("c", json!({})), task("a", json!({})), task("b", json!({}))];
        assert_eq!(ids(&run("", &tasks)), vec!["c", "a", "b"]);
    }

    #[test]
    fn sort_by_title_and_direction() {
        let tasks = [task("b", json!({})), task("c", json!({})), task("a", json!({}))];
        assert_eq!(ids(&run("sort:title", &tasks)), vec!["a", "b", "c"]);
        assert_eq!(ids(&run("sort-asc:title", &tasks)), vec!["a", "b", "c"]);
        assert_eq!(ids(&run("sort-desc:title", &tasks)), vec!["c", "b", "a"]);
    }

    #[test]
    fn sort_by_date_puts_undated_tasks_last_in_either_direction() {
        let tasks = [
            task("none", json!({})),
            task("late", json!({"timeless_due_at": "2026-12-01"})),
            task("soon", json!({"timeless_due_at": "2026-10-20"})),
        ];
        assert_eq!(ids(&run("sort:due", &tasks)), vec!["soon", "late", "none"]);
        assert_eq!(ids(&run("sort-desc:due", &tasks)), vec!["late", "soon", "none"]);
    }

    #[test]
    fn several_sorts_apply_in_order_and_ties_keep_the_incoming_order() {
        let tasks = [
            task("a", json!({"duration_minutes": 30, "title": "Same"})),
            task("b", json!({"duration_minutes": 15, "title": "Same"})),
            task("c", json!({"duration_minutes": 30, "title": "Alpha"})),
            task("d", json!({"duration_minutes": 30, "title": "Same"})),
        ];
        // Title first, then duration descending, then incoming order (a before d).
        assert_eq!(json_ids("sort:title sort-desc:duration", &tasks), vec!["c", "a", "d", "b"]);
        // Only duration: a, c, d tie and keep their incoming order.
        assert_eq!(json_ids("sort:duration", &tasks), vec!["b", "a", "c", "d"]);
    }

    fn json_ids(text: &str, tasks: &[StoredTask]) -> Vec<String> {
        let out = render(&query(text), tasks.iter().collect(), &context(), true);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        parsed.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn sort_by_label_with_priority_targets() {
        let tasks = [
            task("low", json!({"label_ids": ["x-low"]})),
            task("none", json!({})),
            task("high", json!({"label_ids": ["x-high"]})),
            task("med", json!({"label_ids": ["x-med"]})),
        ];
        assert_eq!(ids(&run("sort:label:High|Medium|Low", &tasks)), vec!["high", "med", "low", "none"]);
        assert_eq!(ids(&run("sort-desc:label:High|Medium|Low", &tasks)), vec!["low", "med", "high", "none"]);
        // Without targets: alphabetical.
        assert_eq!(ids(&run("sort:label", &tasks)), vec!["high", "low", "med", "none"]);
    }

    #[test]
    fn labels_not_among_the_targets_come_after_those_that_are() {
        let tasks = [
            task("low", json!({"label_ids": ["x-low"]})),
            task("high", json!({"label_ids": ["x-high"]})),
            task("med", json!({"label_ids": ["x-med"]})),
        ];
        assert_eq!(ids(&run("sort:label:Medium", &tasks)), vec!["med", "high", "low"]);
    }

    #[test]
    fn sort_by_list_updated_and_numbers() {
        let tasks = [
            task("w", json!({"list_id": "l-work", "updated_at": "2026-03-01T00:00:00Z", "snooze_count": 3})),
            task("h", json!({"list_id": "l-home", "updated_at": "2026-01-01T00:00:00Z", "snooze_count": 1})),
        ];
        assert_eq!(ids(&run("sort:list", &tasks)), vec!["h", "w"]);
        assert_eq!(ids(&run("sort:updated", &tasks)), vec!["h", "w"]);
        assert_eq!(ids(&run("sort-desc:snooze-count", &tasks)), vec!["w", "h"]);
        assert_eq!(ids(&run("sort:indent-level", &tasks)), vec!["w", "h"], "both missing: order kept");
    }

    #[test]
    fn limit_applies_after_sorting() {
        let tasks = [task("c", json!({})), task("a", json!({})), task("b", json!({}))];
        assert_eq!(ids(&run("sort:title limit:2", &tasks)), vec!["a", "b"]);
        assert_eq!(ids(&run("limit:0", &tasks)), Vec::<String>::new());
        assert_eq!(ids(&run("limit:10", &tasks)).len(), 3);
    }

    #[test]
    fn grouping_by_list_orders_targets_first_then_alphabetical_then_none() {
        let tasks = [
            task("n", json!({})),
            task("m", json!({"list_id": "l-misc"})),
            task("w", json!({"list_id": "l-work"})),
            task("h", json!({"list_id": "l-home"})),
        ];
        let out = run("group:list:Work", &tasks);
        let headings: Vec<&str> = out.lines().filter(|l| l.ends_with(" (1)")).collect();
        assert_eq!(headings, vec!["Work (1)", "Home (1)", "Misc (1)", "(no list) (1)"]);
    }

    #[test]
    fn groups_are_separated_by_a_blank_line_and_counted() {
        let tasks = [task("a", json!({"list_id": "l-work"})), task("b", json!({"list_id": "l-work"})), task("c", json!({"list_id": "l-home"}))];
        let out = run("group:list", &tasks);
        assert_eq!(out, format!("Home (1)\n{}\n\nWork (2)\n{}\n{}", row("c", "Home"), row("a", "Work"), row("b", "Work")));
    }

    fn row(id: &str, list: &str) -> String {
        format_line(&[format!("Task {id}"), list.to_string(), String::new(), String::new()])
    }

    #[test]
    fn sorting_applies_within_each_group() {
        let tasks = [
            task("b", json!({"list_id": "l-work"})),
            task("a", json!({"list_id": "l-work"})),
            task("z", json!({"list_id": "l-home"})),
        ];
        let out = run("group:list sort:title", &tasks);
        assert_eq!(ids(&out), vec!["z", "a", "b"]);
    }

    #[test]
    fn grouping_by_label_places_a_multi_label_task_once_unless_duplicates_are_on() {
        let tasks = [task("both", json!({"label_ids": ["x-low", "x-high"]})), task("none", json!({}))];
        let once = run("group:label:High|Medium|Low", &tasks);
        assert_eq!(once.lines().filter(|l| l.starts_with("Task both")).count(), 1);
        assert!(once.contains("High (1)") && !once.contains("Low ("));
        assert!(once.contains("(no label) (1)"));

        let twice = run("group:label enable:duplicates", &tasks);
        assert_eq!(twice.lines().filter(|l| l.starts_with("Task both")).count(), 2);
        assert!(twice.contains("High (1)") && twice.contains("Low (1)"));
    }

    #[test]
    fn grouping_by_date_is_chronological_with_undated_last() {
        let tasks = [
            task("none", json!({})),
            task("dec", json!({"timeless_due_at": "2026-12-01"})),
            task("oct", json!({"timeless_due_at": "2026-10-20"})),
        ];
        let out = run("group:due", &tasks);
        let headings: Vec<&str> = out.lines().filter(|l| l.ends_with(" (1)")).collect();
        assert_eq!(headings, vec!["2026-10-20 (1)", "2026-12-01 (1)", "(none) (1)"]);
    }

    #[test]
    fn limit_applies_across_groups() {
        let tasks = [
            task("a", json!({"list_id": "l-work"})),
            task("b", json!({"list_id": "l-home"})),
            task("c", json!({"list_id": "l-work"})),
        ];
        let out = run("group:list limit:2", &tasks);
        assert_eq!(ids(&out).len(), 2);
    }

    #[test]
    fn the_table_has_a_header_and_fixed_width_columns() {
        let tasks = [task(
            "1",
            json!({"title": "Pay rent", "list_id": "l-home", "timeless_due_at": "2026-10-31", "label_ids": ["x-high", "x-low"]}),
        )];
        let out = run("", &tasks);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        let expected_header = format!("{:<80}  {:<12}  {:<10}  LABELS", "TITLE", "LIST", "DUE");
        assert_eq!(lines[0], expected_header);
        let expected_row = format!("{:<80}  {:<12}  {:<10}  High, Low", "Pay rent", "Home", "2026-10-31");
        assert_eq!(lines[1], expected_row);
    }

    #[test]
    fn long_values_are_truncated_with_an_ellipsis_to_their_column_width() {
        let long_title = "x".repeat(100);
        let tasks = [task("1", json!({"title": long_title, "list_id": "l-work"}))];
        let out = run("", &tasks);
        let line = out.lines().nth(1).unwrap();
        let title_cell: String = line.chars().take(80).collect();
        assert_eq!(title_cell, format!("{}…", "x".repeat(79)));
        assert_eq!(line.chars().nth(80), Some(' '));
        // A list name over 12 characters.
        let ctx = Context::new(&[json!({"id": "l", "name": "A very long list name"})], &[], today());
        let t = task("2", json!({"list_id": "l"}));
        let rendered = render(&query(""), vec![&t], &ctx, false);
        assert!(rendered.lines().nth(1).unwrap().contains("A very long…"), "{rendered}");
    }

    #[test]
    fn titles_with_newlines_stay_on_one_line_and_blank_titles_are_labelled() {
        let tasks = [task("1", json!({"title": "line one\nline two"})), task("2", json!({"title": "  "}))];
        let out = run("", &tasks);
        assert_eq!(out.lines().count(), 3);
        assert!(out.contains("line one line two"));
        assert!(out.contains("(untitled)"));
    }

    #[test]
    fn no_results_print_nothing_for_the_table_but_an_empty_array_for_json() {
        assert_eq!(run("", &[]), "");
        assert_eq!(render(&query(""), vec![], &context(), true), "[]");
    }

    #[test]
    fn json_keeps_the_full_task_and_adds_list_and_label_names() {
        let tasks = [task("1", json!({"title": "A very long title ".repeat(10), "list_id": "l-work", "label_ids": ["x-low"], "extra": {"k": 1}}))];
        let out = render(&query(""), tasks.iter().collect(), &context(), true);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["id"], "1");
        assert_eq!(parsed[0]["extra"], json!({"k": 1}));
        assert_eq!(parsed[0]["list_names"], json!(["Work"]));
        assert_eq!(parsed[0]["label_names"], json!(["Low"]));
        assert!(parsed[0]["title"].as_str().unwrap().len() > 80, "JSON is not truncated");
    }

    #[test]
    fn grouped_json_is_an_array_of_groups() {
        let tasks = [task("a", json!({"list_id": "l-work"})), task("b", json!({}))];
        let out = render(&query("group:list"), tasks.iter().collect(), &context(), true);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["group"], "Work");
        assert_eq!(parsed[0]["tasks"][0]["id"], "a");
        assert_eq!(parsed[1]["group"], "(no list)");
    }
}
