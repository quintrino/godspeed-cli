//! The local copy of the user's tasks, lists and labels, in SQLite.
//!
//! Each row keeps the API's JSON as it came back, so fields the CLI doesn't know about yet are
//! not lost. `is_complete` comes from the status the task was fetched with.

use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const SCHEMA_VERSION: i64 = 1;

pub struct CachedTask {
    pub json: Value,
    pub complete: bool,
}

#[derive(Default)]
pub struct Snapshot {
    pub tasks: Vec<CachedTask>,
    pub lists: Vec<Value>,
    pub labels: Vec<Value>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Counts {
    pub tasks: usize,
    pub lists: usize,
    pub labels: usize,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let store = Store { conn: Connection::open(path)? };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "the cache was written by a newer godspeed-cli (schema {version}, this one reads {SCHEMA_VERSION}); \
                 upgrade, or delete the cache file"
            )
            .into());
        }
        if version < 1 {
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS tasks (
                    id TEXT PRIMARY KEY,
                    json TEXT NOT NULL,
                    is_complete INTEGER NOT NULL,
                    updated_at TEXT
                );
                CREATE TABLE IF NOT EXISTS lists (
                    id TEXT PRIMARY KEY,
                    json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS labels (
                    id TEXT PRIMARY KEY,
                    json TEXT NOT NULL
                );
                ",
            )?;
            self.conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }

    /// Replaces everything in the store with `snapshot`, all or nothing.
    pub fn replace_all(&mut self, snapshot: &Snapshot, refreshed_at: &str) -> Result<Counts> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM tasks", [])?;
        tx.execute("DELETE FROM lists", [])?;
        tx.execute("DELETE FROM labels", [])?;

        for task in &snapshot.tasks {
            if let Some(id) = string_field(&task.json, "id") {
                tx.execute(
                    "INSERT OR REPLACE INTO tasks (id, json, is_complete, updated_at) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        id,
                        serde_json::to_string(&task.json)?,
                        i64::from(task.complete),
                        string_field(&task.json, "updated_at"),
                    ],
                )?;
            }
        }
        for (table, values) in [("lists", &snapshot.lists), ("labels", &snapshot.labels)] {
            for value in values {
                if let Some(id) = string_field(value, "id") {
                    tx.execute(
                        &format!("INSERT OR REPLACE INTO {table} (id, json) VALUES (?1, ?2)"),
                        params![id, serde_json::to_string(value)?],
                    )?;
                }
            }
        }

        tx.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES ('refreshed_at', ?1)",
            params![refreshed_at],
        )?;
        tx.commit()?;
        self.counts()
    }

    pub fn counts(&self) -> Result<Counts> {
        let count = |table: &str| -> Result<usize> {
            let n: i64 = self.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?;
            Ok(n as usize)
        };
        Ok(Counts { tasks: count("tasks")?, lists: count("lists")?, labels: count("labels")? })
    }
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn memory() -> Store {
        let store = Store { conn: Connection::open_in_memory().unwrap() };
        store.migrate().unwrap();
        store
    }

    fn task(id: &str, complete: bool) -> CachedTask {
        CachedTask { json: json!({"id": id, "title": format!("Task {id}"), "updated_at": "2026-01-01T00:00:00Z"}), complete }
    }

    fn refreshed_at(store: &Store) -> Option<String> {
        store
            .conn
            .query_row("SELECT value FROM metadata WHERE key = 'refreshed_at'", [], |row| row.get(0))
            .ok()
    }

    #[test]
    fn replace_all_stores_everything_and_reports_the_counts() {
        let mut store = memory();
        let snapshot = Snapshot {
            tasks: vec![task("a", false), task("b", true)],
            lists: vec![json!({"id": "l1", "name": "Inbox"})],
            labels: vec![json!({"id": "x1", "name": "Low"}), json!({"id": "x2", "name": "High"})],
        };

        let counts = store.replace_all(&snapshot, "2026-10-15T00:00:00Z").unwrap();

        assert_eq!(counts, Counts { tasks: 2, lists: 1, labels: 2 });
        assert_eq!(refreshed_at(&store).as_deref(), Some("2026-10-15T00:00:00Z"));
    }

    #[test]
    fn the_completion_flag_and_json_are_kept() {
        let mut store = memory();
        store
            .replace_all(&Snapshot { tasks: vec![task("a", false), task("b", true)], ..Default::default() }, "t")
            .unwrap();

        let rows: Vec<(String, i64, String)> = store
            .conn
            .prepare("SELECT id, is_complete, json FROM tasks ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows[0].0, "a");
        assert_eq!(rows[0].1, 0);
        assert_eq!(rows[1].1, 1);
        let stored: Value = serde_json::from_str(&rows[1].2).unwrap();
        assert_eq!(stored["title"], "Task b");
    }

    #[test]
    fn a_second_replace_discards_what_was_there_before() {
        let mut store = memory();
        store.replace_all(&Snapshot { tasks: vec![task("a", false), task("b", false)], ..Default::default() }, "1").unwrap();

        let counts = store.replace_all(&Snapshot { tasks: vec![task("c", false)], ..Default::default() }, "2").unwrap();

        assert_eq!(counts.tasks, 1);
        assert_eq!(refreshed_at(&store).as_deref(), Some("2"));
    }

    #[test]
    fn repeated_lists_labels_and_tasks_are_stored_once() {
        let mut store = memory();
        let list = json!({"id": "l1", "name": "Inbox"});
        let snapshot = Snapshot {
            tasks: vec![task("a", false), task("a", false)],
            lists: vec![list.clone(), list],
            labels: vec![],
        };

        let counts = store.replace_all(&snapshot, "t").unwrap();

        assert_eq!(counts, Counts { tasks: 1, lists: 1, labels: 0 });
    }

    #[test]
    fn rows_without_an_id_are_skipped() {
        let mut store = memory();
        let snapshot = Snapshot {
            tasks: vec![CachedTask { json: json!({"title": "no id"}), complete: false }],
            lists: vec![json!({"name": "no id"})],
            labels: vec![],
        };

        assert_eq!(store.replace_all(&snapshot, "t").unwrap(), Counts { tasks: 0, lists: 0, labels: 0 });
    }

    #[test]
    fn opening_twice_keeps_the_data_and_a_newer_schema_is_refused() {
        let dir = std::env::temp_dir().join(format!("godspeed-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.sqlite3");
        let _ = std::fs::remove_file(&path);

        Store::open(&path).unwrap().replace_all(&Snapshot { tasks: vec![task("a", false)], ..Default::default() }, "t").unwrap();
        assert_eq!(Store::open(&path).unwrap().counts().unwrap().tasks, 1);

        Connection::open(&path).unwrap().pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        let error = Store::open(&path).err().expect("a newer schema is refused").to_string();
        assert!(error.contains("newer godspeed-cli"), "{error}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
