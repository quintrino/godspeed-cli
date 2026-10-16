//! End-to-end tests: the real binary talks to a mock Godspeed API on localhost.
//!
//! Each run gets its own `XDG_DATA_HOME` and an explicit environment, so the tests don't
//! touch the user's cache or token and can run in parallel.

use chrono::{DateTime, Duration, Local, NaiveDate, SecondsFormat};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

const LISTS: &str = r#"{"lists":[{"id":"list-work","name":"Work Projects"},{"id":"list-home","name":"Home"}]}"#;
const LABELS: &str = r#"{"labels":[{"id":"label-urgent","name":"Urgent"},{"id":"label-writing","name":"Writing"}]}"#;
const CACHE_DIVIDER: &str = "-*-*-*-";

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    body: String,
}

/// A canned answer to `GET /tasks`.
struct Reply {
    status: u16,
    body: String,
    retry_after: Option<u32>,
}

struct MockApi {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task_replies: Arc<Mutex<VecDeque<Reply>>>,
}

impl MockApi {
    /// `post_status` is the status returned for `POST /tasks`.
    fn start(post_status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task_replies = Arc::new(Mutex::new(VecDeque::new()));
        let recorded = Arc::clone(&requests);
        let replies = Arc::clone(&task_replies);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = Arc::clone(&recorded);
                let replies = Arc::clone(&replies);
                thread::spawn(move || serve(stream, &recorded, &replies, post_status));
            }
        });
        MockApi { url, requests, task_replies }
    }

    /// The next `GET /tasks` answers with this page; with none queued it answers 404.
    fn queue_tasks_page(&self, tasks: Vec<Value>) {
        self.queue_page_with(
            tasks,
            json!([{"id": "list-work", "name": "Work Projects"}, {"id": "list-home", "name": "Home"}]),
            json!([{"id": "label-urgent", "name": "Urgent"}]),
        );
    }

    fn queue_page_with(&self, tasks: Vec<Value>, lists: Value, labels: Value) {
        let body = json!({"success": true, "tasks": tasks, "lists": lists, "labels": labels});
        self.task_replies.lock().unwrap().push_back(Reply { status: 200, body: body.to_string(), retry_after: None });
    }

    fn queue_tasks_status(&self, status: u16, retry_after: Option<u32>) {
        self.task_replies.lock().unwrap().push_back(Reply { status, body: "{}".to_string(), retry_after });
    }

    /// The query strings of every `GET /tasks`, in order.
    fn task_queries(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter(|r| r.method == "GET" && r.path == "/tasks")
            .map(|r| r.query.clone())
            .collect()
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    fn count(&self, method: &str, path: &str) -> usize {
        self.requests().iter().filter(|r| r.method == method && r.path == path).count()
    }

    /// The JSON bodies of every `POST /tasks`, in the order they arrived.
    fn posted_tasks(&self) -> Vec<Value> {
        self.requests()
            .iter()
            .filter(|r| r.method == "POST" && r.path == "/tasks")
            .map(|r| serde_json::from_str(&r.body).expect("task body is JSON"))
            .collect()
    }
}

fn serve(stream: TcpStream, recorded: &Mutex<Vec<Recorded>>, replies: &Mutex<VecDeque<Reply>>, post_status: u16) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (path, query) = (path.to_string(), query.to_string());

    let mut content_length = 0;
    let mut authorization = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().unwrap_or(0),
                "authorization" => authorization = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    let body = String::from_utf8(body).unwrap();

    let mut retry_after = None;
    let (status, payload) = match (method.as_str(), path.as_str()) {
        ("GET", "/lists") => (200, LISTS.to_string()),
        ("GET", "/labels") => (200, LABELS.to_string()),
        ("GET", "/tasks") => match replies.lock().unwrap().pop_front() {
            Some(reply) => {
                retry_after = reply.retry_after;
                (reply.status, reply.body)
            }
            None => (404, "{}".to_string()),
        },
        ("POST", "/tasks") => (post_status, r#"{"success":true,"todo_item":{"id":"new-task-id","title":"from the mock"}}"#.to_string()),
        _ => (404, "{}".to_string()),
    };
    recorded.lock().unwrap().push(Recorded { method, path, query, authorization, body });

    let retry_header = retry_after.map(|s| format!("Retry-After: {s}\r\n")).unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\n{retry_header}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let mut stream = stream;
    let _ = stream.write_all(response.as_bytes());
}

/// A private data directory that persists across runs within one test.
struct Sandbox {
    data_home: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let data_home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&data_home);
        fs::create_dir_all(&data_home).unwrap();
        Sandbox { data_home }
    }

    fn write_token_file(&self, text: &str) {
        let path = self.data_home.join("environment/Godspeed");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Writes the settings file where the program looks for it when XDG_CONFIG_HOME is unset.
    fn write_config(&self, text: &str) {
        let path = self.data_home.join(".config/godspeed-cli/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A fact the program keeps about the copy, in the cache database.
    fn fact(&self, key: &str) -> Option<String> {
        let connection = rusqlite::Connection::open(self.data_home.join("godspeed-cli/cache.sqlite3")).unwrap();
        connection
            .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| row.get(0))
            .ok()
    }

    fn set_fact(&self, key: &str, value: &str) {
        let connection = rusqlite::Connection::open(self.data_home.join("godspeed-cli/cache.sqlite3")).unwrap();
        connection
            .execute("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)", [key, value])
            .unwrap();
    }

    fn stored_count(&self, table: &str) -> i64 {
        let connection = rusqlite::Connection::open(self.data_home.join("godspeed-cli/cache.sqlite3")).unwrap();
        connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap()
    }

    fn stored_complete_count(&self) -> i64 {
        let connection = rusqlite::Connection::open(self.data_home.join("godspeed-cli/cache.sqlite3")).unwrap();
        connection.query_row("SELECT COUNT(*) FROM tasks WHERE is_complete = 1", [], |row| row.get(0)).unwrap()
    }

    /// Pre-fills the retry queue, as an earlier failed run would have.
    fn write_queue(&self, tasks: &[&str]) {
        let path = self.cache_file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = tasks.iter().map(|t| format!("{t}\n{CACHE_DIVIDER}\n")).collect();
        fs::write(path, text).unwrap();
    }

    fn cache_file(&self) -> PathBuf {
        self.data_home.join("godspeed-cli/cache")
    }

    fn cache_contents(&self) -> String {
        fs::read_to_string(self.cache_file()).unwrap_or_default()
    }

    fn queued_tasks(&self) -> Vec<String> {
        self.cache_contents()
            .split(CACHE_DIVIDER)
            .map(|task| task.trim().to_string())
            .filter(|task| !task.is_empty())
            .collect()
    }

    /// Runs the binary against `api` with a token set.
    fn run(&self, api: &MockApi, args: &[&str]) -> Output {
        self.run_with(Some(&api.url), Some("test-token"), args, None)
    }

    fn run_stdin(&self, api: &MockApi, args: &[&str], stdin: &str) -> Output {
        self.run_with(Some(&api.url), Some("test-token"), args, Some(stdin))
    }

    fn run_with(&self, api_url: Option<&str>, token: Option<&str>, args: &[&str], stdin: Option<&str>) -> Output {
        self.run_with_env(api_url, token, args, stdin, &[])
    }

    fn run_with_env(
        &self,
        api_url: Option<&str>,
        token: Option<&str>,
        args: &[&str],
        stdin: Option<&str>,
        extra_env: &[(&str, &str)],
    ) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_godspeed-cli"));
        command
            .args(args)
            .env_clear()
            .env("HOME", &self.data_home)
            .env("XDG_DATA_HOME", &self.data_home)
            // With no PATH at all the system default is searched, which on macOS would find
            // osascript and show real notifications while the tests run.
            .env("PATH", "/nonexistent")
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.envs(extra_env.iter().copied());
        if let Some(url) = api_url {
            command.env("GODSPEED_API_URL", url);
        }
        if let Some(token) = token {
            command.env("GODSPEED_TOKEN", token);
        }
        let mut child = command.spawn().unwrap();
        if let Some(text) = stdin {
            // The program may exit without reading stdin (for instance on an unknown command).
            let _ = child.stdin.take().unwrap().write_all(text.as_bytes());
        }
        child.wait_with_output().unwrap()
    }
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// Asserts `value` is tomorrow's date, tolerating the clock rolling past midnight during a test.
fn assert_is_tomorrow(value: &Value, before: NaiveDate) {
    let value = value.as_str().expect("due date is a string");
    let candidates = [before + Duration::days(1), today() + Duration::days(1)];
    assert!(
        candidates.iter().any(|d| d.format("%Y-%m-%d").to_string() == value),
        "{value} is not tomorrow ({candidates:?})"
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_task_with_every_kind_of_syntax_is_posted_with_the_right_fields() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("full-flow");

    sandbox.run(&api, &["add", "Pay rent @work .urgent .writing ~1.5h :20261101 n: ring the landlord"]);

    assert_eq!(
        api.posted_tasks(),
        vec![json!({
            "title": "Pay rent",
            "list_id": "list-work",
            "duration_minutes": 90,
            "timeless_due_at": "2026-11-01",
            "label_ids": ["label-urgent", "label-writing"],
            "notes": "ring the landlord",
        })]
    );
    let post = api.requests().into_iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(post.authorization.as_deref(), Some("Bearer test-token"));
    assert!(!post.body.contains("label_names"));
    assert!(sandbox.queued_tasks().is_empty());
}

#[test]
fn separate_arguments_are_joined_into_one_task() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("separate-args");

    sandbox.run(&api, &["add", "Buy", "milk", "@home", "~10m"]);

    let tasks = api.posted_tasks();
    assert_eq!(tasks[0]["title"], "Buy milk");
    assert_eq!(tasks[0]["list_id"], "list-home");
    assert_eq!(tasks[0]["duration_minutes"], 10);
}

#[test]
fn relative_due_dates_resolve_against_todays_date() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("relative-dates");
    let before = today();

    sandbox.run(&api, &["add", "Ship it :tomorrow"]);
    sandbox.run(&api, &["add", "Ship it later :today"]);
    sandbox.run(&api, &["add", "Ship it eventually :next-week"]);

    let tasks = api.posted_tasks();
    assert_is_tomorrow(&tasks[0]["timeless_due_at"], before);
    let fmt = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
    assert!([fmt(before), fmt(today())].contains(&tasks[1]["timeless_due_at"].as_str().unwrap().to_string()));
    assert!([fmt(before + Duration::weeks(1)), fmt(today() + Duration::weeks(1))]
        .contains(&tasks[2]["timeless_due_at"].as_str().unwrap().to_string()));
}

#[test]
fn the_original_colon_minutes_syntax_still_sets_the_duration() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("legacy-minutes");

    sandbox.run(&api, &["add", "Workout :45"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Workout");
    assert_eq!(task["duration_minutes"], 45);
    assert!(task.get("timeless_due_at").is_none());
}

#[test]
fn words_that_only_resemble_syntax_stay_in_the_title() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("lookalikes");

    sandbox.run(&api, &["add", "Learn C++ and ~roughly everything :someday"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Learn C++ and ~roughly everything :someday");
    assert!(task.get("duration_minutes").is_none());
    assert!(task.get("timeless_due_at").is_none());
}

#[test]
fn notes_are_not_scanned_for_syntax() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("notes");

    sandbox.run(&api, &["add", "Call bank n: ask about ~5m :today @home .urgent"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Call bank");
    assert_eq!(task["notes"], "ask about ~5m :today @home .urgent");
    assert!(task.get("list_id").is_none());
    assert!(task.get("label_ids").is_none());
    assert!(task.get("duration_minutes").is_none());
    assert!(task.get("timeless_due_at").is_none());
}

#[test]
fn a_task_can_be_read_from_stdin() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("stdin");

    sandbox.run_stdin(&api, &["add"], "Weekly review @work ~30m n: look at the board\n");

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Weekly review");
    assert_eq!(task["list_id"], "list-work");
    assert_eq!(task["duration_minutes"], 30);
    assert_eq!(task["notes"], "look at the board");
}

#[test]
fn a_task_starting_with_a_flag_can_be_created_after_double_dash() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("double-dash");

    sandbox.run(&api, &["add", "--", "--version", "bump", "~15m"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "--version bump");
    assert_eq!(task["duration_minutes"], 15);
}

#[test]
fn list_names_match_by_prefix_and_by_substring() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("list-matching");

    sandbox.run(&api, &["add", "One @WORK"]);
    sandbox.run(&api, &["add", "Two @workproj"]);
    sandbox.run(&api, &["add", "Three @ojects"]);
    sandbox.run(&api, &["add", "Four @work-projects"]);

    for task in api.posted_tasks() {
        assert_eq!(task["list_id"], "list-work", "{task}");
    }
}

#[test]
fn an_unknown_list_or_label_does_not_stop_the_task_being_created() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("unknown-names");

    sandbox.run(&api, &["add", "Lost task @nowhere .nothing"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Lost task");
    assert!(task.get("list_id").is_none());
    assert!(task.get("label_ids").is_none());
}

#[test]
fn lists_and_labels_are_fetched_once_and_then_read_from_the_cache() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("name-cache");

    sandbox.run(&api, &["add", "First @work .urgent"]);
    sandbox.run(&api, &["add", "Second @home .writing"]);
    sandbox.run(&api, &["add", "Third @work .urgent"]);

    assert_eq!(api.count("GET", "/lists"), 1);
    assert_eq!(api.count("GET", "/labels"), 1);
    assert_eq!(api.count("POST", "/tasks"), 3);
    let tasks = api.posted_tasks();
    assert_eq!(tasks[1]["list_id"], "list-home");
    assert_eq!(tasks[1]["label_ids"], json!(["label-writing"]));
}

#[test]
fn a_failed_send_is_queued_with_absolute_dates_and_retried_first_next_time() {
    let sandbox = Sandbox::new("offline-queue");
    let before = today();

    let down = MockApi::start(500);
    let output = sandbox.run(&down, &["add", "Buy milk @home ~10m :tomorrow n: oat, not dairy"]);
    assert!(stderr(&output).contains("Failed to send task"), "{}", stderr(&output));

    let queued = sandbox.queued_tasks();
    assert_eq!(queued.len(), 1);
    assert!(!queued[0].contains(":tomorrow"), "{}", queued[0]);
    assert!(queued[0].starts_with("Buy milk @home ~10m :20"), "{}", queued[0]);
    assert!(queued[0].ends_with("n: oat, not dairy"), "{}", queued[0]);

    let up = MockApi::start(200);
    sandbox.run(&up, &["add", "Second task"]);

    let tasks = up.posted_tasks();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "Buy milk");
    assert_eq!(tasks[0]["notes"], "oat, not dairy");
    assert_eq!(tasks[0]["duration_minutes"], 10);
    assert_is_tomorrow(&tasks[0]["timeless_due_at"], before);
    assert_eq!(tasks[1]["title"], "Second task");
    assert!(sandbox.queued_tasks().is_empty(), "{}", sandbox.cache_contents());
}

