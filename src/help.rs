//! The text printed by `--help`.

pub const HELP: &str = r#"Create tasks in Godspeed, and search them, from the command line.

USAGE:
    godspeed-cli add [--json] [--] <TASK>...
    echo "<TASK>" | godspeed-cli add
    godspeed-cli refresh [--full] [--include-completed] [--json]
    godspeed-cli search [--json] [--refresh] [--max-age AGE] [--color WHEN] [QUERY]...
    godspeed-cli completions <bash|fish|zsh>
    godspeed-cli --help | --version

COMMANDS:
    add          Create a task. The task is the rest of the line, or stdin if there is none.
                 `add --json` prints the API's response to the task as JSON on success (as
                 the first argument after `add`; use `add -- --json ...` for a task that
                 starts with it).
    refresh      Keep a local copy of your tasks, lists and labels, for searching. The first
                 refresh downloads everything; after that only what changed is fetched, which
                 is quick. --full rebuilds the copy from scratch (this also drops tasks
                 deleted in Godspeed, so it happens by itself when the last full refresh is a
                 week old). Incomplete tasks only, unless --include-completed is given;
                 switching that on or off does a full refresh. A failed refresh keeps the
                 previous copy. --json prints a report. The API limits reads, so a full
                 refresh of a large account can take several minutes.
    search       Search the local copy made by `refresh`, offline. Run
                 `godspeed-cli search --help` for the query syntax. --refresh updates the
                 copy first, and --max-age AGE does so if it is older than AGE.
    completions  Print a script that adds tab completion for godspeed-cli to your shell,
                 including the names of your lists and labels. Run
                 `godspeed-cli completions --help` for how to install it.

    There are no other commands. A task is always created with `add`, so
    `godspeed-cli "buy milk"` is an error, and `godspeed-cli add "buy milk"` is right.

SETTINGS FILE:
    $XDG_CONFIG_HOME/godspeed-cli/config.toml (usually ~/.config/godspeed-cli/config.toml)
    is optional and can hold max_age, include_completed and full_refresh_after. Flags
    override it. See the README.

OPTIONS:
    -h, --help       Print this help and exit.
    -V, --version    Print the version and exit.

TASK SYNTAX (for `add`):
    .label       Add a label (single word). Removed from the title.
    @list        Put the task in a list (single word, only one allowed).
    ~duration    Set the duration, e.g. ~30m, ~1.5h or ~45 (minutes). The older :45 also works.
    :date        Set the due date: :today, :tomorrow, :next-week, :20261031 or :2026-10-31.
    n: text      Everything after " n:" becomes the task's notes.

    Quote the task so the shell leaves ~ and @ alone (zsh can expand an unquoted ~).

    Example: godspeed-cli add "Write blog post @Work .Writing ~2h :tomorrow n: Include code samples"

ENVIRONMENT:
    GODSPEED_TOKEN    Your Godspeed API key (required to create tasks). If it isn't set,
                      an `export GODSPEED_TOKEN="..."` line in
                      $XDG_DATA_HOME/environment/Godspeed (usually
                      ~/.local/share/environment/Godspeed) is used.

EXIT STATUS:
    0    Success.
    1    Something failed, such as the API being unreachable. A task that couldn't be sent
         is cached and retried the next time `add` runs.
    2    The input can't be used: an unknown command, no token, an invalid task, or a bad
         query. Nothing is cached, because retrying wouldn't help.
"#;

pub const SEARCH_HELP: &str = r#"Search the local copy of your tasks made by `godspeed-cli refresh`. Works offline.
The query language follows Godspeed's search operators guide.

USAGE:
    godspeed-cli search [--json] [--refresh] [--max-age AGE] [--color WHEN] [QUERY]...

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
                         So  a | b c  means  a | (b c),  and  (a | b) c  means both a and b get c.
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
    limit:N              At most N tasks, counted in the order they are shown: after sorting and,
                         with group:, group by group. The first groups are kept, so a group you
                         list first (group:label:Eternal|...) is never cut to make room for a
                         later one. Groups the limit never reaches are not shown.

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
    --color WHEN         Show overdue tasks in red in the table. WHEN is auto (the default: only
                         on a terminal, and not if NO_COLOR is set or TERM is dumb), always (for
                         `| less -R`, and overriding NO_COLOR) or never. Only the table is
                         coloured, never JSON. A task is overdue when its effective due date (the
                         later of its due date and its snooze date) has passed: a date with no
                         time from the day after, a date with a time once that time has passed.
                         Completed tasks are never overdue.
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

pub const COMPLETIONS_HELP: &str = r#"Print a shell completion script for godspeed-cli.

USAGE:
    godspeed-cli completions <bash|fish|zsh>

INSTALL (the script is printed on stdout, so nothing is changed until you use it):
    bash    Add to ~/.bashrc:
                eval "$(godspeed-cli completions bash)"
    zsh     Add to ~/.zshrc, after the line that runs compinit:
                eval "$(godspeed-cli completions zsh)"
    fish    Add to ~/.config/fish/config.fish:
                godspeed-cli completions fish | source

WHAT IT COMPLETES:
    - the commands, their flags (--json, --full, --refresh, --max-age, ...) and the shell names
    - in `add`, the names of your lists after @ and of your labels after .
    - in `search`, list names after list: and in:, and label names after label:
      (including after ! - and an opening parenthesis)
    It does not complete operators such as due: or has:, or anything else in a query.

LISTS AND LABELS:
    Names come from the local copy made by `godspeed-cli refresh` and from the lists and
    labels already used when creating tasks. They are read from disk, so completion works
    offline and needs no token. Run `godspeed-cli refresh` once to have them all.
    A name with spaces is offered without them ("Work Projects" becomes WorkProjects),
    because a task's @list and .label must be one word; godspeed-cli accepts that form.
    Names containing characters a shell could misread are left out.
"#;

#[cfg(test)]
mod tests {
    use super::*;

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
            "\"exact phrase\"", "in 3 days", "\"Oct 14th 12am\"", "--refresh", "--max-age AGE", "--color WHEN", "NO_COLOR", "overdue",
        ] {
            assert!(SEARCH_HELP.contains(text), "{text} is not in the search help");
        }
    }

    #[test]
    fn the_help_text_covers_the_commands_the_options_and_the_task_syntax() {
        for text in [
            "--help", "--version", "GODSPEED_TOKEN", ".label", "", "~duration", ":date", "n: text", "add",
            "EXIT STATUS", "refresh", "--include-completed", "search [--json]", "--full", "--max-age",
            "SETTINGS FILE", "completions", "godspeed-cli add \"buy milk\"", "There are no other commands",
        ] {
            assert!(HELP.contains(text), "{text} is not in the help");
        }
    }

    #[test]
    fn the_main_help_no_longer_describes_creating_a_task_without_add() {
        assert!(!HELP.contains("default, so"), "add is not the default any more");
        assert!(!HELP.contains("[add]"));
        assert!(!HELP.contains("echo \"<TASK>\" | godspeed-cli\n"));
        assert!(HELP.contains("echo \"<TASK>\" | godspeed-cli add"));
    }

    #[test]
    fn the_completions_help_says_how_to_install_for_each_shell_and_what_is_left_out() {
        for text in [
            "godspeed-cli completions <bash|fish|zsh>", "eval \"$(godspeed-cli completions bash)\"",
            "eval \"$(godspeed-cli completions zsh)\"", "godspeed-cli completions fish | source", "compinit",
            "list:", "in:", "label:", "due:", "has:", "WorkProjects", "offline",
        ] {
            assert!(COMPLETIONS_HELP.contains(text), "{text} is not in the completions help");
        }
    }
}
