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
        ("POST", "/tasks") => (post_status, "{}".to_string()),
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_godspeed-cli"));
        command
            .args(args)
            .env_clear()
            .env("HOME", &self.data_home)
            .env("XDG_DATA_HOME", &self.data_home)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(url) = api_url {
            command.env("GODSPEED_API_URL", url);
        }
        if let Some(token) = token {
            command.env("GODSPEED_TOKEN", token);
        }
        let mut child = command.spawn().unwrap();
        if let Some(text) = stdin {
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
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

    sandbox.run(&api, &["Pay rent @work .urgent .writing ~1.5h :20261101 n: ring the landlord"]);

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

    sandbox.run(&api, &["Buy", "milk", "@home", "~10m"]);

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

    sandbox.run(&api, &["Ship it :tomorrow"]);
    sandbox.run(&api, &["Ship it later :today"]);
    sandbox.run(&api, &["Ship it eventually :next-week"]);

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

    sandbox.run(&api, &["Workout :45"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Workout");
    assert_eq!(task["duration_minutes"], 45);
    assert!(task.get("timeless_due_at").is_none());
}

#[test]
fn words_that_only_resemble_syntax_stay_in_the_title() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("lookalikes");

    sandbox.run(&api, &["Learn C++ and ~roughly everything :someday"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Learn C++ and ~roughly everything :someday");
    assert!(task.get("duration_minutes").is_none());
    assert!(task.get("timeless_due_at").is_none());
}

#[test]
fn notes_are_not_scanned_for_syntax() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("notes");

    sandbox.run(&api, &["Call bank n: ask about ~5m :today @home .urgent"]);

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

    sandbox.run_stdin(&api, &[], "Weekly review @work ~30m n: look at the board\n");

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

    sandbox.run(&api, &["--", "--version", "bump", "~15m"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "--version bump");
    assert_eq!(task["duration_minutes"], 15);
}

#[test]
fn list_names_match_by_prefix_and_by_substring() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("list-matching");

    sandbox.run(&api, &["One @WORK"]);
    sandbox.run(&api, &["Two @workproj"]);
    sandbox.run(&api, &["Three @ojects"]);
    sandbox.run(&api, &["Four @work-projects"]);

    for task in api.posted_tasks() {
        assert_eq!(task["list_id"], "list-work", "{task}");
    }
}

#[test]
fn an_unknown_list_or_label_does_not_stop_the_task_being_created() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("unknown-names");

    sandbox.run(&api, &["Lost task @nowhere .nothing"]);

    let task = &api.posted_tasks()[0];
    assert_eq!(task["title"], "Lost task");
    assert!(task.get("list_id").is_none());
    assert!(task.get("label_ids").is_none());
}

#[test]
fn lists_and_labels_are_fetched_once_and_then_read_from_the_cache() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("name-cache");

    sandbox.run(&api, &["First @work .urgent"]);
    sandbox.run(&api, &["Second @home .writing"]);
    sandbox.run(&api, &["Third @work .urgent"]);

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
    let output = sandbox.run(&down, &["Buy milk @home ~10m :tomorrow n: oat, not dairy"]);
    assert!(stderr(&output).contains("Failed to send task"), "{}", stderr(&output));

    let queued = sandbox.queued_tasks();
    assert_eq!(queued.len(), 1);
    assert!(!queued[0].contains(":tomorrow"), "{}", queued[0]);
    assert!(queued[0].starts_with("Buy milk @home ~10m :20"), "{}", queued[0]);
    assert!(queued[0].ends_with("n: oat, not dairy"), "{}", queued[0]);

    let up = MockApi::start(200);
    sandbox.run(&up, &["Second task"]);

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

    sandbox.run(&down, &["First"]);
    sandbox.run(&down, &["Second"]);

    assert_eq!(sandbox.queued_tasks(), vec!["First", "Second"]);
}

#[test]
fn an_unreachable_api_queues_the_task() {
    let sandbox = Sandbox::new("unreachable");
    // Nothing listens on port 1.
    let output = sandbox.run_with(Some("http://127.0.0.1:1"), Some("test-token"), &["Offline task :today"], None);

    assert!(stderr(&output).contains("Failed to send task"), "{}", stderr(&output));
    assert_eq!(sandbox.queued_tasks().len(), 1);
}

#[test]
fn nothing_is_sent_without_a_token() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("no-token");

    let output = sandbox.run_with(Some(&api.url), None, &["Task without a token"], None);

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

    let output = sandbox.run(&api, &["Confused task @work @home"]);

    assert!(stderr(&output).contains("Multiple lists"), "{}", stderr(&output));
    assert_eq!(code(&output), 2);
    assert!(api.requests().is_empty());
    assert!(sandbox.queued_tasks().is_empty(), "{}", sandbox.cache_contents());
}

#[test]
fn a_task_with_no_title_is_rejected_without_contacting_the_api_or_queueing() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("no-title");

    let output = sandbox.run(&api, &["@work .urgent ~10m :tomorrow n: only notes"]);

    assert!(stderr(&output).contains("no title"), "{}", stderr(&output));
    assert_eq!(code(&output), 2);
    assert!(api.requests().is_empty());
    assert!(sandbox.queued_tasks().is_empty());
}

