# Changelog

## 0.16.0: 2026-10-16

### Added

- **Saved searches.** Give a search a name and run it by that name: `godspeed-cli searches add inbox 'list:Inbox sort-desc:created'`, then `godspeed-cli inbox`, or `godspeed-cli search @inbox`. More search words and every `search` option can follow (`godspeed-cli inbox label:Low --json`, `godspeed-cli overdue --ids-only | godspeed-cli edit --snooze tomorrow`). They are kept in a `[searches]` table in the settings file, which can be written by hand; `searches add` and `remove` change only that table's lines, keep the rest of the file, and write nothing if they can't be sure of the result.
  - `@name` in a search is replaced by the saved query in parentheses, so `a | b` stays together and `-@name` works. A saved search can use others, up to eight deep; a loop is an error; a mistake in one says which (`In saved search 'inbox': Unknown sort field 'colour'`). `searches add` tries the query, and what it refers to, before saving it.
  - **`today` (`due:today sort:due`) and `overdue` (`due<today sort:due`) are built in** and can be replaced by saving your own with the same name; removing yours brings the built-in back. Neither mentions completed tasks, which a search leaves out unless it says `is:complete`, so `godspeed-cli overdue is:complete` finds the completed ones.
  - A name is lowercase letters, digits, `-` and `_` (up to 40), and isn't case-sensitive when run. `godspeed-cli NAME --help` shows what a saved search runs, and `godspeed-cli searches --help` is a new help page. `search --help` and the settings-file help describe `@name` and `[searches]`.
  - `searches`, `searches show`, `searches add` and `searches remove`, with `--json` and `--count` on the list.
- **A saved search can't be named like a command.** `searches add` refuses the name of any command, or of a word kept for one (`help`, `version`, `list`, `config`, `settings`, `delete`). If a settings file has such a name, such as after a later version adds a command, every command that reads the settings stops with an error that names the search and says to rename it; `searches remove` and editing the file still work, and `add` is unaffected. The commands are listed in one place, with a test that fails if a command is missing from the list.
- **Tab completion** offers saved searches as commands and after `@`, via a new hidden `__complete searches`.

### Changed

- A first word that isn't a command is now looked up among the saved searches before it is an error. Anything not saved is still an error, so a typo still can't create a task, and the error now points to `godspeed-cli searches`.
- **A word starting with `@` in a search is now a saved-search name.** A name that isn't saved is an error that lists the saved ones. To search for a title that really contains `@something`, quote it: `search '"@something"'`.
- `search` reads the settings file before the query, so a mistake in either is reported as before but a mistake in the settings wins.

## 0.15.0: 2026-10-16

### Added

- **`add` takes the same things `edit` changes**, as options straight after `add`: `--due`, `--duration`, `--list`, `--label` (repeatable), `--notes`, `--snooze`, `--start` and `--complete`. Dates are read as `edit` reads them (`tomorrow`, `"friday 5pm"`, `2026-10-31`). An option wins over the same thing written in the task, and `--label` adds to any `.label`.
  - A snooze, a start date and being complete can't be given when a task is created, so they are sent as a change to the new task straight after. If that can't be sent, the task still exists and the change is queued.
  - A task that can't be sent is queued with its options.
- **`add --each`** makes a task of every argument, or of every line piped in, so `godspeed-cli add --each --list Inbox < todo.txt` works. One failing doesn't stop the others; a summary is printed, and `--json` gives each task's status.
- **`add --dry-run`** shows what would be created and sends nothing, with no token or network needed.
- **`edit --dry-run`** shows what would be done to each task, as it would be sent, and does nothing: nothing is sent, asked, queued, or changed in the local copy. Use it to check a pipe such as `search ... --ids-only | edit --delete` first.

### Changed

- Options of `add` must come straight after `add`, as `--json` always had to. A task that starts with one of the new options (`--due`, `--list`, ...) now needs `add -- ...`.
- `queue` shows a waiting task's options next to its text.

## 0.14.0: 2026-10-16

### Added

