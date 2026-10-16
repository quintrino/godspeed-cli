//! Saved searches: a name for a query, run as `godspeed-cli NAME` or `godspeed-cli search @NAME`.
//!
//! They live in the `[searches]` table of the settings file, one `name = "query"` line each, so
//! they can be written by hand, and `godspeed-cli searches add` and `remove` edit that table
//! without touching the rest of the file. `today` and `overdue` are built in and can be replaced
//! by defining a search with the same name.
//!
//! A name can't be a command (see `commands.rs`), so the command always wins and a saved search
//! can never be hidden by one. A settings file that has such a name is an error.

use crate::commands;
use crate::config::{self};
use crate::{exit, paths, query};
use chrono::Local;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

/// The saved searches that need no settings. Completed tasks are left out of every search unless
/// it says `is:complete`, so these don't need to say so.
pub const BUILT_IN: [(&str, &str); 2] = [("today", "due:today sort:due"), ("overdue", "due<today sort:due")];

const MAX_NAME: usize = 40;

/// Whether `name` is made of what a name may be made of: lowercase letters, digits, `-` and `_`,
/// starting with a letter or digit. Leaves out the question of commands.
pub fn is_well_formed(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && name.len() <= MAX_NAME
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Checks a name (already lowercase) that a saved search is going to have.
pub fn validate_name(name: &str) -> Result<(), String> {
    if commands::is_reserved(name) {
        return Err(format!(
            "'{name}' is the name of a godspeed-cli command, or one kept for a future command, so it can't be a saved search. Rename it in the settings file, such as to '{name}s' or 'my-{name}'"
        ));
    }
    if !is_well_formed(name) {
        return Err(format!(
            "'{name}' isn't a usable name: use lowercase letters, digits, '-' and '_' (up to {MAX_NAME}), starting with a letter or digit"
        ));
    }
    Ok(())
}

/// The built-in searches with the ones from the settings file on top.
pub fn merged(user: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut all: BTreeMap<String, String> = BUILT_IN.iter().map(|(name, text)| (name.to_string(), text.to_string())).collect();
    all.extend(user.iter().map(|(name, text)| (name.clone(), text.clone())));
    all
}

/// Whether the first word of the command line is a saved search to run: its name and query if so. Looks
/// at the settings only if the word could be a name at all, so a typo or a sentence costs nothing,
/// and a settings file with a mistake is reported rather than ignored.
pub fn saved_command(word: &str) -> Result<Option<(String, String)>, String> {
    let name = word.to_lowercase();
    if !is_well_formed(&name) || commands::is_reserved(&name) {
        return Ok(None);
    }
    let config = config::load()?;
    Ok(merged(&config.searches).remove(&name).map(|text| (name, text)))
}

/// The names of the saved searches, for tab completion. Never fails: a settings file that can't
/// be read just means there are only the built-in ones.
pub fn names_lenient() -> Vec<String> {
    let mut names: Vec<String> = BUILT_IN.iter().map(|(name, _)| name.to_string()).collect();
    if let Ok(text) = fs::read_to_string(paths::config_path()) {
        if let Some(table) = text.parse::<toml::Table>().ok().and_then(|t| t.get("searches").and_then(|s| s.as_table().cloned())) {
            names.extend(table.keys().map(|key| key.to_lowercase()).filter(|name| is_well_formed(name) && !commands::is_reserved(name)));
        }
    }
    names.sort();
    names.dedup();
    names
}

// ---- editing the settings file ----

/// Where the `[searches]` table is in the lines of a settings file: its heading, and the line
/// after its last line (the next heading, or the end).
fn section_bounds(lines: &[String]) -> Option<(usize, usize)> {
    let header = lines.iter().position(|line| {
        let line = line.trim();
        line.strip_prefix("[searches]").is_some_and(|rest| rest.trim().is_empty() || rest.trim_start().starts_with('#'))
    })?;
    let end = lines[header + 1..].iter().position(|line| line.trim_start().starts_with('[')).map_or(lines.len(), |offset| header + 1 + offset);
    Some((header, end))
}

/// The name a `name = "query"` line defines, lowercase.
fn key_of(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, _) = line.split_once('=')?;
    Some(key.trim().trim_matches(|c| c == '"' || c == '\'').to_lowercase())
}

