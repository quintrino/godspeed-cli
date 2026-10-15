//! End-to-end tests: the real binary talks to a mock Godspeed API on localhost.
//!
//! Each run gets its own `XDG_DATA_HOME` and an explicit environment, so the tests don't
//! touch the user's cache or token and can run in parallel.

use chrono::{Duration, Local, NaiveDate};
use serde_json::{json, Value};
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
    authorization: Option<String>,
    body: String,
}

struct MockApi {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockApi {
    /// `post_status` is the status returned for `POST /tasks`.
    fn start(post_status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = Arc::clone(&recorded);
                thread::spawn(move || serve(stream, &recorded, post_status));
            }
        });
        MockApi { url, requests }
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

fn serve(stream: TcpStream, recorded: &Mutex<Vec<Recorded>>, post_status: u16) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

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

    let (status, payload) = match (method.as_str(), path.as_str()) {
        ("GET", "/lists") => (200, LISTS),
        ("GET", "/labels") => (200, LABELS),
        ("POST", "/tasks") => (post_status, "{}"),
        _ => (404, "{}"),
    };
    recorded.lock().unwrap().push(Recorded { method, path, authorization, body });

    let response = format!(
        "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
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

#[test]
fn more_than_one_list_is_rejected_without_creating_a_task() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("two-lists");

    let output = sandbox.run(&api, &["Confused task @work @home"]);

    assert!(stderr(&output).contains("Multiple lists"), "{}", stderr(&output));
    assert_eq!(api.count("POST", "/tasks"), 0);
}
