//! The words that are commands of godspeed-cli, in one place.
//!
//! A saved search (see `searches.rs`) can be run by its name alone, `godspeed-cli overdue`, so
//! its name must never be a command: the command would win, and the search could never be run.
//! Every word here is refused as the name of a saved search, and a settings file that already
//! has a saved search with one of these names is an error, so that adding a command later can't
//! quietly take over someone's search.
//!
//! **When you add a command, add its word here.** A test checks that every word here is
//! recognised by `main.rs`, and `main.rs` builds its error messages from this list.

/// Every top-level command, in the order they are listed in errors and in the help.
pub const COMMANDS: [&str; 10] = ["add", "edit", "show", "queue", "lists", "labels", "refresh", "search", "searches", "completions"];

/// Words that are not commands yet but are kept free, so they can become one without clashing
/// with saved searches that were made in the meantime.
pub const RESERVED: [&str; 6] = ["help", "version", "list", "config", "settings", "delete"];

/// Whether `name` can't be the name of a saved search because it is, or is kept for, a command.
pub fn is_reserved(name: &str) -> bool {
    COMMANDS.contains(&name) || RESERVED.contains(&name)
}

/// The commands as a list for a message: `add, edit, show, ...`.
pub fn list() -> String {
    COMMANDS.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_reserved_and_so_is_every_word_kept_for_one() {
        for word in COMMANDS.iter().chain(RESERVED.iter()) {
            assert!(is_reserved(word), "{word}");
        }
        assert!(!is_reserved("today"));
        assert!(!is_reserved("overdue"));
        assert!(!is_reserved("inbox"));
    }

    #[test]
    fn the_lists_have_no_repeats_and_no_overlap() {
        let mut all: Vec<&str> = COMMANDS.iter().chain(RESERVED.iter()).copied().collect();
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count);
    }

    #[test]
    fn commands_are_listed_in_a_message() {
        assert!(list().starts_with("add, edit, show"));
        assert!(list().ends_with("searches, completions"));
    }
}
