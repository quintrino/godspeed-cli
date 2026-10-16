//! Putting search results in order, grouping them, applying `limit:`, and printing them as a
//! fixed-width table or as JSON.

use crate::eval::{Context, Named};
use crate::query::{DateField, GroupField, GroupSpec, NumField, Query, SortField, SortSpec};
use crate::store::StoredTask;
use chrono::NaiveDateTime;
use serde_json::{json, Value};
use std::cmp::Ordering;

/// Column widths of the table: title, list, due date, labels.
const WIDTHS: [usize; 4] = [80, 12, 10, 20];

/// The name of the group for tasks that have no value to group by.
const NONE_GROUP: &str = "None";

/// What a task is ordered by. A value the task doesn't have sorts after every real one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Missing,
    Text(String),
    Num(i64),
    Moment(NaiveDateTime),
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

/// Where a name or id falls among the targets, or after all of them.
fn rank_of(item: &Named, targets: &[String]) -> usize {
    targets.iter().position(|target| item.is(target)).unwrap_or(targets.len())
}

/// The sort key for a task that has several lists, labels or groups: the best-ranked one, or
/// the first alphabetically when there are no targets.
fn key_for(items: &[Named], targets: &[String]) -> Key {
    let best = items.iter().map(|item| (rank_of(item, targets), item.name.to_lowercase())).min();
    match best {
        None => Key::Missing,
        Some((_, name)) if targets.is_empty() => Key::Text(name),
        Some((rank, name)) => Key::Rank(rank, name),
    }
}

fn moment_key(moment: Option<NaiveDateTime>) -> Key {
    moment.map_or(Key::Missing, Key::Moment)
}

