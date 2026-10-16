//! The text printed by `--help`, and the code that lays it out for the width of the terminal.
//!
//! Each help page is written as a small marked-up source, and [`render`] wraps it to a width:
//!
//! - `## TITLE` is a heading, printed as `TITLE:`.
//! - `> text` is a verbatim line (usage, examples), printed indented and never wrapped.
//! - `- term => description` is an item. The terms of a run of items are lined up and the
//!   descriptions wrapped beside them; a term that is too long, or a window that is too narrow,
//!   puts the description on the lines below instead. Lines starting with two spaces carry the
//!   description on.
//! - Any other line is part of a paragraph, which is wrapped. A blank line ends a block.
//!
//! The pages can be written in short lines of any length: they are joined and wrapped again.

use std::io;
use terminal_size::{terminal_size_of, Width};

/// The width used when the terminal's can't be found.
const DEFAULT_WIDTH: usize = 80;

/// The narrowest layout used, however narrow the window is.
const MIN_WIDTH: usize = 40;

/// The longest term that is lined up beside its description.
const MAX_ALIGNED_TERM: usize = 22;

/// The width to use: the terminal's if it is known, else `COLUMNS` if it holds a number, else 80.
/// Never less than 40, which keeps the layout readable in a very narrow window.
pub fn choose_width(terminal: Option<usize>, columns: Option<&str>) -> usize {
    let from_columns = columns.and_then(|value| value.trim().parse::<usize>().ok());
    terminal
        .filter(|width| *width > 0)
        .or(from_columns.filter(|width| *width > 0))
        .unwrap_or(DEFAULT_WIDTH)
        .max(MIN_WIDTH)
}

/// The width of the window this is running in. Output is often piped (`--help | less`), so the
/// first of stdout, stderr and stdin that is a terminal is asked.
pub fn terminal_width() -> usize {
    let size = terminal_size_of(io::stdout())
        .or_else(|| terminal_size_of(io::stderr()))
        .or_else(|| terminal_size_of(io::stdin()));
    choose_width(size.map(|(Width(columns), _)| usize::from(columns)), std::env::var("COLUMNS").ok().as_deref())
}

#[derive(Debug)]
enum Block {
    Heading(String),
    Verbatim(Vec<String>),
    Items(Vec<(String, String)>),
    Paragraph(String),
    Gap,
}

fn parse(source: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for line in source.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            blocks.push(Block::Heading(title.trim().to_string()));
        } else if line.trim().is_empty() {
            if !matches!(blocks.last(), Some(Block::Gap) | None) {
                blocks.push(Block::Gap);
            }
        } else if let Some(text) = line.strip_prefix('>') {
            let text = text.strip_prefix(' ').unwrap_or(text).to_string();
            match blocks.last_mut() {
                Some(Block::Verbatim(lines)) => lines.push(text),
                _ => blocks.push(Block::Verbatim(vec![text])),
            }
        } else if let Some(item) = line.strip_prefix("- ") {
            let (term, description) = match item.split_once(" =>") {
                Some((term, description)) => (term.trim(), description.trim()),
                None => (item.trim(), ""),
            };
            let item = (term.to_string(), description.to_string());
            match blocks.last_mut() {
                Some(Block::Items(items)) => items.push(item),
                _ => blocks.push(Block::Items(vec![item])),
            }
        } else if line.starts_with("  ") && matches!(blocks.last(), Some(Block::Items(_))) {
            if let Some(Block::Items(items)) = blocks.last_mut() {
                let description = &mut items.last_mut().expect("an item comes first").1;
                if !description.is_empty() {
                    description.push(' ');
                }
                description.push_str(line.trim());
            }
        } else {
            match blocks.last_mut() {
                Some(Block::Paragraph(text)) => {
                    text.push(' ');
                    text.push_str(line.trim());
                }
                _ => blocks.push(Block::Paragraph(line.trim().to_string())),
            }
        }
    }
    blocks
}

/// Wraps `text` to `width` columns. The first line starts with `first_indent` spaces and the
/// others with `rest_indent`. A word longer than a line is left whole on a line of its own.
fn wrap(text: &str, first_indent: usize, rest_indent: usize, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = " ".repeat(first_indent);
    let mut has_word = false;
    for word in text.split_whitespace() {
        let length = word.chars().count();
        if has_word && line.chars().count() + 1 + length > width {
            lines.push(std::mem::replace(&mut line, " ".repeat(rest_indent)));
            has_word = false;
        }
        if has_word {
            line.push(' ');
        }
        line.push_str(word);
        has_word = true;
    }
    if has_word {
        lines.push(line);
    }
    lines
}

/// The line or lines for a term on its own. It is printed as written if it fits; a longer one,
/// such as `title:TEXT  notes:TEXT  attachment:TEXT`, is wrapped at its spaces.
fn term_lines(term: &str, width: usize) -> Vec<String> {
    if 4 + term.chars().count() <= width {
        vec![format!("    {term}")]
    } else {
        wrap(term, 4, 6, width)
    }
}

