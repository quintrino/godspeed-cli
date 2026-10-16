//! `godspeed-cli lists` and `godspeed-cli labels`: show your lists or labels, optionally only
//! those matching a string.
//!
//! The names come from the API when there is a token, and from the local copy made by `refresh`
//! when there isn't one or the API can't be reached.

use crate::color::{self, ColorMode, HEADER, RESET};
use crate::store::Store;
use crate::{api, exit, paths, token};
use serde_json::Value;
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lists,
    Labels,
}

impl Kind {
    pub fn singular(self) -> &'static str {
        match self {
            Kind::Lists => "list",
            Kind::Labels => "label",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Kind::Lists => "lists",
            Kind::Labels => "labels",
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    json: bool,
    id: bool,
    ids_only: bool,
    /// Print only how many there are.
    count: bool,
    color: ColorMode,
    /// The words to match, joined with spaces. Empty shows everything.
    filter: String,
}

/// `--id`, `--json` and `--color WHEN` can go anywhere; `--` ends the options. Every other word
/// is part of the string to match.
fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut words: Vec<&str> = Vec::new();
    let mut options_done = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if options_done {
            words.push(arg);
            continue;
        }
        let color_value = if arg == "--color" {
            Some(iter.next().ok_or("--color needs auto, always or never")?.as_str())
        } else {
            arg.strip_prefix("--color=")
        };
        if let Some(value) = color_value {
            parsed.color = ColorMode::parse(value).ok_or_else(|| format!("--color: '{value}' isn't auto, always or never"))?;
            continue;
        }
        match arg.as_str() {
            "--" => options_done = true,
            "--id" => parsed.id = true,
            "--ids-only" => parsed.ids_only = true,
            "--count" => parsed.count = true,
            "--json" => parsed.json = true,
            other if other.starts_with("--") => return Err(format!("unknown option '{other}' (see --help)")),
            _ => words.push(arg),
        }
    }
    parsed.filter = words.join(" ");
    let chosen = [parsed.json, parsed.ids_only, parsed.count].iter().filter(|on| **on).count();
    if chosen > 1 {
        return Err("--json, --ids-only and --count can't be combined".to_string());
    }
    Ok(parsed)
}

fn squash(value: &str) -> String {
    value.chars().filter(|c| !matches!(c, ' ' | '/' | '-')).collect()
}

/// Whether `name` contains `filter`, ignoring case, or ignoring case, spaces, slashes and dashes
/// (so `workproj` finds "Work Projects"). An empty filter matches everything.
pub fn matches(name: &str, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    let name = name.to_lowercase();
    if name.contains(&filter) {
        return true;
    }
    let squashed = squash(&filter);
    !squashed.is_empty() && squash(&name).contains(&squashed)
}

fn text<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Whether a list or label matches: its name, or for a label the name of its group.
fn item_matches(item: &Value, filter: &str) -> bool {
    matches(text(item, "name"), filter) || (!text(item, "group_name").is_empty() && matches(text(item, "group_name"), filter))
}

/// A table with the header row first. Columns are as wide as their widest cell, and the last
/// column isn't padded.
pub(crate) fn table(rows: &[Vec<String>], color: bool) -> String {
    let columns = rows.first().map_or(0, Vec::len);
    let widths: Vec<usize> =
        (0..columns).map(|c| rows.iter().map(|row| row[c].chars().count()).max().unwrap_or(0)).collect();
    let lines: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let cells: Vec<String> = row
                .iter()
                .zip(&widths)
                .map(|(cell, width)| format!("{cell}{}", " ".repeat(width - cell.chars().count())))
                .collect();
            let line = cells.join("  ").trim_end().to_string();
            if index == 0 && color { format!("{HEADER}{line}{RESET}") } else { line }
        })
        .collect();
    lines.join("\n")
}

fn render(kind: Kind, items: &[&Value], args: &Args, color: bool) -> String {
    if args.count {
        return items.len().to_string();
    }
    if args.json {
        return serde_json::to_string_pretty(&items).expect("the items serialise");
    }
    if items.is_empty() {
        return String::new();
    }
    if args.ids_only {
        let ids: Vec<&str> = items.iter().map(|item| text(item, "id")).collect();
        return ids.join("\n");
    }
    let with_group = kind == Kind::Labels && items.iter().any(|item| !text(item, "group_name").is_empty());
    let mut header = vec!["NAME".to_string()];
    if with_group {
        header.push("GROUP".to_string());
    }
    if args.id {
        header.push("ID".to_string());
    }
    let mut rows = vec![header];
    for item in items {
        let mut row = vec![text(item, "name").to_string()];
        if with_group {
            row.push(text(item, "group_name").to_string());
        }
        if args.id {
            row.push(text(item, "id").to_string());
        }
        rows.push(row);
    }
    table(&rows, color)
}

