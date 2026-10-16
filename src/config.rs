//! The optional settings file, `$XDG_CONFIG_HOME/godspeed-cli/config.toml`
//! (`~/.config/godspeed-cli/config.toml` by default). Command-line flags override it.
//!
//! ```toml
//! max_age = "12h"            # search refreshes first when the copy is older than this
//! include_completed = true   # refresh also keeps completed tasks
//! full_refresh_after = "7d"  # a refresh is a full one when the last full one is this old
//! ```
//!
//! Durations are written like `30m`, `12h`, `1 day` or `2 days and 6 hours`; `never` turns a
//! setting off.

use crate::dates::parse_minutes;
use crate::paths;
use chrono::Duration;
use std::fs;
use std::io;
use std::path::Path;

/// How long a copy may get before it is considered too old.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Age {
    Never,
    After(Duration),
}

impl Age {
    pub fn parse(text: &str) -> Option<Age> {
        let text = text.trim().to_lowercase();
        if text == "never" {
            return Some(Age::Never);
        }
        Some(Age::After(Duration::minutes(parse_minutes(&text)?)))
    }

    /// The age as a limit: None when it is "never".
    pub fn limit(self) -> Option<Duration> {
        match self {
            Age::Never => None,
            Age::After(duration) => Some(duration),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `search` refreshes first when the copy is older than this. Off by default.
    pub max_age: Option<Age>,
    /// Whether `refresh` also keeps completed tasks.
    pub include_completed: bool,
    /// A refresh becomes a full one when the last full one is older than this.
    pub full_refresh_after: Age,
}

impl Default for Config {
    fn default() -> Config {
        Config { max_age: None, include_completed: false, full_refresh_after: Age::After(Duration::days(7)) }
    }
}

const KNOWN_KEYS: [&str; 3] = ["max_age", "include_completed", "full_refresh_after"];

fn age_setting(table: &toml::Table, key: &str) -> Result<Option<Age>, String> {
    let Some(value) = table.get(key) else { return Ok(None) };
    let text = value.as_str().ok_or_else(|| format!("'{key}' must be a string such as \"12h\" or \"never\""))?;
    Age::parse(text)
        .map(Some)
        .ok_or_else(|| format!("'{key}' is not a duration: \"{text}\" (use something like \"30m\", \"12h\", \"2 days\" or \"never\")"))
}

pub fn parse(text: &str) -> Result<Config, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
    if let Some(unknown) = table.keys().find(|key| !KNOWN_KEYS.contains(&key.as_str())) {
        return Err(format!("unknown setting '{unknown}' (the settings are: {})", KNOWN_KEYS.join(", ")));
    }
    let mut config = Config::default();
    config.max_age = age_setting(&table, "max_age")?;
    if let Some(age) = age_setting(&table, "full_refresh_after")? {
        config.full_refresh_after = age;
    }
    if let Some(value) = table.get("include_completed") {
        config.include_completed =
            value.as_bool().ok_or_else(|| "'include_completed' must be true or false".to_string())?;
    }
    Ok(config)
}

pub fn load_from(path: &Path) -> Result<Config, String> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// The settings, or the defaults if there is no settings file.
pub fn load() -> Result<Config, String> {
    load_from(&paths::config_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_settings_gives_the_defaults() {
        let config = parse("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.max_age, None);
        assert!(!config.include_completed);
        assert_eq!(config.full_refresh_after, Age::After(Duration::days(7)));
    }

    #[test]
    fn every_setting_can_be_given() {
        let config = parse("max_age = \"12h\"\ninclude_completed = true\nfull_refresh_after = \"2 days\"\n").unwrap();
        assert_eq!(config.max_age, Some(Age::After(Duration::hours(12))));
        assert!(config.include_completed);
        assert_eq!(config.full_refresh_after, Age::After(Duration::days(2)));
    }

    #[test]
    fn durations_may_be_written_in_words_and_never_turns_a_setting_off() {
        assert_eq!(parse("max_age = \"1 day and 6 hours\"").unwrap().max_age, Some(Age::After(Duration::hours(30))));
        assert_eq!(parse("max_age = \"30m\"").unwrap().max_age, Some(Age::After(Duration::minutes(30))));
        assert_eq!(parse("max_age = \"0\"").unwrap().max_age, Some(Age::After(Duration::zero())));
        assert_eq!(parse("max_age = \"never\"").unwrap().max_age, Some(Age::Never));
        assert_eq!(parse("full_refresh_after = \"NEVER\"").unwrap().full_refresh_after, Age::Never);
    }

    #[test]
    fn mistakes_are_reported_by_name() {
        assert!(parse("max_ag = \"1h\"").unwrap_err().contains("unknown setting 'max_ag'"));
        assert!(parse("max_age = \"soon\"").unwrap_err().contains("'max_age' is not a duration"));
        assert!(parse("max_age = 12").unwrap_err().contains("'max_age' must be a string"));
        assert!(parse("include_completed = \"yes\"").unwrap_err().contains("must be true or false"));
        assert!(parse("full_refresh_after = true").unwrap_err().contains("'full_refresh_after' must be a string"));
        assert!(!parse("max_age = ").unwrap_err().is_empty());
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_an_unreadable_one_names_the_file() {
        let dir = std::env::temp_dir().join(format!("godspeed-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_from(&dir.join("absent.toml")).unwrap(), Config::default());

        let bad = dir.join("config.toml");
        std::fs::write(&bad, "max_age = \"never\"\nbogus = 1\n").unwrap();
        let error = load_from(&bad).unwrap_err();
        assert!(error.contains("config.toml") && error.contains("unknown setting 'bogus'"), "{error}");

        std::fs::write(&bad, "max_age = \"3h\"\n").unwrap();
        assert_eq!(load_from(&bad).unwrap().max_age, Some(Age::After(Duration::hours(3))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn age_limit() {
        assert_eq!(Age::Never.limit(), None);
        assert_eq!(Age::After(Duration::hours(1)).limit(), Some(Duration::hours(1)));
        assert_eq!(Age::parse("garbage"), None);
    }
}
