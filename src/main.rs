use chrono::{Duration, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::Command;

#[derive(Serialize, Deserialize, Debug)]
struct TaskRequest {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    list_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_minutes: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeless_due_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    label_ids: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    notes: String,
}

#[derive(Deserialize, Debug)]
struct ListsResponse {
    lists: Vec<ListItem>,
}

#[derive(Deserialize, Debug)]
struct ListItem {
    id: String,
    name: String,
}

#[derive(Deserialize, Debug)]
struct LabelsResponse {
    labels: Vec<LabelItem>,
}

#[derive(Deserialize, Debug)]
struct LabelItem {
    id: String,
    name: String,
}

fn get_xdg_data_home() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = env::var("HOME") {
        PathBuf::from(home).join(".local").join("share")
    } else {
        PathBuf::from(".local").join("share")
    }
}

/// The API endpoint for `path`. `GODSPEED_API_URL` replaces the base URL so the tests can point
/// the binary at a local mock server; the API token is sent to whatever it names.
fn api_url(path: &str) -> String {
    let base = env::var("GODSPEED_API_URL").unwrap_or_else(|_| "https://api.godspeedapp.com".to_string());
    format!("{}/{path}", base.trim_end_matches('/'))
}

fn get_cache_path() -> PathBuf {
    get_xdg_data_home().join("godspeed-cli").join("cache")
}

fn get_lists_path() -> PathBuf {
    get_xdg_data_home().join("godspeed-cli").join("lists.toml")
}

fn get_labels_path() -> PathBuf {
    get_xdg_data_home().join("godspeed-cli").join("labels.toml")
}

/// Where the token is read from when `GODSPEED_TOKEN` isn't set in the environment.
fn token_file_path() -> PathBuf {
    get_xdg_data_home().join("environment").join("Godspeed")
}

/// The API token: `GODSPEED_TOKEN` if it is set and non-empty, otherwise the
/// `GODSPEED_TOKEN=...` line of the token file. The token is never printed.
fn load_token() -> Option<String> {
    if let Ok(token) = env::var("GODSPEED_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }

    let path = token_file_path();
    match fs::read_to_string(&path) {
        Ok(text) => parse_token_assignment(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("Warning: cannot read {}: {}", path.display(), e);
            None
        }
    }
}

/// Finds `GODSPEED_TOKEN=value` in shell-style text, allowing `export`, comments, other
/// variables and single or double quotes around the value.
fn parse_token_assignment(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != "GODSPEED_TOKEN" {
            continue;
        }
        let value = value.trim();
        let value = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q).and_then(|v| v.strip_suffix(*q)))
            .unwrap_or(value)
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn ensure_directories() -> io::Result<()> {
    let data_dir = get_xdg_data_home().join("godspeed-cli");
    fs::create_dir_all(&data_dir)?;
    Ok(())
}

fn send_notification(message: &str) {
    let script = format!(
        r#"display notification "{}" with title "Godspeed CLI""#,
        message.replace('"', "\\\"")
    );
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output();
}

/// Parses a `~30m` / `~1.5h` / `~45` duration word into whole minutes. Anything that isn't
/// exactly `~` plus a positive number and an optional `m` or `h` is not a duration, so
/// words like "~roughly" stay in the title.
fn parse_duration_token(word: &str) -> Option<i32> {
    let rest = word.strip_prefix('~')?;
    let (number, per_unit) = if let Some(n) = rest.strip_suffix('h') {
        (n, 60.0)
    } else if let Some(n) = rest.strip_suffix('m') {
        (n, 1.0)
    } else {
        (rest, 1.0)
    };
    let well_formed = number.chars().all(|c| c.is_ascii_digit() || c == '.')
        && number.chars().any(|c| c.is_ascii_digit())
        && number.matches('.').count() <= 1;
    if !well_formed {
        return None;
    }
    let minutes = (number.parse::<f64>().ok()? * per_unit).round();
    if minutes >= 1.0 && minutes <= i32::MAX as f64 {
        Some(minutes as i32)
    } else {
        None
    }
}

