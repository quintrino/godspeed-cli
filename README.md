# Godspeed CLI User Guide

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

The CLI accepts input in two ways:

### Command-line arguments (recommended: use quotes)
```bash
godspeed-cli "Buy groceries and cook dinner"
```

### Via stdin
```bash
echo "Buy groceries and cook dinner" | godspeed-cli
```

### Commands

| Command | |
| --- | --- |
| `add` | Create a task. This is the default, so `godspeed-cli "Buy milk"` and `godspeed-cli add "Buy milk"` are the same. `add --json` also prints the API's response. See [add --json](#add---json). |
| `refresh` | Download your tasks, lists and labels into a local cache. See [Refresh](#refresh). |
| `search` | Search the local copy made by `refresh`, offline. See [Search](#search). |

A command is recognised only when it is the **whole first argument**, in lowercase. So `godspeed-cli "search the attic"` (quoted) and `godspeed-cli buy add milk` still create tasks. If you type the words unquoted, `godspeed-cli search the attic` runs the `search` command instead. To create a task whose text starts with a command word, quote it, put `add` in front, or use `--`:

```bash
godspeed-cli "search the attic"
godspeed-cli add search the attic
godspeed-cli -- search the attic
```

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
| `-h`, `--help` | Print the usage and task syntax, then exit. |
| `-V`, `--version` | Print the version, then exit. |
| `--` | Treat everything after it as the task, for a task that starts with `-h`, `--help`, `-V` or `--version`. |

These are only recognised as the first argument, so `godspeed-cli "fix the --version bug"` still creates a task. `--help` and `--version` don't need `GODSPEED_TOKEN` and never create a task.

### Exit status

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Something failed, such as the API being unreachable or refusing the request. A task that couldn't be sent is queued and retried (see [Offline Cache](#offline-cache)). |
| `2` | The input can't be used: no token, an invalid task, or a bad query. Nothing is queued, because retrying wouldn't help. |

Use this in scripts: `godspeed-cli "..." || echo "not created"`.

## Special Syntax

### Labels with `.`
Add hashtags anywhere in your task to create labels. They'll be removed from the task title and added as labels (title-cased).

#NOTE: Because of Shell limitations, this only works with single word labels

```bash
godspeed-cli "Review pull request .Urgent .Work"
# Title: "Review pull request"
# Labels: ["Urgent", "Work"]
```

### Lists with `@`
Specify which list to add the task to using `@ListName`. The list name will be matched case-insensitively against your Godspeed lists, trying an exact match first, then a prefix, then any part of the name, then any part of the name ignoring spaces, slashes and dashes (so `@sideproj` finds "Side Projects"). If several lists match at the same level, the shortest name wins.

#NOTE: Because of Shell limitations, this only works with single word lists

```bash
godspeed-cli "Call dentist @Personal"
# Adds to your "Personal" list
```

**Note**: Only one list can be specified per task. If more than one `@list` is found, the task is rejected with an error (exit status 2) and nothing is sent or queued. `@` words after `n:` are part of the notes and don't count.

A task needs a title: one made only of `.label`, `@list`, `~duration` and `:date` words is rejected the same way.

### Duration with `~`
Set the task duration with `~` followed by a number, in minutes (`~45`, `~45m`) or hours (`~2h`, `~1.5h`). Fractions are rounded to whole minutes.

```bash
godspeed-cli "Workout session ~45m"
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
godspeed-cli "Send invoice :tomorrow"
# Title: "Send invoice"
# Due: tomorrow's date
```

`:` followed by a short number such as `:45` is a duration in minutes, not a date. A value that isn't a valid date stays in the title.

If a task with a relative date (`:today`, `:tomorrow`, `:next-week`) can't be sent and is queued offline, the date is saved as an absolute date, so it is still correct when the task is retried on a later day.

### Notes with `n:`
Add detailed notes to your task using `n:` followed by the note content. Everything after `n:` becomes the note.

```bash
godspeed-cli "Buy ingredients n: Need milk, eggs, flour, and butter for baking"
# Title: "Buy ingredients"
# Notes: "Need milk, eggs, flour, and butter for baking"
```

## Combining Features

You can combine all special syntax in a single task:

```bash
godspeed-cli "Write blog post @Work .Writing ~2h :tomorrow n: Focus on the new API features and include code examples"
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
.Management @Work :90" | godspeed-cli
```

## Refresh

`godspeed-cli refresh` downloads your tasks, lists and labels into a local SQLite database, ready for [searching](#search).

```bash
godspeed-cli refresh                      # incomplete tasks only
godspeed-cli refresh --include-completed  # incomplete and completed tasks
godspeed-cli refresh --json               # print the counts as JSON
```

```
Refreshed 1432 tasks, 12 lists, 30 labels
```

- It pages through the whole account (250 tasks per request) and shows progress on stderr, so `--json` output on stdout stays clean.
- The Godspeed API limits reads, so the CLI makes at most 9 requests a minute and then waits. That is about 2,000 tasks a minute, so a few thousand tasks take a couple of minutes. If the API answers "too many requests", it waits as long as it asks and retries.
- Each refresh **replaces** the previous copy. Everything is downloaded first, so if it fails part-way (exit status 1) the previous copy is kept.
- Without `--include-completed`, completed tasks are not stored, and a refresh removes any that an earlier `--include-completed` refresh stored.
- Tasks that share exactly the same last-updated time are all kept unless more than 250 share it, which could only follow a bulk edit.

The database is `$XDG_DATA_HOME/godspeed-cli/cache.sqlite3`. It needs `GODSPEED_TOKEN` like creating tasks does.

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
| `limit:N` | At most N tasks, after sorting |
| `enable:duplicates` `disable:duplicates` | Whether a task with several labels, label groups or lists appears under each when grouping |
| `enable:nested` `disable:nested` | Accepted; results are always a flat list |

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

The default output is a table with a header row. Titles are cut to 80 characters, lists to 12, labels to 20, ending in `…`, and the due date column shows the effective due date. With `group:`, each group gets a heading with a count and no header row. `--json` prints the full task records, with `list_names` and `label_names` added, as an array (or an array of `{"group", "tasks"}` objects when grouping). Messages such as `No matching tasks.` and the age note go to stderr, so stdout holds only results.

| Exit status | Meaning |
| --- | --- |
| `0` | The search ran, even if nothing matched |
| `1` | The local copy couldn't be read |
| `2` | The query is invalid, or `refresh` hasn't been run yet |

If the local copy is a day or more old, a note says so on stderr.

### What differs from Godspeed itself

- It searches a copy, so results are only as fresh as the last `refresh`.
- `is:complete` is an addition, so that completed tasks in the copy can be found.
- Results are a flat list: `enable:nested` and `disable:nested` are accepted but change nothing.
- The default order, with no `sort:`, is most recently updated first.

### Task fields it relies on

Search reads these fields from each task's JSON: `title`, `notes`, `attachments`, `list_id` (and `list_ordinals`), `label_ids`, `due_at`, `timeless_due_at`, `snoozed_until`, `timeless_snoozed_until`, `starts_at`, `timeless_starts_at`, `created_at`, `updated_at`, `duration_minutes`, `snooze_count`, `indent_level`, `repeat` / `recurrence` / `repeat_rule`, `bookmarked_at`, `email` / `email_id` / `source_email`, `cleared_at` / `is_cleared`. Lists use `name`, and `path` or `parent_id` for folders; labels use `name`, and `group_name` / `group_id` for label groups. Lists are ordered as the API returned them, which is taken to be the sidebar order.

`title`, `notes`, `list_id`, `label_ids`, `timeless_due_at` and `duration_minutes` are the fields that task creation sends. The others were taken from another Godspeed client and have **not been checked against a real account**; in particular, how folders and label groups are represented is a guess. If an operator never matches, look at what the API returned for one of your tasks (`sqlite3 ~/.local/share/godspeed-cli/cache.sqlite3 'select json from tasks limit 1'`) and compare the field names. They are all read in `src/eval.rs`.

## Offline Cache

If the API is unreachable or a request fails, the task is automatically cached locally. The next time you run the CLI (for any task), it will:

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
alias gs='godspeed-cli'

# Quick usage
gs "Task with .label @list"
```

### Shell history protection
Because `#` starts a comment in most shells, always use quotes or escape hashes

```bash
# ✅ Good - quoted
godspeed-cli "Task .Urgent #hashtag"

# ✅ Good - escaped
godspeed-cli Task \#hashtag

# ❌ Bad - shell treats #hashtag as a comment
godspeed-cli Task #hashtag
```

### Piping from other tools
Combine with other CLI tools for powerful workflows:

```bash
# From clipboard (macOS)
pbpaste | godspeed-cli

# From a file
cat todo.txt | godspeed-cli

# From a command
echo "Deploy to production @DevOps .Urgent :30" | godspeed-cli
```

### Case-insensitive matching
List names are matched case-insensitively, so these are equivalent:

```bash
godspeed-cli "Task @work"
godspeed-cli "Task @Work"
godspeed-cli "Task @WORK"
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
- `cache.sqlite3`: The local copy of your tasks, lists and labels, written by `refresh` (SQLite)

## Examples

```bash
# Simple task
godspeed-cli "Call mom"

# Task with label
godspeed-cli "Review code .Development"

# Task with list and duration
godspeed-cli "Gym workout @Health :60"

# Complete task with all features
godspeed-cli "Prepare presentation @Work .Important :120 n: Include Q3 metrics and team feedback"

# Multi-line via stdin
echo "Research new framework
- Check documentation
- Test examples
- Write summary
.Development @Learning :180" | godspeed-cli
````

## Development

```bash
cargo test
```

The unit tests cover the parsing. `tests/cli.rs` runs the real binary against a mock API on localhost, with its own data directory, by setting `GODSPEED_API_URL` (default `https://api.godspeedapp.com`). The token is sent to whatever that variable names, so leave it unset in normal use.