fn render_items(items: &[(String, String)], width: usize) -> Vec<String> {
    let aligned_width = items
        .iter()
        .map(|(term, _)| term.chars().count())
        .filter(|length| *length <= MAX_ALIGNED_TERM)
        .max()
        .unwrap_or(0);
    let description_column = 4 + aligned_width + 2;
    // With too little room beside the terms, every description goes on the lines below.
    let stacked = width < description_column + 24;

    let mut lines = Vec::new();
    for (term, description) in items {
        if description.is_empty() {
            lines.extend(term_lines(term, width));
        } else if stacked || term.chars().count() > MAX_ALIGNED_TERM {
            lines.extend(term_lines(term, width));
            lines.extend(wrap(description, 8, 8, width));
        } else {
            let wrapped = wrap(description, description_column, description_column, width);
            for (index, line) in wrapped.iter().enumerate() {
                if index == 0 {
                    lines.push(format!("    {term:<aligned_width$}  {}", &line[description_column..]));
                } else {
                    lines.push(line.clone());
                }
            }
        }
    }
    lines
}

/// Lays a help page out in `width` columns (at least 40).
pub fn render(source: &str, width: usize) -> String {
    let width = width.max(MIN_WIDTH);
    let mut lines: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut gap_before_next = false;
    let mut after_heading = false;
    for block in parse(source) {
        match block {
            Block::Gap => gap_before_next = !after_heading,
            Block::Heading(title) => {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.extend(wrap(&format!("{title}:"), 0, 2, width));
                in_section = true;
                gap_before_next = false;
                after_heading = true;
            }
            other => {
                if gap_before_next && lines.last().is_some_and(|line| !line.is_empty()) {
                    lines.push(String::new());
                }
                gap_before_next = false;
                after_heading = false;
                let indent = if in_section { 4 } else { 0 };
                match other {
                    Block::Paragraph(text) => lines.extend(wrap(&text, indent, indent, width)),
                    Block::Verbatim(verbatim) => {
                        lines.extend(verbatim.iter().map(|line| if line.is_empty() { String::new() } else { format!("    {line}") }));
                    }
                    Block::Items(items) => lines.extend(render_items(&items, width)),
                    Block::Gap | Block::Heading(_) => unreachable!("handled above"),
                }
            }
        }
    }
    lines.join("\n") + "\n"
}

pub const HELP: &str = r#"
Create tasks in Godspeed, and search them, from the command line.

## USAGE
> godspeed-cli add [--json] [--] <TASK>...
> echo "<TASK>" | godspeed-cli add
> godspeed-cli refresh [--full] [--include-completed] [--json]
> godspeed-cli search [--json] [--refresh] [--max-age AGE] [--color WHEN] [QUERY]...
> godspeed-cli completions <bash|fish|zsh>
> godspeed-cli <command> --help
> godspeed-cli --help | --version

## COMMANDS
- add => Create a task. The task is the rest of the line, or stdin if there is none. `godspeed-cli add --help` explains the task syntax and how list and label names are matched.
- refresh => Keep a local copy of your tasks, lists and labels, for searching. The first refresh downloads everything; after that only what changed is fetched, which is quick. --full rebuilds the copy from scratch. `godspeed-cli refresh --help` has the details.
- search => Search the local copy made by `refresh`, offline. `godspeed-cli search --help` explains the query syntax.
- completions => Print a script that adds tab completion for godspeed-cli to your shell, including the names of your lists and labels. `godspeed-cli completions --help` says how to install it.

There are no other commands. A task is always created with `add`, so `godspeed-cli "buy milk"` is an error, and `godspeed-cli add "buy milk"` is right.

## OPTIONS
- -h, --help => Print this help and exit. After a command, print that command's help.
- -V, --version => Print the version and exit.

## TASK SYNTAX (for add)
- .label => Add a label (single word). Removed from the title.
- @list => Put the task in a list (single word, only one allowed).
- ~duration => Set the duration, e.g. ~30m, ~1.5h or ~45 (minutes). The older :45 also works.
- :date => Set the due date: :today, :tomorrow, :next-week, :20261031 or :2026-10-31.
- n: text => Everything after " n:" becomes the task's notes.

Quote the task so the shell leaves ~ and @ alone (zsh can expand an unquoted ~). `godspeed-cli add --help` has the full rules.

> godspeed-cli add "Write blog post @Work .Writing ~2h :tomorrow n: Include code samples"

## SETTINGS FILE
$XDG_CONFIG_HOME/godspeed-cli/config.toml (usually ~/.config/godspeed-cli/config.toml) is optional, and flags override it. Durations are written like 30m, 12h or "1 day", and "never" turns a setting off.
- max_age => `search` refreshes first when the copy is older than this. Default: not set, so search only reads the copy.
- include_completed => true makes `refresh` keep completed tasks too. Default: false.
- full_refresh_after => A refresh is a full one when the last full refresh is this old. Default: "7d".

> max_age = "12h"
> include_completed = true
> full_refresh_after = "7d"

## ENVIRONMENT
- GODSPEED_TOKEN => Your Godspeed API key (required to create or refresh). If it isn't set, an `export GODSPEED_TOKEN="..."` line in $XDG_DATA_HOME/environment/Godspeed (usually ~/.local/share/environment/Godspeed) is used.
- NO_COLOR => If set to anything non-empty, `search` doesn't colour its output (unless --color always is given).
- COLUMNS => The width to lay this help out in when it can't be read from the terminal. The default is 80.

