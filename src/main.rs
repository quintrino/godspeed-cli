mod api;
mod capture;
mod config;
mod dates;
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
    godspeed-cli add --json [--] <TASK>...
    echo "<TASK>" | godspeed-cli
    godspeed-cli refresh [--full] [--include-completed] [--json]
    godspeed-cli search [--json] [--refresh] [--max-age AGE] [QUERY]...
    godspeed-cli --help | --version

COMMANDS:
    add       Create a task. This is the default, so `add` can be left out. `add --json`
              prints the API's response to the task as JSON on success (as the first
              argument after `add`; use `add -- --json ...` for a task that starts with it).
    refresh   Keep a local copy of your tasks, lists and labels, for searching. The first
              refresh downloads everything; after that only what changed is fetched, which
              is quick. --full rebuilds the copy from scratch (this also drops tasks deleted
              in Godspeed, so it happens by itself when the last full refresh is a week old).
              Incomplete tasks only, unless --include-completed is given; switching that on
              or off does a full refresh. A failed refresh keeps the previous copy. --json
              prints a report. The API limits reads, so a full refresh of a large account
              can take several minutes.
    search    Search the local copy made by `refresh`, offline. Run
              `godspeed-cli search --help` for the query syntax. --refresh updates the
              copy first, and --max-age AGE does so if it is older than AGE.

SETTINGS FILE:
    $XDG_CONFIG_HOME/godspeed-cli/config.toml (usually ~/.config/godspeed-cli/config.toml)
    is optional and can hold max_age, include_completed and full_refresh_after. Flags
    override it. See the README.

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
The query language follows Godspeed's search operators guide.

USAGE:
    godspeed-cli search [--json] [--refresh] [--max-age AGE] [QUERY]...

