//! `godspeed-cli completions <shell>`: scripts that add tab completion to bash, zsh and fish,
//! and the hidden `__complete lists|labels` command those scripts call to get names.
//!
//! The scripts complete the commands, their flags and the shell names, and, using the names
//! kept on disk, the lists and labels in `add` (after `@` and `.`) and in `search` (after
//! `list:`, `in:` and `label:`). They deliberately don't complete operators like `due:`.

use crate::store::Store;
use crate::{exit, names, paths};
use std::collections::HashSet;
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Lists,
    Labels,
}

impl Kind {
    fn parse(word: &str) -> Option<Kind> {
        match word {
            "lists" => Some(Kind::Lists),
            "labels" => Some(Kind::Labels),
            _ => None,
        }
    }
}

/// Characters a name may contain to be offered. Anything else could be read by a shell as
/// something other than part of a word (`$(...)`, `;`, quotes, ...), so such names are left out
/// rather than quoted: a completion script must never be a way to run a command named by data.
fn is_safe(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '+' | '/')
}

/// The names to offer: each with its whitespace removed (a task's `@list` and `.label` have to
/// be one word, and godspeed-cli accepts a name without its spaces), only the safe ones, each
/// once, in alphabetical order. A name seen twice that differs only in case keeps the first,
/// so give the names with their real capitalisation first.
fn candidates<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let word: String = name.split_whitespace().collect();
        if word.is_empty() || !word.chars().all(is_safe) {
            continue;
        }
        if seen.insert(word.to_lowercase()) {
            out.push(word);
        }
    }
    out.sort_by_key(|word| (word.to_lowercase(), word.clone()));
    out
}

/// The names from the local copy (as written), then from the lists and labels already used when
/// creating tasks (kept lowercase), so that the copy's capitalisation wins.
fn known_names(kind: Kind) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let store_path = paths::store_path();
    if store_path.exists() {
        if let Ok(store) = Store::open(&store_path) {
            let rows = match kind {
                Kind::Lists => store.lists(),
                Kind::Labels => store.labels(),
            };
            if let Ok(rows) = rows {
                found.extend(rows.iter().filter_map(|row| row.get("name")?.as_str().map(str::to_string)));
            }
        }
    }
    let cache = match kind {
        Kind::Lists => paths::lists_path(),
        Kind::Labels => paths::labels_path(),
    };
    let mut cached: Vec<String> = names::load(&cache).into_keys().collect();
    cached.sort();
    found.extend(cached);
    found
}

/// `__complete lists|labels`: the names, one per line. Never prints an error: this runs while
/// someone is pressing Tab.
pub fn run_names(args: &[String]) -> ExitCode {
    let Some(kind) = args.first().and_then(|word| Kind::parse(word)) else {
        return exit::usage();
    };
    let all = known_names(kind);
    let mut out = io::stdout().lock();
    for name in candidates(all.iter().map(String::as_str)) {
        // Ignore a closed pipe.
        if writeln!(out, "{name}").is_err() {
            break;
        }
    }
    exit::SUCCESS
}

const SHELLS: &str = "bash, fish, zsh";

fn script_for(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH),
        "zsh" => Some(ZSH),
        "fish" => Some(FISH),
        _ => None,
    }
}

/// `completions <shell>`: prints the script.
pub fn run_completions(args: &[String]) -> ExitCode {
    let [shell] = args else {
        eprintln!("Error: say which shell: godspeed-cli completions <bash|fish|zsh>");
        return exit::usage();
    };
    match script_for(shell) {
        Some(script) => {
            // Ignore a closed pipe.
            let _ = io::stdout().write_all(script.as_bytes());
            exit::SUCCESS
        }
        None => {
            eprintln!("Error: '{shell}' isn't a supported shell (supported: {SHELLS})");
            exit::usage()
        }
    }
}

/// For bash. `COMPREPLY` entries replace the word under the cursor, which bash cuts at the
/// characters in `COMP_WORDBREAKS` (including `:`, quotes and parentheses), so the part of the
/// word before the name is worked out from the text typed rather than assumed. Uses nothing newer
/// than bash 3.2, the version macOS ships.
const BASH: &str = r##"# bash completion for godspeed-cli. Add this to ~/.bashrc:
#   eval "$(godspeed-cli completions bash)"