/// Parses a `:today` / `:tomorrow` / `:next-week` / `:YYYYMMDD` / `:YYYY-MM-DD` word into a
/// `YYYY-MM-DD` date. Short numbers like `:45` are durations, not dates.
fn parse_due_token(word: &str, today: NaiveDate) -> Option<String> {
    let value = word.strip_prefix(':')?.to_ascii_lowercase();
    let date = match value.as_str() {
        "today" => today,
        "tomorrow" => today + Duration::days(1),
        "next-week" => today + Duration::weeks(1),
        v if v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit()) => {
            NaiveDate::parse_from_str(v, "%Y%m%d").ok()?
        }
        v => NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?,
    };
    Some(date.format("%Y-%m-%d").to_string())
}

/// The original `:45` duration syntax, in minutes. Digit runs of 8 or more are malformed dates
/// (such as `:20261345`), not durations, so they are left in the title.
fn parse_legacy_minutes(word: &str) -> Option<i32> {
    let value = word.trim_start_matches(':');
    if value.len() >= 8 {
        return None;
    }
    value.parse().ok()
}

/// Rewrites relative due dates (`:today`, `:tomorrow`, `:next-week`) to absolute ones, leaving
/// everything else, including the notes and the whitespace, untouched. A task that is queued
/// offline is retried on a later day, and must keep the date it was written for.
fn resolve_relative_dates(input: &str, today: NaiveDate) -> String {
    let (main, notes) = input.split_at(input.find(" n:").unwrap_or(input.len()));
    let mut out = String::with_capacity(input.len());
    for piece in main.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        match parse_due_token(word, today) {
            Some(date) => {
                out.push(':');
                out.push_str(&date);
                out.push_str(&piece[word.len()..]);
            }
            None => out.push_str(piece),
        }
    }
    out.push_str(notes);
    out
}

fn push_title_word(title: &mut String, word: &str) {
    if !title.is_empty() {
        title.push(' ');
    }
    title.push_str(word);
}

fn parse_task(input: &str) -> (TaskRequest, Option<String>, Vec<String>) {
    parse_task_on(input, Local::now().date_naive())
}

fn parse_task_on(input: &str, today: NaiveDate) -> (TaskRequest, Option<String>, Vec<String>) {
    let mut title = String::new();
    let mut list_name: Option<String> = None;
    let mut duration_minutes: Option<i32> = None;
    let mut timeless_due_at: Option<String> = None;
    let mut label_names: Vec<String> = Vec::new();

    // Check for notes separator
    let (main_part, notes_part) = if let Some(pos) = input.find(" n:") {
        let (main, note) = input.split_at(pos);
        (main, note.trim_start_matches(" n:").trim())
    } else {
        (input, "")
    };

    let notes = notes_part.to_string();

    for word in main_part.split_whitespace() {
        if word.starts_with('.') {
            // Extract label
            let label = word.trim_start_matches('.');
            if !label.is_empty() {
                label_names.push(label.to_string());
            }
        } else if word.starts_with('@') {
            // Extract list name
            let list = word.trim_start_matches('@');
            if !list.is_empty() {
                list_name = Some(list.to_string());
            }
        } else if word.starts_with(':') {
            if let Some(date) = parse_due_token(word, today) {
                timeless_due_at = Some(date);
            } else if let Some(duration) = parse_legacy_minutes(word) {
                duration_minutes = Some(duration);
            } else {
                // If parsing fails, include it in the title
                push_title_word(&mut title, word);
            }
        } else if word.starts_with('~') {
            if let Some(duration) = parse_duration_token(word) {
                duration_minutes = Some(duration);
            } else {
                push_title_word(&mut title, word);
            }
        } else {
            push_title_word(&mut title, word);
        }
    }

    let title = title.trim_end().to_string();

    (
        TaskRequest {
            title,
            list_id: None, // Will be resolved later
            duration_minutes,
            timeless_due_at,
            label_ids: Vec::new(), // Will be resolved later
            notes,
        },
        list_name,
        label_names,
    )
}