#[test]
fn a_queued_task_stays_queued_while_the_api_keeps_failing() {
    let sandbox = Sandbox::new("still-failing");
    let down = MockApi::start(500);

    sandbox.run(&down, &["add", "First"]);
    sandbox.run(&down, &["add", "Second"]);

    assert_eq!(sandbox.queued_tasks(), vec!["First", "Second"]);
}

#[test]
fn an_unreachable_api_queues_the_task() {
    let sandbox = Sandbox::new("unreachable");
    // Nothing listens on port 1.
    let output = sandbox.run_with(Some("http://127.0.0.1:1"), Some("test-token"), &["add", "Offline task :today"], None);

    assert!(stderr(&output).contains("Failed to send task"), "{}", stderr(&output));
    assert_eq!(sandbox.queued_tasks().len(), 1);
}

#[test]
fn nothing_is_sent_without_a_token() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("no-token");

    let output = sandbox.run_with(Some(&api.url), None, &["add", "Task without a token"], None);

    assert!(stderr(&output).contains("GODSPEED_TOKEN"), "{}", stderr(&output));
    assert!(api.requests().is_empty());
}

#[test]
fn help_and_version_need_no_token_and_send_nothing() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("help-version");

    let help = sandbox.run_with(Some(&api.url), None, &["--help"], None);
    let version = sandbox.run_with(Some(&api.url), None, &["-V"], None);

    let help = stdout(&help);
    for text in ["~duration", ":date", "n: text", "@list", ".label", "GODSPEED_TOKEN"] {
        assert!(help.contains(text), "{text} missing from help:\n{help}");
    }
    assert_eq!(stdout(&version).trim(), format!("godspeed-cli {}", env!("CARGO_PKG_VERSION")));
    assert!(api.requests().is_empty());
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("the process exited normally")
}

#[test]
fn more_than_one_list_is_rejected_without_contacting_the_api_or_queueing() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("two-lists");

    let output = sandbox.run(&api, &["add", "Confused task @work @home"]);

    assert!(stderr(&output).contains("Multiple lists"), "{}", stderr(&output));
    assert_eq!(code(&output), 2);
    assert!(api.requests().is_empty());
    assert!(sandbox.queued_tasks().is_empty(), "{}", sandbox.cache_contents());
}

#[test]
fn a_task_with_no_title_is_rejected_without_contacting_the_api_or_queueing() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("no-title");

    let output = sandbox.run(&api, &["add", "@work .urgent ~10m :tomorrow n: only notes"]);

    assert!(stderr(&output).contains("no title"), "{}", stderr(&output));
    assert_eq!(code(&output), 2);
    assert!(api.requests().is_empty());
    assert!(sandbox.queued_tasks().is_empty());
}

#[test]
fn at_signs_in_the_notes_do_not_count_as_lists() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("at-in-notes");

    let output = sandbox.run(&api, &["add", "Call bank @home n: ask @bob and @alice about fees"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let task = &api.posted_tasks()[0];
    assert_eq!(task["list_id"], "list-home");
    assert_eq!(task["notes"], "ask @bob and @alice about fees");
}

#[test]
fn exit_codes_distinguish_success_failure_and_unusable_input() {
    let sandbox = Sandbox::new("exit-codes");
    let up = MockApi::start(200);
    let down = MockApi::start(500);

    let sent = sandbox.run(&up, &["add", "Fine task"]);
    let rejected_by_api = sandbox.run(&down, &["add", "Another task"]);
    let unreachable = sandbox.run_with(Some("http://127.0.0.1:1"), Some("t"), &["add", "Third task"], None);
    let no_token = sandbox.run_with(Some(&up.url), None, &["add", "Fourth task"], None);
    let invalid = sandbox.run(&up, &["add", "@work"]);
    let help = sandbox.run_with(None, None, &["--help"], None);
    let version = sandbox.run_with(None, None, &["--version"], None);

    assert_eq!(code(&sent), 0);
    assert_eq!(code(&rejected_by_api), 1);
    assert_eq!(code(&unreachable), 1);
    assert_eq!(code(&no_token), 2);
    assert_eq!(code(&invalid), 2);
    assert_eq!(code(&help), 0);
    assert_eq!(code(&version), 0);
}

#[test]
fn running_with_nothing_to_send_succeeds_and_sends_nothing() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("empty-stdin");

    let output = sandbox.run_stdin(&api, &["add"], "");

    assert_eq!(code(&output), 0);
    assert!(api.requests().is_empty());
}

#[test]
fn queued_tasks_that_can_never_be_sent_are_dropped_and_shown() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("stuck-queue");
    sandbox.write_queue(&["Stuck @work @home", "Valid queued task @home", "@work .urgent"]);

    let output = sandbox.run(&api, &["add", "New task"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let titles: Vec<String> = api.posted_tasks().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect();
    assert_eq!(titles, vec!["Valid queued task", "New task"]);
    let message = stderr(&output);
    assert!(message.contains("Stuck @work @home"), "{message}");
    assert!(message.contains("Multiple lists"), "{message}");
    assert!(message.contains("no title"), "{message}");
    assert!(sandbox.queued_tasks().is_empty(), "{}", sandbox.cache_contents());
}

#[test]
fn a_queued_task_that_is_only_failing_to_send_is_kept() {
    let sandbox = Sandbox::new("kept-queue");
    sandbox.write_queue(&["Waiting task"]);
    let down = MockApi::start(500);

    sandbox.run(&down, &["add", "Another"]);

    assert_eq!(sandbox.queued_tasks(), vec!["Waiting task", "Another"]);
}

#[test]
fn add_creates_a_task_from_arguments_or_from_stdin() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("add-command");

    let output = sandbox.run(&api, &["add", "Buy", "milk", "@home", "~10m"]);
    let stdin_output = sandbox.run_stdin(&api, &["add"], "Read mail @work");

    assert_eq!(code(&output), 0);
    assert_eq!(code(&stdin_output), 0);
    let tasks = api.posted_tasks();
    assert_eq!(tasks[0]["title"], "Buy milk");
    assert_eq!(tasks[0]["list_id"], "list-home");
    assert_eq!(tasks[1]["title"], "Read mail");
}

#[test]
fn add_help_prints_the_help_and_does_not_create_a_task() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("add-help");

    let output = sandbox.run_with(Some(&api.url), None, &["add", "--help"], None);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("TASK SYNTAX"));
    assert!(api.requests().is_empty());
}

#[test]
fn after_add_a_task_may_start_with_any_word_including_command_names() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("command-word-tasks");

    sandbox.run(&api, &["add", "search", "the", "attic"]);
    sandbox.run(&api, &["add", "--", "refresh", "the", "roof"]);
    sandbox.run(&api, &["add", "search the garage"]);
    sandbox.run(&api, &["add", "--", "--version", "bump"]);
    sandbox.run(&api, &["add", "buy", "add", "milk"]);

    let titles: Vec<String> = api.posted_tasks().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect();
    assert_eq!(
        titles,
        vec!["search the attic", "refresh the roof", "search the garage", "--version bump", "buy add milk"]
    );
}

#[test]
fn search_with_no_copy_asks_for_a_refresh_and_creates_no_task() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("command-word-runs");

    let output = sandbox.run(&api, &["search", "the", "attic"]);

    assert_eq!(code(&output), 2);
    assert_eq!(api.count("POST", "/tasks"), 0);
}

fn authorization_of_post(api: &MockApi) -> Option<String> {
    api.requests().into_iter().find(|r| r.method == "POST").and_then(|r| r.authorization)
}

#[test]
fn the_token_is_read_from_the_token_file_when_the_environment_has_none() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("token-file");
    sandbox.write_token_file("# my token\nexport OTHER=x\nexport GODSPEED_TOKEN=\"file-token\"\n");

    sandbox.run_with(Some(&api.url), None, &["add", "Task using the file token"], None);

    assert_eq!(authorization_of_post(&api).as_deref(), Some("Bearer file-token"));
}

#[test]
fn the_environment_token_wins_over_the_token_file() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("token-env-wins");
    sandbox.write_token_file("export GODSPEED_TOKEN=\"file-token\"\n");

    sandbox.run_with(Some(&api.url), Some("env-token"), &["add", "Task"], None);

    assert_eq!(authorization_of_post(&api).as_deref(), Some("Bearer env-token"));
}

#[test]
fn an_empty_environment_token_falls_back_to_the_token_file() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("token-env-empty");
    sandbox.write_token_file("GODSPEED_TOKEN=file-token\n");

    sandbox.run_with(Some(&api.url), Some("  "), &["add", "Task"], None);

    assert_eq!(authorization_of_post(&api).as_deref(), Some("Bearer file-token"));
}

#[test]
fn a_token_file_without_a_token_line_is_the_same_as_no_token() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("token-file-empty");
    sandbox.write_token_file("# nothing here\nOTHER=x\n");

    let output = sandbox.run_with(Some(&api.url), None, &["add", "Task"], None);

    assert!(stderr(&output).contains("environment/Godspeed"), "{}", stderr(&output));
    assert!(api.requests().is_empty());
}

#[test]
fn the_token_never_appears_in_the_output() {
    let sandbox = Sandbox::new("token-secret");
    sandbox.write_token_file("export GODSPEED_TOKEN=\"super-secret-file-token\"\n");
    let down = MockApi::start(500);

    let from_file = sandbox.run_with(Some(&down.url), None, &["add", "Failing task"], None);
    let from_env = sandbox.run_with(Some(&down.url), Some("super-secret-env-token"), &["add", "Failing task"], None);

    for output in [from_file, from_env] {
        let all = format!("{}{}", stdout(&output), stderr(&output));
        assert!(!all.contains("super-secret"), "{all}");
    }
    assert!(!sandbox.cache_contents().contains("super-secret"));
}

// ---- refresh ----

/// A timestamp `n` minutes before a fixed moment, in the form the API's cursor takes.
fn minutes_ago(n: i64) -> String {
    DateTime::from_timestamp(1_800_000_000 - n * 60, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Tasks `t{i}` for each `i` in `range`, newest first, one minute apart.
fn tasks_page(range: std::ops::Range<i64>) -> Vec<Value> {
    range
        .map(|i| json!({"id": format!("t{i:04}"), "title": format!("Task {i}"), "updated_at": minutes_ago(i)}))
        .collect()
}

#[test]
fn refresh_downloads_tasks_lists_and_labels_into_the_cache() {
    let api = MockApi::start(200);
    api.queue_tasks_page(tasks_page(0..3));
    let sandbox = Sandbox::new("refresh-basic");

    let output = sandbox.run(&api, &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "Refreshed 3 tasks, 2 lists, 1 labels");
    assert_eq!(api.task_queries(), vec!["status=incomplete"]);
    let request = api.requests().into_iter().find(|r| r.path == "/tasks").unwrap();
    assert_eq!(request.authorization.as_deref(), Some("Bearer test-token"));
    assert_eq!(sandbox.stored_count("tasks"), 3);
    assert_eq!(sandbox.stored_count("lists"), 2);
    assert_eq!(sandbox.stored_count("labels"), 1);
    assert_eq!(sandbox.stored_complete_count(), 0);
}

#[test]
fn refresh_follows_updated_before_until_every_task_is_downloaded() {
    let api = MockApi::start(200);
    // A full page, then a short one that starts with the task the first page ended on.
    api.queue_tasks_page(tasks_page(0..250));
    api.queue_tasks_page(tasks_page(249..254));
    let sandbox = Sandbox::new("refresh-pages");

    let output = sandbox.run(&api, &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "Refreshed 254 tasks, 2 lists, 1 labels");
    let cursor = minutes_ago(249).replace(':', "%3A");
    assert_eq!(
        api.task_queries(),
        vec!["status=incomplete".to_string(), format!("status=incomplete&updated_before={cursor}")]
    );
    assert_eq!(sandbox.stored_count("tasks"), 254);
}

#[test]
fn refresh_include_completed_also_downloads_completed_tasks_and_flags_them() {
    let api = MockApi::start(200);
    api.queue_tasks_page(tasks_page(0..2));
    api.queue_tasks_page(tasks_page(100..104));
    let sandbox = Sandbox::new("refresh-completed");

    let output = sandbox.run(&api, &["refresh", "--include-completed"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(api.task_queries(), vec!["status=incomplete", "status=complete"]);
    assert_eq!(sandbox.stored_count("tasks"), 6);
    assert_eq!(sandbox.stored_complete_count(), 4);
}

#[test]
fn refresh_json_prints_only_the_report_on_stdout() {
    let api = MockApi::start(200);
    api.queue_tasks_page(tasks_page(0..4));
    let sandbox = Sandbox::new("refresh-json");

    let output = sandbox.run(&api, &["refresh", "--json"]);

    let report: Value = serde_json::from_str(&stdout(&output)).expect("stdout is only JSON");
    assert_eq!(report["tasks_count"], 4);
    assert_eq!(report["lists_count"], 2);
    assert_eq!(report["labels_count"], 1);
    assert_eq!(report["include_completed"], false);
    assert!(report["refreshed_at"].as_str().unwrap().ends_with('Z'));
    assert!(stderr(&output).contains("Fetched 4 tasks"), "{}", stderr(&output));
}

#[test]
fn a_full_refresh_replaces_the_previous_copy() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("refresh-replaces");
    api.queue_tasks_page(tasks_page(0..5));
    sandbox.run(&api, &["refresh"]);
    api.queue_tasks_page(tasks_page(10..12));

    let output = sandbox.run(&api, &["refresh", "--full"]);

    assert_eq!(stdout(&output).trim(), "Refreshed 2 tasks, 2 lists, 1 labels");
    assert_eq!(sandbox.stored_count("tasks"), 2);
}

#[test]
fn a_refresh_that_fails_part_way_leaves_the_previous_copy_alone() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("refresh-keeps-old");
    api.queue_tasks_page(tasks_page(0..5));
    sandbox.run(&api, &["refresh"]);
    // A full first page, then the API breaks.
    api.queue_tasks_page(tasks_page(0..250));
    api.queue_tasks_status(500, None);

    let output = sandbox.run(&api, &["refresh", "--full"]);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("Refresh failed"), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert_eq!(sandbox.stored_count("tasks"), 5);
}

#[test]
fn refresh_waits_as_told_when_rate_limited_and_then_succeeds() {
    let api = MockApi::start(200);
    api.queue_tasks_status(429, Some(1));
    api.queue_tasks_page(tasks_page(0..2));
    let sandbox = Sandbox::new("refresh-429");
    let started = std::time::Instant::now();

    let output = sandbox.run(&api, &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    assert_eq!(api.task_queries().len(), 2);
    assert!(stderr(&output).contains("Rate limited"), "{}", stderr(&output));
    assert_eq!(sandbox.stored_count("tasks"), 2);
}

#[test]
fn refresh_needs_a_token_and_rejects_unknown_arguments_without_contacting_the_api() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("refresh-usage");

    let no_token = sandbox.run_with(Some(&api.url), None, &["refresh"], None);
    let bad_flag = sandbox.run(&api, &["refresh", "--everything"]);
    let positional = sandbox.run(&api, &["refresh", "now"]);

    assert_eq!(code(&no_token), 2);
    assert!(stderr(&no_token).contains("GODSPEED_TOKEN"));
    assert_eq!(code(&bad_flag), 2);
    assert!(stderr(&bad_flag).contains("--everything"), "{}", stderr(&bad_flag));
    assert_eq!(code(&positional), 2);
    assert!(api.requests().is_empty());
}

#[test]
fn refresh_help_needs_no_token_and_sends_nothing() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("refresh-help");

    let output = sandbox.run_with(Some(&api.url), None, &["refresh", "--help"], None);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("--include-completed"));
    assert!(api.requests().is_empty());
}