## EXIT STATUS
- 0 => Success.
- 1 => Something failed, such as the API being unreachable. A task that couldn't be sent is cached and retried the next time `add` runs.
- 2 => The input can't be used: an unknown command, no token, an invalid task, or a bad query. Nothing is cached, because retrying wouldn't help.
"#;

pub const ADD_HELP: &str = r#"
Create a task in Godspeed.

## USAGE
> godspeed-cli add [--json] [--] <TASK>...
> echo "<TASK>" | godspeed-cli add [--json]

The task is the rest of the line. With no text it is read from stdin, so a task with several lines can be piped in. Quote the task so the shell leaves characters like ~ and @ alone (zsh can expand an unquoted ~).

## OPTIONS
- --json => Print the API's response to the new task, as JSON, on success. It must come straight after `add`; after the task text it is just text.
- -- => End the options, so a task can start with --json, --help, -h, -V or --version.
- -h, --help => Print this help and exit.

## TASK SYNTAX
- .label => Add a label. Any number are allowed. A word that starts with a dot is always a label, so `.gitignore` is the label "gitignore".
- @list => Put the task in a list. Only one is allowed.
- ~duration => Set the duration: ~45 or ~45m for minutes, ~1.5h for hours. Fractions are rounded to whole minutes, and it has to be more than zero.
- :date => Set the due date, which has no time of day: :today, :tomorrow, :next-week, :YYYYMMDD or :YYYY-MM-DD, such as :20261031 or :2026-10-31.
- n: text => Everything after the first " n:" (with a space before it) is the task's notes. Nothing in the notes is read as syntax.

The other words, joined by single spaces, are the title. If a duration or a date is given twice, the last one wins. A word that looks like syntax but isn't, such as ~roughly, :someday or :20261345, stays in the title. A short number after a colon, such as :45, is a duration in minutes (the older syntax).

:today, :tomorrow and :next-week are turned into real dates when you write the task, so a task kept for a retry still gets the day you meant.

## HOW LISTS AND LABELS ARE MATCHED
Names are matched ignoring case, against your lists and labels as last seen. They are kept in lists.toml and labels.toml in the data directory, and fetched again from Godspeed once when a name isn't found.

@list tries these in turn, and uses the first that finds a list:
- exact => The whole name: @work finds "Work".
- prefix => A name that starts with it: @wo finds "Work".
- contains => A name that contains it: @ork finds "Work".
- no spaces => Part of the name, once spaces, slashes and dashes are left out of both: @workprojects, @work-proj and @kproj all find "Work Projects".

If several names are found at the same step, the shortest wins, then the first alphabetically.

.label tries the exact name, then a prefix, then the whole name with spaces, slashes and dashes left out (.workprojects finds "Work Projects"). It does not use "contains" or part of a name, so .work never picks "Homework". Labels are matched one by one, and the ones that aren't found are left out. If a label isn't in the names kept, the labels are fetched from Godspeed again first.

If nothing matches, the task is still created, without that list or label. A task with more than one @list, or with no title, is rejected (exit status 2) and is not queued.

## EXAMPLES
> godspeed-cli add "Buy milk"
> godspeed-cli add "Write blog post @Work .Writing ~2h :tomorrow n: Include code samples"
> godspeed-cli add --json "Pay rent @Home :20261101" | jq -r .todo_item.id
> echo "Plan the week @Work" | godspeed-cli add
> godspeed-cli add -- --version bump

## IF THE TASK CAN'T BE SENT
If the API can't be reached or refuses the task, the task is saved in the `cache` file in the data directory (usually ~/.local/share/godspeed-cli) and sent the next time `add` runs. Exit status 1 says it was queued. A task that can never be sent, such as one with two lists or no title, is rejected and never queued.

## ENVIRONMENT
- GODSPEED_TOKEN => Your Godspeed API key. If it isn't set, an `export GODSPEED_TOKEN="..."` line in $XDG_DATA_HOME/environment/Godspeed is used.

## EXIT STATUS
- 0 => The task was created.
- 1 => It couldn't be sent, and was queued for a retry.
- 2 => It was rejected: no token, two lists, or no title.
"#;

pub const REFRESH_HELP: &str = r#"
Keep a local copy of your tasks, lists and labels, for searching offline.

## USAGE
> godspeed-cli refresh [--full] [--include-completed] [--json]

## OPTIONS
- --full => Rebuild the copy from scratch.
- --include-completed => Keep completed tasks too. Switching this on or off makes the refresh a full one.
- --json => Print a report as JSON: mode, reason, changed_count, tasks_count, lists_count, labels_count, include_completed and refreshed_at.
- -h, --help => Print this help and exit.

## INCREMENTAL AND FULL
The first refresh is a full one: it downloads everything. After that a refresh is incremental: it asks only for what changed since the copy was last brought up to date, and adds or updates those tasks. That is usually two requests, however big the account. --full rebuilds the copy from scratch.

An incremental refresh can't see a task you deleted in Godspeed, so deleted tasks stay in the copy until a full refresh drops them.

## A REFRESH IS FULL WHEN
- --full => you ask for it.
- first time => there is no copy yet.
- completed => you switch --include-completed on or off.
- old => the last full refresh is a week old (see full_refresh_after below).
- unusable => the copy has no usable record of when it last changed.