# $1: lists or labels. $2: what has been typed of the name. $3: what to put before each name.
_godspeed_cli_names() {
    local name keep
    keep=$(shopt -p nocasematch)
    shopt -s nocasematch
    while IFS= read -r name; do
        [[ $name == "$2"* ]] && COMPREPLY+=("$3$name")
    done < <(godspeed-cli __complete "$1" 2>/dev/null)
    eval "$keep"
}

_godspeed_cli() {
    local cur=${COMP_WORDS[COMP_CWORD]} cmd=${COMP_WORDS[1]} flags
    local line=${COMP_LINE:0:COMP_POINT} word lead rest kind= op= typed head
    COMPREPLY=()
    if [[ $COMP_CWORD -eq 1 ]]; then
        COMPREPLY=($(compgen -W "add edit show queue lists labels refresh search completions --help --version" -- "$cur"))
        return
    fi
    case $cmd in
        refresh) COMPREPLY=($(compgen -W "--full --include-completed --json" -- "$cur")); return ;;
        edit) COMPREPLY=($(compgen -W "--interactive --dry-run --title --notes --complete --incomplete --due --duration --add-label --remove-label --snooze --move --delete --yes --json --debug" -- "$cur")); return ;;
        show) COMPREPLY=($(compgen -W "--json --debug" -- "$cur")); return ;;
        queue) COMPREPLY=($(compgen -W "list retry drop --json --count --all --yes --debug" -- "$cur")); return ;;
        lists|labels) COMPREPLY=($(compgen -W "--id --ids-only --count --json --color --debug" -- "$cur")); return ;;
        completions) COMPREPLY=($(compgen -W "bash fish zsh" -- "$cur")); return ;;
        add) flags="--json --dry-run --each --due --duration --snooze --start --notes --list --label --complete --debug" ;;
        search) flags="--json --id --ids-only --count --refresh --max-age --color --debug" ;;
        *) return ;;
    esac
    if [[ $cmd == search && ${COMP_WORDS[COMP_CWORD-1]} == --color ]]; then
        COMPREPLY=($(compgen -W "auto always never" -- "$cur"))
        return
    fi
    if [[ $cur == --* ]]; then
        COMPREPLY=($(compgen -W "$flags" -- "$cur"))
        return
    fi

    # The word being typed is the text after the last space before the cursor, without any
    # quote, ( ! or - in front of it. That is true inside a quoted task or query too.
    word=${line##*[[:space:]]}
    lead=${word%%[^\"\'\(!-]*}
    rest=${word#"$lead"}
    if [[ $cmd == add ]]; then
        case $rest in
            @*) kind=lists; op=@ ;;
            .*) kind=labels; op=. ;;
        esac
    else
        case $rest in
            list:*) kind=lists; op=list: ;;
            in:*) kind=lists; op=in: ;;
            label:*) kind=labels; op=label: ;;
        esac
    fi
    [[ -n $kind ]] || return
    typed=${rest#"$op"}
    head=${cur%"$typed"}
    _godspeed_cli_names "$kind" "$typed" "$head"
}

complete -F _godspeed_cli godspeed-cli
"##;

/// For zsh. Run after `compinit`. `compset -P` takes the text before the last space, and then the
/// operator, off the word, so only the name is completed even inside a quoted task or query.
const ZSH: &str = r##"# zsh completion for godspeed-cli. Add this to ~/.zshrc, after compinit:
#   eval "$(godspeed-cli completions zsh)"