#[test]
fn refresh_never_prints_the_token() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("refresh-secret");
    api.queue_tasks_status(500, None);

    let output = sandbox.run_with(Some(&api.url), Some("super-secret-refresh-token"), &["refresh"], None);

    let all = format!("{}{}", stdout(&output), stderr(&output));
    assert!(!all.contains("super-secret"), "{all}");
}

// ---- search ----

fn search_lists() -> Value {
    json!([{"id": "list-work", "name": "Work Projects"}, {"id": "list-home", "name": "Home"}])
}

fn search_labels() -> Value {
    json!([{"id": "label-urgent", "name": "Urgent", "group_name": "Priority"}, {"id": "label-low", "name": "Low", "group_name": "Priority"}])
}

fn incomplete_tasks() -> Vec<Value> {
    vec![
        json!({"id": "t1", "title": "Pay rent", "notes": "ring the landlord", "list_id": "list-home",
               "label_ids": ["label-urgent"], "timeless_due_at": "2026-10-31", "duration_minutes": 30,
               "updated_at": "2026-03-03T00:00:00Z"}),
        json!({"id": "t2", "title": "Write blog post", "list_id": "list-work", "label_ids": ["label-low"],
               "timeless_due_at": "2026-11-15", "duration_minutes": 120, "updated_at": "2026-03-02T00:00:00Z"}),
        json!({"id": "t3", "title": "Buy milk", "list_id": "list-home", "updated_at": "2026-03-01T00:00:00Z"}),
    ]
}

fn completed_tasks() -> Vec<Value> {
    vec![json!({"id": "t4", "title": "Old report", "list_id": "list-work", "label_ids": ["label-low"],
                "updated_at": "2026-02-01T00:00:00Z"})]
}

/// A sandbox whose cache was filled by a real `refresh` against the mock API.
fn refreshed(name: &str, include_completed: bool) -> Sandbox {
    let api = MockApi::start(200);
    api.queue_page_with(incomplete_tasks(), search_lists(), search_labels());
    let mut args = vec!["refresh"];
    if include_completed {
        api.queue_page_with(completed_tasks(), search_lists(), search_labels());
        args.push("--include-completed");
    }
    let sandbox = Sandbox::new(name);
    let output = sandbox.run(&api, &args);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    sandbox
}

/// Runs `search` with no token and no API: it only reads the cache.
fn search(sandbox: &Sandbox, args: &[&str]) -> Output {
    let mut full = vec!["search"];
    full.extend_from_slice(args);
    sandbox.run_with(None, None, &full, None)
}

fn titles_in_table(output: &Output) -> Vec<String> {
    stdout(output)
        .lines()
        .skip(1)
        .map(|line| line.split("  ").next().unwrap().trim().to_string())
        .collect()
}