#[test]
fn at_signs_in_the_notes_do_not_count_as_lists() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("at-in-notes");

    let output = sandbox.run(&api, &["Call bank @home n: ask @bob and @alice about fees"]);

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

    let sent = sandbox.run(&up, &["Fine task"]);
    let rejected_by_api = sandbox.run(&down, &["Another task"]);
    let unreachable = sandbox.run_with(Some("http://127.0.0.1:1"), Some("t"), &["Third task"], None);
    let no_token = sandbox.run_with(Some(&up.url), None, &["Fourth task"], None);
    let invalid = sandbox.run(&up, &["@work"]);
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

    let output = sandbox.run_stdin(&api, &[], "");

    assert_eq!(code(&output), 0);
    assert!(api.requests().is_empty());
}

#[test]
fn queued_tasks_that_can_never_be_sent_are_dropped_and_shown() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("stuck-queue");
    sandbox.write_queue(&["Stuck @work @home", "Valid queued task @home", "@work .urgent"]);

    let output = sandbox.run(&api, &["New task"]);

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

    sandbox.run(&down, &["Another"]);

    assert_eq!(sandbox.queued_tasks(), vec!["Waiting task", "Another"]);
}

#[test]
fn add_is_an_explicit_spelling_of_the_default_command() {
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
fn a_task_starting_with_a_command_word_is_created_with_add_double_dash_or_quotes() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("command-word-tasks");

    sandbox.run(&api, &["add", "search", "the", "attic"]);
    sandbox.run(&api, &["--", "refresh", "the", "roof"]);
    sandbox.run(&api, &["search the garage"]);
    sandbox.run(&api, &["add", "--", "--version", "bump"]);
    sandbox.run(&api, &["buy", "add", "milk"]);

    let titles: Vec<String> = api.posted_tasks().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect();
    assert_eq!(
        titles,
        vec!["search the attic", "refresh the roof", "search the garage", "--version bump", "buy add milk"]
    );
}

#[test]
fn an_unquoted_command_word_runs_the_command_and_creates_no_task() {
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

    sandbox.run_with(Some(&api.url), None, &["Task using the file token"], None);

    assert_eq!(authorization_of_post(&api).as_deref(), Some("Bearer file-token"));
}

#[test]
fn the_environment_token_wins_over_the_token_file() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("token-env-wins");
    sandbox.write_token_file("export GODSPEED_TOKEN=\"file-token\"\n");

    sandbox.run_with(Some(&api.url), Some("env-token"), &["Task"], None);

    assert_eq!(authorization_of_post(&api).as_deref(), Some("Bearer env-token"));
}

#[test]
fn an_empty_environment_token_falls_back_to_the_token_file() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("token-env-empty");
    sandbox.write_token_file("GODSPEED_TOKEN=file-token\n");

    sandbox.run_with(Some(&api.url), Some("  "), &["Task"], None);

    assert_eq!(authorization_of_post(&api).as_deref(), Some("Bearer file-token"));
}

#[test]
fn a_token_file_without_a_token_line_is_the_same_as_no_token() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("token-file-empty");
    sandbox.write_token_file("# nothing here\nOTHER=x\n");

    let output = sandbox.run_with(Some(&api.url), None, &["Task"], None);

    assert!(stderr(&output).contains("environment/Godspeed"), "{}", stderr(&output));
    assert!(api.requests().is_empty());
}

#[test]
fn the_token_never_appears_in_the_output() {
    let sandbox = Sandbox::new("token-secret");
    sandbox.write_token_file("export GODSPEED_TOKEN=\"super-secret-file-token\"\n");
    let down = MockApi::start(500);

    let from_file = sandbox.run_with(Some(&down.url), None, &["Failing task"], None);
    let from_env = sandbox.run_with(Some(&down.url), Some("super-secret-env-token"), &["Failing task"], None);

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
fn a_new_refresh_replaces_the_previous_copy() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("refresh-replaces");
    api.queue_tasks_page(tasks_page(0..5));
    sandbox.run(&api, &["refresh"]);
    api.queue_tasks_page(tasks_page(10..12));

    let output = sandbox.run(&api, &["refresh"]);

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

    let output = sandbox.run(&api, &["refresh"]);

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
    let grouped = search(&sandbox, &["group:list:Home", "sort:title"]);

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
        ("enable:nested", "nested isn't supported"),
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
    for needle in ["MATCHING:", "DATES", "has:due-time", "group:FIELD", "EXIT STATUS"] {
        assert!(text.contains(needle), "{needle}");
    }
}

#[test]
fn a_task_can_still_be_created_whose_text_starts_with_search() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("create-search-task");

    sandbox.run(&api, &["add", "search", "the", "attic"]);
    sandbox.run(&api, &["search the garage"]);

    let titles: Vec<String> = api.posted_tasks().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect();
    assert_eq!(titles, vec!["search the attic", "search the garage"]);
}