/// The saved searches in a settings file's text, by lowercase name.
fn searches_in(text: &str) -> Result<BTreeMap<String, String>, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| format!("the settings file isn't valid TOML: {}", e.message()))?;
    match table.get("searches") {
        None => Ok(BTreeMap::new()),
        Some(toml::Value::Table(searches)) => {
            Ok(searches.iter().filter_map(|(name, value)| Some((name.to_lowercase(), value.as_str()?.to_string()))).collect())
        }
        Some(_) => Err("'searches' in the settings file isn't a table".to_string()),
    }
}

/// The settings with everything but `[searches]` taken out, to check an edit left them alone.
fn rest_of(text: &str) -> Result<toml::Table, String> {
    let mut table: toml::Table = text.parse().map_err(|e: toml::de::Error| format!("the settings file isn't valid TOML: {}", e.message()))?;
    table.remove("searches");
    Ok(table)
}

/// Checks that an edit did exactly what was meant: the saved searches are those before with
/// `name` set to `query` (or taken away), and nothing else changed. The editing works on lines,
/// so a file written in an unusual style can fool it; this makes sure it never writes a wrong one.
fn check_edit(before: &str, after: &str, name: &str, query: Option<&str>) -> Result<(), String> {
    let mut expected = searches_in(before)?;
    match query {
        Some(query) => expected.insert(name.to_string(), query.to_string()),
        None => expected.remove(name),
    };
    if searches_in(after)? != expected || rest_of(before)? != rest_of(after)? {
        return Err(format!(
            "the settings file is laid out in a way this can't safely edit. Edit {} by hand instead, under a [searches] heading",
            paths::config_path().display()
        ));
    }
    Ok(())
}

/// The settings text with the saved search `name` set to `query`.
pub fn set_search(text: &str, name: &str, query: &str) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let entry = format!("{name} = {}", toml::Value::String(query.to_string()));
    match section_bounds(&lines) {
        Some((header, end)) => match (header + 1..end).find(|&i| key_of(&lines[i]).as_deref() == Some(name)) {
            Some(i) => lines[i] = entry,
            None => {
                let mut at = end;
                while at > header + 1 && lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                lines.insert(at, entry);
            }
        },
        None => {
            if lines.last().is_some_and(|last| !last.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("[searches]".to_string());
            lines.push(entry);
        }
    }
    let after = lines.join("\n") + "\n";
    check_edit(text, &after, name, Some(query))?;
    Ok(after)
}

/// The settings text without the saved search `name`.
pub fn remove_search(text: &str, name: &str) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let found = section_bounds(&lines)
        .and_then(|(header, end)| (header + 1..end).find(|&i| key_of(&lines[i]).as_deref() == Some(name)))
        .ok_or_else(|| {
            format!(
                "'{name}' isn't a [searches] line this can edit in {}; if it is defined some other way, edit the file by hand",
                paths::config_path().display()
            )
        })?;
    lines.remove(found);
    let after = lines.join("\n") + "\n";
    check_edit(text, &after, name, None)?;
    Ok(after)
}