## DETAILS
A full refresh pages through the whole account, 250 tasks at a time. Godspeed limits how fast it can be read, so at most 9 requests a minute are made and then it waits: about 2,000 tasks a minute. If the API says "too many requests" it waits as long as it is told and tries again.

Everything is downloaded before the copy is touched, so a refresh that fails part-way leaves the previous copy exactly as it was.

An incremental refresh asks for completed tasks too, even without --include-completed: a task you completed since the last refresh only shows up as completed, and has to be removed. Without --include-completed it is then not kept.

Progress, including which kind of refresh it is and why, goes to stderr, so --json on stdout stays clean.

## THE LOCAL COPY
It is $XDG_DATA_HOME/godspeed-cli/cache.sqlite3 (usually ~/.local/share/godspeed-cli/cache.sqlite3). It also holds facts about itself, so nothing has to be guessed: when it was refreshed, when it was last rebuilt, whether it holds completed tasks, and the newest change it has seen.

## SETTINGS FILE
$XDG_CONFIG_HOME/godspeed-cli/config.toml (usually ~/.config/godspeed-cli/config.toml) is optional, and flags override it.
- include_completed => true keeps completed tasks, as if --include-completed were given. Default: false.
- full_refresh_after => How old the last full refresh may get before the next refresh is a full one, such as "3d" or "12h". "never" turns it off. Default: "7d".

## EXAMPLES
> godspeed-cli refresh
> godspeed-cli refresh --full
> godspeed-cli refresh --include-completed
> godspeed-cli refresh --json

## ENVIRONMENT
- GODSPEED_TOKEN => Your Godspeed API key (required). If it isn't set, an `export GODSPEED_TOKEN="..."` line in $XDG_DATA_HOME/environment/Godspeed is used.

## EXIT STATUS
- 0 => The copy was refreshed.
- 1 => The refresh failed, and the previous copy was kept.
- 2 => It can't run: no token, or an argument it doesn't know.
"#;

pub const SEARCH_HELP: &str = r#"
Search the local copy of your tasks made by `godspeed-cli refresh`. Works offline. The query language follows Godspeed's search operators guide.

## USAGE
> godspeed-cli search [--json] [--refresh] [--max-age AGE] [--color WHEN] [QUERY]...