fn ids_in_json(output: &Output) -> Vec<String> {
    let parsed: Value = serde_json::from_str(&stdout(output)).expect("stdout is JSON");
    parsed.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn search_finds_a_word_and_prints_a_table_with_a_header() {
    let sandbox = refreshed("search-word", false);

    let output = search(&sandbox, &["landlord"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("TITLE") && lines[0].contains("LIST") && lines[0].contains("DUE") && lines[0].ends_with("LABELS"));
    assert_eq!(lines[1], format!("{:<80}  {:<12}  {:<10}  Urgent", "Pay rent", "Home", "2026-10-31"));
}

#[test]
fn search_needs_no_token_and_no_network() {
    let sandbox = refreshed("search-offline", false);

    let output = sandbox.run_with(Some("http://127.0.0.1:1"), None, &["search", "milk"], None);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(titles_in_table(&output), vec!["Buy milk"]);
}

#[test]
fn an_empty_search_lists_every_incomplete_task_newest_update_first() {
    let sandbox = refreshed("search-all", true);

    let output = search(&sandbox, &[]);

    assert_eq!(titles_in_table(&output), vec!["Pay rent", "Write blog post", "Buy milk"]);
}

#[test]
fn completed_tasks_are_searched_only_with_is_complete() {
    let sandbox = refreshed("search-complete", true);

    let without = search(&sandbox, &["report"]);
    let with = search(&sandbox, &["is:complete", "report"]);
    let negated = search(&sandbox, &["is:complete", "!is:complete"]);

    assert!(stdout(&without).is_empty());
    assert_eq!(titles_in_table(&with), vec!["Old report"]);
    assert!(stdout(&negated).is_empty());
}

#[test]
fn is_complete_without_cached_completed_tasks_says_how_to_get_them() {
    let sandbox = refreshed("search-no-completed", false);

    let output = search(&sandbox, &["is:complete"]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("refresh --include-completed"), "{}", stderr(&output));
}

#[test]
fn field_boolean_and_date_operators_work_together() {
    let sandbox = refreshed("search-operators", false);

    let both = search(&sandbox, &["(label:low | label:urgent) due<=20261101"]);
    let in_work = search(&sandbox, &["list:work", "has:label"]);
    let negated = search(&sandbox, &["-label:low"]);
    let short = search(&sandbox, &["duration<60m"]);
    let long = search(&sandbox, &["duration>=1.5h"]);
    let group = search(&sandbox, &["label-group:priority", "due:2026-11-15"]);

    assert_eq!(titles_in_table(&both), vec!["Pay rent"]);
    assert_eq!(titles_in_table(&in_work), vec!["Write blog post"]);
    assert_eq!(titles_in_table(&negated), vec!["Pay rent", "Buy milk"]);
    assert_eq!(titles_in_table(&short), vec!["Pay rent"]);
    assert_eq!(titles_in_table(&long), vec!["Write blog post"]);
    assert_eq!(titles_in_table(&group), vec!["Write blog post"]);
}

#[test]
fn the_query_can_be_one_argument_or_many() {
    let sandbox = refreshed("search-args", false);

    let one = search(&sandbox, &["label:low due:20261115"]);
    let many = search(&sandbox, &["label:low", "due:20261115"]);

    assert_eq!(titles_in_table(&one), vec!["Write blog post"]);
    assert_eq!(stdout(&one), stdout(&many));
}

#[test]
fn a_query_starting_with_a_dash_is_a_negation_and_not_an_option() {
    let sandbox = refreshed("search-dash", false);

    let output = search(&sandbox, &["-label:low", "--json"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(ids_in_json(&output), vec!["t1", "t3"]);
}

#[test]
fn json_output_has_the_full_task_plus_list_and_label_names() {
    let sandbox = refreshed("search-json", false);

    let output = search(&sandbox, &["rent", "--json"]);

    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 1);
    let task = &parsed[0];
    assert_eq!(task["id"], "t1");
    assert_eq!(task["notes"], "ring the landlord");
    assert_eq!(task["duration_minutes"], 30);
    assert_eq!(task["list_names"], json!(["Home"]));
    assert_eq!(task["label_names"], json!(["Urgent"]));
}

#[test]
fn sorting_limiting_and_grouping() {
    let sandbox = refreshed("search-present", false);

    let by_due = search(&sandbox, &["sort-desc:due", "--json"]);
    let limited = search(&sandbox, &["sort:title", "limit:2", "--json"]);
    let by_label = search(&sandbox, &["sort:label:Urgent|Low", "--json"]);
    let grouped = search(&sandbox, &["group:list:Home|...rest", "sort:title"]);

    assert_eq!(ids_in_json(&by_due), vec!["t2", "t1", "t3"]);
    assert_eq!(ids_in_json(&limited), vec!["t3", "t1"]);
    assert_eq!(ids_in_json(&by_label), vec!["t1", "t2", "t3"]);
    let text = stdout(&grouped);
    let headings: Vec<&str> = text.lines().filter(|l| l.ends_with(')')).collect();
    assert_eq!(headings, vec!["Home (2)", "Work Projects (1)"], "{text}");
    assert!(text.find("Buy milk").unwrap() < text.find("Pay rent").unwrap());
}

#[test]
fn grouped_json_is_an_array_of_groups() {
    let sandbox = refreshed("search-group-json", false);

    let output = search(&sandbox, &["group:list", "--json"]);

    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(parsed[0]["group"], "Home");
    assert_eq!(parsed[0]["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(parsed[1]["group"], "Work Projects");
}

#[test]
fn no_matches_is_success_with_a_message_on_stderr_and_an_empty_array_in_json() {
    let sandbox = refreshed("search-none", false);

    let table = search(&sandbox, &["zebra"]);
    let json = search(&sandbox, &["zebra", "--json"]);

    assert_eq!(code(&table), 0);
    assert!(stdout(&table).is_empty());
    assert!(stderr(&table).contains("No matching tasks."), "{}", stderr(&table));
    assert_eq!(stdout(&json).trim(), "[]");
    assert!(!stderr(&json).contains("No matching"));
}

#[test]
fn an_invalid_query_is_a_usage_error_with_a_helpful_message() {
    let sandbox = refreshed("search-invalid", false);

    for (query, message) in [
        ("has:wings", "Unknown trait"),
        ("(label:low", "Unclosed parenthesis"),
        ("due:someday", "Invalid date"),
        ("sort:colour", "Unknown sort field"),
        ("title:\"open", "Unclosed quote"),
        ("limit:many", "Invalid limit"),
    ] {
        let output = search(&sandbox, &[query]);
        assert_eq!(code(&output), 2, "{query}");
        assert!(stdout(&output).is_empty(), "{query}");
        assert!(stderr(&output).contains(message), "{query}: {}", stderr(&output));
    }
}

#[test]
fn searching_before_the_first_refresh_says_to_refresh_and_creates_no_cache() {
    let sandbox = Sandbox::new("search-no-cache");

    let output = search(&sandbox, &["anything"]);

    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("godspeed-cli refresh"), "{}", stderr(&output));
    assert!(!sandbox.data_home.join("godspeed-cli/cache.sqlite3").exists());
}

#[test]
fn an_old_copy_is_mentioned_on_stderr_only() {
    let sandbox = refreshed("search-stale", false);
    let connection = rusqlite::Connection::open(sandbox.data_home.join("godspeed-cli/cache.sqlite3")).unwrap();
    connection.execute("UPDATE metadata SET value = '2020-01-01T00:00:00Z' WHERE key = 'refreshed_at'", []).unwrap();
    drop(connection);

    let output = search(&sandbox, &["milk", "--json"]);

    assert!(stderr(&output).contains("days old") && stderr(&output).contains("godspeed-cli refresh"), "{}", stderr(&output));
    assert_eq!(ids_in_json(&output), vec!["t3"]);
}

#[test]
fn a_fresh_copy_produces_no_age_note() {
    let sandbox = refreshed("search-fresh", false);

    let output = search(&sandbox, &["milk"]);

    assert!(!stderr(&output).contains("old"), "{}", stderr(&output));
}

#[test]
fn search_help_documents_the_syntax_and_needs_nothing() {
    let sandbox = Sandbox::new("search-help");

    let output = search(&sandbox, &["--help"]);

    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    for needle in ["MATCHING:", "DATES", "has:due-time", "GROUPING:", "SORTING:", "EXIT STATUS"] {
        assert!(text.contains(needle), "{needle}");
    }
}

// ---- the operators guide ----

#[test]
fn group_targets_without_rest_put_every_other_list_in_none() {
    let sandbox = refreshed("search-group-none", false);

    let output = search(&sandbox, &["group:list:Home"]);

    let text = stdout(&output);
    let headings: Vec<&str> = text.lines().filter(|l| l.ends_with(')')).collect();
    assert_eq!(headings, vec!["Home (2)", "None (1)"], "{text}");
}

#[test]
fn the_nested_flags_are_accepted_and_change_nothing() {
    let sandbox = refreshed("search-nested", false);

    let plain = search(&sandbox, &["milk"]);
    let enabled = search(&sandbox, &["milk", "enable:nested"]);
    let disabled = search(&sandbox, &["milk", "disable:nested"]);

    assert_eq!(code(&enabled), 0, "{}", stderr(&enabled));
    assert_eq!(code(&disabled), 0, "{}", stderr(&disabled));
    assert_eq!(stdout(&plain), stdout(&enabled));
    assert_eq!(stdout(&plain), stdout(&disabled));
}

#[test]
fn has_any_matches_every_task() {
    let sandbox = refreshed("search-has-any", false);

    let output = search(&sandbox, &["has:any", "--json"]);

    assert_eq!(ids_in_json(&output), vec!["t1", "t2", "t3"]);
}

#[test]
fn lists_and_labels_can_be_searched_by_id() {
    let sandbox = refreshed("search-ids", false);

    let list = search(&sandbox, &["in:list-work", "--json"]);
    let label = search(&sandbox, &["label:label-urgent", "--json"]);
    let group = search(&sandbox, &["label-group:priority", "--json"]);

    assert_eq!(ids_in_json(&list), vec!["t2"]);
    assert_eq!(ids_in_json(&label), vec!["t1"]);
    assert_eq!(ids_in_json(&group), vec!["t1", "t2"]);
}

#[test]
fn dates_are_read_the_way_the_date_picker_reads_them() {
    let sandbox = refreshed("search-dates", false);

    // Quoted, with spaces; a time; a month name; a relative amount; a keyword.
    let quoted = search(&sandbox, &["due>=\"Oct 31st 12am\" & due<=\"Oct 31st 1159pm\"", "--json"]);
    let named_month = search(&sandbox, &["due:\"nov 15\"", "--json"]);
    let far_future = search(&sandbox, &["due<=\"in 100000 days\"", "--json"]);
    let nothing_within_an_hour = search(&sandbox, &["due>=now & due<=1h", "--json"]);
    let by_year = search(&sandbox, &["due:20261115", "--json"]);

    assert_eq!(ids_in_json(&quoted), vec!["t1"]);
    assert_eq!(ids_in_json(&named_month), vec!["t2"]);
    assert_eq!(ids_in_json(&far_future), vec!["t1", "t2"]);
    assert!(ids_in_json(&nothing_within_an_hour).is_empty());
    assert_eq!(ids_in_json(&by_year), vec!["t2"]);
}

#[test]
fn durations_accept_words_as_well_as_suffixes() {
    let sandbox = refreshed("search-duration-words", false);

    let words = search(&sandbox, &["duration>=\"1 hour and 30 minutes\"", "--json"]);
    let short = search(&sandbox, &["duration<15m", "--json"]);
    let exact = search(&sandbox, &["duration=30", "--json"]);

    assert_eq!(ids_in_json(&words), vec!["t2"]);
    assert!(ids_in_json(&short).is_empty());
    assert_eq!(ids_in_json(&exact), vec!["t1"]);
}

#[test]
fn is_uncleared_brings_in_completed_tasks_without_is_complete() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("search-uncleared");
    api.queue_page_with(incomplete_tasks(), search_lists(), search_labels());
    api.queue_page_with(
        vec![
            json!({"id": "c1", "title": "Done not cleared", "updated_at": "2026-02-01T00:00:00Z"}),
            json!({"id": "c2", "title": "Done and cleared", "cleared_at": "2026-02-02T00:00:00Z", "updated_at": "2026-02-02T00:00:00Z"}),
        ],
        search_lists(),
        search_labels(),
    );
    sandbox.run(&api, &["refresh", "--include-completed"]);

    let uncleared = search(&sandbox, &["is:uncleared", "--json"]);
    let not_uncleared = search(&sandbox, &["!is:uncleared", "--json"]);

    assert_eq!(ids_in_json(&uncleared), vec!["c1"]);
    assert!(!ids_in_json(&not_uncleared).contains(&"c1".to_string()));
    assert!(ids_in_json(&not_uncleared).contains(&"t1".to_string()));
}

#[test]
fn the_guides_today_smart_list_runs() {
    let sandbox = refreshed("search-today-list", false);

    let output = search(&sandbox, &["due:today group:list sort:due"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).is_empty(), "nothing in the sample data is due today");
    assert!(stderr(&output).contains("No matching tasks."));
}

#[test]
fn sort_by_modified_and_list_in_sidebar_order() {
    let sandbox = refreshed("search-modified", false);

    let modified = search(&sandbox, &["sort:modified", "--json"]);
    let modified_desc = search(&sandbox, &["sort-desc:modified", "--json"]);
    // The lists came as Work Projects then Home, so work comes first even though Home sorts first.
    let by_list = search(&sandbox, &["sort:list", "--json"]);

    assert_eq!(ids_in_json(&modified), vec!["t3", "t2", "t1"]);
    assert_eq!(ids_in_json(&modified_desc), vec!["t1", "t2", "t3"]);
    assert_eq!(ids_in_json(&by_list), vec!["t2", "t1", "t3"]);
}

// ---- add --json ----

#[test]
fn add_json_prints_the_apis_response_and_nothing_else() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("add-json");

    let output = sandbox.run(&api, &["add", "--json", "Buy", "milk", "@home", "~10m"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let response: Value = serde_json::from_str(&stdout(&output)).expect("stdout is only JSON");
    assert_eq!(response["success"], true);
    assert_eq!(response["todo_item"]["id"], "new-task-id");
    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Buy milk");
    assert_eq!(task["list_id"], "list-home");
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn add_without_json_prints_nothing_on_success() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("add-no-json");

    let output = sandbox.run(&api, &["add", "Buy milk"]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).is_empty());
}

#[test]
fn add_json_reads_the_task_from_stdin_too() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("add-json-stdin");

    let output = sandbox.run_stdin(&api, &["add", "--json"], "Weekly review @work ~30m");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let response: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(response["success"], true);
    assert_eq!(api.posted_tasks()[0]["title"], "Weekly review");
}

#[test]
fn add_json_prints_nothing_on_stdout_when_the_task_is_queued_or_rejected() {
    let sandbox = Sandbox::new("add-json-failures");
    let down = MockApi::start(500);
    let up = MockApi::start(200);

    let queued = sandbox.run(&down, &["add", "--json", "Offline task"]);

    assert_eq!(code(&queued), 1);
    assert!(stdout(&queued).is_empty());
    assert!(stderr(&queued).contains("Failed to send task"));
    assert_eq!(sandbox.queued_tasks(), vec!["Offline task"]);

    // The next run retries the queued task first, so the queue empties. The invalid task is
    // rejected, and stdout stays empty: the retried task's response isn't printed either.
    let rejected = sandbox.run(&up, &["add", "--json", "@work"]);
    assert_eq!(code(&rejected), 2);
    assert!(stdout(&rejected).is_empty());
    assert!(stderr(&rejected).contains("no title"));
}

#[test]
fn add_json_does_not_print_the_responses_of_retried_queued_tasks() {
    let sandbox = Sandbox::new("add-json-retry");
    sandbox.write_queue(&["Queued earlier"]);
    let api = MockApi::start(200);

    let output = sandbox.run(&api, &["add", "--json", "New task"]);

    assert_eq!(code(&output), 0);
    assert_eq!(api.posted_tasks().len(), 2);
    let response: Value = serde_json::from_str(&stdout(&output)).expect("exactly one JSON document");
    assert_eq!(response["success"], true);
}

#[test]
fn a_task_can_start_with_the_word_json_flag_using_double_dash() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("add-dash-dash-json");

    let output = sandbox.run(&api, &["add", "--", "--json", "format", "notes"]);
    let trailing = sandbox.run(&api, &["add", "Convert", "to", "--json"]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).is_empty());
    assert!(stdout(&trailing).is_empty());
    let titles: Vec<String> = api.posted_tasks().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect();
    assert_eq!(titles, vec!["--json format notes", "Convert to --json"]);
}

// ---- incremental and full refresh, the config file, and refreshing from search ----

fn one_task(id: &str, minutes: i64) -> Value {
    json!({"id": id, "title": format!("Task {id}"), "updated_at": minutes_ago(minutes)})
}

/// A sandbox, with a mock API, that has done its first (full) refresh of tasks 0..3.
fn first_refresh(name: &str) -> (Sandbox, MockApi) {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new(name);
    api.queue_tasks_page(tasks_page(0..3));
    let output = sandbox.run(&api, &["refresh"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    (sandbox, api)
}

#[test]
fn the_first_refresh_records_the_facts_about_the_copy_in_the_database() {
    let (sandbox, _api) = first_refresh("facts");

    assert_eq!(sandbox.fact("include_completed").as_deref(), Some("false"));
    let refreshed_at = sandbox.fact("refreshed_at").expect("refreshed_at");
    assert!(refreshed_at.ends_with('Z'), "{refreshed_at}");
    assert_eq!(sandbox.fact("last_full_refresh_at").as_deref(), Some(refreshed_at.as_str()));
    // The newest change among the tasks, as the API wrote it.
    assert_eq!(sandbox.fact("high_water").as_deref(), Some(minutes_ago(0).as_str()));
}

#[test]
fn the_next_refresh_asks_only_for_what_changed_and_adds_it_to_the_copy() {
    let (sandbox, api) = first_refresh("incremental");
    api.queue_tasks_page(vec![one_task("new1", -5)]);
    api.queue_tasks_page(vec![]);

    let output = sandbox.run(&api, &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        stdout(&output).trim(),
        "Updated 1 task since the last refresh; the copy now has 4 tasks, 2 lists, 1 labels"
    );
    // A minute before the newest change it had seen (08:00:00 -> 07:59:00).
    let after = "2027-01-15T07%3A59%3A00.000000Z";
    assert_eq!(
        api.task_queries(),
        vec![
            "status=incomplete".to_string(),
            format!("status=incomplete&updated_after={after}"),
            format!("status=complete&updated_after={after}"),
        ]
    );
    assert_eq!(sandbox.stored_count("tasks"), 4);
    assert_eq!(sandbox.fact("high_water").as_deref(), Some(minutes_ago(-5).as_str()));
    assert!(stderr(&output).contains("Incremental refresh: changes since 2027-01-15T07:59:00.000000Z"), "{}", stderr(&output));
}

#[test]
fn an_incremental_refresh_updates_a_changed_task_in_place() {
    let (sandbox, api) = first_refresh("incremental-edit");
    let mut edited = one_task("t0001", -10);
    edited["title"] = json!("Renamed");
    api.queue_tasks_page(vec![edited]);
    api.queue_tasks_page(vec![]);

    let output = sandbox.run(&api, &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(sandbox.stored_count("tasks"), 3, "an edit doesn't add a task");
    let found = search(&sandbox, &["renamed", "--json"]);
    assert_eq!(ids_in_json(&found), vec!["t0001"]);
}

#[test]
fn a_task_completed_since_the_last_refresh_leaves_the_copy() {
    let (sandbox, api) = first_refresh("incremental-completed");
    api.queue_tasks_page(vec![]);
    api.queue_tasks_page(vec![one_task("t0001", -10)]);

    let output = sandbox.run(&api, &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).contains("Updated 1 task since"), "{}", stdout(&output));
    assert_eq!(sandbox.stored_count("tasks"), 2);
}

#[test]
fn json_says_whether_the_refresh_was_full_or_incremental_and_why() {
    let (sandbox, api) = first_refresh("refresh-json-mode");
    api.queue_tasks_page(vec![one_task("new1", -5), one_task("new2", -6)]);
    api.queue_tasks_page(vec![]);

    let incremental = sandbox.run(&api, &["refresh", "--json"]);

    let report: Value = serde_json::from_str(&stdout(&incremental)).unwrap();
    assert_eq!(report["mode"], "incremental");
    assert_eq!(report["reason"], Value::Null);
    assert_eq!(report["changed_count"], 2);
    assert_eq!(report["tasks_count"], 5);

    api.queue_tasks_page(tasks_page(0..4));
    let full = sandbox.run(&api, &["refresh", "--full", "--json"]);
    let report: Value = serde_json::from_str(&stdout(&full)).unwrap();
    assert_eq!(report["mode"], "full");
    assert_eq!(report["reason"], "asked for with --full");
    assert_eq!(report["changed_count"], 4);
    assert_eq!(report["tasks_count"], 4);
}

#[test]
fn a_full_refresh_asks_for_everything_again_and_drops_tasks_that_no_longer_exist() {
    let (sandbox, api) = first_refresh("full-flag");
    api.queue_tasks_page(tasks_page(0..2));

    let output = sandbox.run(&api, &["refresh", "--full"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "Refreshed 2 tasks, 2 lists, 1 labels");
    assert_eq!(api.task_queries(), vec!["status=incomplete", "status=incomplete"], "no updated_after");
    assert_eq!(sandbox.stored_count("tasks"), 2);
    assert!(stderr(&output).contains("Full refresh: asked for with --full"), "{}", stderr(&output));
}

#[test]
fn changing_whether_completed_tasks_are_included_makes_the_refresh_full() {
    let (sandbox, api) = first_refresh("completed-switch");
    api.queue_tasks_page(tasks_page(0..3));
    api.queue_tasks_page(tasks_page(100..102));

    let output = sandbox.run(&api, &["refresh", "--include-completed", "--json"]);

    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["mode"], "full");
    assert!(report["reason"].as_str().unwrap().contains("include completed tasks"), "{report}");
    assert_eq!(report["include_completed"], true);
    assert_eq!(api.task_queries()[1..], ["status=incomplete", "status=complete"], "no updated_after");
    assert_eq!(sandbox.fact("include_completed").as_deref(), Some("true"));
    assert_eq!(sandbox.stored_complete_count(), 2);
}

#[test]
fn an_old_last_full_refresh_makes_the_next_refresh_full_automatically() {
    let (sandbox, api) = first_refresh("aged");
    sandbox.set_fact("last_full_refresh_at", "2020-01-01T00:00:00Z");
    api.queue_tasks_page(tasks_page(0..2));

    let output = sandbox.run(&api, &["refresh", "--json"]);

    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["mode"], "full");
    assert!(report["reason"].as_str().unwrap().contains("last full refresh is old"), "{report}");
    assert_eq!(sandbox.stored_count("tasks"), 2, "the deleted task is gone");
}

#[test]
fn the_config_file_can_include_completed_tasks_by_default() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("config-completed");
    sandbox.write_config("include_completed = true\n");
    api.queue_tasks_page(tasks_page(0..2));
    api.queue_tasks_page(tasks_page(100..101));

    let output = sandbox.run(&api, &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(api.task_queries(), vec!["status=incomplete", "status=complete"]);
    assert_eq!(sandbox.stored_complete_count(), 1);
    assert_eq!(sandbox.fact("include_completed").as_deref(), Some("true"));
}

#[test]
fn full_refresh_after_in_the_config_file_sets_how_old_a_full_refresh_may_get() {
    let (sandbox, api) = first_refresh("config-full-after");
    sandbox.write_config("full_refresh_after = \"0\"\n");
    api.queue_tasks_page(tasks_page(0..3));

    let output = sandbox.run(&api, &["refresh", "--json"]);

    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["mode"], "full", "0 means every refresh is a full one");

    sandbox.write_config("full_refresh_after = \"never\"\n");
    sandbox.set_fact("last_full_refresh_at", "2000-01-01T00:00:00Z");
    api.queue_tasks_page(vec![]);
    api.queue_tasks_page(vec![]);
    let never = sandbox.run(&api, &["refresh", "--json"]);
    let report: Value = serde_json::from_str(&stdout(&never)).unwrap();
    assert_eq!(report["mode"], "incremental", "never turns the automatic full refresh off");
}