fn sort_key(spec: &SortSpec, task: &StoredTask, ctx: &Context) -> Key {
    let json = &task.json;
    let number = |field| ctx.number(json, field).map_or(Key::Missing, |n| Key::Num(n));
    match spec.field {
        SortField::Title => json.get("title").and_then(Value::as_str).map_or(Key::Missing, |t| Key::Text(t.to_lowercase())),
        // Lists go in the order of the sidebar unless priority targets are given.
        SortField::List if spec.targets.is_empty() => {
            ctx.list_position(json).map_or(Key::Missing, |position| Key::Num(position as i64))
        }
        SortField::List => key_for(&ctx.lists_of(json), &spec.targets),
        SortField::Label => key_for(&ctx.labels_of(json), &spec.targets),
        SortField::Due => moment_key(ctx.date(json, DateField::Due)),
        SortField::FirstDue => moment_key(ctx.date(json, DateField::FirstDue)),
        SortField::Snoozed => moment_key(ctx.date(json, DateField::Snoozed)),
        SortField::Start => moment_key(ctx.date(json, DateField::Start)),
        SortField::Created => moment_key(ctx.date(json, DateField::Created)),
        SortField::Modified => {
            json.get("updated_at").and_then(Value::as_str).map_or(Key::Missing, |t| Key::Text(t.to_string()))
        }
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

/// Which group a task goes in, and where that group comes in the list of groups.
struct Placement {
    name: String,
    /// The position among the targets, or after them all.
    rank: usize,
    none: bool,
}

impl Placement {
    fn none(rank: usize) -> Placement {
        Placement { name: NONE_GROUP.to_string(), rank, none: true }
    }
}

fn date_placement(moment: Option<NaiveDateTime>) -> Placement {
    match moment {
        Some(moment) => Placement { name: moment.date().format("%Y-%m-%d").to_string(), rank: 0, none: false },
        None => Placement::none(0),
    }
}

/// The groups a task belongs in, for a field that names things (lists, labels, label groups).
///
/// With targets, the task goes in the first target it matches. If it matches none, it goes in
/// "None", or, if the targets ended with `...rest`, in its own groups as if there were no
/// targets. Without targets it goes in the first group alphabetically. `duplicates` puts it in
/// every matching group instead of one.
fn named_placements(items: Vec<Named>, spec: &GroupSpec, duplicates: bool) -> Vec<Placement> {
    let mut items = items;
    items.sort_by_key(|item| item.name.to_lowercase());

    let mut placements: Vec<Placement> = Vec::new();
    for (position, target) in spec.targets.iter().enumerate() {
        if let Some(item) = items.iter().find(|item| item.is(target)) {
            placements.push(Placement { name: item.name.clone(), rank: position, none: false });
        }
    }
    if placements.is_empty() {
        if !spec.targets.is_empty() && !spec.rest {
            return vec![Placement::none(spec.targets.len())];
        }
        placements = items
            .iter()
            .map(|item| Placement { name: item.name.clone(), rank: spec.targets.len(), none: false })
            .collect();
    }
    if placements.is_empty() {
        return vec![Placement::none(spec.targets.len())];
    }
    if !duplicates {
        placements.truncate(1);
    }
    placements
}

fn placements(spec: &GroupSpec, task: &StoredTask, duplicates: bool, ctx: &Context) -> Vec<Placement> {
    let json = &task.json;
    let by = |field| date_placement(ctx.date(json, field));
    match spec.field {
        GroupField::List => named_placements(ctx.lists_of(json), spec, duplicates),
        GroupField::Label => named_placements(ctx.labels_of(json), spec, duplicates),
        GroupField::LabelGroup => named_placements(ctx.label_groups_of(json), spec, duplicates),
        GroupField::Due => vec![by(DateField::Due)],
        GroupField::FirstDue => vec![by(DateField::FirstDue)],
        GroupField::Snoozed => vec![by(DateField::Snoozed)],
        GroupField::Start => vec![by(DateField::Start)],
        GroupField::DueOrStart => {
            vec![date_placement(ctx.date(json, DateField::Due).or_else(|| ctx.date(json, DateField::Start)))]
        }
        GroupField::Created => vec![by(DateField::Created)],
    }
}

/// Splits the tasks into groups, keeping their order within each. Target groups come first in
/// the order given, then the others alphabetically (dates chronologically), then "None".
fn group_tasks<'a>(tasks: &[&'a StoredTask], spec: &GroupSpec, duplicates: bool, ctx: &Context) -> Vec<Group<'a>> {
    let mut groups: Vec<(Placement, Group<'a>)> = Vec::new();
    for &task in tasks {
        for placement in placements(spec, task, duplicates, ctx) {
            match groups.iter_mut().find(|(p, g)| p.none == placement.none && g.name == placement.name) {
                Some((_, group)) => group.tasks.push(task),
                None => {
                    let name = placement.name.clone();
                    groups.push((placement, Group { name, tasks: vec![task] }));
                }
            }
        }
    }
    groups.sort_by_key(|(p, g)| (p.none, p.rank, g.name.to_lowercase()));
    groups.into_iter().map(|(_, group)| group).collect()
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
    let due = ctx
        .date(&task.json, DateField::Due)
        .map(|moment| moment.date().format("%Y-%m-%d").to_string())
        .unwrap_or_default();
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

/// ANSI escape codes for red text and for going back to normal.
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

/// The rows of the table. With `color`, an overdue task's whole row is red. The codes go round
/// the finished row, after the columns are padded, so they can't change how the columns line up.
fn table_rows(tasks: &[&StoredTask], ctx: &Context, color: bool) -> Vec<String> {
    tasks
        .iter()
        .map(|task| {
            let line = format_line(&row_cells(task, ctx));
            if color && ctx.is_overdue(task) { format!("{RED}{line}{RESET}") } else { line }
        })
        .collect()
}

fn task_json(task: &StoredTask, ctx: &Context) -> Value {
    let mut value = task.json.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("list_names".to_string(), json!(ctx.list_names(&task.json)));
        object.insert("label_names".to_string(), json!(ctx.label_names(&task.json)));
    }
    value
}

