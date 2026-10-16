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

/// A task read back from the store.
pub struct StoredTask {
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
                        // The API repeats lists and labels on every page. Keeping the first copy
                        // keeps them in the order the API gave them, which is the sidebar order.
                        &format!("INSERT OR IGNORE INTO {table} (id, json) VALUES (?1, ?2)"),
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

    /// Cached tasks, most recently updated first. Completed ones only if `include_completed`.
    pub fn tasks(&self, include_completed: bool) -> Result<Vec<StoredTask>> {
        let sql = if include_completed {
            "SELECT json, is_complete FROM tasks ORDER BY updated_at DESC, id"
        } else {
            "SELECT json, is_complete FROM tasks WHERE is_complete = 0 ORDER BY updated_at DESC, id"
        };
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
        let mut tasks = Vec::new();
        for row in rows {
            let (json, complete) = row?;
            tasks.push(StoredTask { json: serde_json::from_str(&json)?, complete: complete != 0 });
        }
        Ok(tasks)
    }

    pub fn lists(&self) -> Result<Vec<Value>> {
        self.json_rows("SELECT json FROM lists ORDER BY rowid")
    }

    pub fn labels(&self) -> Result<Vec<Value>> {
        self.json_rows("SELECT json FROM labels ORDER BY rowid")
    }

    fn json_rows(&self, sql: &str) -> Result<Vec<Value>> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut values = Vec::new();
        for row in rows {
            values.push(serde_json::from_str(&row?)?);
        }
        Ok(values)
    }

    /// How many completed tasks the copy holds.
    pub fn complete_count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM tasks WHERE is_complete = 1", [], |row| row.get(0))?;
        Ok(n as usize)
    }

    /// When `refresh` last wrote the copy, as an RFC 3339 string. None if it never has.
    pub fn refreshed_at(&self) -> Result<Option<String>> {
        let found = self.conn.query_row("SELECT value FROM metadata WHERE key = 'refreshed_at'", [], |row| {
            row.get::<_, String>(0)
        });
        match found {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
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
    fn stored_data_reads_back_newest_first_with_completed_tasks_optional() {
        let mut store = memory();
        assert_eq!(store.refreshed_at().unwrap(), None);
        let at = |id: &str, updated: &str, complete| CachedTask {
            json: json!({"id": id, "updated_at": updated}),
            complete,
        };
        let snapshot = Snapshot {
            tasks: vec![
                at("old", "2026-01-01T00:00:00Z", false),
                at("new", "2026-03-01T00:00:00Z", false),
                at("done", "2026-02-01T00:00:00Z", true),
            ],
            lists: vec![json!({"id": "l1", "name": "Inbox"})],
            labels: vec![json!({"id": "x1", "name": "Low"})],
        };
        store.replace_all(&snapshot, "2026-10-15T00:00:00Z").unwrap();

        let open: Vec<String> =
            store.tasks(false).unwrap().iter().map(|t| t.json["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(open, vec!["new", "old"]);
        let all = store.tasks(true).unwrap();
        assert_eq!(all.iter().map(|t| t.json["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["new", "done", "old"]);
        assert_eq!(all.iter().filter(|t| t.complete).count(), 1);
        assert_eq!(store.complete_count().unwrap(), 1);
        assert_eq!(store.lists().unwrap(), vec![json!({"id": "l1", "name": "Inbox"})]);
        assert_eq!(store.labels().unwrap(), vec![json!({"id": "x1", "name": "Low"})]);
        assert_eq!(store.refreshed_at().unwrap().as_deref(), Some("2026-10-15T00:00:00Z"));
    }

    #[test]
    fn lists_keep_the_order_the_api_gave_them_even_when_later_pages_repeat_only_some() {
        let mut store = memory();
        let list = |id: &str| json!({"id": id, "name": id});
        let snapshot = Snapshot {
            tasks: vec![],
            // Page one: a, b, c. Page two repeats just a. Page three repeats b.
            lists: vec![list("a"), list("b"), list("c"), list("a"), list("b")],
            labels: vec![],
        };

        store.replace_all(&snapshot, "t").unwrap();

        let names: Vec<String> =
            store.lists().unwrap().iter().map(|l| l["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
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