#[test]
fn the_config_file_is_found_under_xdg_config_home_when_that_is_set() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("config-xdg");
    let config_home = sandbox.data_home.join("elsewhere");
    fs::create_dir_all(config_home.join("godspeed-cli")).unwrap();
    fs::write(config_home.join("godspeed-cli/config.toml"), "include_completed = true\n").unwrap();
    api.queue_tasks_page(tasks_page(0..1));
    api.queue_tasks_page(tasks_page(100..101));

    let output = sandbox.run_with_env(
        Some(&api.url),
        Some("test-token"),
        &["refresh"],
        None,
        &[("XDG_CONFIG_HOME", config_home.to_str().unwrap())],
    );

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(api.task_queries(), vec!["status=incomplete", "status=complete"]);
}

#[test]
fn a_mistake_in_the_config_file_is_reported_by_name_and_nothing_is_fetched() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("config-bad");
    sandbox.write_config("max_ag = \"1h\"\n");

    let refresh = sandbox.run(&api, &["refresh"]);
    let search = sandbox.run(&api, &["search", "milk"]);

    for output in [refresh, search] {
        assert_eq!(code(&output), 2);
        assert!(stderr(&output).contains("config.toml"), "{}", stderr(&output));
        assert!(stderr(&output).contains("unknown setting 'max_ag'"), "{}", stderr(&output));
    }
    assert!(api.requests().is_empty());
}

#[test]
fn creating_a_task_ignores_the_config_file() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("config-ignored-by-add");
    sandbox.write_config("this is not toml at all [[[\n");

    let output = sandbox.run(&api, &["add", "Buy milk"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(api.posted_tasks().len(), 1);
}

/// A sandbox, with a mock API, whose copy was made from the sample tasks and lists.
fn sample_copy(name: &str) -> (Sandbox, MockApi) {
    let api = MockApi::start(200);
    api.queue_page_with(incomplete_tasks(), search_lists(), search_labels());
    let sandbox = Sandbox::new(name);
    let output = sandbox.run(&api, &["refresh"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    (sandbox, api)
}

/// Runs `search` with a token and the mock API, so it is able to refresh.
fn search_online(sandbox: &Sandbox, api: &MockApi, args: &[&str]) -> Output {
    let mut full = vec!["search"];
    full.extend_from_slice(args);
    sandbox.run(api, &full)
}

fn new_task() -> Value {
    json!({"id": "t-new", "title": "Brand new thing", "list_id": "list-home", "updated_at": "2026-03-10T00:00:00Z"})
}

#[test]
fn search_refresh_brings_in_new_tasks_before_searching() {
    let (sandbox, api) = sample_copy("search-refresh");
    api.queue_page_with(vec![new_task()], search_lists(), search_labels());
    api.queue_page_with(vec![], search_lists(), search_labels());

    let before = search(&sandbox, &["brand", "--json"]);
    let output = search_online(&sandbox, &api, &["--refresh", "brand", "--json"]);

    assert!(ids_in_json(&before).is_empty());
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(ids_in_json(&output), vec!["t-new"]);
    assert!(stderr(&output).contains("Incremental refresh"), "{}", stderr(&output));
    assert_eq!(api.task_queries().len(), 3, "the first refresh and then one incremental refresh");
}

#[test]
fn search_without_flags_or_settings_never_contacts_the_api() {
    let (sandbox, api) = sample_copy("search-offline-by-default");
    sandbox.set_fact("refreshed_at", "2020-01-01T00:00:00Z");

    let output = search_online(&sandbox, &api, &["milk"]);

    assert_eq!(code(&output), 0);
    assert_eq!(api.task_queries().len(), 1, "only the first refresh");
}

#[test]
fn search_max_age_refreshes_only_a_copy_that_is_older_than_that() {
    let (sandbox, api) = sample_copy("search-max-age");

    let fresh = search_online(&sandbox, &api, &["--max-age", "1d", "milk"]);
    assert_eq!(code(&fresh), 0, "{}", stderr(&fresh));
    assert_eq!(api.task_queries().len(), 1, "a copy refreshed moments ago is within a day");

    sandbox.set_fact("refreshed_at", "2020-01-01T00:00:00Z");
    api.queue_page_with(vec![new_task()], search_lists(), search_labels());
    api.queue_page_with(vec![], search_lists(), search_labels());
    let stale = search_online(&sandbox, &api, &["--max-age=1d", "brand", "--json"]);

    assert_eq!(code(&stale), 0, "{}", stderr(&stale));
    assert_eq!(api.task_queries().len(), 3);
    assert_eq!(ids_in_json(&stale), vec!["t-new"]);
}

#[test]
fn max_age_in_the_config_file_applies_unless_the_flag_overrides_it() {
    let (sandbox, api) = sample_copy("search-config-max-age");
    sandbox.write_config("max_age = \"1h\"\n");
    sandbox.set_fact("refreshed_at", "2020-01-01T00:00:00Z");

    let never = search_online(&sandbox, &api, &["--max-age", "never", "milk"]);
    assert_eq!(code(&never), 0, "{}", stderr(&never));
    assert_eq!(api.task_queries().len(), 1, "--max-age never overrides the file");

    api.queue_page_with(vec![new_task()], search_lists(), search_labels());
    api.queue_page_with(vec![], search_lists(), search_labels());
    let by_config = search_online(&sandbox, &api, &["brand", "--json"]);
    assert_eq!(ids_in_json(&by_config), vec!["t-new"], "{}", stderr(&by_config));
    assert_eq!(api.task_queries().len(), 3);

    // Just refreshed, so within the hour: no more requests.
    let again = search_online(&sandbox, &api, &["brand"]);
    assert_eq!(code(&again), 0);
    assert_eq!(api.task_queries().len(), 3);
}

#[test]
fn a_failed_refresh_from_search_falls_back_to_the_existing_copy_with_a_note() {
    let (sandbox, api) = sample_copy("search-refresh-fails");
    api.queue_tasks_status(500, None);

    let output = search_online(&sandbox, &api, &["--refresh", "milk", "--json"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(ids_in_json(&output), vec!["t3"]);
    assert!(stderr(&output).contains("the refresh failed"), "{}", stderr(&output));
    assert!(stderr(&output).contains("searching the existing copy"), "{}", stderr(&output));
}

#[test]
fn search_refresh_without_a_token_falls_back_to_the_existing_copy_with_a_note() {
    let (sandbox, api) = sample_copy("search-refresh-no-token");

    let output = sandbox.run_with(Some(&api.url), None, &["search", "--refresh", "milk", "--json"], None);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(ids_in_json(&output), vec!["t3"]);
    assert!(stderr(&output).contains("GODSPEED_TOKEN is not set, so the copy can't be refreshed"), "{}", stderr(&output));
    assert_eq!(api.task_queries().len(), 1);
}

#[test]
fn search_refresh_with_no_copy_and_no_token_is_an_error() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("search-refresh-nothing");

    let output = sandbox.run_with(Some(&api.url), None, &["search", "--refresh", "milk"], None);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("no local copy") && stderr(&output).contains("GODSPEED_TOKEN"), "{}", stderr(&output));
    assert!(api.requests().is_empty());
}

#[test]
fn search_refresh_with_no_copy_makes_one_with_a_full_refresh_and_then_searches() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("search-refresh-first");
    api.queue_page_with(incomplete_tasks(), search_lists(), search_labels());

    let output = search_online(&sandbox, &api, &["--refresh", "milk", "--json"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(ids_in_json(&output), vec!["t3"]);
    assert!(stderr(&output).contains("Full refresh: there is no local copy yet"), "{}", stderr(&output));
    assert_eq!(api.task_queries(), vec!["status=incomplete"]);
}

#[test]
fn a_refresh_from_search_keeps_completed_tasks_the_copy_already_has() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("search-keeps-completed");
    api.queue_page_with(incomplete_tasks(), search_lists(), search_labels());
    api.queue_page_with(completed_tasks(), search_lists(), search_labels());
    sandbox.run(&api, &["refresh", "--include-completed"]);
    assert_eq!(sandbox.stored_complete_count(), 1);
    api.queue_page_with(vec![], search_lists(), search_labels());
    api.queue_page_with(vec![], search_lists(), search_labels());

    let output = search_online(&sandbox, &api, &["--refresh", "milk"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stderr(&output).contains("Incremental refresh"), "{}", stderr(&output));
    assert_eq!(sandbox.stored_complete_count(), 1, "the completed task is still there");
    assert_eq!(sandbox.fact("include_completed").as_deref(), Some("true"));
}

#[test]
fn a_search_for_completed_tasks_refreshes_with_them_included() {
    let (sandbox, api) = sample_copy("search-wants-completed");
    api.queue_page_with(incomplete_tasks(), search_lists(), search_labels());
    api.queue_page_with(completed_tasks(), search_lists(), search_labels());

    let output = search_online(&sandbox, &api, &["--refresh", "is:complete", "report", "--json"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(ids_in_json(&output), vec!["t4"]);
    assert!(stderr(&output).contains("Full refresh"), "{}", stderr(&output));
    assert_eq!(api.task_queries()[1..], ["status=incomplete", "status=complete"]);
}

#[test]
fn a_bad_max_age_is_a_usage_error_and_nothing_is_fetched() {
    let (sandbox, api) = sample_copy("search-bad-max-age");

    let missing = search_online(&sandbox, &api, &["--max-age"]);
    let bad = search_online(&sandbox, &api, &["--max-age", "soon", "milk"]);

    assert_eq!(code(&missing), 2);
    assert_eq!(code(&bad), 2);
    assert!(stderr(&bad).contains("'soon' is not a duration"), "{}", stderr(&bad));
    assert_eq!(api.task_queries().len(), 1);
}

#[test]
fn a_refresh_from_search_leaves_stdout_for_the_results_only() {
    let (sandbox, api) = sample_copy("search-refresh-stdout");
    api.queue_page_with(vec![new_task()], search_lists(), search_labels());
    api.queue_page_with(vec![], search_lists(), search_labels());

    let output = search_online(&sandbox, &api, &["--refresh", "brand", "--json"]);

    let parsed: Value = serde_json::from_str(&stdout(&output)).expect("stdout is only the JSON results");
    assert_eq!(parsed.as_array().unwrap().len(), 1);
    assert!(stderr(&output).contains("Fetched"), "{}", stderr(&output));
}

// ---- a task is only ever created with `add` ----

#[test]
fn a_bare_word_is_not_a_command_and_creates_nothing() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("bare-word");
    sandbox.write_queue(&["Queued earlier"]);

    let output = sandbox.run(&api, &["string"]);

    assert_eq!(code(&output), 2);
    assert!(stdout(&output).is_empty());
    let message = stderr(&output);
    assert!(message.contains("'string' isn't a valid godspeed-cli command"), "{message}");
    assert!(message.contains("godspeed-cli add \"string\""), "{message}");
    assert!(message.contains("add, refresh, search, completions"), "{message}");
    assert!(!message.contains("GODSPEED_TOKEN"), "it fails before looking at the token: {message}");
    assert!(api.requests().is_empty(), "nothing is sent, and the queue isn't retried");
    assert_eq!(sandbox.queued_tasks(), vec!["Queued earlier"]);
}

#[test]
fn a_task_written_without_add_is_an_error_however_it_is_quoted() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("bare-task");

    let quoted = sandbox.run(&api, &["Buy milk @home ~10m"]);
    let words = sandbox.run(&api, &["Buy", "milk", "@home"]);
    let command_word = sandbox.run(&api, &["search the attic"]);
    let piped = sandbox.run_stdin(&api, &[], "Buy milk");

    assert_eq!(code(&quoted), 2);
    assert!(stderr(&quoted).contains("'Buy milk @home ~10m' isn't a valid godspeed-cli command"), "{}", stderr(&quoted));
    assert_eq!(code(&words), 2);
    assert!(stderr(&words).contains("'Buy' isn't a valid godspeed-cli command"), "{}", stderr(&words));
    assert_eq!(code(&command_word), 2);
    assert!(stderr(&command_word).contains("'search the attic' isn't a valid godspeed-cli command"));
    assert_eq!(code(&piped), 2, "stdin alone is not enough either");
    assert!(stderr(&piped).contains("no command given"), "{}", stderr(&piped));
    assert!(api.requests().is_empty());
    assert!(sandbox.queued_tasks().is_empty());
}

#[test]
fn no_arguments_is_an_error_that_lists_the_commands() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("no-arguments");

    let output = sandbox.run_with(Some(&api.url), Some("test-token"), &[], None);

    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("no command given"), "{}", stderr(&output));
    assert!(stderr(&output).contains("add, refresh, search, completions"), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(api.requests().is_empty());
}

#[test]
fn an_unknown_option_is_an_error_and_so_is_a_double_dash_without_add() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("unknown-option");

    let option = sandbox.run(&api, &["--nope"]);
    let dashes = sandbox.run(&api, &["--", "Buy", "milk"]);
    let json = sandbox.run(&api, &["--json", "Buy milk"]);

    assert_eq!(code(&option), 2);
    assert!(stderr(&option).contains("'--nope' isn't a valid godspeed-cli option"), "{}", stderr(&option));
    assert!(!stderr(&option).contains("To create a task"));
    assert_eq!(code(&dashes), 2);
    assert_eq!(code(&json), 2);
    assert!(api.requests().is_empty());
}

#[test]
fn command_names_are_case_sensitive_and_only_the_first_word_counts() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("command-case");

    for words in [["Add", "x"], ["SEARCH", "x"], ["buy", "add"], ["refreshing", "x"]] {
        let output = sandbox.run(&api, &words);
        assert_eq!(code(&output), 2, "{words:?}");
        assert!(stderr(&output).contains("isn't a valid godspeed-cli command"), "{words:?}: {}", stderr(&output));
    }
    assert!(api.requests().is_empty());
}

#[test]
fn after_add_the_first_word_can_be_anything_so_no_escaping_is_needed() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("add-anything");

    sandbox.run(&api, &["add", "completions", "are", "due", "~5m"]);
    sandbox.run(&api, &["add", "--version", "bump"]);
    sandbox.run(&api, &["add", "--", "--version", "bump"]);

    let tasks = api.posted_tasks();
    assert_eq!(tasks[0]["title"], "completions are due");
    assert_eq!(tasks[1]["title"], "--version bump", "only a lone `--help`, `-h`, `-V`, `--version`, `--json` or `--` first is an option");
}

