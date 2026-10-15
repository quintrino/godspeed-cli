//! `godspeed-cli refresh`: download every task and keep a local copy for searching.

use crate::api::{self, PageError, TaskPage};
use crate::store::{CachedTask, Snapshot, Store};
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

type Error = Box<dyn std::error::Error>;

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

/// Fetches every incomplete task, and every completed one too if asked, by walking back through
/// `updated_before` until a page comes back short.
///
/// The next page starts at the oldest `updated_at` seen, not a moment before it, so tasks that
/// share that timestamp and didn't fit on the page aren't skipped (tasks seen twice are
/// ignored). If a page then adds nothing new, the cut-off steps back a microsecond so the walk
/// always moves on. More than 250 tasks with the very same `updated_at` can't all be reached,
/// which would only happen after a bulk edit.
///
/// `fetch(status, updated_before)` gets one page, `sleep` waits, and `progress` reports status.
pub fn collect_all(
    include_completed: bool,
    mut fetch: impl FnMut(&str, Option<&str>) -> Result<TaskPage, PageError>,
    mut sleep: impl FnMut(StdDuration),
    mut progress: impl FnMut(&str),
) -> Result<Snapshot, Error> {
    let statuses: &[(&str, bool)] =
        if include_completed { &[("incomplete", false), ("complete", true)] } else { &[("incomplete", false)] };

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
                match fetch(status, updated_before.as_deref()) {
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

#[derive(Debug, Default, PartialEq, Eq)]
struct RefreshArgs {
    include_completed: bool,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<RefreshArgs, String> {
    let mut parsed = RefreshArgs::default();
    for arg in args {
        match arg.as_str() {
            "--include-completed" => parsed.include_completed = true,
            "--json" => parsed.json = true,
            other => return Err(format!("unknown argument '{other}' for refresh (see --help)")),
        }
    }
    Ok(parsed)
}

#[derive(Serialize)]
struct RefreshReport {
    tasks_count: usize,
    lists_count: usize,
    labels_count: usize,
    include_completed: bool,
    refreshed_at: String,
}

pub fn run_refresh(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
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

    if let Err(e) = paths::ensure_data_dir() {
        eprintln!("Failed to create directories: {e}");
        return exit::failure();
    }

    // Everything is downloaded before the cache is touched, so a failure part-way through
    // leaves the previous copy intact.
    let snapshot = match collect_all(
        args.include_completed,
        |status, before| api::fetch_task_page(&api_key, status, before),
        std::thread::sleep,
        |message| eprintln!("{message}"),
    ) {
        Ok(snapshot) => snapshot,
        Err(e) => {
            eprintln!("Refresh failed: {e}");
            return exit::failure();
        }
    };

    let refreshed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let counts = match Store::open(&paths::store_path()).and_then(|mut store| store.replace_all(&snapshot, &refreshed_at)) {
        Ok(counts) => counts,
        Err(e) => {
            eprintln!("Refresh failed: could not save the cache: {e}");
            return exit::failure();
        }
    };

    if args.json {
        let report = RefreshReport {
            tasks_count: counts.tasks,
            lists_count: counts.lists,
            labels_count: counts.labels,
            include_completed: args.include_completed,
            refreshed_at,
        };
        println!("{}", serde_json::to_string_pretty(&report).expect("the report serialises"));
    } else {
        println!("Refreshed {} tasks, {} lists, {} labels", counts.tasks, counts.lists, counts.labels);
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

    struct FakeTask {
        id: String,
        updated_at: String,
        complete: bool,
    }

    /// A server holding `tasks`, newest first, that serves pages of 250 like the real one.
    /// `inclusive` says whether `updated_before` includes tasks at exactly that time.
    struct FakeApi {
        tasks: Vec<FakeTask>,
        inclusive: bool,
        calls: RefCell<Vec<(String, Option<String>)>>,
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

        fn fetch(&self, status: &str, before: Option<&str>) -> Result<TaskPage, PageError> {
            self.calls.borrow_mut().push((status.to_string(), before.map(str::to_string)));
            let want_complete = status == "complete";
            let tasks = self
                .tasks
                .iter()
                .filter(|t| t.complete == want_complete)
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
            let mut sleeps = Vec::new();
            let result = collect_all(include_completed, |s, b| self.fetch(s, b), |d| sleeps.push(d), |_| {});
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
        assert_eq!(*api.calls.borrow(), vec![("incomplete".to_string(), None)]);
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
        // The cursor is the oldest updated_at of page one, exactly as the API gave it.
        assert_eq!(calls[1].1.as_deref(), Some(minutes_ago(249).as_str()));
        // The overlap task at the cursor was returned twice and kept once.
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
        // 245 distinct timestamps, then 10 tasks sharing one: the page boundary falls inside them.
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
        // 24 full pages and a short one: 25 requests.
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
        assert_eq!(
            *api.calls.borrow(),
            vec![("incomplete".to_string(), None), ("complete".to_string(), None)]
        );
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
        assert_eq!(calls.last().unwrap(), &("complete".to_string(), None));
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
            false,
            |s, b| {
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() == 1 { Err(PageError::RateLimited(Some(7))) } else { api.fetch(s, b) }
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

        let result = collect_all(false, |_, _| Err(PageError::RateLimited(None)), |d| sleeps.push(d), |_| {});

        assert!(result.err().unwrap().to_string().contains("rate limiting"));
        assert_eq!(sleeps, vec![StdDuration::from_secs(60); 3]);
    }

    #[test]
    fn a_failure_part_way_through_returns_the_error_and_no_partial_result() {
        let api = FakeApi::new(true, FakeApi::distinct(300));
        let calls = RefCell::new(0);

        let result = collect_all(
            false,
            |s, b| {
                *calls.borrow_mut() += 1;
                if *calls.borrow() == 2 { Err(PageError::Failed("API error: 500".into())) } else { api.fetch(s, b) }
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
            false,
            |_, _| Ok(TaskPage { tasks: tasks.clone(), ..Default::default() }),
            |_| {},
            |_| {},
        );

        assert!(result.err().unwrap().to_string().contains("updated_at"));
    }

    #[test]
    fn tasks_without_an_id_are_ignored() {
        let result = collect_all(
            false,
            |_, _| Ok(TaskPage { tasks: vec![json!({"title": "no id"}), json!({"id": "a"})], ..Default::default() }),
            |_| {},
            |_| {},
        );

        assert_eq!(result.unwrap().tasks.len(), 1);
    }

    #[test]
    fn refresh_arguments() {
        assert_eq!(parse_args(&args(&[])), Ok(RefreshArgs { include_completed: false, json: false }));
        assert_eq!(
            parse_args(&args(&["--json", "--include-completed"])),
            Ok(RefreshArgs { include_completed: true, json: true })
        );
        assert!(parse_args(&args(&["--nope"])).unwrap_err().contains("--nope"));
        assert!(parse_args(&args(&["tasks"])).is_err());
    }
}
