use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

pub fn data_home() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = env::var("HOME") {
        PathBuf::from(home).join(".local").join("share")
    } else {
        PathBuf::from(".local").join("share")
    }
}

fn data_dir() -> PathBuf {
    data_home().join("godspeed-cli")
}

/// Tasks that couldn't be sent, waiting to be retried.
pub fn queue_path() -> PathBuf {
    data_dir().join("cache")
}

/// The SQLite copy of the user's tasks, lists and labels, written by `refresh`.
pub fn store_path() -> PathBuf {
    data_dir().join("cache.sqlite3")
}

pub fn lists_path() -> PathBuf {
    data_dir().join("lists.toml")
}

pub fn labels_path() -> PathBuf {
    data_dir().join("labels.toml")
}

/// Where the token is read from when `GODSPEED_TOKEN` isn't set in the environment.
pub fn token_file_path() -> PathBuf {
    data_home().join("environment").join("Godspeed")
}

pub fn ensure_data_dir() -> io::Result<()> {
    fs::create_dir_all(data_dir())
}

pub fn config_home() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    if let Ok(home) = env::var("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        PathBuf::from(".config")
    }
}

/// The optional settings file, `$XDG_CONFIG_HOME/godspeed-cli/config.toml`.
pub fn config_path() -> PathBuf {
    config_home().join("godspeed-cli").join("config.toml")
}