#[test]
fn the_help_describes_add_as_the_only_way_to_create_a_task() {
    let sandbox = Sandbox::new("help-add-only");

    let help = sandbox.run_with(None, None, &["--help"], None);

    let text = stdout(&help);
    assert!(text.contains("godspeed-cli add [--json] [--] <TASK>..."), "{text}");
    assert!(text.contains("echo \"<TASK>\" | godspeed-cli add"), "{text}");
    assert!(text.contains("There are no other commands"), "{text}");
    assert!(!text.contains("default"), "add is no longer a default: {text}");
    assert!(text.contains("completions"), "{text}");
}

// ---- completions ----

#[test]
fn each_supported_shell_gets_its_script_on_stdout_and_needs_nothing_else() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("completions-scripts");

    for shell in ["bash", "zsh", "fish"] {
        let output = sandbox.run_with(Some(&api.url), None, &["completions", shell], None);
        assert_eq!(code(&output), 0, "{shell}: {}", stderr(&output));
        let script = stdout(&output);
        assert!(script.starts_with(&format!("# {shell} completion for godspeed-cli")), "{shell}: {script}");
        assert!(script.contains("godspeed-cli __complete"), "{shell}");
        assert!(stderr(&output).is_empty(), "{shell}: {}", stderr(&output));
    }
    assert!(api.requests().is_empty(), "no token and no network are needed");
    assert!(!sandbox.data_home.join("godspeed-cli").exists(), "printing a script writes nothing");
}

#[test]
fn an_unsupported_or_missing_shell_is_a_usage_error() {
    let sandbox = Sandbox::new("completions-bad-shell");

    let powershell = sandbox.run_with(None, None, &["completions", "powershell"], None);
    let none = sandbox.run_with(None, None, &["completions"], None);
    let two = sandbox.run_with(None, None, &["completions", "bash", "zsh"], None);
    let upper = sandbox.run_with(None, None, &["completions", "Bash"], None);

    assert_eq!(code(&powershell), 2);
    assert!(stderr(&powershell).contains("'powershell' isn't a supported shell (supported: bash, fish, zsh)"), "{}", stderr(&powershell));
    assert_eq!(code(&none), 2);
    assert!(stderr(&none).contains("say which shell"), "{}", stderr(&none));
    assert_eq!(code(&two), 2);
    assert_eq!(code(&upper), 2);
    for output in [powershell, none, two, upper] {
        assert!(stdout(&output).is_empty());
    }
}

#[test]
fn completions_help_says_how_to_install_it() {
    let sandbox = Sandbox::new("completions-help");

    let output = sandbox.run_with(None, None, &["completions", "--help"], None);

    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    assert!(text.contains("eval \"$(godspeed-cli completions bash)\""), "{text}");
    assert!(text.contains("godspeed-cli completions fish | source"), "{text}");
    assert!(text.contains("WorkProjects"), "{text}");
}

fn names(sandbox: &Sandbox, kind: &str) -> Vec<String> {
    let output = sandbox.run_with(None, None, &["__complete", kind], None);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    stdout(&output).lines().map(str::to_string).collect()
}

#[test]
fn names_come_from_the_local_copy_without_spaces_sorted() {
    let (sandbox, _api) = sample_copy("complete-names");

    assert_eq!(names(&sandbox, "lists"), vec!["Home", "WorkProjects"]);
    assert_eq!(names(&sandbox, "labels"), vec!["Low", "Urgent"]);
}

#[test]
fn names_also_come_from_the_lists_and_labels_already_used_when_creating_tasks() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("complete-names-toml");
    sandbox.run(&api, &["add", "Task @home .urgent"]);

    // The creating-tasks caches hold names in lowercase.
    assert_eq!(names(&sandbox, "lists"), vec!["home", "workprojects"]);
    assert_eq!(names(&sandbox, "labels"), vec!["urgent", "writing"]);
}

#[test]
fn the_copys_capitalisation_wins_over_the_lowercase_cache() {
    let (sandbox, api) = sample_copy("complete-names-merge");
    sandbox.run(&api, &["add", "Task @home .urgent"]);

    assert_eq!(names(&sandbox, "lists"), vec!["Home", "WorkProjects"]);
    assert_eq!(names(&sandbox, "labels"), vec!["Low", "Urgent", "writing"], "writing is only in the cache");
}

#[test]
fn names_a_shell_could_misread_are_never_offered() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("complete-names-unsafe");
    api.queue_page_with(
        vec![one_task("a", 1)],
        json!([
            {"id": "l1", "name": "Fine"},
            {"id": "l2", "name": "Evil $(touch pwned)"},
            {"id": "l3", "name": "semi;colon"},
            {"id": "l4", "name": "Back`tick`"},
            {"id": "l5", "name": "Side Projects"},
        ]),
        json!([{"id": "x1", "name": "$(reboot)"}, {"id": "x2", "name": "ok-label"}]),
    );
    sandbox.run(&api, &["refresh"]);

    assert_eq!(names(&sandbox, "lists"), vec!["Fine", "SideProjects"]);
    assert_eq!(names(&sandbox, "labels"), vec!["ok-label"]);
}

#[test]
fn asking_for_names_with_no_copy_or_the_wrong_kind_prints_nothing_and_no_error() {
    let sandbox = Sandbox::new("complete-names-none");

    assert!(names(&sandbox, "lists").is_empty());
    assert!(names(&sandbox, "labels").is_empty());
    let wrong = sandbox.run_with(None, None, &["__complete", "tasks"], None);
    let missing = sandbox.run_with(None, None, &["__complete"], None);
    for output in [wrong, missing] {
        assert_eq!(code(&output), 2);
        assert!(stdout(&output).is_empty() && stderr(&output).is_empty());
    }
}

#[test]
fn the_hidden_names_command_is_not_in_any_help() {
    let sandbox = Sandbox::new("complete-hidden");
    for args in [&["--help"][..], &["search", "--help"], &["completions", "--help"]] {
        let output = sandbox.run_with(None, None, args, None);
        assert!(!stdout(&output).contains("__complete"), "{args:?}");
    }
}

/// bash, if there is one. It is also the shell the completion function is tested in: it is run
/// for real with the variables bash sets while completing.
const BASH: &str = "/bin/bash";

fn have(shell: &str) -> bool {
    Command::new(shell).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Runs the bash completion as bash would, for `line` typed so far with the cursor at its end.
/// `words` is how bash would have cut the line into `COMP_WORDS`: the last is the word being
/// completed. Returns the candidates.
fn bash_complete(sandbox: &Sandbox, line: &str, words: &[&str]) -> Vec<String> {
    let bin_dir = sandbox.data_home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let link = bin_dir.join("godspeed-cli");
    if !link.exists() {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_godspeed-cli"), &link).unwrap();
    }
    let script = r#"
        eval "$(godspeed-cli completions bash)"
        COMP_LINE=$TEST_LINE
        COMP_POINT=${#COMP_LINE}
        IFS=$'\x1f' read -r -a COMP_WORDS <<< "$TEST_WORDS"
        # `read` drops a trailing empty word, which is the word being completed after a space.
        [[ $TEST_WORDS == *$'\x1f' ]] && COMP_WORDS+=("")
        COMP_CWORD=$(( ${#COMP_WORDS[@]} - 1 ))
        _godspeed_cli
        if (( ${#COMPREPLY[@]} )); then printf '%s\n' "${COMPREPLY[@]}"; fi
    "#;
    let output = Command::new(BASH)
        .args(["--noprofile", "--norc", "-c", script])
        .env_clear()
        .env("HOME", &sandbox.data_home)
        .env("XDG_DATA_HOME", &sandbox.data_home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .env("TEST_LINE", line)
        .env("TEST_WORDS", words.join("\x1f"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stderr.is_empty(), "completion must be silent: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().lines().map(str::to_string).collect()
}

macro_rules! needs_bash {
    () => {
        if !have(BASH) {
            eprintln!("skipped: {BASH} is not available");
            return;
        }
    };
}

#[test]
fn bash_completes_commands_flags_and_shell_names() {
    needs_bash!();
    let (sandbox, _api) = sample_copy("bash-commands");

    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli ", &["godspeed-cli", ""]),
        vec!["add", "refresh", "search", "completions", "--help", "--version"]
    );
    assert_eq!(bash_complete(&sandbox, "godspeed-cli re", &["godspeed-cli", "re"]), vec!["refresh"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli s", &["godspeed-cli", "s"]), vec!["search"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli --v", &["godspeed-cli", "--v"]), vec!["--version"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli completions ", &["godspeed-cli", "completions", ""]), vec!["bash", "fish", "zsh"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli completions z", &["godspeed-cli", "completions", "z"]), vec!["zsh"]);
    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli refresh --", &["godspeed-cli", "refresh", "--"]),
        vec!["--full", "--include-completed", "--json"]
    );
    assert_eq!(bash_complete(&sandbox, "godspeed-cli refresh --i", &["godspeed-cli", "refresh", "--i"]), vec!["--include-completed"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search --", &["godspeed-cli", "search", "--"]), vec!["--json", "--refresh", "--max-age", "--color"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search --m", &["godspeed-cli", "search", "--m"]), vec!["--max-age"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli add --", &["godspeed-cli", "add", "--"]), vec!["--json"]);
    assert!(bash_complete(&sandbox, "godspeed-cli add --r", &["godspeed-cli", "add", "--r"]).is_empty());
    assert!(bash_complete(&sandbox, "godspeed-cli nonsense ", &["godspeed-cli", "nonsense", ""]).is_empty());
}

#[test]
fn bash_completes_list_names_after_at_and_label_names_after_dot_in_add() {
    needs_bash!();
    let (sandbox, _api) = sample_copy("bash-add-names");

    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli add Buy milk @", &["godspeed-cli", "add", "Buy", "milk", "@"]),
        vec!["@Home", "@WorkProjects"]
    );
    assert_eq!(bash_complete(&sandbox, "godspeed-cli add Buy @wo", &["godspeed-cli", "add", "Buy", "@wo"]), vec!["@WorkProjects"], "case doesn't matter");
    assert_eq!(bash_complete(&sandbox, "godspeed-cli add Buy @H", &["godspeed-cli", "add", "Buy", "@H"]), vec!["@Home"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli add Buy .", &["godspeed-cli", "add", "Buy", "."]), vec![".Low", ".Urgent"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli add Buy .u", &["godspeed-cli", "add", "Buy", ".u"]), vec![".Urgent"]);
    assert!(bash_complete(&sandbox, "godspeed-cli add Buy @zzz", &["godspeed-cli", "add", "Buy", "@zzz"]).is_empty());
    // An ordinary word, or an operator that isn't completed, offers nothing.
    assert!(bash_complete(&sandbox, "godspeed-cli add Buy mi", &["godspeed-cli", "add", "Buy", "mi"]).is_empty());
    assert!(bash_complete(&sandbox, "godspeed-cli add Buy ~", &["godspeed-cli", "add", "Buy", "~"]).is_empty());
    assert!(bash_complete(&sandbox, "godspeed-cli add Buy :", &["godspeed-cli", "add", "Buy", ":"]).is_empty());
    // `list:` is search syntax, not task syntax.
    assert!(bash_complete(&sandbox, "godspeed-cli add list:", &["godspeed-cli", "add", "list", ":"]).is_empty());
}

#[test]
fn bash_completes_list_and_label_names_in_search_whichever_way_bash_cuts_the_word() {
    needs_bash!();
    let (sandbox, _api) = sample_copy("bash-search-names");
    let g = "godspeed-cli";

    // `:` is one of bash's word breaks by default, so the name is a word of its own.
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search label:U", &[g, "search", "label", ":", "U"]), vec!["Urgent"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search label:", &[g, "search", "label", ":"]), vec![":Low", ":Urgent"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search list:W", &[g, "search", "list", ":", "W"]), vec!["WorkProjects"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search in:h", &[g, "search", "in", ":", "h"]), vec!["Home"]);
    // Many people take `:` out of the word breaks, so the whole `label:U` is the word.
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search label:U", &[g, "search", "label:U"]), vec!["label:Urgent"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search list:", &[g, "search", "list:"]), vec!["list:Home", "list:WorkProjects"]);
    // After ! - and an opening parenthesis.
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search !label:L", &[g, "search", "!label", ":", "L"]), vec!["Low"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search -label:L", &[g, "search", "-label", ":", "L"]), vec!["Low"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search (label:L", &[g, "search", "(", "label", ":", "L"]), vec!["Low"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search milk label:L", &[g, "search", "milk", "label", ":", "L"]), vec!["Low"]);
    assert!(bash_complete(&sandbox, "godspeed-cli search label:zz", &[g, "search", "label", ":", "zz"]).is_empty());
}

#[test]
fn bash_does_not_complete_due_has_or_other_operators() {
    needs_bash!();
    let (sandbox, _api) = sample_copy("bash-not-completed");
    let g = "godspeed-cli";

    for (line, words) in [
        ("godspeed-cli search due:", vec![g, "search", "due", ":"]),
        ("godspeed-cli search due:t", vec![g, "search", "due", ":", "t"]),
        ("godspeed-cli search has:", vec![g, "search", "has", ":"]),
        ("godspeed-cli search has:n", vec![g, "search", "has", ":", "n"]),
        ("godspeed-cli search is:", vec![g, "search", "is", ":"]),
        ("godspeed-cli search sort:", vec![g, "search", "sort", ":"]),
        ("godspeed-cli search group:", vec![g, "search", "group", ":"]),
        ("godspeed-cli search folder:", vec![g, "search", "folder", ":"]),
        ("godspeed-cli search label-group:", vec![g, "search", "label-group", ":"]),
        ("godspeed-cli search milk", vec![g, "search", "milk"]),
        ("godspeed-cli search ", vec![g, "search", ""]),
        ("godspeed-cli search due:today", vec![g, "search", "due:today"]),
    ] {
        assert!(bash_complete(&sandbox, line, &words).is_empty(), "{line}");
    }
}

#[test]
fn bash_completes_inside_a_quoted_task_or_query() {
    needs_bash!();
    let (sandbox, _api) = sample_copy("bash-quoted");
    let g = "godspeed-cli";

    // bash gives the whole quoted text, opening quote included, as the word.
    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli add \"Buy milk @Wo", &[g, "add", "\"Buy milk @Wo"]),
        vec!["\"Buy milk @WorkProjects"]
    );
    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli add 'Buy milk .U", &[g, "add", "'Buy milk .U"]),
        vec!["'Buy milk .Urgent"]
    );
    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli search 'due:today label:U", &[g, "search", "'due:today label:U"]),
        vec!["'due:today label:Urgent"]
    );
    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli search \"(label:L", &[g, "search", "\"(label:L"]),
        vec!["\"(label:Low"]
    );
    assert_eq!(
        bash_complete(&sandbox, "godspeed-cli search '!list:H", &[g, "search", "'!list:H"]),
        vec!["'!list:Home"]
    );
    assert!(bash_complete(&sandbox, "godspeed-cli add \"Buy mi", &[g, "add", "\"Buy mi"]).is_empty());
}

