//! Tasks that couldn't be sent, kept in a plain text file and retried on the next run.

use crate::paths::queue_path;
use std::fs;
use std::io;

const DIVIDER: &str = "-*-*-*-\n";

pub fn add(task: &str) -> io::Result<()> {
    let path = queue_path();
    let mut content = fs::read_to_string(&path).unwrap_or_default();
    content.push_str(task);
    content.push('\n');
    content.push_str(DIVIDER);
    fs::write(&path, content)
}

pub fn tasks() -> Vec<String> {
    match fs::read_to_string(queue_path()) {
        Ok(content) => content
            .split(DIVIDER)
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect(),
        Err(_) => Vec::new(),
    }
}

pub fn remove(task: &str) -> io::Result<()> {
    let path = queue_path();
    let content = fs::read_to_string(&path).unwrap_or_default();
    let remaining: Vec<&str> = content
        .split(DIVIDER)
        .filter(|s| !s.trim().is_empty() && s.trim() != task)
        .collect();

    let new_content = if remaining.is_empty() {
        String::new()
    } else {
        remaining.join(DIVIDER) + "\n" + DIVIDER
    };

    fs::write(&path, new_content)
}