# $1: lists or labels.
_godspeed_cli_names() {
    local -a names
    names=(${(f)"$(godspeed-cli __complete "$1" 2>/dev/null)"})
    (( $#names )) && compadd -M 'm:{a-zA-Z}={A-Za-z}' -a names
}

# Words of `add` and `search`: flags, and list and label names.
_godspeed_cli_words() {
    setopt localoptions extendedglob
    local cmd=$words[2]
    if [[ $cmd == search && $words[CURRENT-1] == --color ]]; then
        compadd -- auto always never
        return
    fi
    if [[ $PREFIX == --* ]]; then
        if [[ $cmd == add ]]; then
            compadd -- --json --dry-run --each --due --duration --snooze --start --notes --list --label --complete --debug
        else
            compadd -- --json --id --ids-only --count --refresh --max-age --color --debug
        fi
        return
    fi
    compset -P '*[[:space:]]'
    compset -P '[(!-]##'
    if [[ $cmd == add ]]; then
        if compset -P '@'; then
            _godspeed_cli_names lists
        elif compset -P '.'; then
            _godspeed_cli_names labels
        fi
    else
        if compset -P 'list:' || compset -P 'in:'; then
            _godspeed_cli_names lists
        elif compset -P 'label:'; then
            _godspeed_cli_names labels
        fi
    fi
}

_godspeed_cli() {
    local -a commands
    commands=(
        'add:create a task'
        'edit:edit, complete, snooze, delete or move a task'
        'show:show everything about one task'
        'queue:show or send what is waiting to be sent'
        'lists:show your lists'
        'labels:show your labels'
        'refresh:update the local copy of your tasks'
        'search:search the local copy'
        'completions:print a shell completion script'
    )
    if (( CURRENT == 2 )); then
        _describe -t commands 'command' commands
        [[ $PREFIX == -* ]] && compadd -- --help --version
        return
    fi
    case $words[2] in
        refresh) compadd -- --full --include-completed --json ;;
        edit) compadd -- --interactive --dry-run --title --notes --complete --incomplete --due --duration --add-label --remove-label --snooze --move --delete --yes --json --debug ;;
        show) compadd -- --json --debug ;;
        queue) compadd -- list retry drop --json --count --all --yes --debug ;;
        lists|labels) compadd -- --id --ids-only --count --json --color --debug ;;
        completions) compadd -- bash fish zsh ;;
        add|search) _godspeed_cli_words ;;
    esac
}

compdef _godspeed_cli godspeed-cli
"##;

/// For fish. The function offers whole replacements for the token under the cursor, as fish
/// expects: what is before the name in the token, then the name.
const FISH: &str = r##"# fish completion for godspeed-cli. Add this to ~/.config/fish/config.fish:
#   godspeed-cli completions fish | source

function __godspeed_cli_names
    set -l words (commandline -opc)
    set -l cmd "$words[2]"
    set -l token (commandline -ct)
    set -l word (string match -r -- '\S*$' "$token")
    set -l lead (string replace -r -- '^([("\'!-]*).*$' '$1' "$word")
    set -l rest (string sub -s (math (string length -- "$lead") + 1) -- "$word")
    set -l kind
    set -l op
    if test "$cmd" = add
        switch "$rest"
            case '@*'
                set kind lists
                set op @
            case '.*'
                set kind labels
                set op .
        end
    else
        switch "$rest"
            case 'list:*'
                set kind lists
                set op list:
            case 'in:*'
                set kind lists
                set op in:
            case 'label:*'
                set kind labels
                set op label:
        end
    end
    test -n "$kind"; or return
    set -l typed (string sub -s (math (string length -- "$op") + 1) -- "$rest")
    set -l head (string sub -l (math (string length -- "$token") - (string length -- "$word")) -- "$token")
    for name in (godspeed-cli __complete $kind 2>/dev/null)
        if string match -q -i -- "$typed*" "$name"
            echo "$head$lead$op$name"
        end
    end
end