#[test]
fn bash_completion_with_no_copy_offers_commands_but_no_names_and_says_nothing() {
    needs_bash!();
    let sandbox = Sandbox::new("bash-no-copy");

    assert!(bash_complete(&sandbox, "godspeed-cli add Buy @", &["godspeed-cli", "add", "Buy", "@"]).is_empty());
    assert_eq!(bash_complete(&sandbox, "godspeed-cli re", &["godspeed-cli", "re"]), vec!["refresh"]);
}

#[test]
fn bash_completion_never_runs_a_command_named_by_a_list() {
    needs_bash!();
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("bash-injection");
    let marker = sandbox.data_home.join("pwned");
    let evil = format!("Evil $(touch {})", marker.display());
    let evil_backtick = format!("`touch {}`", marker.display());
    api.queue_page_with(
        vec![one_task("a", 1)],
        json!([{"id": "l1", "name": "Fine"}, {"id": "l2", "name": evil}, {"id": "l3", "name": evil_backtick}]),
        json!([{"id": "x1", "name": format!("$(touch {})", marker.display())}]),
    );
    sandbox.run(&api, &["refresh"]);

    let lists = bash_complete(&sandbox, "godspeed-cli add Buy @", &["godspeed-cli", "add", "Buy", "@"]);
    let labels = bash_complete(&sandbox, "godspeed-cli add Buy .", &["godspeed-cli", "add", "Buy", "."]);
    let search = bash_complete(&sandbox, "godspeed-cli search list:", &["godspeed-cli", "search", "list:"]);

    assert_eq!(lists, vec!["@Fine"]);
    assert!(labels.is_empty());
    assert_eq!(search, vec!["list:Fine"]);
    assert!(!marker.exists(), "a list name must never be run as a command");
}

#[test]
fn the_generated_scripts_are_valid_for_the_shells_that_are_installed() {
    let sandbox = Sandbox::new("script-syntax");
    let mut checked = 0;
    for (shell, check) in [("/bin/bash", "-n"), ("zsh", "-n"), ("fish", "--no-execute")] {
        if !have(shell) {
            eprintln!("skipped: {shell} is not installed");
            continue;
        }
        let kind = shell.rsplit('/').next().unwrap();
        let script = sandbox.run_with(None, None, &["completions", kind], None);
        let file = sandbox.data_home.join(format!("completion.{kind}"));
        fs::write(&file, stdout(&script)).unwrap();
        let result = Command::new(shell).arg(check).arg(&file).output().unwrap();
        assert!(result.status.success(), "{kind}: {}", String::from_utf8_lossy(&result.stderr));
        checked += 1;
    }
    assert!(checked >= 1 || !have("/bin/bash"), "bash is checked whenever it exists");
}


// ---- limit comes after grouping ----

