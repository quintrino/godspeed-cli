//! `godspeed-cli refresh`: keep a local copy of the tasks for searching.
//!
//! A refresh is either *incremental*, fetching only what changed since the copy was last
//! brought up to date, or *full*, rebuilding the copy from scratch. It is full when asked
//! (`--full`), when there is no copy, when the choice of including completed tasks changes, or
//! when the last full refresh is old: an incremental refresh can't see tasks that were deleted
//! in Godspeed, and a full one drops them.

use crate::api::{self, PageError, TaskPage};
use crate::config::{self, Age};
use crate::store::{CachedTask, Counts, Facts, Snapshot, Store};
use crate::{exit, paths, token};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::process::ExitCode;
use std::time::Duration as StdDuration;

/// The API returns at most this many tasks per request.
const PAGE_SIZE: usize = 250;
/// Reads allowed per minute. Every request after this many waits a minute first.
const READS_PER_MINUTE: usize = 9;
const MAX_RATE_LIMIT_RETRIES: usize = 3;
const DEFAULT_RATE_LIMIT_WAIT_SECS: u64 = 60;
/// An incremental refresh asks for changes from this long before the newest one it has seen,
/// so a change that landed just around that moment is not missed. Re-fetched tasks are harmless.
const OVERLAP_SECS: i64 = 60;

type Error = Box<dyn std::error::Error>;

/// A task status to fetch, and whether tasks with it are completed.
type Status = (&'static str, bool);
const INCOMPLETE: Status = ("incomplete", false);
const COMPLETE: Status = ("complete", true);

/// The `updated_at` of the oldest task on the page, as the API wrote it and as a time.
fn oldest_updated_at(tasks: &[Value]) -> Option<(String, DateTime<Utc>)> {
    tasks
        .iter()
        .filter_map(|task| {
            let raw = task.get("updated_at")?.as_str()?;
            let time = DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc);
            Some((raw.to_string(), time))
        })
        .min_by_key(|(_, time)| *time)
}

/// Fetches the tasks with each of the `statuses`, walking back through `updated_before` until
/// a page comes back short. With `updated_after`, only tasks changed since then.
///
/// The next page starts at the oldest `updated_at` seen, not a moment before it, so tasks that
/// share that timestamp and didn't fit on the page aren't skipped (tasks seen twice are
/// ignored). If a page then adds nothing new, the cut-off steps back a microsecond so the walk
/// always moves on. More than 250 tasks with the very same `updated_at` can't all be reached,
/// which would only happen after a bulk edit.
///
/// `fetch(status, updated_before, updated_after)` gets one page, `sleep` waits, and `progress`
/// reports status.
pub fn collect_all(
    statuses: &[Status],
    updated_after: Option<&str>,
    mut fetch: impl FnMut(&str, Option<&str>, Option<&str>) -> Result<TaskPage, PageError>,
    mut sleep: impl FnMut(StdDuration),
    mut progress: impl FnMut(&str),
) -> Result<Snapshot, Error> {
    let mut snapshot = Snapshot::default();
    let mut seen = HashSet::new();
    let mut requests = 0usize;

    for &(status, complete) in statuses {
        let mut updated_before: Option<String> = None;
        loop {
            let mut retries = 0;
            let page = loop {
                if requests > 0 && requests % READS_PER_MINUTE == 0 {
                    progress("Waiting a minute to stay within the API rate limit...");
                    sleep(StdDuration::from_secs(60));
                }
                requests += 1;
                match fetch(status, updated_before.as_deref(), updated_after) {
                    Ok(page) => break page,
                    Err(PageError::RateLimited(_)) if retries >= MAX_RATE_LIMIT_RETRIES => {
                        return Err("the API kept rate limiting requests; try again later".into());
                    }
                    Err(PageError::RateLimited(wait)) => {
                        retries += 1;
                        let seconds = wait.unwrap_or(DEFAULT_RATE_LIMIT_WAIT_SECS);
                        progress(&format!("Rate limited; waiting {seconds}s..."));
                        sleep(StdDuration::from_secs(seconds));
                        requests = 0; // the wait started a fresh window
                    }
                    Err(PageError::Failed(error)) => return Err(error),
                }
            };

            let page_len = page.tasks.len();
            let oldest = oldest_updated_at(&page.tasks);
            let mut added = 0;
            for task in page.tasks {
                let id = task.get("id").and_then(Value::as_str).map(str::to_string);
                if id.is_some_and(|id| seen.insert(id)) {
                    added += 1;
                    snapshot.tasks.push(CachedTask { json: task, complete });
                }
            }
            snapshot.lists.extend(page.lists);
            snapshot.labels.extend(page.labels);
            progress(&format!("Fetched {} tasks so far", snapshot.tasks.len()));

            if page_len < PAGE_SIZE {
                break;
            }
            let Some((raw, time)) = oldest else {
                return Err("a full page of tasks had no usable updated_at, so the next page can't be requested".into());
            };
            updated_before = Some(if added == 0 {
                (time - Duration::microseconds(1)).to_rfc3339_opts(SecondsFormat::Micros, true)
            } else {
                raw
            });
        }
    }

    Ok(snapshot)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullReason {
    Requested,
    FirstRefresh,
    CompletedChanged,
    NoHighWater,
    Aged,
}

impl FullReason {
    pub fn describe(self) -> &'static str {
        match self {
            FullReason::Requested => "asked for with --full",
            FullReason::FirstRefresh => "there is no local copy yet",
            FullReason::CompletedChanged => "whether to include completed tasks has changed",
            FullReason::NoHighWater => "the last refresh didn't record when the newest change was",
            FullReason::Aged => "the last full refresh is old, and a full one also drops tasks deleted in Godspeed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Full(FullReason),
    /// Fetch what changed after this time.
    Incremental { after: String },
}

/// What kind of refresh to do, given what is known about the copy.
pub fn plan(full: bool, include_completed: bool, facts: &Facts, full_refresh_after: Age, now: DateTime<Utc>) -> Mode {
    if full {
        return Mode::Full(FullReason::Requested);
    }
    if facts.refreshed_at.is_none() || facts.last_full_refresh_at.is_none() {
        return Mode::Full(FullReason::FirstRefresh);
    }
    if facts.include_completed != Some(include_completed) {
        return Mode::Full(FullReason::CompletedChanged);
    }
    let Some(high_water) = facts.high_water.as_deref().and_then(|raw| DateTime::parse_from_rfc3339(raw).ok()) else {
        return Mode::Full(FullReason::NoHighWater);
    };
    if let Some(limit) = full_refresh_after.limit() {
        let last_full = facts
            .last_full_refresh_at
            .as_deref()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|time| time.with_timezone(&Utc));
        if last_full.is_none_or(|time| now - time >= limit) {
            return Mode::Full(FullReason::Aged);
        }
    }
    let after = (high_water.with_timezone(&Utc) - Duration::seconds(OVERLAP_SECS))
        .to_rfc3339_opts(SecondsFormat::Micros, true);
    Mode::Incremental { after }
}

/// The newest `updated_at` among the fetched tasks and `previous`, as the API wrote it.
fn high_water(snapshot: &Snapshot, previous: Option<&str>) -> Option<String> {
    snapshot
        .tasks
        .iter()
        .filter_map(|task| task.json.get("updated_at")?.as_str())
        .chain(previous)
        .filter_map(|raw| Some((DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc), raw)))
        .max_by_key(|(time, _)| *time)
        .map(|(_, raw)| raw.to_string())
}

