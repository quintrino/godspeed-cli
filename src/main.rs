#[macro_use]
mod debug;
mod api;
mod color;
mod capture;
mod completions;
mod config;
mod dates;
mod edit;
mod eval;
mod exit;
mod help;
mod listing;
mod names;
mod notify;
mod paths;
mod pending;
mod present;
mod queue;
mod queue_cmd;
mod query;
mod refresh;
mod search;
mod show;
mod store;
mod task;
mod token;

use std::env;
use std::process::ExitCode;

/// What the command line is asking for. Only the first argument picks the command, and a task
/// is only ever created by `add`.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    /// `add`: its options and the task text, as separate arguments. No text means read it
    /// from stdin.
    Add(Vec<String>),
    Refresh(Vec<String>),
    Search(Vec<String>),
    Lists(Vec<String>),
    Labels(Vec<String>),
    Edit(Vec<String>),
    Show(Vec<String>),
    Queue(Vec<String>),
    AddHelp,
    RefreshHelp,
    SearchHelp,
    ListsHelp,
    LabelsHelp,
    EditHelp,
    ShowHelp,
    QueueHelp,
    Completions(Vec<String>),
    CompletionsHelp,
    /// The hidden command the completion scripts call for names: `__complete lists|labels`.
    Names(Vec<String>),
    /// The command line isn't one this program understands.
    Invalid(Problem),
}

#[derive(Debug, PartialEq, Eq)]
enum Problem {
    NoCommand,
    /// The first argument isn't a command.
    Unknown(String),
}

fn parse_invocation(args: &[String]) -> Invocation {
    let Some(first) = args.first().map(String::as_str) else {
        return Invocation::Invalid(Problem::NoCommand);
    };
    let rest = &args[1..];
    match first {
        "-h" | "--help" => Invocation::Help,
        "-V" | "--version" => Invocation::Version,
        "add" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::AddHelp,
            Some("-V" | "--version") => Invocation::Version,
            _ => Invocation::Add(rest.to_vec()),
        },
        "refresh" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::RefreshHelp,
            _ => Invocation::Refresh(rest.to_vec()),
        },
        "search" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::SearchHelp,
            _ => Invocation::Search(rest.to_vec()),
        },
        "lists" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::ListsHelp,
            _ => Invocation::Lists(rest.to_vec()),
        },
        "labels" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::LabelsHelp,
            _ => Invocation::Labels(rest.to_vec()),
        },
        "edit" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::EditHelp,
            _ => Invocation::Edit(rest.to_vec()),
        },
        "show" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::ShowHelp,
            _ => Invocation::Show(rest.to_vec()),
        },
        "queue" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::QueueHelp,
            _ => Invocation::Queue(rest.to_vec()),
        },
        "completions" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::CompletionsHelp,
            _ => Invocation::Completions(rest.to_vec()),
        },
        "__complete" => Invocation::Names(rest.to_vec()),
        other => Invocation::Invalid(Problem::Unknown(other.to_string())),
    }
}

/// The message for a command line that isn't understood.
fn problem_message(problem: &Problem) -> String {
    const COMMANDS: &str = "Commands: add, edit, show, queue, lists, labels, refresh, search, completions. Run `godspeed-cli --help` for usage.";
    match problem {
        Problem::NoCommand => format!("Error: no command given.\n{COMMANDS}"),
        Problem::Unknown(word) if word.starts_with('-') => {
            format!("Error: '{word}' isn't a valid godspeed-cli option.\n{COMMANDS}")
        }
        Problem::Unknown(word) => format!(
            "Error: '{word}' isn't a valid godspeed-cli command.\n{COMMANDS}\n\
             To create a task, use add: godspeed-cli add \"{word}\""
        ),
    }
}

/// Prints a help page, laid out for the width of the terminal.
fn print_help(page: &str) {
    print!("godspeed-cli {}\n{}", env!("CARGO_PKG_VERSION"), help::render(page, help::terminal_width()));
}

