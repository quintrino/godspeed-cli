# Changelog

## 0.11.0: 2026-10-16

The first release since 0.1.0.

godspeed-cli started as a quick way to capture a task from the terminal. It still does that, with due dates, durations and smarter list matching. It can now also keep a local copy of your tasks and **search it offline with Godspeed's own search operators**, with coloured output, tab completion and a help page for every command.

### Highlights

- **Search your tasks offline**: `godspeed-cli search 'due:today group:list sort:due'`, using the operators from Godspeed's search guide.
- **`refresh` keeps a local copy**, fetching only what changed after the first run.
- **Due dates and durations** when adding a task: `godspeed-cli add "Pay rent @Home ~30m :tomorrow"`.
- **Overdue tasks stand out**: the due date is red, due today is magenta, the list is yellow and the header is bold cyan.
- **Tab completion** for bash, zsh and fish, including your list and label names.
- **Every command has its own `--help`**, laid out to fit your terminal.
- ⚠ **A task is now created with `add`.** See "Upgrading" below.

### Upgrading from 0.1.x: read this first

**`godspeed-cli "text"` no longer creates a task. Use `godspeed-cli add "text"`.** Anything that isn't a command is an error, so a typo can't quietly become a task.

| Before (0.1.x) | Now |
| --- | --- |
| `godspeed-cli "Buy milk"` | `godspeed-cli add "Buy milk"` |
| `echo "Buy milk" \| godspeed-cli` | `echo "Buy milk" \| godspeed-cli add` |
| `alias gs='godspeed-cli'` | `alias gs='godspeed-cli add'` |
| `godspeed-cli -- --version bump` | `godspeed-cli add -- --version bump` |

Update any alias, script, keyboard shortcut or launcher that calls `godspeed-cli` to create tasks. If you miss one, it fails loudly instead of doing the wrong thing:

```
$ godspeed-cli buy milk
Error: 'buy' isn't a valid godspeed-cli command.
Commands: add, refresh, search, completions. Run `godspeed-cli --help` for usage.
To create a task, use add: godspeed-cli add "buy"
```

It exits with status 2 and sends nothing.

Other behaviour that is different:

- **Exit codes now mean something.** `0` success, `1` something failed (such as the API being unreachable; the task is queued and retried), `2` the input can't be used (an unknown command, no token, an invalid task or query). 0.1.0 always exited 0.
- **Invalid tasks are rejected, not queued.** A task with more than one `@list` used to be queued and retried on every run, forever, and a task with no title was sent to the API with an empty title. Both are now refused straight away (exit status 2). If you already have a multi-list task stuck in your queue, it is dropped on the next `add` and its text is printed so you can recreate it.
- **`@` words after `n:` are notes**, not lists, so a note like `n: ask @bob` no longer counts as a second list.
- **The retry queue is only retried by `add`.**
- **Lists are matched more cleverly, and the same way every time**: exact name, then prefix, then part of the name, ignoring spaces and dashes. Before, the first of several prefix matches was picked in no fixed order.
- **The binary is about 2 MB bigger** (roughly 8 MB to 10 MB): it now includes SQLite.
- **New files**: `cache.sqlite3` in your data directory, and an optional settings file (below).

### What's new

#### Creating tasks (`add`)

- Duration with `~`: `~45`, `~45m`, `~1.5h`. The old `:45` still works.
- Due date with `:`: `:today`, `:tomorrow`, `:next-week`, `:20261031`, `:2026-10-31`.
- `:today`, `:tomorrow` and `:next-week` are turned into real dates when you write the task, so a task kept for a retry still gets the day you meant.
- `add --json` prints the API's response to the new task, so a script can read its id.
- The token is read from `GODSPEED_TOKEN`, or from `~/.local/share/environment/Godspeed` if that isn't set.
- `-h`, `--help`, `-V` and `--version`, which 0.1.0 didn't have.
- `godspeed-cli add --help` explains the task syntax and exactly how list and label names are matched.

#### Keeping a local copy (`refresh`)

