# Godspeed CLI User Guide

> Upgrading from 0.1.x? A task is now created with `godspeed-cli add "..."`. See the [changelog](CHANGELOG.md) for this and everything else that changed.

A lightweight command-line utility for quickly creating tasks in [Godspeed App](https://godspeedapp.com/), and for searching them offline.

## Installation

1. Set your API key:
```bash
export GODSPEED_TOKEN="your-api-key-here"
```

Add this to your `.zshrc` or `config.fish` to make it permanent.

Alternatively, put the same line in `~/.local/share/environment/Godspeed` (`$XDG_DATA_HOME/environment/Godspeed` if `XDG_DATA_HOME` is set). It is only read when `GODSPEED_TOKEN` is unset or empty, so the environment variable always wins:

```bash
export GODSPEED_TOKEN="your-api-key-here"
```

The token is never printed or written anywhere else.

Install via homebrew
```
brew install quintrino/tap/godspeed-cli
````

or


2. Build the project:
```bash
cargo build --release
```

3. (Optional) Copy to your PATH:
```bash
cp target/release/godspeed-cli /usr/local/bin/
```


## Basic Usage

Every use starts with a command. **A task is only ever created with `add`**; running `godspeed-cli` with anything that isn't a command is an error, so a typo can't turn into a task by accident:

```console
$ godspeed-cli string
Error: 'string' isn't a valid godspeed-cli command.
Commands: add, refresh, search, completions. Run `godspeed-cli --help` for usage.
To create a task, use add: godspeed-cli add "string"
```

### Creating a task

The text of the task is the rest of the line (recommended: use quotes), or stdin:

```bash
godspeed-cli add "Buy groceries and cook dinner"
echo "Buy groceries and cook dinner" | godspeed-cli add
```

### Commands

| Command | |
| --- | --- |
| `add` | Create a task. `add --json` also prints the API's response. See [add --json](#add---json). |
| `refresh` | Keep a local copy of your tasks, lists and labels. See [Refresh](#refresh). |
| `search` | Search the local copy made by `refresh`, offline. See [Search](#search). |
| `completions` | Print a shell completion script for bash, zsh or fish. See [Shell completions](#shell-completions). |

A command is recognised only as the **first argument**, in lowercase. After `add` the task can start with any word, including a command name, so nothing needs escaping:

```bash
godspeed-cli add search the attic
godspeed-cli add completions are due ~5m
```

The one exception is a task that starts with an option: `-h`, `--help`, `-V`, `--version`, `--json` or `--`. Put `--` first for that: `godspeed-cli add -- --version bump`.

### `add --json`

`godspeed-cli add --json "Pay rent @Home ~30m :20261101"` creates the task and then prints the API's response, as JSON, on stdout, so a script can read the new task's id:

```bash
godspeed-cli add --json "Pay rent @Home" | jq -r '.todo_item.id'
```

- `--json` must come straight after `add`. After the task text it is just text, so `godspeed-cli add Convert notes to --json` still creates a task with that title. For a task that *starts* with `--json`, write `add -- --json ...`.
- It works with the task on stdin too: `echo "Pay rent" | godspeed-cli add --json`.
- Stdout only has the response when the task was created. If the task is queued (exit status 1) or rejected (exit status 2) there is nothing on stdout, and the error is on stderr as usual. Queued tasks that are retried first are not printed.
- If the API's answer isn't JSON, `{}` is printed.

### Options

| Option | |
| --- | --- |
| `-h`, `--help` | Print the usage and the settings, then exit. After a command, print that command's own help (see [Getting help](#getting-help)). |
| `-V`, `--version` | Print the version, then exit. |

These are only recognised as the first argument (or straight after `add`), so `godspeed-cli add "fix the --version bug"` still creates a task. `--help` and `--version` don't need `GODSPEED_TOKEN` and never create a task.

### Getting help

Every command has its own help page, and each explains just that command:

```bash
godspeed-cli --help               # the overview, the settings file and the exit status
godspeed-cli add --help           # the task syntax and how lists and labels are matched
godspeed-cli refresh --help       # incremental and full refreshes
godspeed-cli search --help        # the query syntax
godspeed-cli completions --help   # installing tab completion
```

The help is laid out to fit your terminal: it uses the width of the window, so it re-wraps if you resize it, and `godspeed-cli --help | less` still uses the width of the terminal it is shown in. When the width can't be found (for instance when the output goes to a file) it uses the `COLUMNS` environment variable if that holds a number, and otherwise 80 columns. It is never narrower than 40 columns. Examples and usage lines are never wrapped.

### Exit status

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Something failed, such as the API being unreachable or refusing the request. A task that couldn't be sent is queued and retried (see [Offline Cache](#offline-cache)). |
| `2` | The input can't be used: an unknown command, no token, an invalid task, or a bad query. Nothing is queued, because retrying wouldn't help. |

Use this in scripts: `godspeed-cli add "..." || echo "not created"`.

## Special Syntax

### Labels with `.`
Put `.label` words anywhere in your task to add labels. They are removed from the title and matched against your Godspeed labels (see [How lists and labels are matched](#how-lists-and-labels-are-matched)); a label that doesn't exist is left out. A word that starts with a dot is always a label, so `.gitignore` is the label "gitignore".

#NOTE: Because of Shell limitations, this only works with single word labels

```bash
godspeed-cli add "Review pull request .Urgent .Work"
# Title: "Review pull request"
# Labels: ["Urgent", "Work"]
```

### Lists with `@`
Specify which list to add the task to using `@ListName`. The name is matched against your Godspeed lists as described in [How lists and labels are matched](#how-lists-and-labels-are-matched); if no list matches, the task is still created, without one.

#NOTE: Because of Shell limitations, this only works with single word lists

```bash
godspeed-cli add "Call dentist @Personal"
# Adds to your "Personal" list
```

**Note**: Only one list can be specified per task. If more than one `@list` is found, the task is rejected with an error (exit status 2) and nothing is sent or queued. `@` words after `n:` are part of the notes and don't count.

A task needs a title: one made only of `.label`, `@list`, `~duration` and `:date` words is rejected the same way.

### How lists and labels are matched

Names are matched ignoring case, against your lists and labels as last seen. They are kept in `lists.toml` and `labels.toml` in the data directory, and fetched from Godspeed again, once, when a name isn't found.

**`@list`** tries these in turn and uses the first that finds a list:

| Step | Finds | Example |
| --- | --- | --- |
| exact | the whole name | `@work` finds "Work" |
| prefix | a name that starts with it | `@wo` finds "Work" |
| contains | a name that contains it | `@ork` finds "Work" |
| no spaces | part of the name, once spaces, slashes and dashes are left out of both | `@workprojects`, `@work-proj` and `@kproj` find "Work Projects" |

If several names are found at the same step, the shortest wins, then the first alphabetically. So with lists "Work", "Work Projects" and "Homework", `@work` is "Work" (exact), `@wo` is "Work" (the shortest prefix), and `@mew` is "Homework" (the only one that contains it).

**`.label`** is stricter: the exact name, then a prefix, then the *whole* name with spaces, slashes and dashes left out (`.workprojects` finds "Work Projects"). It never matches part of a name, so `.work` doesn't pick "Homework". Labels are matched one by one, and the ones that aren't found are left out of the task.

If nothing matches, the task is still created, without that list or label. A task with more than one `@list`, or with no title, is rejected (exit status 2) and isn't queued.

The words that make up the rest of the syntax:

- Any other word is part of the title, which is the words joined by single spaces.
- `~45`, `~45m` and `~1.5h` set the duration. It has to be more than zero after rounding to whole minutes, otherwise the word stays in the title. If a duration or a date is given twice, the last one wins.
- `:today`, `:tomorrow`, `:next-week`, `:YYYYMMDD` and `:YYYY-MM-DD` set the due date. A short number after a colon, like `:45`, is a duration in minutes (the older syntax). Something that looks like syntax but isn't, such as `~roughly`, `:someday` or `:20261345`, stays in the title.
- `n:` starts the notes, but only with a space before it: `Buy milk n: oat` has notes, `Buy milkn: oat` doesn't. Everything after the first ` n:` is notes and is never read as syntax.

`godspeed-cli add --help` explains all of this too.

### Duration with `~`
Set the task duration with `~` followed by a number, in minutes (`~45`, `~45m`) or hours (`~2h`, `~1.5h`). Fractions are rounded to whole minutes.

```bash
godspeed-cli add "Workout session ~45m"
# Title: "Workout session"
# Duration: 45 minutes
```

Only `~` followed by a positive number and an optional `m` or `h` counts as a duration, so a word like `~roughly` stays in the title.

The original `:45` form (minutes) still works.

**Quote the task.** An unquoted `~` can be expanded by the shell (zsh may report "no such user or named directory"). Quote the whole task, or escape it as `\~45m`.

### Due date with `:`
Set the due date with `:` followed by a keyword or a date. The date has no time of day.

| Syntax | Due date |
| --- | --- |
| `:today` | Today |
| `:tomorrow` | Tomorrow |
| `:next-week` | Seven days from today |
| `:20261031` | 31 October 2026 |
| `:2026-10-31` | 31 October 2026 |

```bash
godspeed-cli add "Send invoice :tomorrow"
# Title: "Send invoice"
# Due: tomorrow's date
```

`:` followed by a short number such as `:45` is a duration in minutes, not a date. A value that isn't a valid date stays in the title.

If a task with a relative date (`:today`, `:tomorrow`, `:next-week`) can't be sent and is queued offline, the date is saved as an absolute date, so it is still correct when the task is retried on a later day.

### Notes with `n:`
Add detailed notes to your task using `n:` followed by the note content. Everything after `n:` becomes the note.

```bash
godspeed-cli add "Buy ingredients n: Need milk, eggs, flour, and butter for baking"
# Title: "Buy ingredients"
# Notes: "Need milk, eggs, flour, and butter for baking"
```

## Combining Features

You can combine all special syntax in a single task:

```bash
godspeed-cli add "Write blog post @Work .Writing ~2h :tomorrow n: Focus on the new API features and include code examples"
```

This creates a task with:
- Title: "Write blog post"
- List: Your "Work" list
- Labels: ["Writing"]
- Duration: 120 minutes
- Due date: tomorrow
- Notes: "Focus on the new API features and include code examples"

## Multi-line Tasks

The CLI supports multi-line input, which is especially useful for detailed tasks:

```bash
echo "Project planning session
Agenda items:
- Review Q4 goals
- Discuss team resources
.Management @Work :90" | godspeed-cli add
```

## Refresh

`godspeed-cli refresh` keeps a local copy of your tasks, lists and labels in a SQLite database, ready for [searching](#search).

```bash
godspeed-cli refresh                      # bring the copy up to date
godspeed-cli refresh --full               # rebuild the copy from scratch
godspeed-cli refresh --include-completed  # also keep completed tasks
godspeed-cli refresh --json               # print a report as JSON
```

### Incremental and full refreshes

- **The first refresh is a full one**: it downloads everything.
- **After that, a refresh is incremental**: it asks only for tasks changed since the copy was last brought up to date, adds or updates them, and leaves the rest. That is usually two requests, so it takes a second or two however large the account.
- **`--full` rebuilds the copy from scratch.** It downloads everything again and replaces the old copy.

```
Refreshed 1432 tasks, 12 lists, 30 labels                                              (full)
Updated 3 tasks since the last refresh; the copy now has 1435 tasks, 12 lists, 30 labels   (incremental)
```

An incremental refresh can't see everything: **a task deleted in Godspeed stays in the copy**, because the API only reports tasks that still exist. A full refresh drops them, so one happens by itself when:

- you ask for it with `--full`;
- there is no copy yet;
- you switch `--include-completed` on or off (the copy has to be rebuilt to match);
- the last full refresh is a week old (change this with `full_refresh_after`, below); or
- the copy is missing the information an incremental refresh needs.

Progress, including which kind of refresh it is and why, goes to stderr, so `--json` output on stdout stays clean. `--json` reports `mode` (`full` or `incremental`), `reason` (why it was full), `changed_count` (tasks fetched), the counts, `include_completed` and `refreshed_at`.

### Details

- A full refresh pages through the whole account, 250 tasks per request. The Godspeed API limits reads, so the CLI makes at most 9 requests a minute and then waits: about 2,000 tasks a minute, so a few thousand tasks take a couple of minutes. If the API answers "too many requests", it waits as long as it asks and retries.
- Everything is downloaded before the copy is touched, so a refresh that fails part-way (exit status 1) leaves the previous copy exactly as it was.
- An incremental refresh asks for changes from a minute before the newest change it has seen, so one that landed at the same moment isn't missed. Tasks fetched twice are harmless.
- It always asks for completed tasks too, even without `--include-completed`: a task you completed since the last refresh only shows up as completed, and has to be removed from the copy. Without `--include-completed` it then isn't kept.
- Lists and labels are updated in place, so a list keeps its place in the sidebar order; a new one goes at the end until the next full refresh.
- Tasks that share exactly the same last-updated time are all kept unless more than 250 share it, which could only follow a bulk edit.
- Not yet checked against a real account: the API's `updated_after` filter, which incremental refreshes depend on. If an incremental refresh ever seems to miss something, `--full` always gives the true picture.

### What the database remembers

The copy is `$XDG_DATA_HOME/godspeed-cli/cache.sqlite3`. Besides the tasks, lists and labels, it holds a few facts about itself, in its `metadata` table, so the program doesn't have to guess: `refreshed_at` (the last refresh of either kind), `last_full_refresh_at`, `include_completed` (whether the copy holds completed tasks) and `high_water` (the newest change seen, which an incremental refresh continues from). They are written together with the data, in the same transaction.

The database needs `GODSPEED_TOKEN` to refresh, like creating tasks does.

## Settings file

An optional file, `$XDG_CONFIG_HOME/godspeed-cli/config.toml` (`~/.config/godspeed-cli/config.toml` if `XDG_CONFIG_HOME` is unset), sets defaults for `refresh` and `search`. Command-line flags override it, and creating tasks ignores it.

```toml
max_age = "12h"            # search refreshes first when the copy is older than this
include_completed = true   # refresh keeps completed tasks as well
full_refresh_after = "7d"  # a refresh is a full one when the last full one is this old
```

| Setting | Default | Meaning |
| --- | --- | --- |
| `max_age` | none | `search` refreshes first when the copy is older than this. Without it, `search` never refreshes by itself |
| `include_completed` | `false` | `refresh` keeps completed tasks, as if `--include-completed` were given |
| `full_refresh_after` | `"7d"` | How old the last full refresh may get before the next refresh is a full one. `"never"` turns it off |

Durations are written like `30m`, `12h`, `1 day` or `1 day and 6 hours`. `"0"` means always, and `"never"` turns a setting off. A mistake in the file (an unknown setting, a value that isn't a duration) is an error that names the file, so a typo can't silently do nothing.

## Search

`godspeed-cli search` finds tasks in the local copy made by [`refresh`](#refresh). It works offline and needs no token. It follows [Godspeed's search operators guide](https://godspeedapp.com/guides/search-operators); run `godspeed-cli search --help` for the full syntax.

```bash
godspeed-cli search research pdf
godspeed-cli search 'feature & (bug | error)'
godspeed-cli search 'research -"budget meeting"'
godspeed-cli search 'research in:"engineering q2"'
godspeed-cli search 'due<=3h'
godspeed-cli search 'snoozed>="Oct 14th 12am" & snoozed<="Oct 14th 1159pm"'
godspeed-cli search 'due:today group:list sort:due'
godspeed-cli search 'duration>="1 hour and 30 minutes" has:notes limit:10'
godspeed-cli search -label:Low --json
```

Only incomplete tasks are searched. `is:uncleared` and `is:complete` bring in completed tasks, and they are only in the local copy if you refreshed with `--include-completed`. With no query, every incomplete task is listed.

**Quote the query.** The arguments are joined with spaces into one query, so `search '(a | b) !c'` works but an unquoted `|` or `(` is taken by the shell. A phrase needs its own quotes inside the query: `search '"tax return"'`; `search "tax return"` means *tax* and *return*, in any order.

### Keeping the copy fresh

By default `search` only reads the copy, so it works offline and needs no token, but results are only as fresh as the last `refresh`. Two flags (and a setting) make it update the copy first:

```bash
godspeed-cli search --refresh 'due:today'      # refresh now, then search
godspeed-cli search --max-age 6h 'due:today'   # refresh only if the copy is over 6 hours old
godspeed-cli search --max-age never milk       # never, whatever the settings file says
```

- `--refresh` always updates the copy first. It is an ordinary refresh, so incremental when it can be (see [Refresh](#refresh)).
- `--max-age AGE` updates it only if it is older than `AGE` (`30m`, `6h`, `"2 days"`; `0` means always). `max_age` in the [settings file](#settings-file) does the same without a flag, and `--max-age` overrides it.
- Either needs `GODSPEED_TOKEN` and the network. **If the refresh can't be done**, `search` carries on with the existing copy and says why on stderr, so a search never fails just because you're offline. If there is no copy at all, it is an error (exit status 1).
- A refresh from `search` never quietly drops completed tasks the copy already has, and a query using `is:complete` or `is:uncleared` makes it include them.
- The refresh's progress goes to stderr; stdout holds only the results, so `--json` stays clean.

### The operators

| Syntax | Meaning |
| --- | --- |
| `word` | Ignores case and word order; found in the title, notes or attachment names |
| `"exact phrase"` | The exact characters, spaces included |
| `a & b`, `a b`, `a \| b`, `!a`, `-a`, `( )` | And, or, not, grouping (nestable). `\|` binds loosest, then and, then not. Not works on every operator |
| `title:` `notes:` `attachment:` | Just that field (attachments by file name only) |
| `list:` | Tasks in a list |
| `folder:` | Tasks in a list that sits in a folder with that name |
| `in:` | Tasks in a list or a folder |
| `label:` `label-group:` | Tasks with a label, or a label in that group |
| `due` `first-due` `snoozed` `start` `created` | Dates, with `<` `<=` `>` `>=` and a date query (below), or `:today` `:tomorrow` `:week` `:next-week` `:month` `:year` |
| `duration` `snooze-count` `indent-level` | Numbers, with `<` `<=` `>` `>=` `=`. Durations like `15m`, `1.5h`, `"1 hour and 30 minutes"` |
| `has:notes` `has:attachment` `has:label` `has:repeat` `has:due` `has:snooze` `has:start` `has:due-time` `has:start-time` `has:bookmark` `has:email` `has:duration` | The task has that. `has:any` matches every task |
| `is:available` | No start date, or a start date in the past |
| `is:uncleared` | Completed but not yet cleared (never an incomplete task) |
| `sort:` `sort-asc:` `sort-desc:` | `due` `first-due` `snoozed` `start` `created` `modified` `title` `label` `duration` `snooze-count` `list`. Ascending by default; repeat to sort by several |
| `group:` | `list` `label` `label-group` `due` `start` `due-or-start` `created` |
| `limit:N` | At most N tasks, counted in the order they are shown: after sorting and, with `group:`, group by group, so the first groups are kept (see below) |
| `enable:duplicates` `disable:duplicates` | Whether a task with several labels, label groups or lists appears under each when grouping |
| `enable:nested` `disable:nested` | Accepted; results are always a flat list |

**`limit:` and groups.** The limit counts tasks in the order they are shown, so with `group:` it keeps the first groups and cuts the later ones. A group you list first is therefore never dropped to make room for another, however its tasks sort. For example `due<=today group:label:Eternal|Vital|high|medium|low sort:due limit:40` shows the Eternal tasks first even if they have no due date and there are hundreds of overdue tasks, and spends whatever is left of the 40 on the later groups. Heading counts, like `high (30)`, are of the tasks shown. A task that appears under several groups (with `enable:duplicates`) uses one place for each.

**`|` and spaces.** `a | b c` means `a | (b c)`, because `|` binds loosest. So in `label:Eternal | due<=today -label:subtask`, the `-label:subtask` only applies to the `due<=today` side, and an Eternal task that is also a subtask is still found. To apply a term to both sides, use parentheses: `(label:Eternal | due<=today) -label:subtask`.

Names and ids: `list:`, `folder:`, `in:`, `label:` and `label-group:` take part of a name, or an id (the whole id). Quote names that contain spaces: `in:"engineering q2"`.

Group and sort targets: `group:label:p0|p1|p2` makes those groups in that order, and tasks with none of them go in a group called `None`. `group:list:Morning|"After lunch"` quotes names with spaces, and ids work too. End the targets with `...rest` (`group:label:Bug|Feature|...rest`) and tasks matching no target are grouped normally instead of going into `None`. `sort:label:High|Medium|Low` and `sort:list:...` put those first. With several labels or label groups, the first alphabetically is used for grouping.

### Dates

Date queries are read the way Godspeed's date picker reads them, and quoted if they contain spaces:

| You can write | Meaning |
| --- | --- |
| `3h`, `30m`, `2d`, `1w`, `-2d`, `1h30m` | That long from now (`3h ago`, `-3h` for the past) |
| `in 3 days`, `2 hours ago` | The same, in words |
| `today`, `tomorrow`, `yesterday` | That day |
| `friday`, `next friday` | The next Friday, counting today (`next` skips today) |
| `Oct 14th`, `14 oct`, `oct 14 2027`, `2026-10-31`, `20261031` | That date. With no year, the closest one to today |
| `12am`, `3:30pm`, `1159pm`, `14:30`, `noon`, `midnight` | A time, today unless a date is given: `"Oct 14th 12am"`, `tomorrow 3pm` |

A date with no time means the whole day: `due<=2026-10-31` includes tasks due on the 31st, and `due>2026-10-31` does not. With a time, or a relative amount like `3h`, it is that exact moment. A task's date that has no time counts as the start of its day, so `due<=3h` also finds tasks that are overdue. `due:DATE` means that calendar day; `due:today`, `:week`, `:month` and so on cover the whole period (weeks run Monday to Sunday). The period words only work after `:`, not after `<` or `>`.

`due` is the effective due date: the later of the due date and the snooze date. `first-due` is the due date alone. A task that has no value for a date or number never matches a test on it, so `due<today` doesn't match undated tasks (and `!due<today` does).

Something that looks like `operator:value` but isn't an operator, such as `10:30`, is searched for as plain text. A misspelt operator is therefore a plain word rather than an error, but a known operator with a bad value (`due:someday`, `has:wings`) is an error.

### Output

The output is a table that starts with a header row of column labels (`TITLE`, `LIST`, `DUE`, `LABELS`). Titles are cut to 80 characters, lists to 12, labels to 20, ending in `…`, and the due date column shows the effective due date. With `group:`, the header comes once, at the top, followed by each group: a heading with the group's name and a count, then its rows, with a blank line between groups. Nothing, not even the header, is printed when there are no results. `--json` prints the full task records, with `list_names` and `label_names` added, as an array (or an array of `{"group", "tasks"}` objects when grouping). Messages such as `No matching tasks.` and the age note go to stderr, so stdout holds only results.

| Exit status | Meaning |
| --- | --- |
| `0` | The search ran, even if nothing matched |
| `1` | The local copy couldn't be read |
| `2` | The query is invalid, or `refresh` hasn't been run yet |

If the local copy is a day or more old, a note says so on stderr.

### Colour

In a terminal, four things are coloured:

- **The due date of an overdue task is red**, so overdue tasks stand out in a long list. Only the date: not the rest of the row, or the dates of tasks that aren't overdue.
- **The due date of a task due today is magenta**: that date is today and the task is not overdue yet, whether it is a date with no time or a time later today. Once a time has passed, the task is overdue and its date turns red.
- **The list is yellow**, in every row that has one (the whole cell, as shown, so a task in several lists has them all in yellow). A task with no list has nothing to colour.
- **The header row is bold cyan**, so the column labels stand apart from the rows. It is the same in grouped output, where it comes once at the top.

Each has its own colour (red, magenta, yellow, cyan), so an overdue task's row shows a yellow list and a red date. Nothing else is coloured, including the title, the labels and the group headings.

A task is overdue when its effective due date has passed. That is the later of its due date and its snooze date, the same as `due` in a search. So:

- a date with no time is overdue from the day after (a task due today is not overdue yet);
- a date with a time is overdue once that time has passed;
- a task snoozed past its due date is not overdue until the snooze ends;
- a task with no due date, and a completed task, is never overdue.

`due<today` finds the red tasks that have a date with no time, and `due:today` finds all the magenta ones (plus any task whose due time earlier today has already passed, which is red).

| `--color WHEN` | Meaning |
| --- | --- |
| `auto` (the default) | Colour only when output goes to a terminal. Not if the `NO_COLOR` environment variable is set to anything non-empty (see [no-color.org](https://no-color.org)), and not on a `dumb` terminal |
| `always` | Always colour, even into a pipe or a file, and even if `NO_COLOR` is set. Use it with a pager: `godspeed-cli search 'due<=today' --color always \| less -R` |
| `never` | Never colour |

JSON (`--json`) is never coloured. The colour is added around the header and around each list and date, after the columns are laid out, so the text and the columns are exactly the same with or without it. No results print nothing at all, not even a coloured header.

### What differs from Godspeed itself

- It searches a copy, so results are only as fresh as the last `refresh`.
- `is:complete` is an addition, so that completed tasks in the copy can be found.
- Results are a flat list: `enable:nested` and `disable:nested` are accepted but change nothing.
- The default order, with no `sort:`, is most recently updated first.

### Task fields it relies on

Search reads these fields from each task's JSON: `title`, `notes`, `attachments`, `list_id` (and `list_ordinals`), `label_ids`, `due_at`, `timeless_due_at`, `snoozed_until`, `timeless_snoozed_until`, `starts_at`, `timeless_starts_at`, `created_at`, `updated_at`, `duration_minutes`, `snooze_count`, `indent_level`, `repeat` / `recurrence` / `repeat_rule`, `bookmarked_at`, `email` / `email_id` / `source_email`, `cleared_at` / `is_cleared`. Lists use `name`, and `path` or `parent_id` for folders; labels use `name`, and `group_name` / `group_id` for label groups. Lists are ordered as the API returned them, which is taken to be the sidebar order.

`title`, `notes`, `list_id`, `label_ids`, `timeless_due_at` and `duration_minutes` are the fields that task creation sends. The others were taken from another Godspeed client and have **not been checked against a real account**; in particular, how folders and label groups are represented is a guess. If an operator never matches, look at what the API returned for one of your tasks (`sqlite3 ~/.local/share/godspeed-cli/cache.sqlite3 'select json from tasks limit 1'`) and compare the field names. They are all read in `src/eval.rs`.

## Shell completions

`godspeed-cli completions <shell>` prints a script that adds tab completion to **bash**, **zsh** or **fish**. Nothing is installed or changed until you use the script:

| Shell | Add to | The line |
| --- | --- | --- |
| bash | `~/.bashrc` | `eval "$(godspeed-cli completions bash)"` |
| zsh | `~/.zshrc`, after `compinit` | `eval "$(godspeed-cli completions zsh)"` |
| fish | `~/.config/fish/config.fish` | `godspeed-cli completions fish \| source` |

Open a new shell, and Tab completes:

- the commands (`add`, `refresh`, `search`, `completions`), `--help` and `--version`;
- each command's flags: `--json`, `--full`, `--include-completed`, `--refresh`, `--max-age`, `--color` (and its values `auto`, `always`, `never`);
- the shell names after `completions`;
- **in `add`, the names of your lists after `@` and your labels after `.`**: `godspeed-cli add "Buy milk @Wo<Tab>` becomes `@WorkProjects`;
- **in `search`, list names after `list:` and `in:`, and label names after `label:`**, including after `!`, `-` and `(`.

It deliberately does **not** complete other operators such as `due:` or `has:`, or any other part of a task or query.

**Where the names come from.** The names are read from disk, so completion works offline and needs no token: from the local copy made by `godspeed-cli refresh` (run it once to have all your lists and labels), and from the lists and labels already used when creating tasks. Names are matched ignoring case.

**Names with spaces.** A task's `@list` and `.label` have to be one word, so a name with spaces is offered without them: "Work Projects" becomes `WorkProjects`. godspeed-cli accepts that form, in tasks (`@WorkProjects`, `.WorkProjects`) and in searches (`list:WorkProjects`).

**Names are never run.** A name is only offered if it is made of letters, digits and `_ - . : + /`. A list called `Evil $(rm -rf ~)` is simply left out, so a completion script can never be made to run a command by the name of a list or label.

Notes:
- Completing inside a quoted task or query (`add "Buy milk @Wo<Tab>`) works in bash and zsh and is meant to in fish.
- The bash script runs on bash 3.2, the version macOS ships. In bash, `:` is a word break, which the script handles either way, so `search label:U<Tab>` works whether or not you have changed `COMP_WORDBREAKS`.
- The bash completion is covered by the test suite, which runs it in real bash. **The zsh and fish scripts have not been run in those shells**, as they weren't available where this was written, so please say if one misbehaves.

## Offline Cache

If the API is unreachable or a request fails, the task is automatically cached locally. The next time you run `add` (for any task), it will:

1. Attempt to send all cached tasks first
2. Remove successfully sent tasks from the cache
3. Process your new task

This ensures you never lose tasks due to connectivity issues.

Only tasks that failed because of the connection or the API are queued. A task that can never be sent, such as one with several lists or no title, is rejected immediately and is not queued. If an earlier version left one of those in the queue, it is dropped on the next run and its text is printed so you can recreate it.

Cache location: `$XDG_DATA_HOME/godspeed-cli/cache` (usually `~/.local/share/godspeed-cli/cache`)

## List Caching

When you first reference a list with `@ListName`, the CLI fetches all your lists from the Godspeed API and caches them locally for fast lookups.

Cache location: `$XDG_DATA_HOME/godspeed-cli/lists.toml`

To refresh the list cache, simply delete this file and the CLI will re-fetch on the next run.

## Error Notifications

The CLI uses macOS notifications (via `osascript`) to alert you of errors:

- **"GODSPEED_TOKEN is not set"**: You need to set your API key (environment variable or token file)
- **"Failed to send task"**: The API request failed (task is cached for retry)
- **"Error: Multiple lists specified"**: You used more than one `@list` in a single task (not queued)
- **"Error: The task has no title"**: The task was only syntax words such as `@list` (not queued)

## Tips and Tricks

### Quick capture from anywhere
Create a shell alias or keyboard shortcut:

```bash
# In .zshrc or config.fish
alias gs='godspeed-cli add'

# Quick usage
gs "Task with .label @list"
```

### Shell history protection
Because `#` starts a comment in most shells, always use quotes or escape hashes

```bash
# ✅ Good - quoted
godspeed-cli add "Task .Urgent #hashtag"

# ✅ Good - escaped
godspeed-cli add Task \#hashtag

# ❌ Bad - shell treats #hashtag as a comment
godspeed-cli add Task #hashtag
```

### Piping from other tools
Combine with other CLI tools for powerful workflows:

```bash
# From clipboard (macOS)
pbpaste | godspeed-cli add

# From a file
cat todo.txt | godspeed-cli add

# From a command
echo "Deploy to production @DevOps .Urgent :30" | godspeed-cli add
```

### Case-insensitive matching
List names are matched case-insensitively, so these are equivalent:

```bash
godspeed-cli add "Task @work"
godspeed-cli add "Task @Work"
godspeed-cli add "Task @WORK"
```

## Troubleshooting

### "GODSPEED_TOKEN is not set"
Make sure you've exported your API key:
```bash
export GODSPEED_TOKEN="your-key"
```
or that `~/.local/share/environment/Godspeed` contains an `export GODSPEED_TOKEN="your-key"` line.

### List not found
If your `@ListName` isn't being recognized:
1. Check the spelling matches your Godspeed list
2. Delete `~/.local/share/godspeed-cli/lists.toml` to refresh the cache
3. Run the CLI again to re-fetch your lists

### Task appears in cache repeatedly
If a task keeps failing and accumulating in the cache, check:
1. Your API key is valid
2. The Godspeed API is accessible
3. Your list name (if using `@`) exists

The queue only holds tasks that failed to send. A task with several lists or no title is rejected straight away, and is never queued.

You can manually edit or clear the cache file at `~/.local/share/godspeed-cli/cache`

## Data Storage

All data is stored in `$XDG_DATA_HOME/godspeed-cli/` (typically `~/.local/share/godspeed-cli/`):

- `cache`: Failed tasks waiting to be sent (plain text, separated by `-*-*-*-` lines)
- `lists.toml`, `labels.toml`: Cached list and label name → ID mappings (TOML format)
- `cache.sqlite3`: The local copy of your tasks, lists and labels, and facts about it, written by `refresh` (SQLite)

Optional settings are kept separately, in `$XDG_CONFIG_HOME/godspeed-cli/config.toml` (usually `~/.config/godspeed-cli/config.toml`); see [Settings file](#settings-file).

## Examples

```bash
# Simple task
godspeed-cli add "Call mom"

# Task with label
godspeed-cli add "Review code .Development"

# Task with list and duration
godspeed-cli add "Gym workout @Health :60"

# Complete task with all features
godspeed-cli add "Prepare presentation @Work .Important :120 n: Include Q3 metrics and team feedback"

# Multi-line via stdin
echo "Research new framework
- Check documentation
- Test examples
- Write summary
.Development @Learning :180" | godspeed-cli add
````

## Development

```bash
cargo test
```

The unit tests cover the parsing. `tests/cli.rs` runs the real binary against a mock API on localhost, with its own data directory, by setting `GODSPEED_API_URL` (default `https://api.godspeedapp.com`). The token is sent to whatever that variable names, so leave it unset in normal use.