/// Takes `--debug` out of the arguments and turns debugging on if it was there. It can come
/// before the command or anywhere after it, except that for `add` it only counts straight after
/// the command, among the options of `add` (like `--json`), so a task can contain the word, and
/// that after `--` it is text.
fn take_debug(args: Vec<String>) -> Vec<String> {
    let mut found = false;
    let mut kept: Vec<String> = Vec::new();
    let mut iter = args.into_iter().peekable();
    while iter.peek().is_some_and(|arg| arg == "--debug") {
        found = true;
        iter.next();
    }
    let command = iter.next();
    let only_leading = command.as_deref() == Some("add");
    let mut scanning = true;
    // The next argument is the value of an option of `add`, so it is not looked at.
    let mut value_next = false;
    for arg in iter {
        if scanning && value_next {
            value_next = false;
            kept.push(arg);
            continue;
        }
        if scanning && arg == "--debug" {
            found = true;
            continue;
        }
        if arg == "--" {
            scanning = false;
        } else if only_leading {
            match capture::option_width(&arg) {
                Some(2) => value_next = true,
                Some(_) => {}
                None => scanning = false,
            }
        }
        kept.push(arg);
    }
    if found {
        debug::enable();
    }
    command.into_iter().chain(kept).collect()
}

fn main() -> ExitCode {
    let args = take_debug(env::args().skip(1).collect());
    debug!("godspeed-cli {} on {} {}", env!("CARGO_PKG_VERSION"), env::consts::OS, env::consts::ARCH);
    debug!("arguments: {args:?}");
    debug!("data directory: {}", paths::data_home().display());
    match parse_invocation(&args) {
        Invocation::Help => {
            print_help(help::HELP);
            exit::SUCCESS
        }
        Invocation::Version => {
            println!("godspeed-cli {}", env!("CARGO_PKG_VERSION"));
            exit::SUCCESS
        }
        Invocation::Add(task_args) => capture::run_add(&task_args),
        Invocation::Refresh(refresh_args) => refresh::run_refresh(&refresh_args),
        Invocation::Search(search_args) => search::run_search(&search_args),
        Invocation::Lists(list_args) => listing::run(listing::Kind::Lists, &list_args),
        Invocation::Labels(label_args) => listing::run(listing::Kind::Labels, &label_args),
        Invocation::Edit(edit_args) => edit::run_edit(&edit_args),
        Invocation::Show(show_args) => show::run(&show_args),
        Invocation::Queue(queue_args) => queue_cmd::run(&queue_args),
        Invocation::ShowHelp => {
            print_help(help::SHOW_HELP);
            exit::SUCCESS
        }
        Invocation::QueueHelp => {
            print_help(help::QUEUE_HELP);
            exit::SUCCESS
        }
        Invocation::ListsHelp => {
            print_help(help::LISTS_HELP);
            exit::SUCCESS
        }
        Invocation::LabelsHelp => {
            print_help(help::LABELS_HELP);
            exit::SUCCESS
        }
        Invocation::EditHelp => {
            print_help(help::EDIT_HELP);
            exit::SUCCESS
        }
        Invocation::AddHelp => {
            print_help(help::ADD_HELP);
            exit::SUCCESS
        }
        Invocation::RefreshHelp => {
            print_help(help::REFRESH_HELP);
            exit::SUCCESS
        }
        Invocation::SearchHelp => {
            print_help(help::SEARCH_HELP);
            exit::SUCCESS
        }
        Invocation::Completions(completions_args) => completions::run_completions(&completions_args),
        Invocation::CompletionsHelp => {
            print_help(help::COMPLETIONS_HELP);
            exit::SUCCESS
        }
        Invocation::Names(names_args) => completions::run_names(&names_args),
        Invocation::Invalid(problem) => {
            eprintln!("{}", problem_message(&problem));
            exit::usage()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn unknown(word: &str) -> Invocation {
        Invocation::Invalid(Problem::Unknown(word.to_string()))
    }

    #[test]
    fn help_and_version_flags_are_recognised_in_both_spellings() {
        assert_eq!(parse_invocation(&args(&["--help"])), Invocation::Help);
        assert_eq!(parse_invocation(&args(&["-h"])), Invocation::Help);
        assert_eq!(parse_invocation(&args(&["--version"])), Invocation::Version);
        assert_eq!(parse_invocation(&args(&["-V"])), Invocation::Version);
    }

    #[test]
    fn a_task_is_only_created_by_add() {
        assert_eq!(
            parse_invocation(&args(&["add", "Buy", "milk", "@Home"])),
            Invocation::Add(args(&["Buy", "milk", "@Home"]))
        );
        assert_eq!(parse_invocation(&args(&["add", "Buy milk"])), Invocation::Add(args(&["Buy milk"])));
        assert_eq!(parse_invocation(&args(&["add"])), Invocation::Add(vec![]), "no text means read stdin");
    }

    #[test]
    fn anything_that_is_not_a_command_is_an_error() {
        assert_eq!(parse_invocation(&args(&["string"])), unknown("string"));
        assert_eq!(parse_invocation(&args(&["Buy", "milk"])), unknown("Buy"));
        assert_eq!(parse_invocation(&args(&["Buy milk @Home"])), unknown("Buy milk @Home"));
        assert_eq!(parse_invocation(&args(&["search the attic"])), unknown("search the attic"));
        assert_eq!(parse_invocation(&args(&["--"])), unknown("--"));
        assert_eq!(parse_invocation(&args(&["--json", "x"])), unknown("--json"));
        assert_eq!(parse_invocation(&args(&["--nope"])), unknown("--nope"));
        assert_eq!(parse_invocation(&args(&[""])), unknown(""));
    }

    #[test]
    fn no_arguments_at_all_is_an_error_too() {
        assert_eq!(parse_invocation(&args(&[])), Invocation::Invalid(Problem::NoCommand));
    }

    #[test]
    fn commands_are_recognised_exactly() {
        for word in ["Search", "ADD", "Refresh", "refreshing", "adding", "searches", "completion", "Completions"] {
            assert_eq!(parse_invocation(&args(&[word, "x"])), unknown(word), "{word}");
        }
        // Only the first argument is the command.
        assert_eq!(parse_invocation(&args(&["buy", "add", "milk"])), unknown("buy"));
    }

    #[test]
    fn add_gets_everything_after_it_to_sort_out_its_options_itself() {
        assert_eq!(
            parse_invocation(&args(&["add", "--", "--version", "bump"])),
            Invocation::Add(args(&["--", "--version", "bump"]))
        );
        assert_eq!(parse_invocation(&args(&["add", "--"])), Invocation::Add(args(&["--"])));
        assert_eq!(
            parse_invocation(&args(&["add", "--json", "--due", "friday", "Buy", "milk"])),
            Invocation::Add(args(&["--json", "--due", "friday", "Buy", "milk"]))
        );
        assert_eq!(parse_invocation(&args(&["add", "--json"])), Invocation::Add(args(&["--json"])));
        assert_eq!(parse_invocation(&args(&["add", "-h"])), Invocation::AddHelp);
        assert_eq!(parse_invocation(&args(&["add", "-V"])), Invocation::Version);
    }

    #[test]
    fn a_task_after_add_may_start_with_any_word_including_command_names() {
        assert_eq!(
            parse_invocation(&args(&["add", "search", "the", "attic"])),
            Invocation::Add(args(&["search", "the", "attic"]))
        );
        assert_eq!(
            parse_invocation(&args(&["add", "refresh", "the", "roof"])),
            Invocation::Add(args(&["refresh", "the", "roof"]))
        );
        assert_eq!(
            parse_invocation(&args(&["add", "completions", "are", "due"])),
            Invocation::Add(args(&["completions", "are", "due"]))
        );
    }

    #[test]
    fn add_help_and_version() {
        assert_eq!(parse_invocation(&args(&["add", "--help"])), Invocation::AddHelp);
        assert_eq!(parse_invocation(&args(&["add", "-h"])), Invocation::AddHelp);
        assert_eq!(parse_invocation(&args(&["add", "-V"])), Invocation::Version);
    }

    #[test]
    fn refresh_and_search_take_their_arguments() {
        assert_eq!(parse_invocation(&args(&["refresh"])), Invocation::Refresh(vec![]));
        assert_eq!(
            parse_invocation(&args(&["refresh", "--include-completed", "--json"])),
            Invocation::Refresh(args(&["--include-completed", "--json"]))
        );
        assert_eq!(parse_invocation(&args(&["refresh", "--help"])), Invocation::RefreshHelp);
        assert_eq!(parse_invocation(&args(&["refresh", "-h"])), Invocation::RefreshHelp);
        assert_eq!(
            parse_invocation(&args(&["search", "label:Low", "due:today"])),
            Invocation::Search(args(&["label:Low", "due:today"]))
        );
        assert_eq!(parse_invocation(&args(&["search"])), Invocation::Search(vec![]));
    }

    #[test]
    fn search_help_is_available_and_the_search_command_still_takes_a_query() {
        assert_eq!(parse_invocation(&args(&["search", "--help"])), Invocation::SearchHelp);
        assert_eq!(parse_invocation(&args(&["search", "-h"])), Invocation::SearchHelp);
        assert_eq!(
            parse_invocation(&args(&["search", "-label:Low", "--json"])),
            Invocation::Search(args(&["-label:Low", "--json"]))
        );
    }

    #[test]
    fn completions_take_a_shell_and_have_their_own_help() {
        assert_eq!(parse_invocation(&args(&["completions", "zsh"])), Invocation::Completions(args(&["zsh"])));
        assert_eq!(parse_invocation(&args(&["completions"])), Invocation::Completions(vec![]));
        assert_eq!(parse_invocation(&args(&["completions", "--help"])), Invocation::CompletionsHelp);
        assert_eq!(parse_invocation(&args(&["completions", "-h"])), Invocation::CompletionsHelp);
    }

    #[test]
    fn the_hidden_names_command_is_not_in_the_help_but_is_recognised() {
        assert_eq!(parse_invocation(&args(&["__complete", "lists"])), Invocation::Names(args(&["lists"])));
        assert!(!help::HELP.contains("__complete"));
        assert!(!help::SEARCH_HELP.contains("__complete"));
        assert!(!help::COMPLETIONS_HELP.contains("__complete"));
        assert!(!help::ADD_HELP.contains("__complete"));
        assert!(!help::REFRESH_HELP.contains("__complete"));
    }

    #[test]
    fn the_error_for_an_unknown_command_names_it_and_shows_how_to_create_a_task() {
        let message = problem_message(&Problem::Unknown("string".to_string()));
        assert!(message.contains("'string' isn't a valid godspeed-cli command"), "{message}");
        assert!(message.contains("add, edit, show, queue, lists, labels, refresh, search, completions"), "{message}");
        assert!(message.contains("godspeed-cli add \"string\""), "{message}");
    }

    #[test]
    fn the_error_for_an_unknown_option_does_not_suggest_creating_a_task() {
        let message = problem_message(&Problem::Unknown("--nope".to_string()));
        assert!(message.contains("'--nope' isn't a valid godspeed-cli option"), "{message}");
        assert!(!message.contains("To create a task"), "{message}");
    }

    #[test]
    fn the_error_for_no_command_lists_the_commands() {
        let message = problem_message(&Problem::NoCommand);
        assert!(message.contains("no command given") && message.contains("add, edit, show, queue, lists, labels, refresh, search, completions"), "{message}");
    }
}
