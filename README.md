# Godspeed CLI User Guide

A lightweight command-line utility for quickly creating tasks in [Godspeed App](https://godspeedapp.com/).

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
| `add` | Create a task. This is the default, so `godspeed-cli "Buy milk"` and `godspeed-cli add "Buy milk"` are the same. |
| `refresh` | Reserved for downloading your tasks (coming in a later release). |
| `search` | Reserved for searching your tasks (coming in a later release). |

A command is recognised only when it is the **whole first argument**, in lowercase. So `godspeed-cli "search the attic"` (quoted) and `godspeed-cli buy add milk` still create tasks. If you type the words unquoted, `godspeed-cli search the attic` runs the `search` command instead. To create a task whose text starts with a command word, quote it, put `add` in front, or use `--`:

```bash
godspeed-cli "search the attic"
godspeed-cli add search the attic
godspeed-cli -- search the attic
```

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

You can manually edit or clear the cache file at `~/.local/share/godspeed-cli/cache`

## Data Storage

All data is stored in `$XDG_DATA_HOME/godspeed-cli/` (typically `~/.local/share/godspeed-cli/`):

- `cache`: Failed tasks waiting to be sent (plain text, separator: `---`)
- `lists.toml`: Cached list name → ID mappings (TOML format)

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