/// An account like the one this was reported on: a few undated "Eternal" tasks, and many
/// overdue ones.
fn eternal_account(name: &str) -> (Sandbox, MockApi) {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new(name);
    let lists = json!([{"id": "l-a", "name": "Alpha"}, {"id": "l-b", "name": "Beta"}]);
    let labels = json!([
        {"id": "x-eternal", "name": "Eternal"}, {"id": "x-vital", "name": "Vital"}, {"id": "x-high", "name": "high"},
        {"id": "x-med", "name": "medium"}, {"id": "x-low", "name": "low"}, {"id": "x-wait", "name": "Waiting For"},
        {"id": "x-sub", "name": "subtask"},
    ]);
    let task = |id: String, labels: Value, due: Option<&str>| {
        let mut v = json!({"id": id, "title": format!("Task {id}"), "list_id": "l-a", "label_ids": labels,
                           "updated_at": "2026-03-01T00:00:00Z"});
        if let Some(due) = due {
            v["timeless_due_at"] = json!(due);
        }
        v
    };
    let mut tasks = vec![
        task("e1".into(), json!(["x-eternal"]), None),
        task("e2".into(), json!(["x-eternal", "x-wait"]), None),
        task("e3".into(), json!(["x-eternal", "x-sub"]), None),
        task("e4".into(), json!(["x-eternal", "x-high"]), None),
        task("waiting".into(), json!(["x-wait"]), Some("2020-01-01")),
    ];
    // Sixty overdue tasks: more than the limit, and sorted before any task with no due date.
    for i in 0..60 {
        let label = if i % 2 == 0 { "x-high" } else { "x-low" };
        tasks.push(task(format!("late{i:02}"), json!([label]), Some("2020-01-01")));
    }
    api.queue_page_with(tasks, lists, labels);
    let output = sandbox.run(&api, &["refresh"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    (sandbox, api)
}

const ETERNAL_QUERY: &str = "label:\"Eternal\" | due<=today sort-asc:list group:label:Eternal|Vital|high|medium|low \
                             sort:due -label:\"Waiting For\" -label:subtask limit:40";

#[test]
fn the_reported_query_returns_all_the_eternal_tasks_within_the_limit() {
    let (sandbox, _api) = eternal_account("eternal-limit");

    let output = search(&sandbox, &[ETERNAL_QUERY, "--json"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let groups: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(groups[0]["group"], "Eternal", "the first target comes first");
    let eternal: Vec<&str> = groups[0]["tasks"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(eternal.len(), 4, "all four Eternal tasks: {eternal:?}");
    for id in ["e1", "e2", "e3", "e4"] {
        assert!(eternal.contains(&id), "{id} is missing: {eternal:?}");
    }
    let total: usize = groups.as_array().unwrap().iter().map(|g| g["tasks"].as_array().unwrap().len()).sum();
    assert_eq!(total, 40, "the limit still caps the whole result");
}

#[test]
fn the_limit_counts_tasks_in_the_order_they_are_shown() {
    let (sandbox, _api) = eternal_account("eternal-limit-table");

    let output = search(&sandbox, &[ETERNAL_QUERY]);

    let text = stdout(&output);
    let headings: Vec<&str> = text.lines().filter(|l| !l.starts_with("Task") && l.ends_with(')')).collect();
    assert_eq!(headings[0], "Eternal (4)", "{text}");
    assert_eq!(text.lines().filter(|l| l.starts_with("Task")).count(), 40, "{text}");
    // 30 `high` and 30 `low` tasks are overdue: the limit takes all the `high` ones and then 6 `low`.
    assert_eq!(headings, vec!["Eternal (4)", "high (30)", "low (6)"], "{text}");
}

#[test]
fn without_a_group_the_limit_still_follows_the_sort() {
    let (sandbox, _api) = eternal_account("eternal-limit-ungrouped");

    let output = search(&sandbox, &["label:Eternal | due<=today", "sort:due", "limit:5", "--json"]);

    let tasks: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let ids: Vec<&str> = tasks.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 5);
    assert!(ids.iter().all(|id| id.starts_with("late") || id.starts_with("waiting")), "dated tasks come first: {ids:?}");
}

#[test]
fn the_negations_after_the_or_only_apply_to_the_right_hand_side() {
    let (sandbox, _api) = eternal_account("eternal-precedence");

    let output = search(&sandbox, &["label:Eternal | due<=today -label:\"Waiting For\" -label:subtask", "--json"]);

    let tasks: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let ids: Vec<&str> = tasks.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    // `a | b c d` is `a | (b c d)`: e2 (Waiting For) and e3 (subtask) are Eternal, so they stay.
    for id in ["e1", "e2", "e3", "e4"] {
        assert!(ids.contains(&id), "{id}: {ids:?}");
    }
    assert!(!ids.contains(&"waiting"), "the Waiting For task is only reached through the right-hand side");

    let grouped = search(&sandbox, &["(label:Eternal | due<=today) -label:\"Waiting For\" -label:subtask", "--json"]);
    let tasks: Value = serde_json::from_str(&stdout(&grouped)).unwrap();
    let ids: Vec<&str> = tasks.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"e1") && ids.contains(&"e4"));
    assert!(!ids.contains(&"e2") && !ids.contains(&"e3"), "with parentheses the negations apply to everything: {ids:?}");
}

// ---- overdue tasks in colour ----

const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const HEADER: &str = "\x1b[1;36m";
const RESET: &str = "\x1b[0m";

/// A copy with an overdue task, a task due far ahead, an undated one and an overdue one that is
/// snoozed until much later.
fn overdue_copy(name: &str) -> Sandbox {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new(name);
    api.queue_page_with(
        vec![
            json!({"id": "late", "title": "Overdue task", "timeless_due_at": "2020-01-01", "updated_at": "2026-03-04T00:00:00Z"}),
            json!({"id": "soon", "title": "Future task", "timeless_due_at": "2099-01-01", "updated_at": "2026-03-03T00:00:00Z"}),
            json!({"id": "none", "title": "Undated task", "updated_at": "2026-03-02T00:00:00Z"}),
            json!({"id": "snoozed", "title": "Snoozed task", "timeless_due_at": "2020-01-01",
                   "timeless_snoozed_until": "2099-01-01", "updated_at": "2026-03-01T00:00:00Z"}),
        ],
        search_lists(),
        search_labels(),
    );
    let output = sandbox.run(&api, &["refresh"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    sandbox
}

#[test]
fn overdue_tasks_are_red_with_color_always_and_nothing_else_is() {
    let sandbox = overdue_copy("color-always");

    let output = search(&sandbox, &["--color", "always"]);

    let text = stdout(&output);
    let red_rows: Vec<&str> = text.lines().filter(|l| l.contains(RED)).collect();
    assert_eq!(red_rows.len(), 1, "{text:?}");
    assert!(red_rows[0].starts_with("Overdue task"), "the title is not coloured: {:?}", red_rows[0]);
    assert!(red_rows[0].contains(&format!("{RED}2020-01-01{RESET}")), "just the date is red: {:?}", red_rows[0]);
    assert_eq!(text.matches(RED).count(), 1);
    for plain in ["Future task", "Undated task", "Snoozed task"] {
        let line = text.lines().find(|l| l.contains(plain)).unwrap();
        assert!(!line.contains('\x1b'), "{plain}: {line:?}");
    }
    let header = text.lines().next().unwrap();
    assert!(header.starts_with(HEADER) && header.ends_with(RESET), "the header is bold cyan: {header:?}");
    assert!(!header.contains(RED), "and not red: {header:?}");
    assert_eq!(text.matches(HEADER).count(), 1);
}

#[test]
fn output_that_is_not_a_terminal_has_no_colour_by_default() {
    let sandbox = overdue_copy("color-pipe");

    let default = search(&sandbox, &[]);
    let auto = search(&sandbox, &["--color=auto"]);
    let never = search(&sandbox, &["--color", "never"]);

    for output in [default, auto, never] {
        assert!(!stdout(&output).contains('\x1b'), "{:?}", stdout(&output));
        assert!(stdout(&output).contains("Overdue task"));
    }
}

#[test]
fn colour_only_adds_the_codes_and_never_changes_the_text() {
    let sandbox = overdue_copy("color-same-text");

    let coloured = stdout(&search(&sandbox, &["--color=always"]));
    let plain = stdout(&search(&sandbox, &["--color=never"]));

    assert_eq!(coloured.replace(RED, "").replace(YELLOW, "").replace(HEADER, "").replace(RESET, ""), plain);
}

#[test]
fn an_explicit_color_flag_beats_no_color() {
    let sandbox = overdue_copy("color-no-color");
    let run = |args: &[&str]| sandbox.run_with_env(None, None, args, None, &[("NO_COLOR", "1")]);

    let always = run(&["search", "--color", "always"]);
    let auto = run(&["search"]);

    assert!(stdout(&always).contains(RED), "--color always wins over NO_COLOR");
    assert!(!stdout(&auto).contains('\x1b'));
}

#[test]
fn json_is_never_coloured_even_with_color_always() {
    let sandbox = overdue_copy("color-json");

    let output = search(&sandbox, &["--json", "--color", "always"]);

    assert!(!stdout(&output).contains('\x1b'));
    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 4);
}

#[test]
fn grouped_output_colours_overdue_dates_but_not_the_group_headings() {
    let sandbox = overdue_copy("color-grouped");

    let output = search(&sandbox, &["group:due", "--color=always"]);

    let text = stdout(&output);
    for heading in text.lines().filter(|l| l.ends_with(')') && !l.starts_with(' ') && !l.contains("task")) {
        assert!(!heading.contains('\x1b'), "{heading:?}");
    }
    assert_eq!(text.matches(RED).count(), 1, "{text:?}");
}

#[test]
fn a_bad_or_missing_color_value_is_a_usage_error() {
    let sandbox = overdue_copy("color-bad");

    let bad = search(&sandbox, &["--color", "red"]);
    let missing = search(&sandbox, &["--color"]);

    assert_eq!(code(&bad), 2);
    assert!(stderr(&bad).contains("'red' isn't auto, always or never"), "{}", stderr(&bad));
    assert_eq!(code(&missing), 2);
    assert!(stdout(&bad).is_empty() && stdout(&missing).is_empty());
}

#[test]
fn search_help_describes_the_colour() {
    let sandbox = Sandbox::new("color-help");

    let output = search(&sandbox, &["--help"]);

    let text = stdout(&output);
    assert!(text.contains("--color WHEN"), "{text}");
    assert!(text.contains("NO_COLOR"), "{text}");
    assert!(text.contains("overdue"), "{text}");
}

#[test]
fn bash_completes_the_color_flag_and_its_values() {
    needs_bash!();
    let (sandbox, _api) = sample_copy("bash-color");
    let g = "godspeed-cli";

    assert_eq!(bash_complete(&sandbox, "godspeed-cli search --c", &[g, "search", "--c"]), vec!["--color"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search --color ", &[g, "search", "--color", ""]), vec!["auto", "always", "never"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search --color a", &[g, "search", "--color", "a"]), vec!["auto", "always"]);
    assert_eq!(bash_complete(&sandbox, "godspeed-cli search --color n", &[g, "search", "--color", "n"]), vec!["never"]);
    assert!(bash_complete(&sandbox, "godspeed-cli add --c", &[g, "add", "--c"]).is_empty(), "add has no --color");
}

// ---- the header row ----

#[test]
fn search_results_have_a_header_row_grouped_or_not() {
    let sandbox = overdue_copy("header-rows");

    let plain = search(&sandbox, &[]);
    let grouped = search(&sandbox, &["group:due"]);
    let sorted_grouped = search(&sandbox, &["group:due", "sort:title", "limit:2"]);

    for (name, output) in [("plain", &plain), ("grouped", &grouped), ("sorted and limited", &sorted_grouped)] {
        let text = stdout(output);
        let first = text.lines().next().unwrap();
        assert!(
            first.starts_with("TITLE") && first.contains("LIST") && first.contains("DUE") && first.ends_with("LABELS"),
            "{name}: {first:?}"
        );
        assert_eq!(text.matches("TITLE").count(), 1, "{name}: the header comes once: {text}");
    }
}

#[test]
fn the_header_comes_before_the_first_group_and_not_between_groups() {
    let sandbox = overdue_copy("header-grouped");

    let text = stdout(&search(&sandbox, &["group:due"]));

    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("TITLE"), "{text}");
    assert!(lines[1].ends_with(')'), "a group heading follows the header: {text}");
    assert!(lines.iter().skip(1).all(|l| !l.starts_with("TITLE")), "{text}");
}

#[test]
fn the_columns_of_the_header_and_the_rows_line_up() {
    let sandbox = overdue_copy("header-aligned");

    let text = stdout(&search(&sandbox, &["--color=always"]));

    let plain: Vec<String> =
        text.lines().map(|l| l.replace(RED, "").replace(YELLOW, "").replace(HEADER, "").replace(RESET, "")).collect();
    let due_column = plain[0].find("DUE").unwrap();
    let overdue = plain.iter().find(|l| l.starts_with("Overdue task")).unwrap();
    assert_eq!(overdue.find("2020-01-01"), Some(due_column), "{overdue:?} under {:?}", plain[0]);
}

#[test]
fn no_results_print_no_header() {
    let sandbox = overdue_copy("header-none");

    let plain = search(&sandbox, &["zzzznothing"]);
    let grouped = search(&sandbox, &["zzzznothing", "group:due"]);

    assert!(stdout(&plain).is_empty());
    assert!(stdout(&grouped).is_empty());
}

#[test]
fn json_has_no_header_row() {
    let sandbox = overdue_copy("header-json");

    let output = search(&sandbox, &["--json"]);

    assert!(!stdout(&output).contains("TITLE"));
    assert!(serde_json::from_str::<Value>(&stdout(&output)).unwrap().is_array());
}

// ---- the header colour ----

#[test]
fn the_header_is_bold_cyan_in_both_plain_and_grouped_output_when_colour_is_on() {
    let sandbox = overdue_copy("header-colour");

    for args in [&["--color", "always"][..], &["group:due", "--color=always"], &["sort:title", "limit:2", "--color", "always"]] {
        let text = stdout(&search(&sandbox, args));
        let first = text.lines().next().unwrap();
        assert!(first.starts_with(HEADER) && first.ends_with(RESET), "{args:?}: {first:?}");
        assert!(first.contains("TITLE") && first.contains("LABELS"), "{args:?}: {first:?}");
        assert_eq!(text.matches(HEADER).count(), 1, "{args:?}: once, not under each group");
        assert!(!text.lines().skip(1).any(|l| l.contains(HEADER)), "{args:?}");
    }
}

#[test]
fn the_header_is_plain_without_colour_so_pipes_and_files_get_clean_text() {
    let sandbox = overdue_copy("header-plain");
    let run = |args: &[&str]| sandbox.run_with_env(None, None, args, None, &[("NO_COLOR", "1")]);

    for output in [search(&sandbox, &[]), search(&sandbox, &["--color", "never"]), run(&["search"]), search(&sandbox, &["group:due"])] {
        let text = stdout(&output);
        assert!(text.starts_with("TITLE"), "{text:?}");
        assert!(!text.contains('\x1b'), "{text:?}");
    }
}

#[test]
fn the_header_colour_and_the_overdue_colour_are_different() {
    let sandbox = overdue_copy("header-vs-overdue");

    let text = stdout(&search(&sandbox, &["--color", "always"]));

    assert_ne!(HEADER, RED);
    let header = text.lines().next().unwrap();
    let overdue = text.lines().find(|l| l.starts_with("Overdue task")).unwrap();
    assert!(header.contains(HEADER) && !header.contains(RED));
    assert!(overdue.contains(RED) && !overdue.contains(HEADER));
}

#[test]
fn no_results_print_no_coloured_header_either() {
    let sandbox = overdue_copy("header-colour-none");

    let output = search(&sandbox, &["zzzznothing", "--color", "always"]);

    assert!(stdout(&output).is_empty(), "{:?}", stdout(&output));
}

#[test]
fn json_has_no_colour_even_for_the_header_case() {
    let sandbox = overdue_copy("header-colour-json");

    let output = search(&sandbox, &["--json", "--color", "always"]);

    assert!(!stdout(&output).contains('\x1b'));
}

// ---- the list in yellow ----

/// A copy whose tasks are in lists: an overdue one in "Work", a future one in "Home",
/// one with no list, and one with a long list name.
fn listed_copy(name: &str) -> Sandbox {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new(name);
    api.queue_page_with(
        vec![
            json!({"id": "w", "title": "Work task", "list_id": "list-work", "timeless_due_at": "2020-01-01",
                   "label_ids": ["label-low"], "updated_at": "2026-03-04T00:00:00Z"}),
            json!({"id": "h", "title": "Home task", "list_id": "list-home", "timeless_due_at": "2099-01-01",
                   "updated_at": "2026-03-03T00:00:00Z"}),
            json!({"id": "n", "title": "No list task", "updated_at": "2026-03-02T00:00:00Z"}),
            json!({"id": "l", "title": "Long list task", "list_id": "list-long", "updated_at": "2026-03-01T00:00:00Z"}),
        ],
        json!([
            {"id": "list-work", "name": "Work"},
            {"id": "list-home", "name": "Home"},
            {"id": "list-long", "name": "A very long list name"},
        ]),
        search_labels(),
    );
    let output = sandbox.run(&api, &["refresh"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    sandbox
}

#[test]
fn the_list_is_yellow_with_color_always_and_only_the_list() {
    let sandbox = listed_copy("yellow-list");

    let text = stdout(&search(&sandbox, &["--color", "always"]));

    let work = text.lines().find(|l| l.starts_with("Work task")).unwrap();
    assert!(work.contains(&format!("{YELLOW}Work{RESET}")), "{work:?}");
    assert!(work.contains(&format!("{RED}2020-01-01{RESET}")), "the overdue date is still red: {work:?}");
    assert!(work.starts_with("Work task") && !work.starts_with('\x1b'), "the title is plain");
    let home = text.lines().find(|l| l.starts_with("Home task")).unwrap();
    assert!(home.contains(&format!("{YELLOW}Home{RESET}")) && !home.contains(RED), "{home:?}");
    let no_list = text.lines().find(|l| l.starts_with("No list task")).unwrap();
    assert!(!no_list.contains('\x1b'), "no list, nothing coloured: {no_list:?}");
    let first = text.lines().next().unwrap();
    assert!(first.starts_with(HEADER) && !first.contains(YELLOW), "the header stays cyan: {first:?}");
}

#[test]
fn a_long_list_name_is_cut_and_then_coloured() {
    let sandbox = listed_copy("yellow-long-list");

    let text = stdout(&search(&sandbox, &["--color", "always"]));

    let long = text.lines().find(|l| l.starts_with("Long list task")).unwrap();
    assert!(long.contains(&format!("{YELLOW}A very long…{RESET}")), "{long:?}");
}

#[test]
fn the_list_is_not_yellow_without_colour() {
    let sandbox = listed_copy("yellow-off");
    let no_color = sandbox.run_with_env(None, None, &["search"], None, &[("NO_COLOR", "1")]);

    for output in [search(&sandbox, &[]), search(&sandbox, &["--color", "never"]), no_color, search(&sandbox, &["group:list"])] {
        assert!(!stdout(&output).contains('\x1b'), "{:?}", stdout(&output));
        assert!(stdout(&output).contains("Work"));
    }
}

#[test]
fn colour_only_adds_codes_so_the_text_is_the_same_with_the_list_coloured() {
    let sandbox = listed_copy("yellow-same-text");
    let strip = |text: String| text.replace(RED, "").replace(YELLOW, "").replace(HEADER, "").replace(RESET, "");

    for args in [&[][..], &["group:list"], &["group:due", "sort:title"]] {
        let mut coloured_args = args.to_vec();
        coloured_args.extend(["--color", "always"]);
        let mut plain_args = args.to_vec();
        plain_args.extend(["--color", "never"]);
        assert_eq!(strip(stdout(&search(&sandbox, &coloured_args))), stdout(&search(&sandbox, &plain_args)), "{args:?}");
    }
}

#[test]
fn grouped_by_list_the_headings_are_plain_and_each_row_has_a_yellow_list() {
    let sandbox = listed_copy("yellow-grouped");

    let text = stdout(&search(&sandbox, &["group:list", "--color", "always"]));

    let headings: Vec<&str> = text.lines().filter(|l| l.ends_with(')') && !l.starts_with("Task")).collect();
    assert!(headings.len() >= 3, "{text}");
    assert!(headings.iter().all(|h| !h.contains('\x1b')), "{headings:?}");
    assert_eq!(text.matches(YELLOW).count(), 3, "one per task that is in a list: {text:?}");
}

#[test]
fn json_has_no_yellow_either() {
    let sandbox = listed_copy("yellow-json");

    let output = search(&sandbox, &["--json", "--color", "always"]);

    assert!(!stdout(&output).contains('\x1b'));
}

#[test]
fn search_help_says_the_list_is_yellow() {
    let sandbox = Sandbox::new("yellow-help");

    let text = stdout(&search(&sandbox, &["--help"]));

    assert!(text.contains("yellow"), "{text}");
}