fn load_cache(path: &PathBuf) -> HashMap<String, String> {
    if let Ok(content) = fs::read_to_string(path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            let mut map = HashMap::new();
            for (key, value) in table {
                if let Some(val_str) = value.as_str() {
                    map.insert(key.to_lowercase(), val_str.to_string());
                }
            }
            return map;
        }
    }
    HashMap::new()
}

fn save_cache(path: &PathBuf, cache: &HashMap<String, String>) -> io::Result<()> {
    let mut table = toml::Table::new();
    for (key, value) in cache {
        table.insert(key.clone(), toml::Value::String(value.clone()));
    }
    let toml_string = toml::to_string(&table).unwrap();
    fs::write(path, toml_string)?;
    Ok(())
}

fn fetch_lists(api_key: &str) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(api_url("lists"))
        .header("Authorization", format!("Bearer {}", api_key))
        .send()?;

    let lists_response: ListsResponse = response.json()?;
    let mut map = HashMap::new();
    for list in lists_response.lists {
        map.insert(list.name.to_lowercase(), list.id);
    }
    Ok(map)
}

fn fetch_labels(api_key: &str) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(api_url("labels"))
        .header("Authorization", format!("Bearer {}", api_key))
        .send()?;

    let labels_response: LabelsResponse = response.json()?;
    let mut map = HashMap::new();
    for label in labels_response.labels {
        map.insert(label.name.to_lowercase(), label.id);
    }
    Ok(map)
}

fn find_matching_key(cache: &HashMap<String, String>, search: &str) -> Option<String> {
    let search_lower = search.to_lowercase();

    // First try exact match
    if let Some(id) = cache.get(&search_lower) {
        return Some(id.clone());
    }

    // Then try prefix match
    best_match(cache, |key| key.starts_with(&search_lower))
}

/// The id of the shortest matching key (ties broken alphabetically), so the result doesn't
/// depend on hash-map ordering.
fn best_match(cache: &HashMap<String, String>, matches: impl Fn(&str) -> bool) -> Option<String> {
    cache
        .iter()
        .filter(|(key, _)| matches(key))
        .min_by_key(|(key, _)| (key.len(), key.as_str()))
        .map(|(_, id)| id.clone())
}

fn squash(value: &str) -> String {
    value.chars().filter(|c| !matches!(c, ' ' | '/' | '-')).collect()
}

/// Looks a list up by exact name, then prefix, then substring, then substring ignoring
/// spaces, slashes and dashes. Labels keep using the stricter `find_matching_key`.
fn find_list_key(cache: &HashMap<String, String>, search: &str) -> Option<String> {
    if let Some(id) = find_matching_key(cache, search) {
        return Some(id);
    }

    let search_lower = search.to_lowercase();
    if let Some(id) = best_match(cache, |key| key.contains(&search_lower)) {
        return Some(id);
    }

    let search_squashed = squash(&search_lower);
    if search_squashed.is_empty() {
        return None;
    }
    best_match(cache, |key| squash(key).contains(&search_squashed))
}

fn send_task(task: &TaskRequest, api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(api_url("tasks"))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(task)
        .send()?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("API error: {}", response.status()).into())
    }
}

fn add_to_cache(task_str: &str) -> io::Result<()> {
    let cache_path = get_cache_path();
    let mut cache_content = fs::read_to_string(&cache_path).unwrap_or_default();
    cache_content.push_str(task_str);
    cache_content.push('\n');
    cache_content.push_str("-*-*-*-\n");
    fs::write(&cache_path, cache_content)?;
    Ok(())
}