pub struct Options {
    /// Rebuild the copy from scratch.
    pub full: bool,
    /// Whether the copy holds completed tasks.
    pub include_completed: bool,
    pub full_refresh_after: Age,
}

pub struct Report {
    /// `full` or `incremental`.
    pub mode: &'static str,
    /// Why the refresh was a full one.
    pub reason: Option<&'static str>,
    /// How many tasks were fetched.
    pub changed: usize,
    pub counts: Counts,
    pub include_completed: bool,
    pub refreshed_at: String,
}

/// Refreshes `store`. The fetching is injected so it can be tested; see [`perform`] for the
/// real thing. Everything is downloaded before the copy is touched, so a failure part-way
/// leaves the previous copy intact.
pub fn perform_with(
    store: &mut Store,
    options: &Options,
    now: DateTime<Utc>,
    fetch: impl FnMut(&str, Option<&str>, Option<&str>) -> Result<TaskPage, PageError>,
    sleep: impl FnMut(StdDuration),
    mut progress: impl FnMut(&str),
) -> Result<Report, Error> {
    let facts = store.facts()?;
    let mode = plan(options.full, options.include_completed, &facts, options.full_refresh_after, now);
    let refreshed_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);

    match mode {
        Mode::Full(reason) => {
            progress(&format!("Full refresh: {}", reason.describe()));
            let statuses: &[Status] =
                if options.include_completed { &[INCOMPLETE, COMPLETE] } else { &[INCOMPLETE] };
            let snapshot = collect_all(statuses, None, fetch, sleep, &mut progress)?;
            let new_facts = Facts {
                refreshed_at: Some(refreshed_at.clone()),
                last_full_refresh_at: Some(refreshed_at.clone()),
                include_completed: Some(options.include_completed),
                high_water: high_water(&snapshot, None),
            };
            let counts = store.replace_all(&snapshot, &new_facts)?;
            Ok(Report {
                mode: "full",
                reason: Some(reason.describe()),
                changed: snapshot.tasks.len(),
                counts,
                include_completed: options.include_completed,
                refreshed_at,
            })
        }
        Mode::Incremental { after } => {
            progress(&format!("Incremental refresh: changes since {after}"));
            // Both statuses, even without completed tasks: a task completed since the last
            // refresh only shows up as completed, and has to be removed from the copy.
            let snapshot = collect_all(&[INCOMPLETE, COMPLETE], Some(&after), fetch, sleep, &mut progress)?;
            let new_facts = Facts {
                refreshed_at: Some(refreshed_at.clone()),
                include_completed: Some(options.include_completed),
                high_water: high_water(&snapshot, facts.high_water.as_deref()),
                ..Default::default()
            };
            let (counts, changed) = store.apply_changes(&snapshot, options.include_completed, &new_facts)?;
            Ok(Report {
                mode: "incremental",
                reason: None,
                changed,
                counts,
                include_completed: options.include_completed,
                refreshed_at,
            })
        }
    }
}

