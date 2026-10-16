//! What is waiting in the retry queue: tasks that `add` couldn't send and changes that `edit`
//! couldn't send. Both commands send what is waiting before they do their own work, so older
//! things go first.

use crate::capture::{process_task_with, split_entry, TaskError};
use crate::edit::PendingEdit;
use crate::queue;
use std::collections::HashSet;

/// Tries everything in the queue once, in the order it was queued. What is sent is removed.
/// Something that can never be sent (a task that is invalid, a change Godspeed refuses, such as
/// for a task that no longer exists) is dropped, and printed so it isn't lost. Anything else
/// stays for next time.
///
/// If a change to a task fails, later changes to the same task wait too, so they can't be
/// applied out of order.
pub fn process(api_key: &str) {
    let entries = queue::tasks();
    debug!("retry queue: {} waiting", entries.len());
    let mut blocked: HashSet<String> = HashSet::new();
    for entry in entries {
        match PendingEdit::parse(&entry) {
            Some(edit) => {
                if blocked.contains(&edit.id) {
                    debug!("not retrying a change to {}: an earlier one hasn't gone through", edit.id);
                    continue;
                }
                match edit.send_queued(api_key) {
                    Ok(()) => {
                        debug!("sent the queued change: {}", edit.describe());
                        let _ = queue::remove(&entry);
                    }
                    Err(e) if e.is_retryable() => {
                        debug!("the queued change is still waiting ({e}): {}", edit.describe());
                        blocked.insert(edit.id.clone());
                    }
                    Err(e) => {
                        eprintln!("Dropping a queued change that can never be sent ({e}): {}", edit.describe());
                        let _ = queue::remove(&entry);
                    }
                }
            }
            None => {
                let (text, options) = split_entry(&entry);
                match process_task_with(&text, &options, api_key) {
                    Ok(_) => {
                        let _ = queue::remove(&entry);
                    }
                    Err(TaskError::Invalid(reason)) => {
                        eprintln!("Dropping a queued task that can never be sent ({reason}): {entry}");
                        let _ = queue::remove(&entry);
                    }
                    Err(TaskError::Failed(_)) => {}
                }
            }
        }
    }
}