fn read_settings(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Writes the file whole or not at all: to a temporary file next to it, then renamed over it.
fn write_settings(path: &Path, text: &str) -> Result<(), String> {
    let fail = |e: io::Error| format!("cannot write {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    let temporary = path.with_extension("toml.new");
    fs::write(&temporary, text).map_err(fail)?;
    fs::rename(&temporary, path).map_err(fail)
}

// ---- the `searches` command ----

fn fail(message: &str) -> ExitCode {
    eprintln!("Error: {message}");
    exit::usage()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ListArgs {
    json: bool,
    count: bool,
}

fn parse_list_args(args: &[String]) -> Result<ListArgs, String> {
    let mut parsed = ListArgs::default();
    for arg in args {
        match arg.as_str() {
            "--json" => parsed.json = true,
            "--count" => parsed.count = true,
            other => return Err(format!("unknown argument '{other}' for searches (see `godspeed-cli searches --help`)")),
        }
    }
    if parsed.json && parsed.count {
        return Err("--json and --count can't be combined".to_string());
    }
    Ok(parsed)
}

fn source_of(name: &str, config: &config::Config) -> &'static str {
    match (config.searches.contains_key(name), BUILT_IN.iter().any(|(built_in, _)| *built_in == name)) {
        (true, true) => "settings (replaces the built-in)",
        (true, false) => "settings",
        _ => "built-in",
    }
}

fn list(args: &[String]) -> ExitCode {
    let args = match parse_list_args(args) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };
    let config = match config::load() {
        Ok(config) => config,
        Err(message) => return fail(&message),
    };
    let all = merged(&config.searches);
    let output = if args.count {
        all.len().to_string()
    } else if args.json {
        let items: Vec<_> =
            all.iter().map(|(name, query)| json!({"name": name, "query": query, "source": source_of(name, &config)})).collect();
        serde_json::to_string_pretty(&items).expect("the searches serialise")
    } else {
        let mut rows = vec![vec!["NAME".to_string(), "QUERY".to_string(), "FROM".to_string()]];
        rows.extend(all.iter().map(|(name, query)| vec![name.clone(), query.clone(), source_of(name, &config).to_string()]));
        crate::listing::table(&rows, false)
    };
    // Ignore a closed pipe.
    let _ = writeln!(io::stdout(), "{output}");
    exit::SUCCESS
}

fn show(args: &[String]) -> ExitCode {
    let [name] = args else { return fail("say which saved search: godspeed-cli searches show <name>") };
    let name = name.trim_start_matches('@').to_lowercase();
    let config = match config::load() {
        Ok(config) => config,
        Err(message) => return fail(&message),
    };
    let all = merged(&config.searches);
    let Some(text) = all.get(&name) else {
        let names: Vec<&str> = all.keys().map(String::as_str).collect();
        return fail(&format!("no saved search '{name}' (saved searches: {})", names.join(", ")));
    };
    println!("{name}  ({})\n  {text}\nRun it with: godspeed-cli {name}   (or: godspeed-cli search @{name})", source_of(&name, &config));
    exit::SUCCESS
}

fn add(args: &[String]) -> ExitCode {
    let Some((name, words)) = args.split_first() else {
        return fail("say what to save: godspeed-cli searches add <name> <query>...");
    };
    let name = name.to_lowercase();
    if let Err(message) = validate_name(&name) {
        return fail(&message);
    }
    let text = words.join(" ").trim().to_string();
    if text.is_empty() {
        return fail(&format!("say what '{name}' searches for: godspeed-cli searches add {name} <query>..."));
    }
    let config = match config::load() {
        Ok(config) => config,
        Err(message) => return fail(&message),
    };
    // The search has to work, including what it refers to, before it is saved.
    let mut all = merged(&config.searches);
    let replaced = config.searches.get(&name).cloned();
    all.insert(name.clone(), text.clone());
    if let Err(message) = query::parse_with(&[format!("@{name}")], Local::now().naive_local(), Some(&all)) {
        return fail(&message);
    }
    let path = paths::config_path();
    let result = read_settings(&path).and_then(|before| set_search(&before, &name, &text)).and_then(|after| write_settings(&path, &after));
    if let Err(message) = result {
        return fail(&message);
    }
    match replaced {
        Some(old) => println!("Replaced '{name}' (it was: {old})"),
        None => println!("Saved '{name}'"),
    }
    println!("Run it with: godspeed-cli {name}");
    exit::SUCCESS
}

fn remove(args: &[String]) -> ExitCode {
    let [name] = args else { return fail("say which saved search to remove: godspeed-cli searches remove <name>") };
    let name = name.trim_start_matches('@').to_lowercase();
    let path = paths::config_path();
    // Read as it is, not through `config::load`, so that a search whose name is a command (which
    // makes the settings an error) can be removed with this.
    let before = match read_settings(&path) {
        Ok(text) => text,
        Err(message) => return fail(&message),
    };
    let user = match searches_in(&before) {
        Ok(user) => user,
        Err(message) => return fail(&message),
    };
    if !user.contains_key(&name) {
        let hint = if BUILT_IN.iter().any(|(built_in, _)| *built_in == name) {
            " It is built in. To change it, save your own with that name: godspeed-cli searches add"
        } else {
            ""
        };
        return fail(&format!("'{name}' isn't in the settings file.{hint}"));
    }
    let result = remove_search(&before, &name).and_then(|after| write_settings(&path, &after));
    if let Err(message) = result {
        return fail(&message);
    }
    println!("Removed '{name}'");
    if BUILT_IN.iter().any(|(built_in, _)| *built_in == name) {
        println!("The built-in '{name}' is used again.");
    }
    exit::SUCCESS
}

pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None => list(&[]),
        Some("list") => list(&args[1..]),
        Some("show") => show(&args[1..]),
        Some("add" | "save") => add(&args[1..]),
        Some("remove" | "rm") => remove(&args[1..]),
        Some(flag) if flag.starts_with("--") => list(args),
        Some(other) => fail(&format!("'{other}' isn't a searches command (list, show, add or remove; see `godspeed-cli searches --help`)")),
    }
}

