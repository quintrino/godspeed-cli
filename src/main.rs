mod api;
mod capture;
mod eval;
mod exit;
mod names;
mod notify;
mod paths;
mod present;
mod queue;
mod query;
mod refresh;
mod search;
mod store;
mod task;
mod token;

use std::env;
use std::process::ExitCode;

const HELP: &str = r#"Create a task in Godspeed from the command line.

USAGE:
    godspeed-cli [add] [--] <TASK>...
    echo "<TASK>" | godspeed-cli
    godspeed-cli refresh [--include-completed] [--json]
    godspeed-cli search [--json] [QUERY]...
    godspeed-cli --help | --version

COMMANDS:
    add       Create a task. This is the default, so `add` can be left out.
    refresh   Download all your tasks, lists and labels into a local cache (for searching).
              Incomplete tasks only, unless --include-completed is given. Replaces the
              previous copy, and keeps it if the download fails. --json prints the counts as
              JSON. The API limits reads, so a large account can take several minutes.
    search    Search the local copy made by `refresh`, offline. Run
              `godspeed-cli search --help` for the query syntax.

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

const SEARCH_HELP: &str = r#"Search the local copy of your tasks made by `godspeed-cli refresh`. Works offline.

USAGE:
    godspeed-cli search [--json] [QUERY]...

By default only incomplete tasks are searched. Add is:complete to include completed ones
(they are only there if you refreshed with --include-completed). With no query, every
incomplete task is listed. The words of the query are joined with spaces, so quote the whole
query when it contains characters the shell would use, such as ( ) | & ! < > "

EXAMPLES:
    godspeed-cli search invoice
    godspeed-cli search 'due:20261031 label:Low'
    godspeed-cli search 'is:complete label:Low sort-desc:created'
    godspeed-cli search 'title:"tax return" list:Admin'
    godspeed-cli search '(label:Low | label:Medium) due<=2026-10-31'
    godspeed-cli search 'has:notes duration>=30m group:list sort-desc:due limit:20'

MATCHING:
    word                 Case-insensitive; found anywhere in the title, notes or attachment names
    "exact phrase"       The phrase, found anywhere in the same text
    a b                  Both (a space means AND), as does a & b
    a | b                Either
    !a  or  -a           Not a
    ( ... )              Grouping. | binds loosest, then AND, then !
    Anything else that looks like operator:value but isn't an operator below is a plain word.

FIELDS (case-insensitive, matched anywhere in the value; quote values with spaces):
    title:VALUE  notes:VALUE  attachment:VALUE
    list:VALUE           The list's name
    folder:VALUE         The list's path, or its name if it has none (in:VALUE is the same)
    label:VALUE          A label's name
    label-group:VALUE    The group a label belongs to

DATES (due, first-due, snoozed, start, created):
    due:DATE             On that date. due=DATE is the same
    due<DATE  due<=DATE  due>DATE  due>=DATE
    due is the later of the due date and the snooze date; first-due ignores snoozes.
    A task without that date never matches a date test (so !due:today does match it).
    DATE is today, tomorrow, yesterday, YYYYMMDD, YYYY-MM-DD, or relative: 3d, -2d, 2w, 24h
    (hours are rounded down to whole days). After : or = you can also use the periods
    week, next-week, month and year (weeks run Monday to Sunday).

NUMBERS:
    duration<30m  duration>=1.5h  duration:45     Minutes, or with an m or h suffix
    snooze-count=2  indent-level>0
    <  <=  >  >=  =  (or : for =). A task without the value never matches.

TRAITS:
    has:notes  has:attachment  has:label  has:repeat  has:due  has:snooze  has:start
    has:due-time  has:start-time  has:bookmark  has:email  has:duration
    has:any              Has at least one of the traits above (not counting the two time ones)

STATES:
    is:complete          Completed tasks; adds them to the search
    is:available         No start date or snooze still in the future
    is:uncleared         Completed but not yet cleared