complete -c godspeed-cli -f
complete -c godspeed-cli -n __fish_use_subcommand -a add -d 'Create a task'
complete -c godspeed-cli -n __fish_use_subcommand -a edit -d 'Edit, complete, snooze, delete or move a task'
complete -c godspeed-cli -n __fish_use_subcommand -a show -d 'Show everything about one task'
complete -c godspeed-cli -n __fish_use_subcommand -a queue -d 'Show or send what is waiting to be sent'
complete -c godspeed-cli -n __fish_use_subcommand -a lists -d 'Show your lists'
complete -c godspeed-cli -n __fish_use_subcommand -a labels -d 'Show your labels'
complete -c godspeed-cli -n __fish_use_subcommand -a refresh -d 'Update the local copy of your tasks'
complete -c godspeed-cli -n __fish_use_subcommand -a search -d 'Search the local copy'
complete -c godspeed-cli -n __fish_use_subcommand -a completions -d 'Print a shell completion script'
complete -c godspeed-cli -n __fish_use_subcommand -s h -l help -d 'Show the help'
complete -c godspeed-cli -n __fish_use_subcommand -s V -l version -d 'Show the version'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l json -d 'Print the API response as JSON'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l dry-run -d 'Show what would be created'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l each -d 'Make a task of every line or argument'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l due -x -d 'Set the due date'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l duration -x -d 'Set the duration'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l snooze -x -d 'Snooze until'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l start -x -d 'Set the start date'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l notes -x -d 'Set the notes'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l list -x -a '(godspeed-cli __complete lists)' -d 'Put the task in a list'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l label -x -a '(godspeed-cli __complete labels)' -d 'Add a label'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add' -l complete -d 'Create the task already complete'
complete -c godspeed-cli -n '__fish_seen_subcommand_from refresh' -l full -d 'Rebuild the copy from scratch'
complete -c godspeed-cli -n '__fish_seen_subcommand_from refresh' -l include-completed -d 'Also keep completed tasks'
complete -c godspeed-cli -n '__fish_seen_subcommand_from refresh' -l json -d 'Print a report as JSON'
complete -c godspeed-cli -n '__fish_seen_subcommand_from search' -l json -d 'Print the results as JSON'
complete -c godspeed-cli -n '__fish_seen_subcommand_from search lists labels' -l id -d 'Show ids'
complete -c godspeed-cli -n '__fish_seen_subcommand_from lists labels' -l json -d 'Print JSON'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l title -x -d 'New title'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l notes -x -d 'New notes'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l complete -d 'Complete the task'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l incomplete -d 'Reopen the task'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l snooze -x -d 'Snooze until'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l due -x -d 'Set the due date'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l duration -x -d 'Set the duration'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l add-label -x -a '(godspeed-cli __complete labels)' -d 'Add a label'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l remove-label -x -a '(godspeed-cli __complete labels)' -d 'Remove a label'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l json -d 'Print the results as JSON'
complete -c godspeed-cli -n '__fish_seen_subcommand_from search lists labels' -l ids-only -d 'Print only the ids'
complete -c godspeed-cli -n '__fish_seen_subcommand_from search lists labels queue' -l count -d 'Print only the count'
complete -c godspeed-cli -n '__fish_seen_subcommand_from show queue' -l json -d 'Print JSON'
complete -c godspeed-cli -n '__fish_seen_subcommand_from queue' -a 'list retry drop'
complete -c godspeed-cli -n '__fish_seen_subcommand_from queue' -l all -d 'Drop everything'
complete -c godspeed-cli -n '__fish_seen_subcommand_from queue' -l yes -d 'Do not ask'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l dry-run -d 'Show what would be done'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l interactive -s i -d 'Choose the task from a list'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l move -x -a '(godspeed-cli __complete lists)' -d 'Move to a list'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l delete -d 'Delete the task'
complete -c godspeed-cli -n '__fish_seen_subcommand_from edit' -l yes -d 'Do not ask before deleting'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add edit show queue lists labels refresh search' -l debug -d 'Show what the program is doing'
complete -c godspeed-cli -n '__fish_seen_subcommand_from search' -l refresh -d 'Update the copy first'
complete -c godspeed-cli -n '__fish_seen_subcommand_from search' -l max-age -x -d 'Update the copy first if older than this'
complete -c godspeed-cli -n '__fish_seen_subcommand_from search' -l color -x -a 'auto always never' -d 'When to use colour'
complete -c godspeed-cli -n '__fish_seen_subcommand_from completions' -a 'bash fish zsh'
complete -c godspeed-cli -n '__fish_seen_subcommand_from add search' -a '(__godspeed_cli_names)'
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(names: &[&str]) -> Vec<String> {
        candidates(names.iter().copied())
    }

    #[test]
    fn names_are_sorted_ignoring_case_and_listed_once() {
        assert_eq!(offered(&["work", "Home", "alpha", "Zed"]), vec!["alpha", "Home", "work", "Zed"]);
        assert_eq!(offered(&["Home", "home", "HOME"]), vec!["Home"], "the first capitalisation wins");
        assert_eq!(offered(&["b", "a", "b", "a"]), vec!["a", "b"]);
    }

    #[test]
    fn spaces_are_removed_so_the_name_is_one_word() {
        assert_eq!(offered(&["Work Projects", "Side  Projects", "Tab\tName"]), vec!["SideProjects", "TabName", "WorkProjects"]);
        // The squashed form of two names counts as one.
        assert_eq!(offered(&["Work Projects", "workprojects"]), vec!["WorkProjects"]);
    }

    #[test]
    fn only_plain_characters_are_ever_offered() {
        let names = offered(&[
            "Fine",
            "high-priority",
            "v1.2",
            "a/b",
            "Re:Plan",
            "c++",
            "naïve",
            "日本",
            "Evil $(touch x)",
            "semi;colon",
            "back`tick`",
            "quo\"te",
            "it's",
            "pipe|pipe",
            "amp&amp",
            "dollar$HOME",
            "paren(s)",
            "star*",
            "glob?",
            "[bracket]",
            "tilde~",
            "at@sign",
            "hash#",
            "back\\slash",
            "bang!",
            "less<than",
            "{brace}",
            "",
            "   ",
        ]);
        assert_eq!(names, vec!["a/b", "c++", "Fine", "high-priority", "naïve", "Re:Plan", "v1.2", "日本"]);
    }

    #[test]
    fn nothing_offered_can_be_misread_by_a_shell() {
        // Whatever the names are, what comes out has no character with a special meaning.
        let nasty = ["$(rm -rf ~)", "`id`", "a b; c", "x\ny", "'; echo hi; '", "${HOME}", "\\n"];
        for name in offered(&nasty) {
            assert!(name.chars().all(is_safe), "{name}");
            assert!(!name.contains(['$', '`', ';', '\'', '"', '\\', '(', ')', '{', '}', '&', '|', '<', '>', '*', '?', ' ', '\n']));
        }
    }

    #[test]
    fn the_kind_is_lists_or_labels() {
        assert_eq!(Kind::parse("lists"), Some(Kind::Lists));
        assert_eq!(Kind::parse("labels"), Some(Kind::Labels));
        assert_eq!(Kind::parse("List"), None);
        assert_eq!(Kind::parse(""), None);
    }

    #[test]
    fn each_shell_has_a_script_and_others_do_not() {
        for shell in ["bash", "zsh", "fish"] {
            let script = script_for(shell).unwrap();
            assert!(script.contains("godspeed-cli __complete"), "{shell}");
            assert!(script.contains("completions"), "{shell}");
        }
        for shell in ["", "sh", "powershell", "Bash", "ksh", "elvish", "nu"] {
            assert!(script_for(shell).is_none(), "{shell}");
        }
    }

    #[test]
    fn the_scripts_complete_the_commands_flags_lists_and_labels_but_not_due_or_has() {
        for (shell, script) in [("bash", BASH), ("zsh", ZSH), ("fish", FISH)] {
            for needle in ["add", "refresh", "search", "completions", "list:", "in:", "label:", "bash fish zsh"] {
                assert!(script.contains(needle), "{shell} script lacks {needle}");
            }
            // Fish declares a long option as `-l json`; the others spell it out.
            for flag in ["json", "full", "include-completed", "refresh", "max-age", "color"] {
                let spelled = if shell == "fish" { format!("-l {flag}") } else { format!("--{flag}") };
                assert!(script.contains(&spelled), "{shell} script lacks {spelled}");
            }
            // Operators beyond lists and labels are not completed.
            for left_out in ["due:", "has:", "first-due", "snoozed", "folder:", "label-group", "sort:", "group:"] {
                assert!(!script.contains(left_out), "{shell} script mentions {left_out}");
            }
        }
    }

    #[test]
    fn the_scripts_run_nothing_but_godspeed_cli_and_shell_builtins() {
        for (shell, script) in [("bash", BASH), ("zsh", ZSH), ("fish", FISH)] {
            let code: Vec<&str> = script.lines().filter(|line| !line.trim_start().starts_with('#')).collect();
            let code = code.join("\n");
            // Only the install hint in the comments mentions the command that prints the script.
            assert!(!code.contains("godspeed-cli completions"), "{shell}");
            // The one `eval` is bash putting a shell option back as it was.
            let evals: Vec<&str> = code.lines().filter(|l| l.contains("eval")).collect();
            if shell == "bash" {
                assert_eq!(evals, vec!["    eval \"$keep\""]);
            } else {
                assert!(evals.is_empty(), "{shell}: {evals:?}");
            }
            // Names are only ever passed to the shell as quoted words.
            for risky in ["compgen -W \"$(", "compgen -W `", "$(godspeed-cli __complete $", "| sh", "| bash", "source <("] {
                assert!(!code.contains(risky), "{shell}: {risky}");
            }
        }
    }

    #[test]
    fn the_scripts_are_plain_ascii_and_end_with_a_newline() {
        for (shell, script) in [("bash", BASH), ("zsh", ZSH), ("fish", FISH)] {
            assert!(script.is_ascii(), "{shell}");
            assert!(script.ends_with('\n'), "{shell}");
            assert!(!script.contains('\t'), "{shell}");
            assert!(!script.contains('\r'), "{shell}");
        }
    }
}