/// What `godspeed-cli NAME --help` says: what the saved search is, and that the options are
/// those of `search`.
pub fn help_for(name: &str, query: &str) -> String {
    format!(
        "godspeed-cli {name}\n\n'{name}' is a saved search: it runs\n\n  godspeed-cli search \"{query}\"\n\nIt takes the options of `godspeed-cli search` (--json, --id, --ids-only, --count, --refresh, --max-age, --color), and more search words after it are added to the query: `godspeed-cli {name} label:Low`. See `godspeed-cli search --help` and `godspeed-cli searches --help`.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(name, text)| (name.to_string(), text.to_string())).collect()
    }

    #[test]
    fn a_name_is_lowercase_words_and_not_a_command() {
        for good in ["today", "my-list", "q2_plan", "a", "3d"] {
            assert!(validate_name(good).is_ok(), "{good}");
        }
        for bad in ["", "Today", "two words", "-x", "_x", "a/b", "@x", "x:y", &"x".repeat(41)] {
            assert!(validate_name(bad).unwrap_err().contains("isn't a usable name"), "{bad:?}");
        }
        for command in commands::COMMANDS.iter().chain(commands::RESERVED.iter()) {
            let message = validate_name(command).unwrap_err();
            assert!(message.contains("name of a godspeed-cli command"), "{command}: {message}");
        }
    }

    #[test]
    fn built_in_searches_do_not_mention_completed_tasks_and_can_be_replaced() {
        for (_, text) in BUILT_IN {
            assert!(!text.contains("is:complete"), "{text}");
        }
        let all = merged(&user(&[("overdue", "due<today list:Work"), ("inbox", "list:Inbox")]));
        assert_eq!(all["today"], "due:today sort:due");
        assert_eq!(all["overdue"], "due<today list:Work");
        assert_eq!(all["inbox"], "list:Inbox");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn a_search_is_added_to_a_file_without_a_searches_table_and_the_rest_is_kept() {
        let before = "# my settings\nmax_age = \"6h\"\n";

        let after = set_search(before, "inbox", "list:Inbox sort-desc:created").unwrap();

        assert_eq!(after, "# my settings\nmax_age = \"6h\"\n\n[searches]\ninbox = \"list:Inbox sort-desc:created\"\n");
        assert_eq!(set_search("", "a", "b").unwrap(), "[searches]\na = \"b\"\n");
    }

    #[test]
    fn a_search_is_added_to_and_replaced_in_an_existing_table_keeping_comments() {
        let before = "max_age = \"6h\"\n\n[searches]\n# the ones I use daily\ninbox = \"list:Inbox\"\nwork = \"list:Work\"\n\n[other]\nx = 1\n";

        let added = set_search(before, "waiting", "label:Waiting").unwrap();
        let replaced = set_search(&added, "inbox", "list:Inbox -has:notes").unwrap();

        assert_eq!(
            added,
            "max_age = \"6h\"\n\n[searches]\n# the ones I use daily\ninbox = \"list:Inbox\"\nwork = \"list:Work\"\nwaiting = \"label:Waiting\"\n\n[other]\nx = 1\n"
        );
        assert!(replaced.contains("inbox = \"list:Inbox -has:notes\"\n") && !replaced.contains("inbox = \"list:Inbox\"\n"), "{replaced}");
        assert!(replaced.contains("# the ones I use daily") && replaced.contains("[other]\nx = 1"), "{replaced}");
    }

    #[test]
    fn a_query_with_quotes_and_backslashes_is_written_so_it_reads_back_the_same() {
        let query = "\"budget meeting\" -label:\"Low\" title:C:\\temp";
        let text = set_search("", "tricky", query).unwrap();
        assert_eq!(searches_in(&text).unwrap()["tricky"], query);
    }

    #[test]
    fn a_search_is_removed_and_the_rest_of_the_file_is_kept() {
        let before = "max_age = \"6h\"\n\n[searches]\n# daily\ninbox = \"list:Inbox\"\nwork = \"list:Work\"\n";

        let after = remove_search(before, "inbox").unwrap();

        assert_eq!(after, "max_age = \"6h\"\n\n[searches]\n# daily\nwork = \"list:Work\"\n");
        assert!(remove_search(before, "nothing").is_err());
        assert!(remove_search("max_age = \"6h\"\n", "inbox").is_err());
    }

    #[test]
    fn names_are_found_whatever_their_case_or_quoting() {
        let before = "[searches]\n\"Inbox\" = \"list:Inbox\"\n";
        let after = remove_search(before, "inbox").unwrap();
        assert_eq!(searches_in(&after).unwrap().len(), 0);
        let replaced = set_search(before, "inbox", "list:Other").unwrap();
        assert_eq!(searches_in(&replaced).unwrap()["inbox"], "list:Other");
        assert_eq!(searches_in(&replaced).unwrap().len(), 1);
    }

    #[test]
    fn a_layout_the_line_editing_cannot_handle_is_refused_and_not_written() {
        // An inline table: the search is not on a line of its own under a heading.
        let inline = "searches = { inbox = \"list:Inbox\" }\n";
        assert!(set_search(inline, "work", "list:Work").is_err());
        assert!(remove_search(inline, "inbox").is_err());
        // A multi-line string that looks like a heading inside.
        let tricky = "[searches]\ninbox = \"\"\"\n[x]\n\"\"\"\n";
        assert!(set_search(tricky, "work", "list:Work").is_ok() || set_search(tricky, "work", "list:Work").unwrap_err().contains("can't safely edit"));
        assert!(set_search("not [ toml", "a", "b").is_err());
    }

    #[test]
    fn a_saved_command_is_a_lowercase_word_that_is_saved_and_not_a_command() {
        // Words that can't be names are never looked up (so no settings are read).
        for word in ["buy milk", "-x", "Add", "search", "queue", ""] {
            assert_eq!(saved_command(word), Ok(None), "{word:?}");
        }
    }

    #[test]
    fn the_list_flags_are_json_or_count_but_not_both() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_list_args(&args(&["--count"])).unwrap(), ListArgs { count: true, json: false });
        assert!(parse_list_args(&args(&["--json", "--count"])).is_err());
        assert!(parse_list_args(&args(&["x"])).is_err());
    }

    #[test]
    fn the_help_for_a_saved_search_says_what_it_runs() {
        let text = help_for("overdue", "due<today sort:due");
        assert!(text.contains("godspeed-cli search \"due<today sort:due\"") && text.contains("godspeed-cli search --help"), "{text}");
    }
}