PRESENTATION:
    sort:FIELD  sort-asc:FIELD  sort-desc:FIELD     Repeat to sort by several fields in turn
        FIELD is title, list, label, due, first-due, snoozed, start, created, updated,
        duration, snooze-count or indent-level. Tasks without a value come last.
        Put names first with a priority list: sort:label:High|Medium|Low
    group:FIELD          list, label, due, first-due, snoozed, start or created
        group:list:Work|Home puts those groups first. A task with several labels is
        shown under one, unless enable:duplicates is given.
    limit:N              At most N tasks, after sorting
    enable:duplicates  disable:duplicates
    enable:nested and disable:nested are not supported and are rejected.

OPTIONS:
    --json               Print the tasks as JSON (the full records, plus list_names and
                         label_names), or as {"group", "tasks"} objects when grouping. The
                         table shows the title (80 characters), list (12), due date and
                         labels (20), cut short with an ellipsis if longer.
    --                   End the options, so the query can contain --json.
    -h, --help           Print this help.

EXIT STATUS:
    0    The search ran (even if nothing matched).
    1    The local copy couldn't be read.
    2    The query is invalid, or `refresh` hasn't been run yet.

Messages ("No matching tasks.", a note that the copy is old) go to stderr, so stdout holds
only the results.
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
    SearchHelp,
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
        "refresh" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::Help,
            _ => Invocation::Refresh(rest.to_vec()),
        },
        "search" => match rest.first().map(String::as_str) {
            Some("-h" | "--help") => Invocation::SearchHelp,
            _ => Invocation::Search(rest.to_vec()),
        },
        _ => Invocation::Add(args.to_vec()),
    }
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
        Invocation::Refresh(refresh_args) => refresh::run_refresh(&refresh_args),
        Invocation::Search(search_args) => search::run_search(&search_args),
        Invocation::SearchHelp => {
            print!("godspeed-cli {}\n{SEARCH_HELP}", env!("CARGO_PKG_VERSION"));
            exit::SUCCESS
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
            parse_invocation(&args(&["refresh", "--include-completed", "--json"])),
            Invocation::Refresh(args(&["--include-completed", "--json"]))
        );
        assert_eq!(parse_invocation(&args(&["refresh", "--help"])), Invocation::Help);
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
    fn search_help_is_available_and_the_search_command_still_takes_a_query() {
        assert_eq!(parse_invocation(&args(&["search", "--help"])), Invocation::SearchHelp);
        assert_eq!(parse_invocation(&args(&["search", "-h"])), Invocation::SearchHelp);
        assert_eq!(
            parse_invocation(&args(&["search", "-label:Low", "--json"])),
            Invocation::Search(args(&["-label:Low", "--json"]))
        );
    }

    #[test]
    fn the_search_help_documents_every_operator() {
        for text in [
            "title:VALUE", "notes:VALUE", "attachment:VALUE", "list:VALUE", "folder:VALUE", "in:VALUE",
            "label:VALUE", "label-group:VALUE", "due:DATE", "due<=DATE", "first-due", "snoozed", "start", "created",
            "duration<30m", "snooze-count=2", "indent-level>0", "has:notes", "has:attachment", "has:label",
            "has:repeat", "has:due", "has:snooze", "has:start", "has:due-time", "has:start-time", "has:bookmark",
            "has:email", "has:duration", "has:any", "is:complete", "is:available", "is:uncleared", "sort:FIELD",
            "sort-asc:FIELD", "sort-desc:FIELD", "group:FIELD", "limit:N", "enable:duplicates", "disable:duplicates",
            "enable:nested", "--json", "EXIT STATUS", "next-week", "sort:label:High|Medium|Low", "group:list:Work|Home",
        ] {
            assert!(SEARCH_HELP.contains(text), "{text} is not in the search help");
        }
    }

    #[test]
    fn the_help_text_covers_the_options_and_the_task_syntax() {
        for text in [
            "--help", "--version", "--", "GODSPEED_TOKEN", ".label", "@list", "~duration", ":date", "n: text",
            "add", "EXIT STATUS", "search the attic", "refresh", "--include-completed", "search [--json]",
        ] {
            assert!(HELP.contains(text), "{text} is not in the help");
        }
    }
}