/// Refreshes the real local copy from the real API, reporting progress on stderr.
pub fn perform(api_key: &str, options: &Options) -> Result<Report, Error> {
    paths::ensure_data_dir()?;
    let mut store = Store::open(&paths::store_path())?;
    perform_with(
        &mut store,
        options,
        Utc::now(),
        |status, before, after| api::fetch_task_page(api_key, status, before, after),
        std::thread::sleep,
        |message| eprintln!("{message}"),
    )
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RefreshArgs {
    include_completed: bool,
    full: bool,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<RefreshArgs, String> {
    let mut parsed = RefreshArgs::default();
    for arg in args {
        match arg.as_str() {
            "--include-completed" => parsed.include_completed = true,
            "--full" => parsed.full = true,
            "--json" => parsed.json = true,
            other => return Err(format!("unknown argument '{other}' for refresh (see --help)")),
        }
    }
    Ok(parsed)
}

#[derive(Serialize)]
struct RefreshReport {
    mode: &'static str,
    reason: Option<&'static str>,
    changed_count: usize,
    tasks_count: usize,
    lists_count: usize,
    labels_count: usize,
    include_completed: bool,
    refreshed_at: String,
}

fn tasks_noun(count: usize) -> &'static str {
    if count == 1 { "task" } else { "tasks" }
}

pub fn run_refresh(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };
    let config = match config::load() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };

    let Some(api_key) = token::load_token() else {
        eprintln!(
            "Error: GODSPEED_TOKEN is not set (environment variable, or an `export GODSPEED_TOKEN=...` line in {})",
            paths::token_file_path().display()
        );
        return exit::usage();
    };

    let options = Options {
        full: args.full,
        include_completed: args.include_completed || config.include_completed,
        full_refresh_after: config.full_refresh_after,
    };
    let report = match perform(&api_key, &options) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("Refresh failed: {e}");
            return exit::failure();
        }
    };

    let counts = &report.counts;
    if args.json {
        let json = RefreshReport {
            mode: report.mode,
            reason: report.reason,
            changed_count: report.changed,
            tasks_count: counts.tasks,
            lists_count: counts.lists,
            labels_count: counts.labels,
            include_completed: report.include_completed,
            refreshed_at: report.refreshed_at,
        };
        println!("{}", serde_json::to_string_pretty(&json).expect("the report serialises"));
    } else if report.mode == "full" {
        println!("Refreshed {} tasks, {} lists, {} labels", counts.tasks, counts.lists, counts.labels);
    } else {
        println!(
            "Updated {} {} since the last refresh; the copy now has {} tasks, {} lists, {} labels",
            report.changed,
            tasks_noun(report.changed),
            counts.tasks,
            counts.lists,
            counts.labels
        );
    }
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// A timestamp `n` minutes before a fixed moment, written the way the cursor is.
    fn minutes_ago(n: i64) -> String {
        let time = DateTime::from_timestamp(1_800_000_000 - n * 60, 0).unwrap();
        time.to_rfc3339_opts(SecondsFormat::Micros, true)
    }

    /// The fixed moment the timestamps count back from.
    fn base() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    #[derive(Clone)]
    struct FakeTask {
        id: String,
        updated_at: String,
        complete: bool,
    }

    /// A server holding `tasks`, newest first, that serves pages of 250 like the real one.
    /// `inclusive` says whether `updated_before` includes tasks at exactly that time;
    /// `updated_after` includes them.
    struct FakeApi {
        tasks: Vec<FakeTask>,
        inclusive: bool,
        calls: RefCell<Vec<(String, Option<String>, Option<String>)>>,
    }

    impl FakeApi {
        fn new(inclusive: bool, tasks: Vec<FakeTask>) -> Self {
            let mut tasks = tasks;
            tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
            FakeApi { tasks, inclusive, calls: RefCell::new(Vec::new()) }
        }

        fn distinct(count: usize) -> Vec<FakeTask> {
            (0..count)
                .map(|i| FakeTask { id: format!("t{i:05}"), updated_at: minutes_ago(i as i64), complete: false })
                .collect()
        }

        fn fetch(&self, status: &str, before: Option<&str>, after: Option<&str>) -> Result<TaskPage, PageError> {
            self.calls.borrow_mut().push((status.to_string(), before.map(str::to_string), after.map(str::to_string)));
            let want_complete = status == "complete";
            let tasks = self
                .tasks
                .iter()
                .filter(|t| t.complete == want_complete)
                .filter(|t| after.is_none_or(|a| t.updated_at.as_str() >= a))
                .filter(|t| match before {
                    None => true,
                    Some(b) if self.inclusive => t.updated_at.as_str() <= b,
                    Some(b) => t.updated_at.as_str() < b,
                })
                .take(PAGE_SIZE)
                .map(|t| json!({"id": t.id, "title": t.id, "updated_at": t.updated_at}))
                .collect();
            Ok(TaskPage {
                tasks,
                lists: vec![json!({"id": "list-1", "name": "Inbox"})],
                labels: vec![json!({"id": "label-1", "name": "Low"})],
            })
        }

        fn collect(&self, include_completed: bool) -> (Result<Snapshot, Error>, Vec<StdDuration>) {
            let statuses: &[Status] = if include_completed { &[INCOMPLETE, COMPLETE] } else { &[INCOMPLETE] };
            let mut sleeps = Vec::new();
            let result = collect_all(statuses, None, |s, b, a| self.fetch(s, b, a), |d| sleeps.push(d), |_| {});
            (result, sleeps)
        }
    }

    fn ids(snapshot: &Snapshot) -> Vec<String> {
        snapshot.tasks.iter().map(|t| t.json["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_short_page_is_a_single_request_without_a_cursor() {
        let api = FakeApi::new(true, FakeApi::distinct(5));

        let (result, sleeps) = api.collect(false);

        assert_eq!(ids(&result.unwrap()).len(), 5);
        assert_eq!(*api.calls.borrow(), vec![("incomplete".to_string(), None, None)]);
        assert!(sleeps.is_empty());
    }

    #[test]
    fn pages_are_walked_with_the_cursor_until_one_comes_back_short() {
        let api = FakeApi::new(true, FakeApi::distinct(255));

        let (result, _) = api.collect(false);

        let snapshot = result.unwrap();
        assert_eq!(snapshot.tasks.len(), 255);
        let calls = api.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].1.as_deref(), Some(minutes_ago(249).as_str()));
        let mut unique = ids(&snapshot);
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 255);
    }

    #[test]
    fn exactly_one_full_page_costs_one_extra_empty_request() {
        let api = FakeApi::new(false, FakeApi::distinct(250));

        let (result, _) = api.collect(false);

        assert_eq!(result.unwrap().tasks.len(), 250);
        assert_eq!(api.calls.borrow().len(), 2);
    }

    #[test]
    fn tasks_sharing_the_boundary_timestamp_are_all_collected_when_the_cursor_is_inclusive() {
        let mut tasks = FakeApi::distinct(245);
        tasks.extend((0..10).map(|i| FakeTask { id: format!("tie{i}"), updated_at: minutes_ago(300), complete: false }));
        let api = FakeApi::new(true, tasks);

        let (result, _) = api.collect(false);

        assert_eq!(result.unwrap().tasks.len(), 255);
    }

    #[test]
    fn a_huge_group_of_identical_timestamps_ends_instead_of_looping_forever() {
        let tasks = (0..300)
            .map(|i| FakeTask { id: format!("same{i:03}"), updated_at: minutes_ago(5), complete: false })
            .collect();
        let api = FakeApi::new(true, tasks);

        let (result, _) = api.collect(false);

        assert!(result.unwrap().tasks.len() >= PAGE_SIZE);
        assert!(api.calls.borrow().len() < 10, "{} requests", api.calls.borrow().len());
    }

    #[test]
    fn every_ninth_request_waits_a_minute_first() {
        let api = FakeApi::new(false, FakeApi::distinct(PAGE_SIZE * 24 + 1));

        let (result, sleeps) = api.collect(false);

        assert_eq!(result.unwrap().tasks.len(), PAGE_SIZE * 24 + 1);
        assert_eq!(api.calls.borrow().len(), 25);
        assert_eq!(sleeps, vec![StdDuration::from_secs(60); 2]);
    }

    #[test]
    fn completed_tasks_are_fetched_second_and_flagged() {
        let mut tasks = FakeApi::distinct(3);
        tasks.extend((0..2).map(|i| FakeTask { id: format!("done{i}"), updated_at: minutes_ago(100 + i), complete: true }));
        let api = FakeApi::new(true, tasks);

        let (without, _) = api.collect(false);
        assert_eq!(without.unwrap().tasks.len(), 3);

        api.calls.borrow_mut().clear();
        let (with, _) = api.collect(true);
        let snapshot = with.unwrap();
        assert_eq!(snapshot.tasks.len(), 5);
        let statuses: Vec<String> = api.calls.borrow().iter().map(|c| c.0.clone()).collect();
        assert_eq!(statuses, vec!["incomplete", "complete"]);
        for task in &snapshot.tasks {
            assert_eq!(task.complete, task.json["id"].as_str().unwrap().starts_with("done"));
        }
    }

    #[test]
    fn the_cursor_starts_over_for_the_second_status() {
        let mut tasks = FakeApi::distinct(251);
        tasks.extend((0..3).map(|i| FakeTask { id: format!("done{i}"), updated_at: minutes_ago(1000 + i), complete: true }));
        let api = FakeApi::new(true, tasks);

        let (result, _) = api.collect(true);

        assert_eq!(result.unwrap().tasks.len(), 254);
        let calls = api.calls.borrow();
        assert_eq!(calls.last().unwrap(), &("complete".to_string(), None, None));
    }

    #[test]
    fn updated_after_is_sent_with_every_request_including_later_pages() {
        let api = FakeApi::new(true, FakeApi::distinct(300));
        let after = minutes_ago(280);

        let result = collect_all(
            &[INCOMPLETE, COMPLETE],
            Some(&after),
            |s, b, a| api.fetch(s, b, a),
            |_| {},
            |_| {},
        )
        .unwrap();

        // Tasks 0..=280 changed after the cut-off: 281 of them, over two pages.
        assert_eq!(result.tasks.len(), 281);
        let calls = api.calls.borrow();
        assert_eq!(calls.len(), 3, "two incomplete pages and one complete page");
        assert!(calls.iter().all(|c| c.2.as_deref() == Some(after.as_str())));
    }

    #[test]
    fn lists_and_labels_from_every_page_are_collected() {
        let api = FakeApi::new(true, FakeApi::distinct(255));

        let (result, _) = api.collect(false);

        let snapshot = result.unwrap();
        assert_eq!(snapshot.lists.len(), 2);
        assert_eq!(snapshot.labels.len(), 2);
    }

    #[test]
    fn a_rate_limited_request_waits_as_told_and_is_retried() {
        let api = FakeApi::new(true, FakeApi::distinct(3));
        let attempts = RefCell::new(0);
        let mut sleeps = Vec::new();

        let result = collect_all(
            &[INCOMPLETE],
            None,
            |s, b, a| {
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() == 1 { Err(PageError::RateLimited(Some(7))) } else { api.fetch(s, b, a) }
            },
            |d| sleeps.push(d),
            |_| {},
        );

        assert_eq!(result.unwrap().tasks.len(), 3);
        assert_eq!(sleeps, vec![StdDuration::from_secs(7)]);
    }

    #[test]
    fn a_rate_limit_without_a_retry_after_waits_a_minute_and_gives_up_after_three_retries() {
        let mut sleeps = Vec::new();

        let result =
            collect_all(&[INCOMPLETE], None, |_, _, _| Err(PageError::RateLimited(None)), |d| sleeps.push(d), |_| {});

        assert!(result.err().unwrap().to_string().contains("rate limiting"));
        assert_eq!(sleeps, vec![StdDuration::from_secs(60); 3]);
    }

    #[test]
    fn a_failure_part_way_through_returns_the_error_and_no_partial_result() {
        let api = FakeApi::new(true, FakeApi::distinct(300));
        let calls = RefCell::new(0);

        let result = collect_all(
            &[INCOMPLETE],
            None,
            |s, b, a| {
                *calls.borrow_mut() += 1;
                if *calls.borrow() == 2 { Err(PageError::Failed("API error: 500".into())) } else { api.fetch(s, b, a) }
            },
            |_| {},
            |_| {},
        );

        assert_eq!(result.err().unwrap().to_string(), "API error: 500");
    }

    #[test]
    fn a_full_page_without_usable_timestamps_is_an_error_not_a_silent_truncation() {
        let tasks: Vec<Value> = (0..PAGE_SIZE).map(|i| json!({"id": format!("t{i}")})).collect();

        let result = collect_all(
            &[INCOMPLETE],
            None,
            |_, _, _| Ok(TaskPage { tasks: tasks.clone(), ..Default::default() }),
            |_| {},
            |_| {},
        );

        assert!(result.err().unwrap().to_string().contains("updated_at"));
    }

    #[test]
    fn tasks_without_an_id_are_ignored() {
        let result = collect_all(
            &[INCOMPLETE],
            None,
            |_, _, _| Ok(TaskPage { tasks: vec![json!({"title": "no id"}), json!({"id": "a"})], ..Default::default() }),
            |_| {},
            |_| {},
        );

        assert_eq!(result.unwrap().tasks.len(), 1);
    }

    #[test]
    fn refresh_arguments() {
        assert_eq!(parse_args(&args(&[])), Ok(RefreshArgs::default()));
        assert_eq!(
            parse_args(&args(&["--json", "--include-completed", "--full"])),
            Ok(RefreshArgs { include_completed: true, full: true, json: true })
        );
        assert!(parse_args(&args(&["--nope"])).unwrap_err().contains("--nope"));
        assert!(parse_args(&args(&["tasks"])).is_err());
    }

    // ---- planning ----

    fn facts(refreshed: &str, full: &str, completed: bool, high_water: &str) -> Facts {
        Facts {
            refreshed_at: Some(refreshed.to_string()),
            last_full_refresh_at: Some(full.to_string()),
            include_completed: Some(completed),
            high_water: Some(high_water.to_string()),
        }
    }

    fn days_ago(n: i64) -> String {
        (base() - Duration::days(n)).to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    const WEEK: Age = Age::After(Duration::days(7));

    #[test]
    fn a_refresh_is_incremental_from_a_minute_before_the_newest_change() {
        let f = facts(&days_ago(1), &days_ago(2), false, "2027-01-14T10:00:00.500000Z");

        let mode = plan(false, false, &f, WEEK, base());

        assert_eq!(mode, Mode::Incremental { after: "2027-01-14T09:59:00.500000Z".to_string() });
    }

    #[test]
    fn full_is_chosen_when_asked_for() {
        let f = facts(&days_ago(1), &days_ago(2), false, "2027-01-14T10:00:00Z");
        assert_eq!(plan(true, false, &f, WEEK, base()), Mode::Full(FullReason::Requested));
    }

    #[test]
    fn full_is_chosen_when_there_is_no_copy_yet() {
        assert_eq!(plan(false, false, &Facts::default(), WEEK, base()), Mode::Full(FullReason::FirstRefresh));
        let no_full_time = Facts { last_full_refresh_at: None, ..facts(&days_ago(1), "", false, "2027-01-14T10:00:00Z") };
        assert_eq!(plan(false, false, &no_full_time, WEEK, base()), Mode::Full(FullReason::FirstRefresh));
    }

    #[test]
    fn full_is_chosen_when_the_choice_of_completed_tasks_changes_either_way() {
        let without = facts(&days_ago(1), &days_ago(1), false, "2027-01-14T10:00:00Z");
        let with = facts(&days_ago(1), &days_ago(1), true, "2027-01-14T10:00:00Z");
        assert_eq!(plan(false, true, &without, WEEK, base()), Mode::Full(FullReason::CompletedChanged));
        assert_eq!(plan(false, false, &with, WEEK, base()), Mode::Full(FullReason::CompletedChanged));
        assert!(matches!(plan(false, true, &with, WEEK, base()), Mode::Incremental { .. }));
        let unknown = Facts { include_completed: None, ..without };
        assert_eq!(plan(false, false, &unknown, WEEK, base()), Mode::Full(FullReason::CompletedChanged));
    }

    #[test]
    fn full_is_chosen_without_a_usable_newest_change_time() {
        let missing = Facts { high_water: None, ..facts(&days_ago(1), &days_ago(1), false, "") };
        let garbage = facts(&days_ago(1), &days_ago(1), false, "not a time");
        assert_eq!(plan(false, false, &missing, WEEK, base()), Mode::Full(FullReason::NoHighWater));
        assert_eq!(plan(false, false, &garbage, WEEK, base()), Mode::Full(FullReason::NoHighWater));
    }

    #[test]
    fn full_is_chosen_once_the_last_full_refresh_is_old_enough() {
        let recent = facts(&days_ago(0), &days_ago(6), false, "2027-01-14T10:00:00Z");
        let exactly = facts(&days_ago(0), &days_ago(7), false, "2027-01-14T10:00:00Z");
        let old = facts(&days_ago(0), &days_ago(30), false, "2027-01-14T10:00:00Z");
        let garbage = facts(&days_ago(0), "yesterday", false, "2027-01-14T10:00:00Z");
        assert!(matches!(plan(false, false, &recent, WEEK, base()), Mode::Incremental { .. }));
        assert_eq!(plan(false, false, &exactly, WEEK, base()), Mode::Full(FullReason::Aged));
        assert_eq!(plan(false, false, &old, WEEK, base()), Mode::Full(FullReason::Aged));
        assert_eq!(plan(false, false, &garbage, WEEK, base()), Mode::Full(FullReason::Aged));
    }

    #[test]
    fn never_turns_the_periodic_full_refresh_off() {
        let old = facts(&days_ago(0), &days_ago(3650), false, "2027-01-14T10:00:00Z");
        assert!(matches!(plan(false, false, &old, Age::Never, base()), Mode::Incremental { .. }));
        // A shorter limit applies sooner.
        let two_days = Age::After(Duration::days(2));
        let three = facts(&days_ago(0), &days_ago(3), false, "2027-01-14T10:00:00Z");
        assert_eq!(plan(false, false, &three, two_days, base()), Mode::Full(FullReason::Aged));
    }

    #[test]
    fn every_full_reason_has_a_description() {
        for reason in [
            FullReason::Requested,
            FullReason::FirstRefresh,
            FullReason::CompletedChanged,
            FullReason::NoHighWater,
            FullReason::Aged,
        ] {
            assert!(!reason.describe().is_empty());
        }
    }

    #[test]
    fn the_high_water_mark_is_the_newest_update_seen() {
        let snapshot = Snapshot {
            tasks: vec![
                CachedTask { json: json!({"id": "a", "updated_at": "2027-01-10T00:00:00Z"}), complete: false },
                CachedTask { json: json!({"id": "b", "updated_at": "2027-01-12T00:00:00.250000Z"}), complete: true },
                CachedTask { json: json!({"id": "c"}), complete: false },
            ],
            ..Default::default()
        };
        assert_eq!(high_water(&snapshot, None).as_deref(), Some("2027-01-12T00:00:00.250000Z"));
        assert_eq!(high_water(&snapshot, Some("2027-01-20T00:00:00Z")).as_deref(), Some("2027-01-20T00:00:00Z"));
        assert_eq!(high_water(&snapshot, Some("2027-01-01T00:00:00Z")).as_deref(), Some("2027-01-12T00:00:00.250000Z"));
        assert_eq!(high_water(&Snapshot::default(), None), None);
        assert_eq!(high_water(&Snapshot::default(), Some("2027-01-01T00:00:00Z")).as_deref(), Some("2027-01-01T00:00:00Z"));
    }

    // ---- refreshing a store ----

    fn options(full: bool, include_completed: bool) -> Options {
        Options { full, include_completed, full_refresh_after: WEEK }
    }

    fn run(store: &mut Store, api: &FakeApi, options: &Options, now: DateTime<Utc>) -> Result<Report, Error> {
        perform_with(store, options, now, |s, b, a| api.fetch(s, b, a), |_| {}, |_| {})
    }

    fn task(id: &str, minutes: i64, complete: bool) -> FakeTask {
        FakeTask { id: id.to_string(), updated_at: minutes_ago(minutes), complete }
    }

    fn stored_ids(store: &Store) -> Vec<String> {
        let mut ids: Vec<String> =
            store.tasks(true).unwrap().iter().map(|t| t.json["id"].as_str().unwrap().to_string()).collect();
        ids.sort();
        ids
    }

    #[test]
    fn the_first_refresh_is_full_and_records_the_facts() {
        let mut store = Store::in_memory();
        let api = FakeApi::new(true, vec![task("a", 10, false), task("b", 20, false)]);

        let report = run(&mut store, &api, &options(false, false), base()).unwrap();

        assert_eq!((report.mode, report.changed, report.counts.tasks), ("full", 2, 2));
        assert_eq!(report.reason, Some(FullReason::FirstRefresh.describe()));
        let facts = store.facts().unwrap();
        assert_eq!(facts.include_completed, Some(false));
        assert_eq!(facts.high_water.as_deref(), Some(minutes_ago(10).as_str()));
        assert_eq!(facts.refreshed_at, facts.last_full_refresh_at);
    }

    #[test]
    fn the_second_refresh_is_incremental_and_picks_up_new_and_changed_tasks() {
        let mut store = Store::in_memory();
        run(&mut store, &FakeApi::new(true, vec![task("a", 100, false), task("b", 200, false)]), &options(false, false), base())
            .unwrap();

        // Later: "a" was edited, "c" was created, "b" is untouched.
        let later = base() + Duration::hours(1);
        let server = FakeApi::new(true, vec![task("a", -30, false), task("b", 200, false), task("c", -20, false)]);
        let report = run(&mut store, &server, &options(false, false), later).unwrap();

        assert_eq!(report.mode, "incremental");
        assert_eq!(report.reason, None);
        assert_eq!(report.changed, 2, "only the two changed tasks were fetched");
        assert_eq!(report.counts.tasks, 3);
        assert_eq!(stored_ids(&store), vec!["a", "b", "c"]);
        // The request asked only for changes after a minute before the previous newest (a, 100).
        let calls = server.calls.borrow();
        let after = (DateTime::parse_from_rfc3339(&minutes_ago(100)).unwrap().with_timezone(&Utc) - Duration::seconds(60))
            .to_rfc3339_opts(SecondsFormat::Micros, true);
        assert!(calls.iter().all(|c| c.2.as_deref() == Some(after.as_str())), "{calls:?}");
        // The high-water mark moved on to the newest change, which is the edit of "a" (30 minutes
        // after the base time), not the later-listed creation of "c" (20 minutes after).
        assert_eq!(store.facts().unwrap().high_water.as_deref(), Some(minutes_ago(-30).as_str()));
    }

    #[test]
    fn an_incremental_refresh_costs_two_requests_when_little_changed() {
        let mut store = Store::in_memory();
        let many: Vec<FakeTask> = (0..600).map(|i| task(&format!("t{i:04}"), 1000 + i, false)).collect();
        run(&mut store, &FakeApi::new(false, many.clone()), &options(false, false), base()).unwrap();

        let server = FakeApi::new(false, many);
        let report = run(&mut store, &server, &options(false, false), base() + Duration::hours(1)).unwrap();

        assert_eq!(report.mode, "incremental");
        assert_eq!(server.calls.borrow().len(), 2, "one incomplete page and one complete page");
    }

    #[test]
    fn a_task_completed_since_is_dropped_by_an_incremental_refresh_without_completed_tasks() {
        let mut store = Store::in_memory();
        run(&mut store, &FakeApi::new(true, vec![task("a", 100, false), task("b", 200, false)]), &options(false, false), base())
            .unwrap();

        let server = FakeApi::new(true, vec![task("a", -10, true), task("b", 200, false)]);
        let report = run(&mut store, &server, &options(false, false), base() + Duration::hours(1)).unwrap();

        assert_eq!(report.mode, "incremental");
        assert_eq!(stored_ids(&store), vec!["b"]);
    }

    #[test]
    fn a_task_completed_since_is_kept_and_flagged_when_completed_tasks_are_included() {
        let mut store = Store::in_memory();
        run(&mut store, &FakeApi::new(true, vec![task("a", 100, false)]), &options(false, true), base()).unwrap();

        let server = FakeApi::new(true, vec![task("a", -10, true)]);
        let report = run(&mut store, &server, &options(false, true), base() + Duration::hours(1)).unwrap();

        assert_eq!(report.mode, "incremental");
        assert_eq!(store.complete_count().unwrap(), 1);
        assert_eq!(report.counts.tasks, 1);
    }

    #[test]
    fn full_rebuilds_the_copy_so_deleted_tasks_disappear() {
        let mut store = Store::in_memory();
        run(&mut store, &FakeApi::new(true, vec![task("a", 100, false), task("gone", 200, false)]), &options(false, false), base())
            .unwrap();

        // "gone" was deleted. An incremental refresh can't see that...
        let server = FakeApi::new(true, vec![task("a", 100, false)]);
        run(&mut store, &server, &options(false, false), base() + Duration::hours(1)).unwrap();
        assert_eq!(stored_ids(&store), vec!["a", "gone"]);

        // ...but a full one can.
        let report = run(&mut store, &server, &options(true, false), base() + Duration::hours(2)).unwrap();
        assert_eq!((report.mode, report.reason), ("full", Some(FullReason::Requested.describe())));
        assert_eq!(stored_ids(&store), vec!["a"]);
    }

    #[test]
    fn an_old_enough_last_full_refresh_makes_the_next_refresh_full_automatically() {
        let mut store = Store::in_memory();
        run(&mut store, &FakeApi::new(true, vec![task("a", 100, false), task("gone", 200, false)]), &options(false, false), base())
            .unwrap();

        let server = FakeApi::new(true, vec![task("a", 100, false)]);
        let report = run(&mut store, &server, &options(false, false), base() + Duration::days(8)).unwrap();

        assert_eq!((report.mode, report.reason), ("full", Some(FullReason::Aged.describe())));
        assert_eq!(stored_ids(&store), vec!["a"]);
    }

    #[test]
    fn switching_whether_completed_tasks_are_included_does_a_full_refresh() {
        let mut store = Store::in_memory();
        let server = FakeApi::new(true, vec![task("open", 100, false), task("done", 200, true)]);
        run(&mut store, &server, &options(false, false), base()).unwrap();
        assert_eq!(stored_ids(&store), vec!["open"]);

        let with = run(&mut store, &server, &options(false, true), base() + Duration::hours(1)).unwrap();
        assert_eq!((with.mode, with.reason), ("full", Some(FullReason::CompletedChanged.describe())));
        assert_eq!(stored_ids(&store), vec!["done", "open"]);

        let without = run(&mut store, &server, &options(false, false), base() + Duration::hours(2)).unwrap();
        assert_eq!(without.mode, "full");
        assert_eq!(stored_ids(&store), vec!["open"]);
    }

    #[test]
    fn a_failed_incremental_refresh_leaves_the_copy_and_its_facts_alone() {
        let mut store = Store::in_memory();
        run(&mut store, &FakeApi::new(true, vec![task("a", 100, false)]), &options(false, false), base()).unwrap();
        let before = store.facts().unwrap();

        let failed = perform_with(
            &mut store,
            &options(false, false),
            base() + Duration::hours(1),
            |_, _, _| Err(PageError::Failed("API error: 500".into())),
            |_| {},
            |_| {},
        );

        assert_eq!(failed.err().unwrap().to_string(), "API error: 500");
        assert_eq!(store.facts().unwrap(), before);
        assert_eq!(stored_ids(&store), vec!["a"]);
    }

    #[test]
    fn an_empty_account_refreshes_in_full_each_time_until_it_has_a_task() {
        let mut store = Store::in_memory();
        let empty = FakeApi::new(true, vec![]);

        run(&mut store, &empty, &options(false, false), base()).unwrap();
        let again = run(&mut store, &empty, &options(false, false), base() + Duration::hours(1)).unwrap();

        assert_eq!((again.mode, again.reason), ("full", Some(FullReason::NoHighWater.describe())));
    }

    #[test]
    fn progress_says_what_kind_of_refresh_it_is() {
        let mut store = Store::in_memory();
        let server = FakeApi::new(true, vec![task("a", 100, false)]);
        let mut messages: Vec<String> = Vec::new();

        perform_with(&mut store, &options(false, false), base(), |s, b, a| server.fetch(s, b, a), |_| {}, |m| messages.push(m.to_string()))
            .unwrap();
        perform_with(&mut store, &options(false, false), base(), |s, b, a| server.fetch(s, b, a), |_| {}, |m| messages.push(m.to_string()))
            .unwrap();

        assert!(messages.iter().any(|m| m.starts_with("Full refresh: there is no local copy yet")), "{messages:?}");
        assert!(messages.iter().any(|m| m.starts_with("Incremental refresh: changes since ")), "{messages:?}");
    }
}