By default only incomplete tasks are searched. is:complete (not part of Godspeed's guide) and is:uncleared add completed tasks; they are only in the local copy if you refreshed with --include-completed. With no query, every incomplete task is listed. The words of the query are joined with spaces, so quote the whole query when it contains characters the shell would use, such as ( ) | & ! < > "

## EXAMPLES
> godspeed-cli search research pdf
> godspeed-cli search 'feature & (bug | error)'
> godspeed-cli search 'research -"budget meeting"'
> godspeed-cli search 'research in:"engineering q2"'
> godspeed-cli search 'due<=3h'
> godspeed-cli search 'snoozed>="Oct 14th 12am" & snoozed<="Oct 14th 1159pm"'
> godspeed-cli search 'due:today group:list sort:due'
> godspeed-cli search 'duration>="1 hour and 30 minutes" has:notes limit:10'

## MATCHING
- word => Ignores case and word order; found in the title, notes or attachment names
- "exact phrase" => The exact characters, spaces included
- a b => Both words (a space means AND), as does a & b
- a | b => Either
- !a  or  -a => Not a. Works on words, phrases and every operator below
- ( ... ) => Grouping, which can be nested. | binds loosest, then AND, then !. So `a | b c` means `a | (b c)`, and `(a | b) c` means both a and b get c.

Anything that looks like operator:value but isn't an operator below is a plain word.

## TITLE, NOTES, ATTACHMENT
- title:TEXT  notes:TEXT  attachment:TEXT => Just that field. attachment searches the file name only.

## LISTS, FOLDERS, LABELS
A name, or an id. Names match anywhere in the name, and also ignoring spaces, slashes and dashes. Quote a name that contains spaces.
- list:VALUE => Tasks in a list
- folder:VALUE => Tasks in a list inside a folder with that name
- in:VALUE => Tasks in a list or in a folder
- label:VALUE => Tasks with a label
- label-group:VALUE => Tasks with a label in that label group

## DATES (due, first-due, snoozed, start, created)
- due<DATE  due<=DATE  due>DATE  due>=DATE => Compare with a date.
- due:today  due:tomorrow  due:week  due:next-week  due:month  due:year => Due in that period.

due is the effective due date, the later of the due date and the snooze date; first-due is the due date alone. A task without that date never matches (so !due:today does match it).

DATE is read as in Godspeed's date picker: 3h, 2d, 1w, -2d, in 3 days, tomorrow, friday, Oct 14th, "Oct 14th 12am", 2026-10-31, 20261031, noon, 3pm. Quote it if it has spaces.

A date with no time covers the whole day: due<=2026-10-31 includes things due that day. With a time, or a relative amount like 3h, it is that exact moment. A task's date with no time counts as the start of its day. due:DATE with a date means that calendar day. Weeks run Monday to Sunday. A month and day with no year is the closest one to today.

## DURATION, SNOOZE COUNT, INDENT LEVEL (<  <=  >  >=  =)
- duration<15m  duration>=1.5h  duration>="1 hour and 30 minutes"  duration=45 => Duration.
- snooze-count>=10 => How many times it has been snoozed.
- indent-level=0 => Finds top-level tasks.

A task without the value never matches.

## TRAITS
- has:notes  has:attachment  has:label  has:repeat  has:due  has:snooze  has:start  has:due-time  has:start-time  has:bookmark  has:email  has:duration => The task has that.
- has:any => Every task.

## STATES
- is:available => No start date, or a start date in the past (snoozes don't matter)
- is:uncleared => Completed but not yet cleared. Incomplete tasks never match
- is:complete => Completed tasks (not in Godspeed's guide)

## GROUPING
- group:list  group:label  group:label-group  group:due  group:start  group:due-or-start  group:created => Group the results. first-due and snoozed work too.

With several labels or label groups, the first alphabetically is used. due is the date of the effective due date; due-or-start uses the start date if there is no due date. Tasks with no value go in a group called None.

Target lists: group:label:p0|p1|p2 makes those groups, in that order, and puts any task with none of them in None. group:list:Morning|"After lunch" quotes names with spaces, and ids work too. End with ...rest (group:label:Bug|Feature|...rest) and tasks matching no target are grouped normally instead of going into None.

## SORTING
- sort:FIELD  sort-asc:FIELD  sort-desc:FIELD => Ascending is the default; repeat to sort by several fields in turn. FIELD is due, first-due, snoozed, start, created, modified, title, label, duration, snooze-count, or list (the order of your sidebar; also indent-level). Tasks without a value come last. label and list take a target list, as in sort:label:High|Medium|Low
- limit:N => At most N tasks, counted in the order they are shown: after sorting and, with group:, group by group. The first groups are kept, so a group you list first (group:label:Eternal|...) is never cut to make room for a later one. Groups the limit never reaches are not shown.

## NESTED AND DUPLICATE TASKS
- enable:duplicates  disable:duplicates => With duplicates on, a task with several labels, label groups or lists appears in each group when grouping.
- enable:nested  disable:nested => Accepted, but results are always a flat list.

## OPTIONS
- --refresh => Update the local copy first (an incremental refresh when possible). Needs a token. If the refresh fails, the existing copy is searched, with a note on stderr.
- --max-age AGE => Update the copy first if it is older than AGE, such as 30m, 6h, "2 days" or 0 (always). "never" turns off a max_age from the settings file.
- --color WHEN => Colour the table: the header is bold cyan, the list is yellow, the due date of a task due today is magenta, and the due date of an overdue task is red. Never JSON. WHEN is auto (the default: only on a terminal, and not if NO_COLOR is set or TERM is dumb), always (for `| less -R`, and overriding NO_COLOR) or never. A task is overdue when its effective due date (the later of its due date and its snooze date) has passed: a date with no time from the day after, a date with a time once that time has passed. A task due today has that date as its effective due date and isn't overdue yet. Completed tasks are never either.
- --json => Print the tasks as JSON (the full records, plus list_names and label_names), or as {"group", "tasks"} objects when grouping. The table shows the title (80 characters), list (12), due date and labels (20), cut short with an ellipsis if longer.
- -- => End the options, so the query can contain --json.
- -h, --help => Print this help.

## EXIT STATUS
- 0 => The search ran (even if nothing matched).
- 1 => The local copy couldn't be read.
- 2 => The query is invalid, or `refresh` hasn't been run yet.

Messages ("No matching tasks.", a note that the copy is old) go to stderr, so stdout holds only the results.
"#;

pub const COMPLETIONS_HELP: &str = r#"
Print a shell completion script for godspeed-cli.

## USAGE
> godspeed-cli completions <bash|fish|zsh>

## INSTALL
The script is printed on stdout, so nothing is changed until you use it.
- bash => Add to ~/.bashrc: `eval "$(godspeed-cli completions bash)"`
- zsh => Add to ~/.zshrc, after the line that runs compinit: `eval "$(godspeed-cli completions zsh)"`
- fish => Add to ~/.config/fish/config.fish: `godspeed-cli completions fish | source`

## WHAT IT COMPLETES
- commands => the commands, their flags (--json, --full, --refresh, --max-age, --color, ...) and the shell names
- add => the names of your lists after @ and of your labels after .
- search => list names after list: and in:, and label names after label: (including after ! - and an opening parenthesis)

It does not complete operators such as due: or has:, or anything else in a query.

## LISTS AND LABELS
Names come from the local copy made by `godspeed-cli refresh` and from the lists and labels already used when creating tasks. They are read from disk, so completion works offline and needs no token. Run `godspeed-cli refresh` once to have them all.

A name with spaces is offered without them ("Work Projects" becomes WorkProjects), because a task's @list and .label must be one word; godspeed-cli accepts that form. Names containing characters a shell could misread are left out.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A page laid out so wide that nothing wraps, so phrases can be looked for in it.
    fn wide(source: &str) -> String {
        render(source, 100_000)
    }

    #[test]
    fn the_width_is_the_terminals_then_columns_then_eighty() {
        assert_eq!(choose_width(Some(120), Some("100")), 120);
        assert_eq!(choose_width(Some(61), None), 61);
        assert_eq!(choose_width(None, Some("100")), 100);
        assert_eq!(choose_width(None, Some(" 90 ")), 90);
        assert_eq!(choose_width(None, None), 80);
    }

    #[test]
    fn a_bad_width_is_ignored_and_a_tiny_one_is_raised() {
        assert_eq!(choose_width(None, Some("wide")), 80);
        assert_eq!(choose_width(None, Some("")), 80);
        assert_eq!(choose_width(None, Some("0")), 80);
        assert_eq!(choose_width(None, Some("-5")), 80);
        assert_eq!(choose_width(Some(0), Some("100")), 100, "a terminal that reports no width");
        assert_eq!(choose_width(Some(0), None), 80);
        assert_eq!(choose_width(Some(12), None), 40, "never narrower than 40");
        assert_eq!(choose_width(None, Some("20")), 40);
    }

    #[test]
    fn a_paragraph_is_wrapped_to_the_width_and_loses_no_words() {
        let text = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen";
        let out = render(text, 40);

        for line in out.lines() {
            assert!(line.chars().count() <= 40, "{line:?}");
        }
        assert_eq!(out.split_whitespace().collect::<Vec<_>>().join(" "), text);
        assert!(out.lines().count() > 1);
        assert_eq!(render(text, 100_000).lines().count(), 1, "one line when there is room");
    }

    #[test]
    fn source_lines_of_any_length_are_joined_before_wrapping() {
        let broken = "alpha beta\ngamma delta\nepsilon";
        assert_eq!(render(broken, 80), "alpha beta gamma delta epsilon\n");
    }

    #[test]
    fn a_word_longer_than_the_line_is_kept_whole() {
        let long = "x".repeat(60);
        let out = render(&format!("short {long} tail"), 40);
        assert!(out.lines().any(|line| line == long), "{out:?}");
        assert_eq!(out.split_whitespace().count(), 3);
    }

    #[test]
    fn headings_have_a_colon_and_a_blank_line_before_them() {
        let out = render("intro\n\n## FIRST\ntext\n## SECOND\nmore\n", 80);
        assert_eq!(out, "intro\n\nFIRST:\n    text\n\nSECOND:\n    more\n");
    }

    #[test]
    fn a_heading_too_long_for_the_window_is_wrapped_too() {
        let out = render("## DATES (due, first-due, snoozed, start, created)\ntext\n", 40);

        assert!(out.lines().all(|l| l.chars().count() <= 40), "{out}");
        assert_eq!(out.lines().next().unwrap(), "DATES (due, first-due, snoozed, start,");
        assert_eq!(out.lines().nth(1).unwrap(), "  created):");
        assert_eq!(render("## DATES (due, first-due)\nx\n", 80).lines().next().unwrap(), "DATES (due, first-due):");
    }

    #[test]
    fn a_leading_blank_line_in_the_source_is_not_printed() {
        assert_eq!(render("\n\nhello\n", 80), "hello\n");
        assert_eq!(render("\n## TOP\nx\n", 80), "TOP:\n    x\n");
    }

    #[test]
    fn verbatim_lines_are_indented_and_never_wrapped() {
        let long = format!("godspeed-cli add \"{}\"", "word ".repeat(30));
        let out = render(&format!("## EXAMPLES\n> {long}\n> short\n"), 40);
        assert_eq!(out, format!("EXAMPLES:\n    {long}\n    short\n"));
    }

    #[test]
    fn a_gap_after_a_block_is_kept_as_one_blank_line() {
        let out = render("## A\ntext\n\n\n\nmore text\n", 80);
        assert_eq!(out, "A:\n    text\n\n    more text\n");
    }

    #[test]
    fn items_have_their_terms_lined_up_and_their_descriptions_wrapped_beside_them() {
        let source = "## OPTIONS\n- -h => Print the help.\n- --json => Print the results as JSON and carry on to say much more than fits on one line of the window.\n";
        let out = render(source, 60);

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[1], "    -h      Print the help.");
        assert!(lines[2].starts_with("    --json  Print the results as JSON"), "{out}");
        let column = lines[2].find("Print").unwrap();
        assert_eq!(column, 12);
        assert!(lines[3..].iter().all(|l| l.starts_with(&" ".repeat(column)) && l.chars().count() <= 60), "{out}");
    }

    #[test]
    fn a_long_term_has_its_description_on_the_lines_below() {
        let source = "## SORTING\n- sort:FIELD  sort-asc:FIELD  sort-desc:FIELD => Ascending is the default.\n- limit:N => At most N tasks.\n";
        let out = render(source, 80);
        assert_eq!(
            out,
            "SORTING:\n    sort:FIELD  sort-asc:FIELD  sort-desc:FIELD\n        Ascending is the default.\n    limit:N  At most N tasks.\n"
        );
    }

    #[test]
    fn in_a_narrow_window_every_description_goes_below_its_term() {
        // The longest term is 19 columns, so at 40 columns there is no room beside it.
        let source = "## OPTIONS\n- --include-completed => Keep completed tasks.\n- -h => Print the help.\n";
        let out = render(source, 40);
        assert_eq!(out, "OPTIONS:\n    --include-completed\n        Keep completed tasks.\n    -h\n        Print the help.\n");
        // With more room they sit beside their terms.
        assert!(render(source, 80).lines().nth(1).unwrap().contains("--include-completed  Keep completed tasks."));
    }

    #[test]
    fn a_term_too_long_for_the_window_is_wrapped_at_its_spaces() {
        let source = "## X\n- title:TEXT  notes:TEXT  attachment:TEXT => Just that field.\n";
        let narrow = render(source, 40);

        assert!(narrow.lines().all(|l| l.chars().count() <= 40), "{narrow}");
        assert_eq!(narrow.lines().nth(1).unwrap(), "    title:TEXT notes:TEXT");
        assert_eq!(narrow.lines().nth(2).unwrap(), "      attachment:TEXT");
        assert_eq!(narrow.lines().nth(3).unwrap(), "        Just that field.");
        assert_eq!(render(source, 80).lines().nth(1).unwrap(), "    title:TEXT  notes:TEXT  attachment:TEXT", "the spacing is kept when it fits");
    }

    #[test]
    fn an_item_with_no_description_is_just_its_term() {
        assert_eq!(render("## X\n- lonely\n", 80), "X:\n    lonely\n");
    }

    #[test]
    fn continuation_lines_carry_a_description_on() {
        let out = render("## X\n- term => first part\n  second part\n", 100_000);
        assert_eq!(out, "X:\n    term  first part second part\n");
    }

    #[test]
    fn paragraphs_before_the_first_heading_are_not_indented_and_after_it_are() {
        let out = render("intro text\n\n## S\nbody text\n", 80);
        assert_eq!(out, "intro text\n\nS:\n    body text\n");
    }

    fn pages() -> [(&'static str, &'static str); 5] {
        [("main", HELP), ("add", ADD_HELP), ("refresh", REFRESH_HELP), ("search", SEARCH_HELP), ("completions", COMPLETIONS_HELP)]
    }

    #[test]
    fn every_page_keeps_all_its_words_at_every_width() {
        for (name, page) in pages() {
            let mut reference: Vec<String> = wide(page).split_whitespace().map(str::to_string).collect();
            reference.sort();
            for width in [40, 60, 80, 100, 140] {
                let mut words: Vec<String> = render(page, width).split_whitespace().map(str::to_string).collect();
                words.sort();
                assert_eq!(reference, words, "{name} at width {width} changed its words");
            }
        }
    }

    /// A line that is allowed to be wider than the window: a verbatim example or an unbreakable word.
    fn may_overflow(line: &str) -> bool {
        let text = line.trim_start();
        text.starts_with("godspeed-cli") || text.starts_with("echo ") || text.starts_with("max_age =")
            || text.starts_with("include_completed =") || text.starts_with("full_refresh_after =") || !text.contains(' ')
    }

    #[test]
    fn no_line_is_wider_than_the_window_except_examples_and_unbreakable_words() {
        for (name, page) in pages() {
            for width in [40, 60, 80, 100] {
                for line in render(page, width).lines() {
                    assert!(line.chars().count() <= width || may_overflow(line), "{name} at {width}: {line:?}");
                }
            }
        }
    }

    #[test]
    fn eighty_columns_fit_the_ordinary_text_of_every_page() {
        for (name, page) in pages() {
            let too_wide: Vec<String> =
                render(page, 80).lines().filter(|line| line.chars().count() > 80 && !may_overflow(line)).map(str::to_string).collect();
            assert!(too_wide.is_empty(), "{name}: {too_wide:#?}");
        }
    }

    #[test]
    fn a_wider_window_gives_fewer_lines() {
        for (name, page) in pages() {
            if name == "completions" {
                continue;
            }
            assert!(render(page, 120).lines().count() < render(page, 80).lines().count(), "{name}");
            assert!(render(page, 80).lines().count() < render(page, 50).lines().count(), "{name}");
        }
    }

    #[test]
    fn the_pages_start_with_their_own_subject_and_end_with_one_newline() {
        assert!(wide(HELP).starts_with("Create tasks in Godspeed"));
        assert!(wide(ADD_HELP).starts_with("Create a task in Godspeed."));
        assert!(wide(REFRESH_HELP).starts_with("Keep a local copy"));
        assert!(wide(SEARCH_HELP).starts_with("Search the local copy"));
        assert!(wide(COMPLETIONS_HELP).starts_with("Print a shell completion script"));
        for (_, page) in pages() {
            let text = render(page, 80);
            assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
        }
    }

    #[test]
    fn the_search_help_documents_every_operator() {
        let text = wide(SEARCH_HELP);
        for needle in [
            "title:TEXT", "notes:TEXT", "attachment:TEXT", "list:VALUE", "folder:VALUE", "in:VALUE",
            "label:VALUE", "label-group:VALUE", "due<DATE", "due<=DATE", "due>DATE", "due>=DATE", "due:today",
            "due:tomorrow", "due:week", "due:next-week", "due:month", "due:year", "first-due", "snoozed", "start",
            "created", "duration<15m", "snooze-count>=10", "indent-level=0", "has:notes", "has:attachment",
            "has:label", "has:repeat", "has:due", "has:snooze", "has:start", "has:due-time", "has:start-time",
            "has:bookmark", "has:email", "has:duration", "has:any", "is:complete", "is:available", "is:uncleared",
            "sort:FIELD", "sort-asc:FIELD", "sort-desc:FIELD", "limit:N", "enable:duplicates", "disable:duplicates",
            "enable:nested", "disable:nested", "--json", "EXIT STATUS", "group:label:p0|p1|p2", "...rest",
            "group:due-or-start", "group:label-group", "modified", "sidebar", "None", "sort:label:High|Medium|Low",
            "\"exact phrase\"", "in 3 days", "\"Oct 14th 12am\"", "--refresh", "--max-age AGE", "--color WHEN",
            "NO_COLOR", "overdue", "yellow", "magenta", "due today", "MATCHING:", "DATES", "GROUPING:", "SORTING:",
        ] {
            assert!(!needle.is_empty(), "a blank entry would always pass");
            assert!(text.contains(needle), "{needle} is not in the search help");
        }
    }

    #[test]
    fn the_main_help_covers_the_commands_the_options_and_the_task_syntax() {
        let text = wide(HELP);
        for needle in [
            "--help", "--version", "GODSPEED_TOKEN", ".label", "@list", "~duration", ":date", "n: text", "add",
            "EXIT STATUS", "refresh", "--include-completed", "search [--json]", "--full", "--max-age",
            "SETTINGS FILE", "completions", "godspeed-cli add \"buy milk\"", "There are no other commands",
            "max_age", "include_completed", "full_refresh_after", "NO_COLOR", "COLUMNS", "godspeed-cli <command> --help",
            "godspeed-cli add --help", "godspeed-cli refresh --help",
        ] {
            assert!(!needle.is_empty(), "a blank entry would always pass");
            assert!(text.contains(needle), "{needle} is not in the help");
        }
    }

    #[test]
    fn the_settings_are_explained_in_the_help_and_not_only_in_the_readme() {
        let text = wide(HELP);
        assert!(!text.contains("See the README"));
        for setting in ["max_age", "include_completed", "full_refresh_after"] {
            let line = text
                .lines()
                .find(|l| l.trim_start().starts_with(setting) && l.contains("Default"))
                .unwrap_or_else(|| panic!("{setting} has no explanation with a default"));
            assert!(line.contains("Default"), "{line}");
        }
    }

    #[test]
    fn the_main_help_no_longer_describes_creating_a_task_without_add() {
        let text = wide(HELP);
        assert!(!text.contains("default, so"), "add is not the default any more");
        assert!(!text.contains("[add]"));
        assert!(!text.contains("echo \"<TASK>\" | godspeed-cli\n"));
        assert!(text.contains("echo \"<TASK>\" | godspeed-cli add"));
    }

    #[test]
    fn the_add_help_explains_the_syntax_and_how_lists_and_labels_are_matched() {
        let text = wide(ADD_HELP);
        for needle in [
            "godspeed-cli add [--json] [--] <TASK>...", "--json", "TASK SYNTAX", ".label", "@list", "~duration", ":date",
            "n: text", "HOW LISTS AND LABELS ARE MATCHED", "ignoring case", "exact", "prefix", "contains", "no spaces",
            "@workprojects, @work-proj and @kproj all find \"Work Projects\"", "the shortest wins", "never picks \"Homework\"", "fetched from Godspeed again", "more than one @list",
            "no title", "EXAMPLES", "IF THE TASK CAN'T BE SENT", "EXIT STATUS", "GODSPEED_TOKEN", "lists.toml", "labels.toml",
            ".gitignore", "stays in the title", "the last one wins", "turned into real dates",
        ] {
            assert!(!needle.is_empty());
            assert!(text.contains(needle), "{needle} is not in the add help");
        }
    }

    #[test]
    fn the_refresh_help_explains_incremental_and_full_refreshes() {
        let text = wide(REFRESH_HELP);
        for needle in [
            "godspeed-cli refresh [--full] [--include-completed] [--json]", "--full", "--include-completed", "--json",
            "INCREMENTAL AND FULL", "A REFRESH IS FULL WHEN", "deleted", "full_refresh_after", "include_completed",
            "cache.sqlite3", "9 requests a minute", "leaves the previous copy exactly as it was", "stderr", "EXIT STATUS",
            "GODSPEED_TOKEN", "SETTINGS FILE", "changed_count", "refreshed_at",
        ] {
            assert!(!needle.is_empty());
            assert!(text.contains(needle), "{needle} is not in the refresh help");
        }
    }

    #[test]
    fn the_completions_help_says_how_to_install_for_each_shell_and_what_is_left_out() {
        let text = wide(COMPLETIONS_HELP);
        for needle in [
            "godspeed-cli completions <bash|fish|zsh>", "eval \"$(godspeed-cli completions bash)\"",
            "eval \"$(godspeed-cli completions zsh)\"", "godspeed-cli completions fish | source", "compinit",
            "list:", "in:", "label:", "due:", "has:", "WorkProjects", "offline", "--color",
        ] {
            assert!(!needle.is_empty(), "a blank entry would always pass");
            assert!(text.contains(needle), "{needle} is not in the completions help");
        }
    }

    #[test]
    fn no_page_mentions_the_hidden_names_command() {
        for (_, page) in pages() {
            assert!(!wide(page).contains("__complete"));
        }
    }
}