fn get_cached_tasks() -> Vec<String> {
    let cache_path = get_cache_path();
    if let Ok(content) = fs::read_to_string(&cache_path) {
        content
            .split("-*-*-*-\n")
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect()
    } else {
        Vec::new()
    }
}

fn remove_from_cache(task_str: &str) -> io::Result<()> {
    let cache_path = get_cache_path();
    let content = fs::read_to_string(&cache_path).unwrap_or_default();
    let remaining: Vec<&str> = content
        .split("-*-*-*-\n")
        .filter(|s| !s.trim().is_empty() && s.trim() != task_str)
        .collect();

    let new_content = if remaining.is_empty() {
        String::new()
    } else {
        remaining.join("-*-*-*-\n") + "\n-*-*-*-\n"
    };

    fs::write(&cache_path, new_content)?;
    Ok(())
}

fn process_task(task_str: &str, api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (mut parsed, list_name, label_names) = parse_task(task_str);

    // Handle list resolution
    if let Some(list_name_clean) = list_name {
        let mut lists_cache = load_cache(&get_lists_path());

        if let Some(list_id) = find_list_key(&lists_cache, &list_name_clean) {
            parsed.list_id = Some(list_id);
        } else {
            // Fetch lists from API
            lists_cache = fetch_lists(api_key)?;
            save_cache(&get_lists_path(), &lists_cache)?;

            if let Some(list_id) = find_list_key(&lists_cache, &list_name_clean) {
                parsed.list_id = Some(list_id);
            }
        }
    }

    // Handle label resolution
    if !label_names.is_empty() {
        let mut labels_cache = load_cache(&get_labels_path());

        // Check if we need to fetch labels
        let mut need_fetch = false;
        for label_name in &label_names {
            if find_matching_key(&labels_cache, label_name).is_none() {
                need_fetch = true;
                break;
            }
        }

        if need_fetch {
            labels_cache = fetch_labels(api_key)?;
            save_cache(&get_labels_path(), &labels_cache)?;
        }

        // Resolve all label names to IDs
        for label_name in label_names {
            if let Some(label_id) = find_matching_key(&labels_cache, &label_name) {
                parsed.label_ids.push(label_id);
            }
        }
    }

    // Check for multiple lists
    let list_count = task_str.split_whitespace().filter(|w| w.starts_with('@')).count();
    if list_count > 1 {
        send_notification("Error: Multiple lists specified");
        return Err("Multiple lists specified".into());
    }

    send_task(&parsed, api_key)?;
    Ok(())
}

const HELP: &str = r#"Create a task in Godspeed from the command line.

USAGE:
    godspeed-cli [--] <TASK>...
    echo "<TASK>" | godspeed-cli
    godspeed-cli --help | --version

OPTIONS:
    -h, --help       Print this help and exit.
    -V, --version    Print the version and exit.
    --               Treat everything after it as the task, for a task whose text starts
                     with -h, --help, -V or --version.

TASK SYNTAX:
    .label       Add a label (single word). Removed from the title.
    @list        Put the task in a list (single word, only one allowed).
    ~duration    Set the duration, e.g. ~30m, ~1.5h or ~45 (minutes). The older :45 also works.
    :date        Set the due date: :today, :tomorrow, :next-week, :20261031 or :2026-10-31.
    n: text      Everything after " n:" becomes the task's notes.

    Quote the task so the shell leaves ~ and @ alone (zsh can expand an unquoted ~).

    Example: godspeed-cli "Write blog post @Work .Writing ~2h :tomorrow n: Include code samples"

ENVIRONMENT:
    GODSPEED_TOKEN    Your Godspeed API key (required to create tasks). If it isn't set,
                      an `export GODSPEED_TOKEN="..."` line in
                      $XDG_DATA_HOME/environment/Godspeed (usually
                      ~/.local/share/environment/Godspeed) is used.

If a task cannot be sent it is cached and retried the next time the CLI runs.
"#;