/// The lists or labels from the local copy, if there is one.
fn from_copy(kind: Kind) -> Result<Vec<Value>, String> {
    let path = paths::store_path();
    if !path.exists() {
        return Err("there is no local copy of your lists yet. Set GODSPEED_TOKEN, or run `godspeed-cli refresh`.".to_string());
    }
    debug!("reading {} from the local copy {}", kind.noun(), path.display());
    let store = Store::open(&path).map_err(|e| format!("cannot read the local copy: {e}"))?;
    let rows = match kind {
        Kind::Lists => store.lists(),
        Kind::Labels => store.labels(),
    };
    rows.map_err(|e| format!("cannot read the local copy: {e}"))
}

/// The items from the API, or if that isn't possible, from the local copy.
fn load(kind: Kind) -> Result<Vec<Value>, String> {
    let Some(api_key) = token::load_token() else {
        debug!("no token, so the {} come from the local copy", kind.noun());
        return from_copy(kind);
    };
    let fetched = match kind {
        Kind::Lists => api::fetch_list_items(&api_key),
        Kind::Labels => api::fetch_label_items(&api_key),
    };
    match fetched {
        Ok(items) => Ok(items),
        Err(e) => {
            debug!("fetching the {} failed: {e}", kind.noun());
            match from_copy(kind) {
                Ok(items) => {
                    eprintln!("Note: could not reach Godspeed ({e}); showing the {} in the local copy.", kind.noun());
                    Ok(items)
                }
                Err(_) => Err(format!("could not fetch your {}: {e}", kind.noun())),
            }
        }
    }
}

pub fn run(kind: Kind, args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::usage();
        }
    };
    debug!("{}: filter {:?}, ids {}, json {}", kind.noun(), args.filter, args.id, args.json);
    let items = match load(kind) {
        Ok(items) => items,
        Err(message) => {
            eprintln!("Error: {message}");
            return exit::failure();
        }
    };
    let total = items.len();
    let shown: Vec<&Value> = items.iter().filter(|item| item_matches(item, &args.filter)).collect();
    debug!("{} of {total} {} match {:?}", shown.len(), kind.noun(), args.filter);
    if shown.is_empty() && !args.json && !args.count {
        if args.filter.is_empty() {
            eprintln!("No {}.", kind.noun());
        } else {
            eprintln!("No {} match '{}'.", kind.noun(), args.filter);
        }
    }
    let use_color = !args.json && color::stdout_color(args.color);
    let output = render(kind, &shown, &args, use_color);
    if !output.is_empty() {
        // Ignore a closed pipe.
        let _ = writeln!(io::stdout(), "{output}");
    }
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_words_are_the_filter_and_the_flags_can_go_anywhere() {
        let parsed = parse_args(&args(&["work", "--id", "proj"])).unwrap();
        assert_eq!(parsed, Args { id: true, filter: "work proj".to_string(), ..Default::default() });
        assert!(parse_args(&args(&["--json"])).unwrap().json);
        assert_eq!(parse_args(&args(&["--", "--id"])).unwrap().filter, "--id");
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["--color=sometimes"])).is_err());
    }

    #[test]
    fn a_name_matches_by_part_ignoring_case_spaces_and_dashes() {
        assert!(matches("Work Projects", "work"));
        assert!(matches("Work Projects", "PROJ"));
        assert!(matches("Work Projects", "workproj"));
        assert!(matches("Work Projects", ""));
        assert!(!matches("Home", "work"));
        assert!(!matches("Home", "-"));
    }

    #[test]
    fn a_label_also_matches_by_its_group() {
        let label = json!({"id": "1", "name": "High", "group_name": "Severity"});
        assert!(item_matches(&label, "sever"));
        assert!(item_matches(&label, "hig"));
        assert!(!item_matches(&label, "home"));
        assert!(!item_matches(&json!({"id": "2", "name": "Plain"}), "sever"));
    }

    #[test]
    fn ids_are_only_shown_when_asked_for() {
        let work = json!({"id": "list-work", "name": "Work"});
        let home = json!({"id": "list-home", "name": "Home"});
        let items = [&work, &home];
        let plain = render(Kind::Lists, &items, &Args::default(), false);
        assert_eq!(plain, "NAME\nWork\nHome");
        let with_ids = render(Kind::Lists, &items, &Args { id: true, ..Default::default() }, false);
        assert_eq!(with_ids, "NAME  ID\nWork  list-work\nHome  list-home");
    }

    #[test]
    fn labels_show_their_group_column_only_when_some_label_has_one() {
        let high = json!({"id": "x1", "name": "High", "group_name": "Severity"});
        let plain = json!({"id": "x2", "name": "Plain"});
        let out = render(Kind::Labels, &[&high, &plain], &Args::default(), false);
        assert_eq!(out, "NAME   GROUP\nHigh   Severity\nPlain");
        let out = render(Kind::Labels, &[&plain], &Args::default(), false);
        assert_eq!(out, "NAME\nPlain");
    }
}
