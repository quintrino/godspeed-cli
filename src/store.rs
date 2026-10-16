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

/// What is known about the copy, kept in the store next to the data so it can be trusted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Facts {
    /// When the copy was last refreshed, fully or incrementally (RFC 3339).
    pub refreshed_at: Option<String>,
    /// When it was last rebuilt from scratch.
    pub last_full_refresh_at: Option<String>,
    /// Whether the copy holds completed tasks.
    pub include_completed: Option<bool>,
    /// The newest `updated_at` among the tasks, as the API wrote it: an incremental refresh
    /// asks for what changed after this.
    pub high_water: Option<String>,
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

    #[cfg(test)]
    pub fn in_memory() -> Store {
        let store = Store { conn: Connection::open_in_memory().unwrap() };
        store.migrate().unwrap();
        store
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

    /// Replaces everything in the store, facts included, with `snapshot`, all or nothing.
    pub fn replace_all(&mut self, snapshot: &Snapshot, facts: &Facts) -> Result<Counts> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM tasks", [])?;
        tx.execute("DELETE FROM lists", [])?;
        tx.execute("DELETE FROM labels", [])?;
        tx.execute("DELETE FROM metadata", [])?;

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

        write_facts(&tx, facts)?;
        tx.commit()?;
        self.counts()
    }

    /// Applies what changed since the last refresh: adds or updates the tasks in `snapshot`,
    /// and the lists and labels, leaving everything else alone. Tasks that are completed are
    /// removed instead unless `include_completed`. Returns the counts afterwards and how many
    /// tasks were in `snapshot`. The facts given are written; the others are kept.
    pub fn apply_changes(
        &mut self,
        snapshot: &Snapshot,
        include_completed: bool,
        facts: &Facts,
    ) -> Result<(Counts, usize)> {
        let tx = self.conn.transaction()?;
        let mut changed = std::collections::HashSet::new();
        for task in &snapshot.tasks {
            let Some(id) = string_field(&task.json, "id") else { continue };
            if task.complete && !include_completed {
                tx.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
            } else {
                tx.execute(
                    "INSERT INTO tasks (id, json, is_complete, updated_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(id) DO UPDATE SET json = excluded.json, is_complete = excluded.is_complete,
                                                   updated_at = excluded.updated_at",
                    params![
                        id,
                        serde_json::to_string(&task.json)?,
                        i64::from(task.complete),
                        string_field(&task.json, "updated_at"),
                    ],
                )?;
            }
            changed.insert(id);
        }
        for (table, values) in [("lists", &snapshot.lists), ("labels", &snapshot.labels)] {
            for value in values {
                if let Some(id) = string_field(value, "id") {
                    // Update in place, so a list keeps its position in the sidebar order.
                    tx.execute(
                        &format!(
                            "INSERT INTO {table} (id, json) VALUES (?1, ?2)
                             ON CONFLICT(id) DO UPDATE SET json = excluded.json"
                        ),
                        params![id, serde_json::to_string(value)?],
                    )?;
                }
            }
        }
        write_facts(&tx, facts)?;
        tx.commit()?;
        Ok((self.counts()?, changed.len()))
    }

    pub fn facts(&self) -> Result<Facts> {
        let get = |key: &str| -> Result<Option<String>> {
            match self.conn.query_row("SELECT value FROM metadata WHERE key = ?1", params![key], |row| row.get::<_, String>(0)) {
                Ok(value) => Ok(Some(value)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        Ok(Facts {
            refreshed_at: get("refreshed_at")?,
            last_full_refresh_at: get("last_full_refresh_at")?,
            include_completed: get("include_completed")?.map(|value| value == "true"),
            high_water: get("high_water")?,
        })
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


    /// Looks a task up by its id.
    pub fn task(&self, id: &str) -> Result<Option<StoredTask>> {
        match self.conn.query_row("SELECT json, is_complete FROM tasks WHERE id = ?1", params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) {
            Ok((json, complete)) => Ok(Some(StoredTask { json: serde_json::from_str(&json)?, complete: complete != 0 })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Forgets a task, after it was deleted in Godspeed. True if the copy had it.
    pub fn remove_task(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM tasks WHERE id = ?1", params![id])? > 0)
    }

    /// Records in the copy a change that was just made in Godspeed, so a search sees it before
    /// the next `refresh`. `fields` are the ones sent to the API; the ones the copy keeps are
    /// written into the task, and setting one snooze field clears the other. The task's
    /// `updated_at` is left as it was, so the next incremental refresh still brings the API's own
    /// version. A task that is completed is removed unless the copy keeps completed tasks.
    pub fn apply_edit(&self, id: &str, fields: &Value, complete: Option<bool>) -> Result<()> {
        let Some(mut task) = self.task(id)? else { return Ok(()) };
        let keeps_completed = self.facts()?.include_completed == Some(true);
        let complete = complete.unwrap_or(task.complete);
        if complete && !keeps_completed {
            self.remove_task(id)?;
            return Ok(());
        }
        if let (Some(object), Some(changes)) = (task.json.as_object_mut(), fields.as_object()) {
            for key in ["title", "notes", "list_id", "snoozed_until", "timeless_snoozed_until"] {
                if let Some(value) = changes.get(key) {
                    object.insert(key.to_string(), value.clone());
                }
            }
            if changes.contains_key("snoozed_until") {
                object.remove("timeless_snoozed_until");
            }
            if changes.contains_key("timeless_snoozed_until") {
                object.remove("snoozed_until");
            }
        }
        self.conn.execute(
            "UPDATE tasks SET json = ?2, is_complete = ?3 WHERE id = ?1",
            params![id, serde_json::to_string(&task.json)?, i64::from(complete)],
        )?;
        Ok(())
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
        Ok(self.facts()?.refreshed_at)
    }

    pub fn counts(&self) -> Result<Counts> {
        let count = |table: &str| -> Result<usize> {
            let n: i64 = self.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?;
            Ok(n as usize)
        };
        Ok(Counts { tasks: count("tasks")?, lists: count("lists")?, labels: count("labels")? })
    }
}

fn write_facts(tx: &rusqlite::Transaction<'_>, facts: &Facts) -> Result<()> {
    let entries = [
        ("refreshed_at", facts.refreshed_at.clone()),
        ("last_full_refresh_at", facts.last_full_refresh_at.clone()),
        ("include_completed", facts.include_completed.map(|b| b.to_string())),
        ("high_water", facts.high_water.clone()),
    ];
    for (key, value) in entries {
        if let Some(value) = value {
            tx.execute("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)", params![key, value])?;
        }
    }
    Ok(())
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    impl Facts {
        fn refreshed(at: &str) -> Facts {
            Facts { refreshed_at: Some(at.to_string()), ..Default::default() }
        }
    }

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

        let counts = store.replace_all(&snapshot, &Facts::refreshed("2026-10-15T00:00:00Z")).unwrap();

        assert_eq!(counts, Counts { tasks: 2, lists: 1, labels: 2 });
        assert_eq!(refreshed_at(&store).as_deref(), Some("2026-10-15T00:00:00Z"));
    }

    #[test]
    fn the_completion_flag_and_json_are_kept() {
        let mut store = memory();
        store
            .replace_all(&Snapshot { tasks: vec![task("a", false), task("b", true)], ..Default::default() }, &Facts::refreshed("t"))
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
        store.replace_all(&Snapshot { tasks: vec![task("a", false), task("b", false)], ..Default::default() }, &Facts::refreshed("1")).unwrap();

        let counts = store.replace_all(&Snapshot { tasks: vec![task("c", false)], ..Default::default() }, &Facts::refreshed("2")).unwrap();

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

        let counts = store.replace_all(&snapshot, &Facts::refreshed("t")).unwrap();

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
        store.replace_all(&snapshot, &Facts::refreshed("2026-10-15T00:00:00Z")).unwrap();

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

        store.replace_all(&snapshot, &Facts::refreshed("t")).unwrap();

        let names: Vec<String> =
            store.lists().unwrap().iter().map(|l| l["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    fn task_at(id: &str, title: &str, updated: &str, complete: bool) -> CachedTask {
        CachedTask { json: json!({"id": id, "title": title, "updated_at": updated}), complete }
    }

    fn title_of(store: &Store, id: &str) -> Option<String> {
        store
            .tasks(true)
            .unwrap()
            .iter()
            .find(|t| t.json["id"] == id)
            .map(|t| t.json["title"].as_str().unwrap().to_string())
    }

    #[test]
    fn facts_are_stored_and_read_back() {
        let mut store = memory();
        assert_eq!(store.facts().unwrap(), Facts::default());
        let facts = Facts {
            refreshed_at: Some("2026-10-15T00:00:00Z".to_string()),
            last_full_refresh_at: Some("2026-10-14T00:00:00Z".to_string()),
            include_completed: Some(true),
            high_water: Some("2026-10-13T12:00:00.123456Z".to_string()),
        };

        store.replace_all(&Snapshot::default(), &facts).unwrap();

        assert_eq!(store.facts().unwrap(), facts);
        let none = Facts { include_completed: Some(false), ..Default::default() };
        store.replace_all(&Snapshot::default(), &none).unwrap();
        assert_eq!(store.facts().unwrap(), none, "a full replace forgets facts it isn't given");
    }

    #[test]
    fn apply_changes_updates_and_adds_tasks_and_leaves_the_rest() {
        let mut store = memory();
        let first = Snapshot {
            tasks: vec![task_at("a", "Old A", "2026-01-01T00:00:00Z", false), task_at("b", "B", "2026-01-02T00:00:00Z", false)],
            ..Default::default()
        };
        store.replace_all(&first, &Facts { last_full_refresh_at: Some("full".into()), ..Facts::refreshed("1") }).unwrap();

        let changes = Snapshot {
            tasks: vec![task_at("a", "New A", "2026-02-01T00:00:00Z", false), task_at("c", "C", "2026-02-02T00:00:00Z", false)],
            ..Default::default()
        };
        let (counts, changed) = store.apply_changes(&changes, false, &Facts::refreshed("2")).unwrap();

        assert_eq!((counts.tasks, changed), (3, 2));
        assert_eq!(title_of(&store, "a").as_deref(), Some("New A"));
        assert_eq!(title_of(&store, "b").as_deref(), Some("B"));
        assert_eq!(title_of(&store, "c").as_deref(), Some("C"));
        let facts = store.facts().unwrap();
        assert_eq!(facts.refreshed_at.as_deref(), Some("2"));
        assert_eq!(facts.last_full_refresh_at.as_deref(), Some("full"), "an incremental refresh keeps the last full time");
    }

    #[test]
    fn a_task_completed_since_is_removed_unless_completed_tasks_are_kept() {
        let open = || Snapshot { tasks: vec![task_at("a", "A", "2026-01-01T00:00:00Z", false)], ..Default::default() };
        let done = || Snapshot { tasks: vec![task_at("a", "A", "2026-02-01T00:00:00Z", true)], ..Default::default() };

        let mut without = memory();
        without.replace_all(&open(), &Facts::refreshed("1")).unwrap();
        let (counts, changed) = without.apply_changes(&done(), false, &Facts::refreshed("2")).unwrap();
        assert_eq!((counts.tasks, changed), (0, 1));

        let mut with = memory();
        with.replace_all(&open(), &Facts::refreshed("1")).unwrap();
        let (counts, _) = with.apply_changes(&done(), true, &Facts::refreshed("2")).unwrap();
        assert_eq!(counts.tasks, 1);
        assert_eq!(with.complete_count().unwrap(), 1);
    }

    #[test]
    fn a_reopened_task_becomes_incomplete_again() {
        let mut store = memory();
        let done = Snapshot { tasks: vec![task_at("a", "A", "2026-01-01T00:00:00Z", true)], ..Default::default() };
        store.replace_all(&done, &Facts::refreshed("1")).unwrap();
        let reopened = Snapshot { tasks: vec![task_at("a", "A", "2026-03-01T00:00:00Z", false)], ..Default::default() };

        store.apply_changes(&reopened, true, &Facts::refreshed("2")).unwrap();

        assert_eq!(store.complete_count().unwrap(), 0);
        assert_eq!(store.tasks(false).unwrap().len(), 1);
    }

    #[test]
    fn apply_changes_keeps_lists_in_their_order_updates_them_and_appends_new_ones() {
        let mut store = memory();
        let list = |id: &str, name: &str| json!({"id": id, "name": name});
        let first = Snapshot { lists: vec![list("a", "A"), list("b", "B"), list("c", "C")], ..Default::default() };
        store.replace_all(&first, &Facts::refreshed("1")).unwrap();

        let changes = Snapshot { lists: vec![list("b", "B renamed"), list("d", "D")], labels: vec![json!({"id": "x", "name": "X"})], ..Default::default() };
        let (counts, _) = store.apply_changes(&changes, false, &Facts::refreshed("2")).unwrap();

        let names: Vec<String> = store.lists().unwrap().iter().map(|l| l["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["A", "B renamed", "C", "D"]);
        assert_eq!((counts.lists, counts.labels), (4, 1));
    }

    #[test]
    fn an_empty_change_set_only_updates_the_facts() {
        let mut store = memory();
        store.replace_all(&Snapshot { tasks: vec![task_at("a", "A", "2026-01-01T00:00:00Z", false)], ..Default::default() }, &Facts::refreshed("1")).unwrap();

        let (counts, changed) = store.apply_changes(&Snapshot::default(), false, &Facts::refreshed("2")).unwrap();

        assert_eq!((counts.tasks, changed), (1, 0));
        assert_eq!(store.facts().unwrap().refreshed_at.as_deref(), Some("2"));
    }

    #[test]
    fn rows_without_an_id_are_skipped() {
        let mut store = memory();
        let snapshot = Snapshot {
            tasks: vec![CachedTask { json: json!({"title": "no id"}), complete: false }],
            lists: vec![json!({"name": "no id"})],
            labels: vec![],
        };

        assert_eq!(store.replace_all(&snapshot, &Facts::refreshed("t")).unwrap(), Counts { tasks: 0, lists: 0, labels: 0 });
    }

    #[test]
    fn opening_twice_keeps_the_data_and_a_newer_schema_is_refused() {
        let dir = std::env::temp_dir().join(format!("godspeed-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.sqlite3");
        let _ = std::fs::remove_file(&path);

        Store::open(&path).unwrap().replace_all(&Snapshot { tasks: vec![task("a", false)], ..Default::default() }, &Facts::refreshed("t")).unwrap();
        assert_eq!(Store::open(&path).unwrap().counts().unwrap().tasks, 1);

        Connection::open(&path).unwrap().pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        let error = Store::open(&path).err().expect("a newer schema is refused").to_string();
        assert!(error.contains("newer godspeed-cli"), "{error}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