/// What the command line is asking for. Only the first argument is checked for a flag,
/// so a task like "fix the --version bug" is still a task.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    /// The task text, as separate arguments. Empty means read it from stdin.
    Task(Vec<String>),
}

fn parse_invocation(args: &[String]) -> Invocation {
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => Invocation::Help,
        Some("-V") | Some("--version") => Invocation::Version,
        Some("--") => Invocation::Task(args[1..].to_vec()),
        _ => Invocation::Task(args.to_vec()),
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let task_args = match parse_invocation(&args) {
        Invocation::Help => {
            print!("godspeed-cli {}\n{HELP}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Invocation::Version => {
            println!("godspeed-cli {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Invocation::Task(task_args) => task_args,
    };

    if let Err(e) = ensure_directories() {
        eprintln!("Failed to create directories: {}", e);
        return;
    }

    let api_key = match load_token() {
        Some(key) => key,
        None => {
            send_notification("GODSPEED_TOKEN is not set");
            eprintln!(
                "Error: GODSPEED_TOKEN is not set (environment variable, or an `export GODSPEED_TOKEN=...` line in {})",
                token_file_path().display()
            );
            return;
        }
    };

    // Get input from args or stdin
    let input = {
        if !task_args.is_empty() {
            // Join all arguments with spaces to handle multi-word input
            task_args.join(" ")
        } else {
            // Read from stdin
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer).unwrap_or_default();
            buffer.trim_end().to_string()
        }
    };

    // Process cached tasks first
    let cached_tasks = get_cached_tasks();
    for cached_task in cached_tasks {
        if let Ok(_) = process_task(&cached_task, &api_key) {
            let _ = remove_from_cache(&cached_task);
        }
    }

    // Process current input
    if !input.is_empty() {
        if let Err(e) = process_task(&input, &api_key) {
            eprintln!("Failed to send task: {}", e);
            let _ = add_to_cache(&resolve_relative_dates(&input, Local::now().date_naive()));
            send_notification("Failed to send task");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_and_version_flags_are_recognised_in_both_spellings() {
        assert_eq!(parse_invocation(&args(&["--help"])), Invocation::Help);
        assert_eq!(parse_invocation(&args(&["-h"])), Invocation::Help);
        assert_eq!(parse_invocation(&args(&["--version"])), Invocation::Version);
        assert_eq!(parse_invocation(&args(&["-V"])), Invocation::Version);
    }

    #[test]
    fn an_ordinary_task_is_passed_through() {
        assert_eq!(
            parse_invocation(&args(&["Buy", "milk", "@Home"])),
            Invocation::Task(args(&["Buy", "milk", "@Home"]))
        );
    }

    #[test]
    fn no_arguments_means_read_stdin() {
        assert_eq!(parse_invocation(&args(&[])), Invocation::Task(vec![]));
    }

    #[test]
    fn a_flag_that_is_not_the_first_argument_is_part_of_the_task() {
        assert_eq!(
            parse_invocation(&args(&["fix", "the", "--version", "bug"])),
            Invocation::Task(args(&["fix", "the", "--version", "bug"]))
        );
    }

    #[test]
    fn double_dash_makes_everything_after_it_the_task() {
        assert_eq!(
            parse_invocation(&args(&["--", "--version", "bump"])),
            Invocation::Task(args(&["--version", "bump"]))
        );
        assert_eq!(parse_invocation(&args(&["--"])), Invocation::Task(vec![]));
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()
    }

    fn parsed(input: &str) -> TaskRequest {
        parse_task_on(input, today()).0
    }

    fn lists(names: &[(&str, &str)]) -> HashMap<String, String> {
        names.iter().map(|(n, id)| (n.to_string(), id.to_string())).collect()
    }

    #[test]
    fn tilde_durations_accept_minutes_hours_and_fractions() {
        assert_eq!(parsed("Run ~30m").duration_minutes, Some(30));
        assert_eq!(parsed("Run ~90").duration_minutes, Some(90));
        assert_eq!(parsed("Run ~2h").duration_minutes, Some(120));
        assert_eq!(parsed("Run ~1.5h").duration_minutes, Some(90));
        assert_eq!(parsed("Run ~0.5h").duration_minutes, Some(30));
        assert_eq!(parsed("Run ~30m").title, "Run");
    }

    #[test]
    fn words_that_only_look_like_durations_stay_in_the_title() {
        for word in ["~roughly", "~", "~0", "~0m", "~1.2.3", "~m", "~-5", "~30x"] {
            let task = parsed(&format!("Fix {word} thing"));
            assert_eq!(task.duration_minutes, None, "{word}");
            assert_eq!(task.title, format!("Fix {word} thing"), "{word}");
        }
    }

    #[test]
    fn due_dates_accept_keywords_and_both_numeric_forms() {
        assert_eq!(parsed("A :today").timeless_due_at.as_deref(), Some("2026-10-15"));
        assert_eq!(parsed("A :tomorrow").timeless_due_at.as_deref(), Some("2026-10-16"));
        assert_eq!(parsed("A :Next-Week").timeless_due_at.as_deref(), Some("2026-10-22"));
        assert_eq!(parsed("A :20261031").timeless_due_at.as_deref(), Some("2026-10-31"));
        assert_eq!(parsed("A :2026-10-31").timeless_due_at.as_deref(), Some("2026-10-31"));
        assert_eq!(parsed("A :tomorrow").title, "A");
        assert_eq!(parsed("A :tomorrow").duration_minutes, None);
    }

    #[test]
    fn colon_followed_by_a_short_number_is_still_minutes() {
        let task = parsed("Workout :45");
        assert_eq!(task.duration_minutes, Some(45));
        assert_eq!(task.timeless_due_at, None);
        assert_eq!(task.title, "Workout");
    }

    #[test]
    fn malformed_dates_stay_in_the_title() {
        for word in [":20261345", ":2026-13-01", ":someday", ":"] {
            let task = parsed(&format!("Do {word}"));
            assert_eq!(task.timeless_due_at, None, "{word}");
            assert_eq!(task.duration_minutes, None, "{word}");
            assert_eq!(task.title, format!("Do {word}"), "{word}");
        }
    }

    #[test]
    fn all_syntax_combines_and_notes_are_not_parsed() {
        let (task, list, labels) =
            parse_task_on("Write post @Work .Writing ~2h :tomorrow n: call ~5m :today @Home", today());
        assert_eq!(task.title, "Write post");
        assert_eq!(list.as_deref(), Some("Work"));
        assert_eq!(labels, vec!["Writing"]);
        assert_eq!(task.duration_minutes, Some(120));
        assert_eq!(task.timeless_due_at.as_deref(), Some("2026-10-16"));
        assert_eq!(task.notes, "call ~5m :today @Home");
    }

    #[test]
    fn the_request_serialises_the_due_date_and_label_ids() {
        let mut task = parsed("Pay rent :20261101");
        task.label_ids.push("label-1".to_string());
        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["timeless_due_at"], "2026-11-01");
        assert_eq!(json["label_ids"], serde_json::json!(["label-1"]));
        assert!(json.get("label_names").is_none());
        assert!(serde_json::to_value(parsed("Plain")).unwrap().get("timeless_due_at").is_none());
    }

    #[test]
    fn relative_dates_are_made_absolute_before_a_task_is_queued() {
        assert_eq!(
            resolve_relative_dates("Buy milk :tomorrow @Home ~10m", today()),
            "Buy milk :2026-10-16 @Home ~10m"
        );
        assert_eq!(
            resolve_relative_dates("Line one\nPlan :today  .Work\n:next-week", today()),
            "Line one\nPlan :2026-10-15  .Work\n:2026-10-22"
        );
        // Notes and tasks without relative dates are untouched.
        assert_eq!(
            resolve_relative_dates("Call n: ask about :tomorrow", today()),
            "Call n: ask about :tomorrow"
        );
        assert_eq!(resolve_relative_dates("Do :45 later", today()), "Do :45 later");
    }

    #[test]
    fn list_lookup_prefers_exact_then_prefix_then_substring() {
        let cache = lists(&[("work", "1"), ("work projects", "2"), ("homework", "3")]);
        assert_eq!(find_list_key(&cache, "Work").as_deref(), Some("1"));
        assert_eq!(find_list_key(&cache, "work p").as_deref(), Some("2"));
        assert_eq!(find_list_key(&cache, "wor").as_deref(), Some("1"));
        assert_eq!(find_list_key(&cache, "mew").as_deref(), Some("3"));
        assert_eq!(find_list_key(&cache, "nothing"), None);
    }

    #[test]
    fn list_lookup_ignores_spaces_slashes_and_dashes() {
        let cache = lists(&[("side projects", "1"), ("home/garden", "2")]);
        assert_eq!(find_list_key(&cache, "sideproj").as_deref(), Some("1"));
        assert_eq!(find_list_key(&cache, "homegarden").as_deref(), Some("2"));
        assert_eq!(find_list_key(&cache, "-"), None);
    }

    #[test]
    fn ambiguous_prefixes_resolve_deterministically_to_the_shortest_name() {
        let cache = lists(&[("work later", "1"), ("work now", "2"), ("work", "3x"), ("workshop", "4")]);
        assert_eq!(find_matching_key(&cache, "wor").as_deref(), Some("3x"));
        let cache = lists(&[("work later", "1"), ("work now", "2")]);
        assert_eq!(find_matching_key(&cache, "wor").as_deref(), Some("2"));
    }

    #[test]
    fn labels_keep_the_stricter_matching() {
        let cache = lists(&[("homework", "1")]);
        assert_eq!(find_matching_key(&cache, "work"), None);
        assert_eq!(find_list_key(&cache, "work").as_deref(), Some("1"));
    }

    #[test]
    fn the_token_is_found_in_shell_style_text() {
        let token = |text: &str| parse_token_assignment(text);
        assert_eq!(token("export GODSPEED_TOKEN=\"abc123\"\n").as_deref(), Some("abc123"));
        assert_eq!(token("GODSPEED_TOKEN=abc123").as_deref(), Some("abc123"));
        assert_eq!(token("export GODSPEED_TOKEN='abc123'").as_deref(), Some("abc123"));
        assert_eq!(token("  export   GODSPEED_TOKEN = \"abc123\"  ").as_deref(), Some("abc123"));
        assert_eq!(
            token("# comment\nOTHER=x\n\nexport GODSPEED_TOKEN=\"abc123\"\nexport LATER=y").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn text_without_a_usable_token_assignment_gives_none() {
        assert_eq!(parse_token_assignment(""), None);
        assert_eq!(parse_token_assignment("# GODSPEED_TOKEN=commented-out"), None);
        assert_eq!(parse_token_assignment("OTHER_GODSPEED_TOKEN=x\nGODSPEED_TOKENS=y"), None);
        assert_eq!(parse_token_assignment("GODSPEED_TOKEN="), None);
        assert_eq!(parse_token_assignment("GODSPEED_TOKEN=\"\""), None);
        assert_eq!(parse_token_assignment("just some text"), None);
    }

    #[test]
    fn the_first_non_empty_assignment_wins() {
        assert_eq!(
            parse_token_assignment("GODSPEED_TOKEN=\nGODSPEED_TOKEN=second\nGODSPEED_TOKEN=third").as_deref(),
            Some("second")
        );
    }

    #[test]
    fn the_help_text_covers_the_options_and_the_task_syntax() {
        for text in ["--help", "--version", "--", "GODSPEED_TOKEN", ".label", "@list", "~duration", ":date", "n: text"] {
            assert!(HELP.contains(text), "{text} is not in the help");
        }
    }
}
