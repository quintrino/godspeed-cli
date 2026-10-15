mod api;
mod capture;
mod exit;
mod names;
mod notify;
mod paths;
mod queue;
mod task;
mod token;

use std::env;
use std::process::ExitCode;

const HELP: &str = r#"Create a task in Godspeed from the command line.

USAGE:
    godspeed-cli [add] [--] <TASK>...
    echo "<TASK>" | godspeed-cli
    godspeed-cli --help | --version

COMMANDS:
    add       Create a task. This is the default, so `add` can be left out.

    A first argument of exactly `add`, `refresh` or `search` is a command. To create a task
    whose text starts with one of those words, write `add search the attic` or quote the
    whole task: "search the attic".

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

EXIT STATUS:
    0    Success.
    1    Something failed, such as the API being unreachable. A task that couldn't be sent
         is cached and retried the next time the CLI runs.
    2    The input can't be used: no token, an invalid task, or a bad query. Nothing is
         cached, because retrying wouldn't help.
"#;

/// What the command line is asking for. Only the first argument is checked, so a task like
/// "fix the --version bug" is still a task.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    /// The task text, as separate arguments. Empty means read it from stdin.
    Add(Vec<String>),
    Refresh(Vec<String>),
    Search(Vec<String>),
}

fn parse_invocation(args: &[String]) -> Invocation {
    let Some(first) = args.first().map(String::as_str) else {
        return Invocation::Add(Vec::new());
    };
    let rest = &args[1..];
    match first {
        "-h" | "--help" => Invocation::Help,
        "-V" | "--version" => Invocation::Version,
        "--" => Invocation::Add(rest.to_vec()),
        "add" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::Help,
            Some("-V" | "--version") => Invocation::Version,
            Some("--") => Invocation::Add(rest[1..].to_vec()),
            _ => Invocation::Add(rest.to_vec()),
        },
        "refresh" => Invocation::Refresh(rest.to_vec()),
        "search" => Invocation::Search(rest.to_vec()),
        _ => Invocation::Add(args.to_vec()),
    }
}

fn not_available(command: &str) -> ExitCode {
    eprintln!("Error: `{command}` isn't available in this version yet");
    exit::usage()
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_invocation(&args) {
        Invocation::Help => {
            print!("godspeed-cli {}\n{HELP}", env!("CARGO_PKG_VERSION"));
            exit::SUCCESS
        }
        Invocation::Version => {
            println!("godspeed-cli {}", env!("CARGO_PKG_VERSION"));
            exit::SUCCESS
        }
        Invocation::Add(task_args) => capture::run_add(&task_args),
        Invocation::Refresh(_) => not_available("refresh"),
        Invocation::Search(_) => not_available("search"),
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
            Invocation::Add(args(&["Buy", "milk", "@Home"]))
        );
    }

    #[test]
    fn no_arguments_means_read_stdin() {
        assert_eq!(parse_invocation(&args(&[])), Invocation::Add(vec![]));
    }

    #[test]
    fn a_flag_that_is_not_the_first_argument_is_part_of_the_task() {
        assert_eq!(
            parse_invocation(&args(&["fix", "the", "--version", "bug"])),
            Invocation::Add(args(&["fix", "the", "--version", "bug"]))
        );
    }

    #[test]
    fn double_dash_makes_everything_after_it_the_task() {
        assert_eq!(
            parse_invocation(&args(&["--", "--version", "bump"])),
            Invocation::Add(args(&["--version", "bump"]))
        );
        assert_eq!(parse_invocation(&args(&["--"])), Invocation::Add(vec![]));
    }

    #[test]
    fn add_is_the_default_command_and_can_be_written_out() {
        assert_eq!(
            parse_invocation(&args(&["add", "Buy", "milk"])),
            Invocation::Add(args(&["Buy", "milk"]))
        );
        assert_eq!(parse_invocation(&args(&["add"])), Invocation::Add(vec![]));
        assert_eq!(
            parse_invocation(&args(&["add", "--", "--version", "bump"])),
            Invocation::Add(args(&["--version", "bump"]))
        );
        assert_eq!(parse_invocation(&args(&["add", "--help"])), Invocation::Help);
        assert_eq!(parse_invocation(&args(&["add", "-V"])), Invocation::Version);
    }

    #[test]
    fn refresh_and_search_are_commands_when_they_are_the_first_argument() {
        assert_eq!(parse_invocation(&args(&["refresh"])), Invocation::Refresh(vec![]));
        assert_eq!(
            parse_invocation(&args(&["search", "label:Low", "due:today"])),
            Invocation::Search(args(&["label:Low", "due:today"]))
        );
    }

    #[test]
    fn a_task_starting_with_a_command_word_needs_add_or_quotes_or_double_dash() {
        // As separate words, the first one is a command.
        assert_eq!(
            parse_invocation(&args(&["search", "the", "attic"])),
            Invocation::Search(args(&["the", "attic"]))
        );
        // The escapes.
        assert_eq!(
            parse_invocation(&args(&["add", "search", "the", "attic"])),
            Invocation::Add(args(&["search", "the", "attic"]))
        );
        assert_eq!(
            parse_invocation(&args(&["--", "search", "the", "attic"])),
            Invocation::Add(args(&["search", "the", "attic"]))
        );
        // A single quoted argument is not the bare word, so it is a task.
        assert_eq!(
            parse_invocation(&args(&["search the attic"])),
            Invocation::Add(args(&["search the attic"]))
        );
    }

    #[test]
    fn command_words_are_only_recognised_exactly() {
        for word in ["Search", "ADD", "refreshing", "adding", "searches"] {
            assert_eq!(
                parse_invocation(&args(&[word, "x"])),
                Invocation::Add(args(&[word, "x"])),
                "{word}"
            );
        }
        // Only the first argument counts.
        assert_eq!(
            parse_invocation(&args(&["buy", "add", "milk"])),
            Invocation::Add(args(&["buy", "add", "milk"]))
        );
    }

    #[test]
    fn the_help_text_covers_the_options_and_the_task_syntax() {
        for text in [
            "--help", "--version", "--", "GODSPEED_TOKEN", ".label", "@list", "~duration", ":date", "n: text",
            "add", "EXIT STATUS", "search the attic",
        ] {
            assert!(HELP.contains(text), "{text} is not in the help");
        }
    }
}