By default only incomplete tasks are searched. is:complete (not part of Godspeed's guide) and
is:uncleared add completed tasks; they are only in the local copy if you refreshed with
--include-completed. With no query, every incomplete task is listed. The words of the query
are joined with spaces, so quote the whole query when it contains characters the shell would
use, such as ( ) | & ! < > "

EXAMPLES:
    godspeed-cli search research pdf
    godspeed-cli search 'feature & (bug | error)'
    godspeed-cli search 'research -"budget meeting"'
    godspeed-cli search 'research in:"engineering q2"'
    godspeed-cli search 'due<=3h'
    godspeed-cli search 'snoozed>="Oct 14th 12am" & snoozed<="Oct 14th 1159pm"'
    godspeed-cli search 'due:today group:list sort:due'
    godspeed-cli search 'duration>="1 hour and 30 minutes" has:notes limit:10'

MATCHING:
    word                 Ignores case and word order; found in the title, notes or attachment names
    "exact phrase"       The exact characters, spaces included
    a b                  Both words (a space means AND), as does a & b
    a | b                Either
    !a  or  -a           Not a. Works on words, phrases and every operator below
    ( ... )              Grouping, which can be nested. | binds loosest, then AND, then !
    Anything that looks like operator:value but isn't an operator below is a plain word.

TITLE, NOTES, ATTACHMENT:
    title:TEXT  notes:TEXT  attachment:TEXT      (attachment searches the file name only)

LISTS, FOLDERS, LABELS (a name, or an id; names match anywhere in the name, quote them if they
contain spaces):
    list:VALUE           Tasks in a list
    folder:VALUE         Tasks in a list inside a folder with that name
    in:VALUE             Tasks in a list or in a folder
    label:VALUE          Tasks with a label
    label-group:VALUE    Tasks with a label in that label group

DATES (due, first-due, snoozed, start, created):
    due<DATE  due<=DATE  due>DATE  due>=DATE
    due:today  due:tomorrow  due:week  due:next-week  due:month  due:year
    due is the effective due date, the later of the due date and the snooze date; first-due is
    the due date alone. A task without that date never matches (so !due:today does match it).
    DATE is read as in Godspeed's date picker: 3h, 2d, 1w, -2d, in 3 days, tomorrow, friday,
    Oct 14th, "Oct 14th 12am", 2026-10-31, 20261031, noon, 3pm. Quote it if it has spaces.
    A date with no time covers the whole day: due<=2026-10-31 includes things due that day.
    With a time, or a relative amount like 3h, it is that exact moment. A task's date with no
    time counts as the start of its day. due:DATE with a date means that calendar day.
    Weeks run Monday to Sunday. A month and day with no year is the closest one to today.

DURATION, SNOOZE COUNT, INDENT LEVEL (<  <=  >  >=  =):
    duration<15m  duration>=1.5h  duration>="1 hour and 30 minutes"  duration=45
    snooze-count>=10     indent-level=0 finds top-level tasks
    A task without the value never matches.

TRAITS:
    has:notes  has:attachment  has:label  has:repeat  has:due  has:snooze  has:start
    has:due-time  has:start-time  has:bookmark  has:email  has:duration
    has:any              Every task

STATES:
    is:available         No start date, or a start date in the past (snoozes don't matter)
    is:uncleared         Completed but not yet cleared. Incomplete tasks never match
    is:complete          Completed tasks (not in Godspeed's guide)

GROUPING:
    group:list  group:label  group:label-group  group:due  group:start  group:due-or-start
    group:created        (first-due and snoozed work too)
        With several labels or label groups, the first alphabetically is used.
        due is the date of the effective due date; due-or-start uses the start date if there
        is no due date. Tasks with no value go in a group called None.
        Target lists: group:label:p0|p1|p2 makes those groups, in that order, and puts any
        task with none of them in None. group:list:Morning|"After lunch" quotes names with
        spaces, and ids work too. End with ...rest (group:label:Bug|Feature|...rest) and tasks
        matching no target are grouped normally instead of going into None.

SORTING:
    sort:FIELD  sort-asc:FIELD  sort-desc:FIELD     Ascending is the default; repeat to sort
        by several fields in turn. FIELD is due, first-due, snoozed, start, created, modified,
        title, label, duration, snooze-count, or list (the order of your sidebar; also
        indent-level). Tasks without a value come last.
        label and list take a target list, as in sort:label:High|Medium|Low
    limit:N              At most N tasks, after sorting

NESTED AND DUPLICATE TASKS:
    enable:duplicates  disable:duplicates   With duplicates on, a task with several labels,
        label groups or lists appears in each group when grouping.
    enable:nested  disable:nested           Accepted, but results are always a flat list.

OPTIONS:
    --refresh            Update the local copy first (an incremental refresh when possible).
                         Needs a token. If the refresh fails, the existing copy is searched, with
                         a note on stderr.
    --max-age AGE        Update the copy first if it is older than AGE, such as 30m, 6h, "2 days"
                         or 0 (always). "never" turns off a max_age from the settings file.
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
    /// `add --json`: the same, then print the API's response.
    AddJson(Vec<String>),
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
            Some("--json") => match rest.get(1).map(String::as_str) {
                Some("--") => Invocation::AddJson(rest[2..].to_vec()),
                _ => Invocation::AddJson(rest[1..].to_vec()),
            },
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
        Invocation::Add(task_args) => capture::run_add(&task_args, false),
        Invocation::AddJson(task_args) => capture::run_add(&task_args, true),
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
    fn add_json_is_only_a_flag_straight_after_add() {
        assert_eq!(
            parse_invocation(&args(&["add", "--json", "Buy", "milk"])),
            Invocation::AddJson(args(&["Buy", "milk"]))
        );
        assert_eq!(parse_invocation(&args(&["add", "--json"])), Invocation::AddJson(vec![]));
        assert_eq!(
            parse_invocation(&args(&["add", "--json", "--", "--json", "x"])),
            Invocation::AddJson(args(&["--json", "x"]))
        );
        // After the task text, or after `--`, it is just text; and without `add` it is a task.
        assert_eq!(
            parse_invocation(&args(&["add", "Buy", "--json"])),
            Invocation::Add(args(&["Buy", "--json"]))
        );
        assert_eq!(
            parse_invocation(&args(&["add", "--", "--json", "x"])),
            Invocation::Add(args(&["--json", "x"]))
        );
        assert_eq!(
            parse_invocation(&args(&["--json", "x"])),
            Invocation::Add(args(&["--json", "x"]))
        );
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
            "title:TEXT", "notes:TEXT", "attachment:TEXT", "list:VALUE", "folder:VALUE", "in:VALUE",
            "label:VALUE", "label-group:VALUE", "due<DATE", "due<=DATE", "due>DATE", "due>=DATE", "due:today",
            "due:tomorrow", "due:week", "due:next-week", "due:month", "due:year", "first-due", "snoozed", "start",
            "created", "duration<15m", "snooze-count>=10", "indent-level=0", "has:notes", "has:attachment",
            "has:label", "has:repeat", "has:due", "has:snooze", "has:start", "has:due-time", "has:start-time",
            "has:bookmark", "has:email", "has:duration", "has:any", "is:complete", "is:available", "is:uncleared",
            "sort:FIELD", "sort-asc:FIELD", "sort-desc:FIELD", "limit:N", "enable:duplicates", "disable:duplicates",
            "enable:nested", "disable:nested", "--json", "EXIT STATUS", "group:label:p0|p1|p2", "...rest",
            "group:due-or-start", "group:label-group", "modified", "sidebar", "None", "sort:label:High|Medium|Low",
            "\"exact phrase\"", "in 3 days", "\"Oct 14th 12am\"", "--refresh", "--max-age AGE",
        ] {
            assert!(SEARCH_HELP.contains(text), "{text} is not in the search help");
        }
    }

    #[test]
    fn the_help_text_covers_the_options_and_the_task_syntax() {
        for text in [
            "--help", "--version", "--", "GODSPEED_TOKEN", ".label", "@list", "~duration", ":date", "n: text",
            "add", "EXIT STATUS", "search the attic", "refresh", "--include-completed", "search [--json]", "--full", "--max-age", "SETTINGS FILE",
        ] {
            assert!(HELP.contains(text), "{text} is not in the help");
        }
    }
}