/// Keeps the first `limit` tasks, counting them group by group in the order the groups are shown,
/// and drops the groups that are left empty. A group the limit cuts through keeps its first tasks.
fn limit_groups(groups: &mut Vec<Group<'_>>, limit: usize) {
    let mut remaining = limit;
    for group in groups.iter_mut() {
        group.tasks.truncate(remaining);
        remaining -= group.tasks.len();
    }
    groups.retain(|group| !group.tasks.is_empty());
}

/// Sorts, groups and limits `tasks` as the query asks, and formats the result. The table has
/// a header row; with `group:` each group gets a heading and a count instead. In JSON, the
/// result is an array of tasks, or of `{"group", "tasks"}` objects when grouping. Every task
/// also gets `list_names` and `label_names`.
///
/// `limit:` counts tasks in the order they are shown. With `group:` that means the first groups
/// are kept, so a group listed first (`group:label:Eternal|...`) is never cut to make room for a
/// later one, however the tasks inside it sort.
///
/// With `color`, overdue tasks are shown in red in the table. JSON is never coloured.
pub fn render(query: &Query, mut tasks: Vec<&StoredTask>, ctx: &Context, json: bool, color: bool) -> String {
    sort_tasks(&mut tasks, &query.sorts, ctx);

    let mut groups = query.group.as_ref().map(|spec| group_tasks(&tasks, spec, query.duplicates, ctx));
    match (&mut groups, query.limit) {
        (Some(groups), Some(limit)) => limit_groups(groups, limit),
        (None, Some(limit)) => tasks.truncate(limit),
        (_, None) => {}
    }

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
                lines.extend(table_rows(&g.tasks, ctx, color));
                lines.join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        None if tasks.is_empty() => String::new(),
        None => {
            let header = format_line(&["TITLE", "LIST", "DUE", "LABELS"].map(String::from));
            let mut lines = vec![header];
            lines.extend(table_rows(&tasks, ctx, color));
            lines.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;
    use chrono::NaiveDate;

    /// Thursday 15 October 2026, 10:30.
    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(10, 30, 0).unwrap()
    }

    fn context() -> Context {
        // The lists are given in sidebar order: Work, Home, Misc.
        Context::new(
            &[
                json!({"id": "l-work", "name": "Work"}),
                json!({"id": "l-home", "name": "Home"}),
                json!({"id": "l-misc", "name": "Misc"}),
            ],
            &[
                json!({"id": "x-high", "name": "High", "group_name": "Severity", "group_id": "g-sev"}),
                json!({"id": "x-med", "name": "Medium", "group_name": "Severity", "group_id": "g-sev"}),
                json!({"id": "x-low", "name": "Low", "group_name": "Severity", "group_id": "g-sev"}),
                json!({"id": "x-home", "name": "Chores", "group_name": "Errands", "group_id": "g-err"}),
                json!({"id": "x-plain", "name": "Plain"}),
            ],
            now(),
        )
    }

    fn task(id: &str, extra: Value) -> StoredTask {
        let mut json = json!({"id": id, "title": format!("Task {id}"), "updated_at": "2026-01-01T00:00:00Z"});
        json.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        StoredTask { json, complete: false }
    }

    fn query(text: &str) -> Query {
        parse(&[text.to_string()], now()).unwrap()
    }

    fn ids(output: &str) -> Vec<String> {
        output
            .lines()
            .filter_map(|line| line.split_whitespace().nth(1).filter(|_| line.starts_with("Task")))
            .map(str::to_string)
            .collect()
    }

    fn run(text: &str, tasks: &[StoredTask]) -> String {
        render(&query(text), tasks.iter().collect(), &context(), false, false)
    }

    fn json_ids(text: &str, tasks: &[StoredTask]) -> Vec<String> {
        let out = render(&query(text), tasks.iter().collect(), &context(), true, false);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        parsed.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect()
    }

    /// The group headings, such as "Work (2)".
    fn headings(output: &str) -> Vec<String> {
        output.lines().filter(|l| !l.starts_with("Task") && l.ends_with(')') && !l.is_empty()).map(str::to_string).collect()
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
    fn sort_by_due_start_snoozed_first_due_and_created() {
        let tasks = [
            task("a", json!({"timeless_due_at": "2026-10-20", "timeless_starts_at": "2026-10-03", "timeless_snoozed_until": "2026-10-25", "created_at": "2026-09-02T12:00:00Z"})),
            task("b", json!({"timeless_due_at": "2026-10-22", "timeless_starts_at": "2026-10-01", "created_at": "2026-09-01T12:00:00Z"})),
        ];
        // Effective due: a is snoozed to the 25th, so it is after b (the 22nd).
        assert_eq!(json_ids("sort:due", &tasks), vec!["b", "a"]);
        assert_eq!(json_ids("sort:first-due", &tasks), vec!["a", "b"]);
        assert_eq!(json_ids("sort:start", &tasks), vec!["b", "a"]);
        assert_eq!(json_ids("sort:snoozed", &tasks), vec!["a", "b"]);
        assert_eq!(json_ids("sort:created", &tasks), vec!["b", "a"]);
    }

    #[test]
    fn several_sorts_apply_in_order_and_ties_keep_the_incoming_order() {
        let tasks = [
            task("a", json!({"duration_minutes": 30, "title": "Same"})),
            task("b", json!({"duration_minutes": 15, "title": "Same"})),
            task("c", json!({"duration_minutes": 30, "title": "Alpha"})),
            task("d", json!({"duration_minutes": 30, "title": "Same"})),
        ];
        assert_eq!(json_ids("sort:title sort-desc:duration", &tasks), vec!["c", "a", "d", "b"]);
        assert_eq!(json_ids("sort:duration", &tasks), vec!["b", "a", "c", "d"]);
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
    fn sort_targets_may_be_ids() {
        let tasks = [task("high", json!({"label_ids": ["x-high"]})), task("low", json!({"label_ids": ["x-low"]}))];
        assert_eq!(ids(&run("sort:label:x-low|x-high", &tasks)), vec!["low", "high"]);
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
    fn sort_by_list_follows_the_sidebar_order_not_the_alphabet() {
        let tasks = [
            task("misc", json!({"list_id": "l-misc"})),
            task("home", json!({"list_id": "l-home"})),
            task("none", json!({})),
            task("work", json!({"list_id": "l-work"})),
        ];
        // Sidebar order is Work, Home, Misc; alphabetical would put Home first.
        assert_eq!(ids(&run("sort:list", &tasks)), vec!["work", "home", "misc", "none"]);
        assert_eq!(ids(&run("sort-desc:list", &tasks)), vec!["misc", "home", "work", "none"]);
    }

    #[test]
    fn sort_by_list_with_targets_uses_them_first() {
        let tasks = [task("work", json!({"list_id": "l-work"})), task("misc", json!({"list_id": "l-misc"}))];
        assert_eq!(ids(&run("sort:list:Misc", &tasks)), vec!["misc", "work"]);
    }

    #[test]
    fn sort_by_modified_numbers_and_the_updated_alias() {
        let tasks = [
            task("w", json!({"updated_at": "2026-03-01T00:00:00Z", "snooze_count": 3})),
            task("h", json!({"updated_at": "2026-01-01T00:00:00Z", "snooze_count": 1})),
        ];
        assert_eq!(ids(&run("sort:modified", &tasks)), vec!["h", "w"]);
        assert_eq!(ids(&run("sort-desc:modified", &tasks)), vec!["w", "h"]);
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
    fn grouping_by_list_with_targets_orders_them_first_then_alphabetical_then_none() {
        let tasks = [
            task("n", json!({})),
            task("m", json!({"list_id": "l-misc"})),
            task("w", json!({"list_id": "l-work"})),
            task("h", json!({"list_id": "l-home"})),
        ];
        // Without ...rest the lists that aren't targets collapse into "None".
        assert_eq!(headings(&run("group:list:Home|Work", &tasks)), vec!["Home (1)", "Work (1)", "None (2)"]);
        // With ...rest they keep their own groups, after the targets.
        assert_eq!(
            headings(&run("group:list:Work|...rest", &tasks)),
            vec!["Work (1)", "Home (1)", "Misc (1)", "None (1)"]
        );
    }

    #[test]
    fn grouping_without_targets_is_alphabetical_with_none_last() {
        let tasks = [
            task("n", json!({})),
            task("w", json!({"list_id": "l-work"})),
            task("h", json!({"list_id": "l-home"})),
        ];
        assert_eq!(headings(&run("group:list", &tasks)), vec!["Home (1)", "Work (1)", "None (1)"]);
    }

    #[test]
    fn group_targets_can_be_ids_and_quoted_names() {
        let tasks = [task("w", json!({"list_id": "l-work"})), task("h", json!({"list_id": "l-home"}))];
        assert_eq!(headings(&run("group:list:l-home|l-work", &tasks)), vec!["Home (1)", "Work (1)"]);
        let lists = Context::new(
            &[json!({"id": "a", "name": "After lunch"}), json!({"id": "b", "name": "Morning"})],
            &[],
            now(),
        );
        let t = [task("1", json!({"list_id": "a"})), task("2", json!({"list_id": "b"}))];
        let out = render(&query("group:list:Morning|\"After lunch\""), t.iter().collect(), &lists, false, false);
        assert_eq!(headings(&out), vec!["Morning (1)", "After lunch (1)"]);
    }

    #[test]
    fn the_guides_label_example_puts_unlisted_labels_in_none() {
        let tasks = [
            task("p0", json!({"label_ids": ["x-high"]})),
            task("other", json!({"label_ids": ["x-plain"]})),
            task("p1", json!({"label_ids": ["x-med"]})),
        ];
        let out = run("group:label:Medium|High", &tasks);
        assert_eq!(headings(&out), vec!["Medium (1)", "High (1)", "None (1)"]);
    }

    #[test]
    fn groups_are_separated_by_a_blank_line_and_counted() {
        let tasks = [
            task("a", json!({"list_id": "l-work"})),
            task("b", json!({"list_id": "l-work"})),
            task("c", json!({"list_id": "l-home"})),
        ];
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
        assert_eq!(ids(&run("group:list sort:title", &tasks)), vec!["z", "a", "b"]);
    }

    #[test]
    fn a_task_with_several_labels_is_grouped_under_the_first_alphabetically() {
        let tasks = [task("both", json!({"label_ids": ["x-low", "x-high"]}))];
        let out = run("group:label", &tasks);
        assert_eq!(headings(&out), vec!["High (1)"]);
    }

    #[test]
    fn with_targets_a_task_goes_under_the_first_target_it_matches_unless_duplicates_are_on() {
        let tasks = [task("both", json!({"label_ids": ["x-low", "x-high"]})), task("none", json!({}))];
        let once = run("group:label:Low|High", &tasks);
        assert_eq!(headings(&once), vec!["Low (1)", "None (1)"]);

        let twice = run("group:label:Low|High enable:duplicates", &tasks);
        assert_eq!(headings(&twice), vec!["Low (1)", "High (1)", "None (1)"]);
        assert_eq!(twice.lines().filter(|l| l.starts_with("Task both")).count(), 2);

        let all = run("group:label enable:duplicates", &tasks);
        assert_eq!(headings(&all), vec!["High (1)", "Low (1)", "None (1)"]);
    }

    #[test]
    fn grouping_by_label_group_uses_the_first_group_alphabetically() {
        let tasks = [
            task("both", json!({"label_ids": ["x-low", "x-home"]})),
            task("sev", json!({"label_ids": ["x-high"]})),
            task("plain", json!({"label_ids": ["x-plain"]})),
            task("nolabel", json!({})),
        ];
        let out = run("group:label-group", &tasks);
        // "Errands" sorts before "Severity".
        assert_eq!(headings(&out), vec!["Errands (1)", "Severity (1)", "None (2)"]);
        assert_eq!(headings(&run("group:label-group:Severity", &tasks)), vec!["Severity (2)", "None (2)"]);
        assert_eq!(headings(&run("group:label-group:g-err", &tasks)), vec!["Errands (1)", "None (3)"]);
    }

    #[test]
    fn grouping_by_date_is_chronological_with_undated_last() {
        let tasks = [
            task("none", json!({})),
            task("dec", json!({"timeless_due_at": "2026-12-01"})),
            task("oct", json!({"timeless_due_at": "2026-10-20"})),
        ];
        assert_eq!(headings(&run("group:due", &tasks)), vec!["2026-10-20 (1)", "2026-12-01 (1)", "None (1)"]);
    }

    #[test]
    fn grouping_by_start_and_created_uses_the_calendar_date() {
        let tasks = [
            task("a", json!({"timeless_starts_at": "2026-11-02", "created_at": "2026-09-05T12:00:00Z"})),
            task("b", json!({"timeless_starts_at": "2026-11-01", "created_at": "2026-09-05T13:00:00Z"})),
        ];
        assert_eq!(headings(&run("group:start", &tasks)), vec!["2026-11-01 (1)", "2026-11-02 (1)"]);
        assert_eq!(headings(&run("group:created", &tasks)), vec!["2026-09-05 (2)"]);
    }

    #[test]
    fn grouping_by_due_or_start_uses_the_due_date_or_else_the_start_date() {
        let tasks = [
            task("due", json!({"timeless_due_at": "2026-10-20", "timeless_starts_at": "2026-10-01"})),
            task("start", json!({"timeless_starts_at": "2026-10-10"})),
            task("neither", json!({})),
        ];
        assert_eq!(
            headings(&run("group:due-or-start", &tasks)),
            vec!["2026-10-10 (1)", "2026-10-20 (1)", "None (1)"]
        );
    }

    #[test]
    fn limit_applies_across_groups() {
        let tasks = [
            task("a", json!({"list_id": "l-work"})),
            task("b", json!({"list_id": "l-home"})),
            task("c", json!({"list_id": "l-work"})),
        ];
        assert_eq!(ids(&run("group:list limit:2", &tasks)).len(), 2);
    }

    #[test]
    fn the_limit_is_applied_after_grouping_so_a_first_group_is_never_cut_for_a_later_one() {
        // Undated tasks sort last, so a limit applied before grouping would drop them.
        let mut tasks: Vec<StoredTask> =
            (0..10).map(|i| task(&format!("dated{i}"), json!({"list_id": "l-home", "timeless_due_at": "2020-01-01"}))).collect();
        tasks.extend((0..3).map(|i| task(&format!("undated{i}"), json!({"list_id": "l-work"}))));

        let out = run("group:list:Work|Home sort:due limit:5", &tasks);

        assert_eq!(headings(&out), vec!["Work (3)", "Home (2)"], "{out}");
        assert_eq!(ids(&out), vec!["undated0", "undated1", "undated2", "dated0", "dated1"]);
    }

    #[test]
    fn the_limit_cuts_through_a_group_and_drops_the_groups_after_it() {
        let tasks = [
            task("a", json!({"list_id": "l-work"})),
            task("b", json!({"list_id": "l-work"})),
            task("c", json!({"list_id": "l-work"})),
            task("d", json!({"list_id": "l-home"})),
        ];

        let out = run("group:list:Work|Home limit:2", &tasks);

        assert_eq!(headings(&out), vec!["Work (2)"], "the count is of what is shown: {out}");
        assert_eq!(ids(&out), vec!["a", "b"]);
        assert_eq!(json_groups("group:list:Work|Home limit:2", &tasks), vec![("Work".to_string(), 2)]);
        assert_eq!(json_groups("group:list:Work|Home limit:4", &tasks), vec![("Work".to_string(), 3), ("Home".to_string(), 1)]);
        assert_eq!(json_groups("group:list:Work|Home limit:0", &tasks), vec![]);
        assert_eq!(json_groups("group:list:Work|Home limit:99", &tasks).len(), 2);
    }

    fn json_groups(text: &str, tasks: &[StoredTask]) -> Vec<(String, usize)> {
        let out = render(&query(text), tasks.iter().collect(), &context(), true, false);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        parsed
            .as_array()
            .unwrap()
            .iter()
            .map(|g| (g["group"].as_str().unwrap().to_string(), g["tasks"].as_array().unwrap().len()))
            .collect()
    }

    #[test]
    fn the_limit_counts_a_task_once_for_each_group_it_is_shown_in() {
        let tasks = [task("both", json!({"label_ids": ["x-low", "x-high"]})), task("one", json!({"label_ids": ["x-low"]}))];

        let out = run("group:label:High|Low enable:duplicates limit:2", &tasks);

        assert_eq!(headings(&out), vec!["High (1)", "Low (1)"], "the same task shown twice uses two places: {out}");
    }

    fn dated(id: &str, due: Option<&str>, complete: bool) -> StoredTask {
        let mut t = task(id, json!({}));
        if let Some(due) = due {
            t.json["timeless_due_at"] = json!(due);
        }
        t.complete = complete;
        t
    }

    /// The same text without the colour codes.
    fn uncoloured(text: &str) -> String {
        text.replace(RED, "").replace(RESET, "")
    }

    fn coloured_run(text: &str, tasks: &[StoredTask]) -> String {
        render(&query(text), tasks.iter().collect(), &context(), false, true)
    }

    #[test]
    fn overdue_tasks_are_shown_in_red_and_nothing_else_is() {
        let tasks = [
            dated("overdue", Some("2020-01-01"), false),
            dated("yesterday", Some("2026-10-14"), false),
            dated("today", Some("2026-10-15"), false),
            dated("future", Some("2099-01-01"), false),
            dated("undated", None, false),
        ];

        let out = coloured_run("", &tasks);

        for line in out.lines() {
            let red = line.starts_with(RED) && line.ends_with(RESET);
            let overdue = line.starts_with(&format!("{RED}Task overdue")) || line.starts_with(&format!("{RED}Task yesterday"));
            assert_eq!(red, overdue, "{line:?}");
            if !red {
                assert!(!line.contains('\x1b'), "{line:?}");
            }
        }
        assert_eq!(out.matches(RED).count(), 2, "only the two overdue rows: {out}");
        assert!(out.lines().next().unwrap().starts_with("TITLE"), "the header is plain");
    }

    #[test]
    fn colour_never_changes_the_text_or_the_alignment() {
        let tasks = [
            dated("overdue", Some("2020-01-01"), false),
            dated("future", Some("2099-01-01"), false),
            task("long", json!({"title": "x".repeat(100), "timeless_due_at": "2020-01-01", "list_id": "l-work"})),
        ];

        assert_eq!(uncoloured(&coloured_run("", &tasks)), run("", &tasks));
        assert_eq!(uncoloured(&coloured_run("group:list sort:due", &tasks)), run("group:list sort:due", &tasks));
    }

    #[test]
    fn without_colour_there_are_no_escape_codes() {
        let tasks = [dated("overdue", Some("2020-01-01"), false)];
        assert!(!run("", &tasks).contains('\x1b'));
        assert!(!run("group:list", &tasks).contains('\x1b'));
    }

    #[test]
    fn completed_tasks_are_never_red_however_old_their_due_date() {
        let tasks = [dated("done", Some("2020-01-01"), true), dated("open", Some("2020-01-01"), false)];

        let out = render(&query("is:complete | !is:complete"), tasks.iter().collect(), &context(), false, true);

        let done = out.lines().find(|l| l.contains("Task done")).unwrap();
        let open = out.lines().find(|l| l.contains("Task open")).unwrap();
        assert!(!done.contains('\x1b'), "{done:?}");
        assert!(open.starts_with(RED), "{open:?}");
    }

    #[test]
    fn grouped_output_colours_the_rows_but_not_the_headings() {
        let tasks = [
            task("a", json!({"list_id": "l-work", "timeless_due_at": "2020-01-01"})),
            task("b", json!({"list_id": "l-work", "timeless_due_at": "2099-01-01"})),
            task("c", json!({"list_id": "l-home", "timeless_due_at": "2020-01-01"})),
        ];

        let out = coloured_run("group:list", &tasks);

        assert_eq!(headings(&out), vec!["Home (1)", "Work (2)"], "headings stay plain: {out}");
        assert_eq!(out.matches(RED).count(), 2);
        assert!(out.lines().filter(|l| l.contains("Task b")).all(|l| !l.contains('\x1b')));
    }

    #[test]
    fn json_is_never_coloured() {
        let tasks = [dated("overdue", Some("2020-01-01"), false)];

        let out = render(&query(""), tasks.iter().collect(), &context(), true, true);

        assert!(!out.contains('\x1b'));
        assert!(serde_json::from_str::<Value>(&out).is_ok());
    }

    #[test]
    fn a_snoozed_task_is_not_red_until_the_snooze_ends() {
        let mut snoozed = task("snoozed", json!({"timeless_due_at": "2020-01-01"}));
        snoozed.json["timeless_snoozed_until"] = json!("2099-01-01");

        let out = coloured_run("", &[snoozed]);

        assert!(!out.contains('\x1b'), "{out}");
    }

    #[test]
    fn the_guides_today_smart_list_shape() {
        // due:today group:list sort:due
        let tasks = [
            task("late", json!({"list_id": "l-work", "due_at": "2026-10-15T14:00:00Z"})),
            task("early", json!({"list_id": "l-work", "due_at": "2026-10-15T09:00:00Z"})),
            task("home", json!({"list_id": "l-home", "due_at": "2026-10-15T12:00:00Z"})),
        ];
        let out = run("group:list sort:due", &tasks);
        assert_eq!(headings(&out), vec!["Home (1)", "Work (2)"]);
        assert_eq!(ids(&out), vec!["home", "early", "late"]);
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
        assert_eq!(lines[0], format!("{:<80}  {:<12}  {:<10}  LABELS", "TITLE", "LIST", "DUE"));
        assert_eq!(lines[1], format!("{:<80}  {:<12}  {:<10}  High, Low", "Pay rent", "Home", "2026-10-31"));
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
        let ctx = Context::new(&[json!({"id": "l", "name": "A very long list name"})], &[], now());
        let t = task("2", json!({"list_id": "l"}));
        let rendered = render(&query(""), vec![&t], &ctx, false, false);
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
        assert_eq!(render(&query(""), vec![], &context(), true, false), "[]");
    }

    #[test]
    fn json_keeps_the_full_task_and_adds_list_and_label_names() {
        let tasks = [task("1", json!({"title": "A very long title ".repeat(10), "list_id": "l-work", "label_ids": ["x-low"], "extra": {"k": 1}}))];
        let out = render(&query(""), tasks.iter().collect(), &context(), true, false);
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
        let out = render(&query("group:list"), tasks.iter().collect(), &context(), true, false);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["group"], "Work");
        assert_eq!(parsed[0]["tasks"][0]["id"], "a");
        assert_eq!(parsed[1]["group"], "None");
    }
}
