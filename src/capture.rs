//! Creating a task: parse it, resolve its list and labels, send it, and queue it if that fails.

use crate::names::{find_list_key, find_matching_key, NameMap};
use crate::notify::send_notification;
use crate::task::{parse_task, resolve_relative_dates, ParsedTask, TaskRequest};
use crate::{api, exit, names, paths, queue, token};
use chrono::Local;
use std::fmt;
use std::io::{self, Read};
use std::process::ExitCode;

pub enum TaskError {
    /// The task itself is wrong, so sending it again can never work and it must not be queued.
    Invalid(String),
    /// The task is fine but couldn't be sent, so it is queued and retried.
    Failed(Box<dyn std::error::Error>),
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Invalid(message) => f.write_str(message),
            TaskError::Failed(error) => write!(f, "{error}"),
        }
    }
}

fn resolve_names(
    request: &mut TaskRequest,
    list_names: &[String],
    label_names: &[String],
    api_key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(list_name) = list_names.first() {
        let mut lists_cache = names::load(&paths::lists_path());

        if let Some(list_id) = find_list_key(&lists_cache, list_name) {
            request.list_id = Some(list_id);
        } else {
            // Fetch lists from API
            lists_cache = api::fetch_lists(api_key)?;
            names::save(&paths::lists_path(), &lists_cache)?;
            request.list_id = find_list_key(&lists_cache, list_name);
        }
    }

    if !label_names.is_empty() {
        let mut labels_cache: NameMap = names::load(&paths::labels_path());

        let need_fetch = label_names
            .iter()
            .any(|label_name| find_matching_key(&labels_cache, label_name).is_none());
        if need_fetch {
            labels_cache = api::fetch_labels(api_key)?;
            names::save(&paths::labels_path(), &labels_cache)?;
        }

        for label_name in label_names {
            if let Some(label_id) = find_matching_key(&labels_cache, label_name) {
                request.label_ids.push(label_id);
            }
        }
    }

    Ok(())
}

pub fn process_task(task_str: &str, api_key: &str) -> Result<(), TaskError> {
    let ParsedTask { mut request, list_names, label_names } = parse_task(task_str);

    if list_names.len() > 1 {
        let listed: Vec<String> = list_names.iter().map(|name| format!("@{name}")).collect();
        return Err(TaskError::Invalid(format!(
            "Multiple lists specified ({}); a task can be in only one list",
            listed.join(", ")
        )));
    }
    if request.title.is_empty() {
        return Err(TaskError::Invalid("The task has no title".to_string()));
    }

    resolve_names(&mut request, &list_names, &label_names, api_key).map_err(TaskError::Failed)?;
    api::send_task(&request, api_key).map_err(TaskError::Failed)
}

/// Sends the tasks queued by earlier failures. A task that is invalid can never be sent, so it
/// is dropped (and printed, so the text isn't lost) instead of being retried on every run.
fn retry_queued_tasks(api_key: &str) {
    for queued in queue::tasks() {
        match process_task(&queued, api_key) {
            Ok(()) => {
                let _ = queue::remove(&queued);
            }
            Err(TaskError::Invalid(reason)) => {
                eprintln!("Dropping a queued task that can never be sent ({reason}): {queued}");
                let _ = queue::remove(&queued);
            }
            Err(TaskError::Failed(_)) => {}
        }
    }
}

fn read_input(task_args: &[String]) -> String {
    if !task_args.is_empty() {
        // Join all arguments with spaces to handle multi-word input
        return task_args.join(" ");
    }
    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer).unwrap_or_default();
    buffer.trim_end().to_string()
}

/// `godspeed-cli [add] <TASK>...`, or the task on stdin.
pub fn run_add(task_args: &[String]) -> ExitCode {
    if let Err(e) = paths::ensure_data_dir() {
        eprintln!("Failed to create directories: {}", e);
        return exit::failure();
    }

    let Some(api_key) = token::load_token() else {
        send_notification("GODSPEED_TOKEN is not set");
        eprintln!(
            "Error: GODSPEED_TOKEN is not set (environment variable, or an `export GODSPEED_TOKEN=...` line in {})",
            paths::token_file_path().display()
        );
        return exit::usage();
    };

    let input = read_input(task_args);

    retry_queued_tasks(&api_key);

    if input.is_empty() {
        return exit::SUCCESS;
    }

    match process_task(&input, &api_key) {
        Ok(()) => exit::SUCCESS,
        Err(TaskError::Invalid(reason)) => {
            eprintln!("Error: {reason}");
            send_notification(&format!("Error: {reason}"));
            exit::usage()
        }
        Err(TaskError::Failed(e)) => {
            eprintln!("Failed to send task: {}", e);
            if let Err(queue_error) = queue::add(&resolve_relative_dates(&input, Local::now().date_naive())) {
                eprintln!("Could not save the task to the retry queue: {queue_error}");
            }
            send_notification("Failed to send task");
            exit::failure()
        }
    }
}