- **`godspeed-cli show <id | words>`** prints everything the local copy knows about one task: id, status, list, labels, due date (marked overdue or today), snooze, start date, duration, created and updated times, and notes. `--json` prints the full record. It says if changes to the task are waiting to be sent.
- **`godspeed-cli queue`** shows what is waiting in the retry queue, numbered: tasks `add` couldn't send, and changes `edit` couldn't send. `queue retry` sends it all now, `queue drop N...` and `queue drop --all` throw items away (after asking, unless `--yes`), and `--json` and `--count` are there for scripts.
- **`edit --interactive`** (`-i`) chooses the task from a list of your incomplete tasks: type a number to choose, or words to narrow the list. Then the menu appears, or the options you gave are applied.
- **`--count`** on `search`, `lists`, `labels` and `queue` prints just how many there are. A search that finds nothing prints `0` and exits with status 0.
- Tab completion and the help pages cover all of this.

## 0.13.0: 2026-10-16

### Added

- **`edit` can set the due date and duration**: `--due WHEN` (`tomorrow`, `"friday 5pm"`, `2026-10-31`) and `--duration LENGTH` (`45`, `45m`, `1.5h`). The menu has them too.
- **`edit` can change labels**: `--add-label NAME` and `--remove-label NAME`, each repeatable, matched like `.label` in `add`. An unknown label is an error that lists your labels.
- **Edit many tasks at once**: with options and no task, `edit` reads ids from stdin, one per line, so `godspeed-cli search 'due:today' --ids-only | godspeed-cli edit --complete` works. A line that isn't an id is refused, deleting this way needs `--yes`, and a summary is printed.
- **`--ids-only`** on `search`, `lists` and `labels` prints just the ids, one per line, in result order.
- **`edit --json`** prints what happened to each task (`done`, `queued` or `failed`).
- **Changes are queued when Godspeed can't be reached.** An `edit` that fails because of the connection or a 5xx, 429 or 408 is saved in the retry queue, and the local copy is updated anyway. Every `add` and `edit` now sends what is waiting first, in order, so an older change can't overwrite a newer one. A change Godspeed refuses (such as a 404) isn't queued, and a queued one that is refused later is dropped with a message.

### Changed

- `edit` looks up lists and labels in the local copy before asking Godspeed.
- Tasks piped to `edit` are one id per line; ids on the same line are refused.

### Fixed

- The clippy warnings in the existing code.

## 0.12.0: 2026-10-16

### Added

- **`godspeed-cli edit <id>`** changes a task: edit its title or notes, complete or reopen it, snooze it, delete it, or move it to another list. With just an id it shows a menu; with options (`--title`, `--notes`, `--complete`, `--incomplete`, `--snooze WHEN`, `--move LIST`, `--delete`, `--yes`) it does the change at once. The task can also be named by words from its title.
- **`godspeed-cli lists [STRING]`** and **`godspeed-cli labels [STRING]`** show your lists and labels, only those matching the string if one is given.
- **`--id`** adds the id to the output of `search`, `lists` and `labels`.
- **`--debug`** on any command prints, on stderr, what the program is doing: the arguments, where the token came from, the settings, every request and answer, and the choices made. The token is never printed.
- Tab completion knows the new commands and options.

### Fixed

- The release workflow no longer runs `git pull --rebase` in the Homebrew tap, which left it in a conflict ("could not apply ... godspeed-cli 0.1.0.1") when two releases collided. It now starts again from the tap's latest commit.

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

**Please read these.** The test suite is large (about 480 tests, including end-to-end tests that run the real program against a mock of the API), but a mock isn't Godspeed:

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
- The notes on each GitHub release are now this file's section for the version being released (`## X.Y.Z`), instead of generated ones. `bash scripts/release-notes.sh X.Y.Z` prints what a release would use. A test fails if this file has no section for the version in `Cargo.toml`, so write the notes before bumping the version; if a release is made without them anyway, it falls back to the notes GitHub generates and says so in the workflow log.

## 0.1.0

Create a task from the command line with labels (`.label`), a list (`@list`), a duration (`:minutes`) and notes (`n:`); tasks that can't be sent are queued and retried.