- The first refresh downloads everything. After that a refresh fetches **only what changed**, usually two requests however big your account is.
- `refresh --full` rebuilds the copy from scratch. A refresh is also full when there is no copy, when you switch `--include-completed` on or off, and when the last full refresh is a week old (an incremental refresh can't see tasks you deleted, and a full one drops them).
- `refresh --include-completed` keeps completed tasks too.
- It stays within Godspeed's read limit (at most 9 requests a minute) and waits as long as the API asks if it is told to slow down.
- A refresh that fails part-way leaves the previous copy exactly as it was.
- `--json` prints a report. Progress goes to stderr, so stdout stays clean.
- The copy is a SQLite database, `cache.sqlite3`, which also records facts about itself (when it was refreshed, whether it holds completed tasks, the newest change it has seen).

#### Searching (`search`)

- **Godspeed's search operators**, searching the local copy offline: words and `"phrases"`, `&`, `|`, `!`, `-` and parentheses; `title:`, `notes:`, `attachment:`; `list:`, `folder:`, `in:`, `label:`, `label-group:` (by name or id); `due`, `first-due`, `snoozed`, `start`, `created` with `<`, `<=`, `>`, `>=` or `:today` / `:week` / `:month`; `duration`, `snooze-count`, `indent-level`; every `has:` trait; `is:available` and `is:uncleared`; `sort:`, `group:` (with target lists and `...rest`), `limit:`, and `enable:` / `disable:duplicates`.
- **Dates and durations are read the way the date picker reads them**: `3h`, `in 3 days`, `friday`, `"Oct 14th 12am"`, `2026-10-31`, `noon`; and `"1 hour and 30 minutes"` for durations.
- `limit:` counts tasks in the order they are shown, so with `group:` a group you list first is never cut to make room for a later one.
- A table with a header row, or `--json` (the full records, plus `list_names` and `label_names`).
- `search --refresh` updates the copy first, `--max-age 6h` does so only if it is older than that. If the refresh can't be done, the existing copy is searched and a note says why.
- `is:complete` is an addition to Godspeed's operators, so completed tasks in the local copy can be found.

#### Colour

In a terminal, the search table is coloured: the **due date of an overdue task is red**, the **due date of a task due today is magenta**, the **list is yellow** and the **header row is bold cyan**. Nothing else is, and JSON never is.

It is only used for a terminal by default, never when `NO_COLOR` is set or the terminal is `dumb`, and `--color auto|always|never` overrides that (`--color always` is for `| less -R`).

A task is overdue when its effective due date (the later of its due date and its snooze date) has passed: a date with no time from the day after, a date with a time once it has passed.

#### Settings file

An optional `~/.config/godspeed-cli/config.toml` (or under `$XDG_CONFIG_HOME`):

```toml
max_age = "12h"            # search refreshes first when the copy is older than this
include_completed = true   # refresh keeps completed tasks
full_refresh_after = "7d"  # a refresh is a full one when the last full one is this old
```

Flags override it, and a mistake in the file is an error that names the file.

#### Shell completion

`godspeed-cli completions bash|zsh|fish` prints a script (`eval "$(godspeed-cli completions zsh)"`, and so on). It completes the commands and flags, list names after `@`, label names after `.`, and `list:` / `in:` / `label:` in a search. It deliberately doesn't complete other operators such as `due:` or `has:`. Names come from files on disk, so it works offline, and a list or label with unsafe characters in its name is never offered, so a name can't run a command.

#### Help

- `godspeed-cli --help`, and `add --help`, `refresh --help`, `search --help` and `completions --help`, each explain their own command.
- The help fits your terminal: it uses the width of the window, or `COLUMNS` if that can't be read, or 80 columns. It is never narrower than 40.

### Fixed since 0.1.0

- A task with several `@lists` was queued and retried forever, and a task with no title was sent with an empty title.
- `@` words in the notes counted as lists.
- With several lists starting with the same letters, which one was picked could vary.
- Failures were silent: every run exited 0.

### Known limitations, and what has not been verified

**Please read these.** The test suite is large (about 470 tests, including end-to-end tests that run the real program against a mock of the API), but a mock isn't Godspeed:

- **Not yet checked against a real account:**
  - the API's `updated_after` filter, which incremental refresh depends on (`refresh --full` always gives the true picture);
  - the names of most task fields (`due_at`, `snoozed_until`, `label_ids`, and so on) beyond the ones task creation sends;
  - how folders and label groups appear in the data (`folder:`, `label-group:` and `group:label-group` rest on a guess);
  - that the API returns 250 tasks a page, and the 9 requests a minute limit;
  - the exact shape of the response `add --json` prints.

  If an operator never matches anything, the README says how to look at one of your tasks and compare the field names.
- **The zsh and fish completion scripts have not been run in those shells.** The bash one is tested in real bash. Please report problems.
- **Tasks you delete in Godspeed stay in the local copy until a full refresh** (which happens by itself weekly, or with `refresh --full`).
- More than 250 tasks with exactly the same last-updated time can't all be fetched, which could only follow a bulk edit.
- **Results are a flat list**: `enable:nested` and `disable:nested` are accepted but change nothing.
- Weeks are Monday to Sunday, and `friday` means the next Friday, counting today. These are guesses where the search guide doesn't say.
- Only macOS and Linux are built. Error notifications use macOS notifications, and do nothing elsewhere.

### Under the hood

- The code is split into modules, and the release workflow now runs the tests on Ubuntu and macOS before building or publishing anything.
- The build binary is no longer tracked in git.
- The notes on each GitHub release are now this file's section for the version being released, instead of generated ones.

## 0.1.0

Create a task from the command line with labels (`.label`), a list (`@list`), a duration (`:minutes`) and notes (`n:`); tasks that can't be sent are queued and retried.
