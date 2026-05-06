//! Durable command-history storage for term41.
//!
//! This crate owns the database schema and synchronous store/query operations.
//! Callers decide how to schedule those operations.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::params;
use tether_map::LinkedHashMap;

const SCHEMA_VERSION: u64 = 1;
const MILLIS_PER_SEC: u64 = 1_000;
const DEFAULT_MAX_ENTRIES_PER_CWD: usize = 200;
const DEFAULT_MAX_GLOBAL_ENTRIES: usize = 4_000;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

type HistoryEntryMap = LinkedHashMap<String, HistoryEntry>;

#[derive(Debug, Clone)]
pub struct HistoryStore {
    path: Arc<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCommandRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub submitted_at: SystemTime,
    pub retention: HistoryRetention,
    pub ignore_leading_space: bool,
}

impl StoreCommandRequest {
    pub fn new(
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            command: command.into(),
            cwd: cwd.into(),
            submitted_at: SystemTime::now(),
            retention: HistoryRetention::default(),
            ignore_leading_space: true,
        }
    }
}

impl Default for StoreCommandRequest {
    fn default() -> Self {
        Self {
            command: String::new(),
            cwd: PathBuf::new(),
            submitted_at: UNIX_EPOCH,
            retention: HistoryRetention::default(),
            ignore_leading_space: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRetention {
    pub max_entries_per_cwd: usize,
    pub max_global_entries: usize,
}

impl Default for HistoryRetention {
    fn default() -> Self {
        Self {
            max_entries_per_cwd: DEFAULT_MAX_ENTRIES_PER_CWD,
            max_global_entries: DEFAULT_MAX_GLOBAL_ENTRIES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryQuery {
    pub cwd: PathBuf,
    pub limit: usize,
    pub include_global_fallback: bool,
}

impl HistoryQuery {
    pub fn cwd(
        cwd: impl Into<PathBuf>,
        limit: usize,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            limit,
            include_global_fallback: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub command: String,
    pub cwd: PathBuf,
    pub submitted_at: SystemTime,
    pub source: HistoryEntrySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHistoryEntry {
    pub command: String,
    pub cwd: PathBuf,
    pub submitted_at: SystemTime,
    pub key: HistoryEntryKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HistoryEntryKey {
    cwd_key: String,
    command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryEntrySource {
    Cwd,
    GlobalFallback,
}

#[derive(Debug)]
pub enum HistoryError {
    Io(std::io::Error),
    Database(rusqlite::Error),
    TimestampOutOfRange(u64),
}

impl fmt::Display for HistoryError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "history I/O error: {error}"),
            Self::Database(error) => write!(f, "history database error: {error}"),
            Self::TimestampOutOfRange(millis) => {
                write!(f, "history timestamp out of SQLite INTEGER range: {millis}")
            }
        }
    }
}

impl Error for HistoryError {}

impl From<std::io::Error> for HistoryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for HistoryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

pub fn open(path: &Path) -> Result<HistoryStore, HistoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let store = HistoryStore {
        path: Arc::new(path.to_owned()),
    };
    let conn = open_connection(path)?;
    initialize_schema(&conn)?;
    Ok(store)
}

pub fn store_command(
    store: &HistoryStore,
    request: StoreCommandRequest,
) -> Result<(), HistoryError> {
    if should_skip_command(&request) {
        return Ok(());
    }

    let cwd = resolve_cwd(&request.cwd);
    let submitted_at_millis = system_time_to_millis(request.submitted_at);
    let submitted_at_sql = millis_to_sql(submitted_at_millis)?;
    let mut conn = open_store_connection(store)?;
    let tx = conn.transaction()?;
    if most_recent_cwd_command(&tx, &cwd.key)? == Some(request.command.as_str().to_owned()) {
        tx.commit()?;
        return Ok(());
    }

    insert_cwd_record(&tx, &cwd, &request.command, submitted_at_sql)?;
    insert_global_record(&tx, &cwd, &request.command, submitted_at_sql)?;
    trim_cwd_table(&tx, &cwd.key, request.retention.max_entries_per_cwd.max(1))?;
    trim_global_table(&tx, request.retention.max_global_entries.max(1))?;
    tx.commit()?;
    Ok(())
}

pub fn recent_commands(
    store: &HistoryStore,
    query: HistoryQuery,
) -> Result<Vec<HistoryEntry>, HistoryError> {
    if query.limit == 0 {
        return Ok(Vec::new());
    }

    let cwd = resolve_cwd(&query.cwd);
    let conn = open_store_connection(store)?;
    let mut entries = recent_cwd_entries(&conn, &cwd.key, query.limit)?;
    if query.include_global_fallback && entries.len() < query.limit {
        append_global_fallback(&conn, &cwd.key, query.limit, &mut entries)?;
    }
    Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}

pub fn all_commands(store: &HistoryStore) -> Result<Vec<StoredHistoryEntry>, HistoryError> {
    let conn = open_store_connection(store)?;
    let mut stmt = conn.prepare(
        "\
        SELECT cwd_key, display_cwd, command, submitted_millis
        FROM commands_global
        ORDER BY submitted_millis DESC, id DESC
        ",
    )?;
    let rows = stmt.query_map([], stored_history_entry_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn clear_all(store: &HistoryStore) -> Result<usize, HistoryError> {
    let mut conn = open_store_connection(store)?;
    let tx = conn.transaction()?;
    let deleted = tx.execute("DELETE FROM commands_by_cwd", [])?
        + tx.execute("DELETE FROM commands_global", [])?;
    tx.commit()?;
    Ok(deleted)
}

pub fn clear_cwd(
    store: &HistoryStore,
    cwd: &Path,
) -> Result<usize, HistoryError> {
    let cwd = resolve_cwd(cwd);
    clear_cwd_key(store, &cwd.key)
}

pub fn delete_entries(
    store: &HistoryStore,
    keys: &[HistoryEntryKey],
) -> Result<usize, HistoryError> {
    if keys.is_empty() {
        return Ok(0);
    }

    let mut keys = keys.to_vec();
    keys.sort();
    keys.dedup();

    let mut conn = open_store_connection(store)?;
    let tx = conn.transaction()?;
    let mut deleted = 0;
    for key in keys {
        deleted += delete_cwd_command_rows(&tx, &key.cwd_key, &key.command)?;
        deleted += delete_global_command_rows(&tx, &key.cwd_key, &key.command)?;
    }
    tx.commit()?;
    Ok(deleted)
}

fn open_store_connection(store: &HistoryStore) -> Result<Connection, HistoryError> {
    open_connection(&store.path)
}

fn open_connection(path: &Path) -> Result<Connection, HistoryError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    conn.execute_batch(
        "\
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        ",
    )?;
    Ok(conn)
}

fn initialize_schema(conn: &Connection) -> Result<(), HistoryError> {
    conn.execute_batch(
        "\
        PRAGMA journal_mode = WAL;

        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY NOT NULL,
            value INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS commands_by_cwd (
            id INTEGER PRIMARY KEY,
            cwd_key TEXT NOT NULL,
            display_cwd TEXT NOT NULL,
            command TEXT NOT NULL,
            submitted_millis INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS commands_by_cwd_recent
            ON commands_by_cwd (cwd_key, submitted_millis DESC, id DESC);

        CREATE TABLE IF NOT EXISTS commands_global (
            id INTEGER PRIMARY KEY,
            cwd_key TEXT NOT NULL,
            display_cwd TEXT NOT NULL,
            command TEXT NOT NULL,
            submitted_millis INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS commands_global_recent
            ON commands_global (submitted_millis DESC, id DESC);
        ",
    )?;
    conn.execute(
        "\
        INSERT INTO metadata (key, value)
        VALUES ('schema_version', ?1)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        ",
        params![millis_to_sql(SCHEMA_VERSION)?],
    )?;
    Ok(())
}

fn should_skip_command(request: &StoreCommandRequest) -> bool {
    request.command.trim().is_empty()
        || request.command.starts_with(char::is_whitespace) && request.ignore_leading_space
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedCwd {
    key: String,
    display: String,
}

fn resolve_cwd(path: &Path) -> ResolvedCwd {
    let display = path_to_string(path);
    let key = fs::canonicalize(path)
        .map(|path| path_to_string(&path))
        .unwrap_or_else(|_| display.clone());
    ResolvedCwd { key, display }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn most_recent_cwd_command(
    tx: &Transaction<'_>,
    cwd_key: &str,
) -> Result<Option<String>, HistoryError> {
    Ok(tx
        .query_row(
            "\
            SELECT command
            FROM commands_by_cwd
            WHERE cwd_key = ?1
            ORDER BY submitted_millis DESC, id DESC
            LIMIT 1
            ",
            params![cwd_key],
            |row| row.get(0),
        )
        .optional()?)
}

fn insert_cwd_record(
    tx: &Transaction<'_>,
    cwd: &ResolvedCwd,
    command: &str,
    submitted_millis: i64,
) -> Result<(), HistoryError> {
    tx.execute(
        "\
        INSERT INTO commands_by_cwd (cwd_key, display_cwd, command, submitted_millis)
        VALUES (?1, ?2, ?3, ?4)
        ",
        params![
            cwd.key.as_str(),
            cwd.display.as_str(),
            command,
            submitted_millis
        ],
    )?;
    Ok(())
}

fn insert_global_record(
    tx: &Transaction<'_>,
    cwd: &ResolvedCwd,
    command: &str,
    submitted_millis: i64,
) -> Result<(), HistoryError> {
    tx.execute(
        "\
        INSERT INTO commands_global (cwd_key, display_cwd, command, submitted_millis)
        VALUES (?1, ?2, ?3, ?4)
        ",
        params![
            cwd.key.as_str(),
            cwd.display.as_str(),
            command,
            submitted_millis
        ],
    )?;
    Ok(())
}

fn recent_cwd_entries(
    conn: &Connection,
    cwd_key: &str,
    limit: usize,
) -> Result<HistoryEntryMap, HistoryError> {
    let mut out = HistoryEntryMap::with_capacity(limit);
    let mut stmt = conn.prepare(
        "\
        SELECT display_cwd, command, submitted_millis
        FROM commands_by_cwd
        WHERE cwd_key = ?1
        ORDER BY submitted_millis DESC, id DESC
        ",
    )?;
    let rows = stmt.query_map(params![cwd_key], |row| {
        history_entry_from_row(row, HistoryEntrySource::Cwd)
    })?;
    for row in rows {
        push_unique_entry(&mut out, row?);
        if out.len() >= limit {
            return Ok(out);
        }
    }
    Ok(out)
}

fn append_global_fallback(
    conn: &Connection,
    cwd_key: &str,
    limit: usize,
    out: &mut HistoryEntryMap,
) -> Result<(), HistoryError> {
    let mut stmt = conn.prepare(
        "\
        SELECT cwd_key, display_cwd, command, submitted_millis
        FROM commands_global
        ORDER BY submitted_millis DESC, id DESC
        ",
    )?;
    let rows = stmt.query_map([], |row| {
        let entry_cwd_key: String = row.get(0)?;
        let display_cwd: String = row.get(1)?;
        let command: String = row.get(2)?;
        let submitted_millis: i64 = row.get(3)?;
        Ok((
            entry_cwd_key,
            HistoryEntry {
                command,
                cwd: PathBuf::from(display_cwd),
                submitted_at: sql_millis_to_system_time(submitted_millis),
                source: HistoryEntrySource::GlobalFallback,
            },
        ))
    })?;
    for row in rows {
        let (entry_cwd_key, entry) = row?;
        if entry_cwd_key == cwd_key {
            continue;
        }
        push_unique_entry(out, entry);
        if out.len() >= limit {
            return Ok(());
        }
    }
    Ok(())
}

fn history_entry_from_row(
    row: &rusqlite::Row<'_>,
    source: HistoryEntrySource,
) -> rusqlite::Result<HistoryEntry> {
    let display_cwd: String = row.get(0)?;
    let command: String = row.get(1)?;
    let submitted_millis: i64 = row.get(2)?;
    Ok(HistoryEntry {
        command,
        cwd: PathBuf::from(display_cwd),
        submitted_at: sql_millis_to_system_time(submitted_millis),
        source,
    })
}

fn stored_history_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredHistoryEntry> {
    let cwd_key: String = row.get(0)?;
    let display_cwd: String = row.get(1)?;
    let command: String = row.get(2)?;
    let submitted_millis: i64 = row.get(3)?;
    Ok(StoredHistoryEntry {
        command: command.clone(),
        cwd: PathBuf::from(display_cwd),
        submitted_at: sql_millis_to_system_time(submitted_millis),
        key: HistoryEntryKey { cwd_key, command },
    })
}

fn push_unique_entry(
    out: &mut HistoryEntryMap,
    entry: HistoryEntry,
) {
    if out.get(&entry.command).is_some() {
        return;
    }
    out.insert(entry.command.clone(), entry);
}

fn trim_cwd_table(
    tx: &Transaction<'_>,
    cwd_key: &str,
    limit: usize,
) -> Result<(), HistoryError> {
    tx.execute(
        "\
        DELETE FROM commands_by_cwd
        WHERE id IN (
            SELECT id
            FROM commands_by_cwd
            WHERE cwd_key = ?1
            ORDER BY submitted_millis DESC, id DESC
            LIMIT -1 OFFSET ?2
        )
        ",
        params![cwd_key, retention_offset(limit)],
    )?;
    Ok(())
}

fn clear_cwd_key(
    store: &HistoryStore,
    cwd_key: &str,
) -> Result<usize, HistoryError> {
    let mut conn = open_store_connection(store)?;
    let tx = conn.transaction()?;
    let deleted = tx.execute(
        "DELETE FROM commands_by_cwd WHERE cwd_key = ?1",
        params![cwd_key],
    )? + tx.execute(
        "DELETE FROM commands_global WHERE cwd_key = ?1",
        params![cwd_key],
    )?;
    tx.commit()?;
    Ok(deleted)
}

fn delete_cwd_command_rows(
    tx: &Transaction<'_>,
    cwd_key: &str,
    command: &str,
) -> Result<usize, HistoryError> {
    Ok(tx.execute(
        "DELETE FROM commands_by_cwd WHERE cwd_key = ?1 AND command = ?2",
        params![cwd_key, command],
    )?)
}

fn delete_global_command_rows(
    tx: &Transaction<'_>,
    cwd_key: &str,
    command: &str,
) -> Result<usize, HistoryError> {
    Ok(tx.execute(
        "DELETE FROM commands_global WHERE cwd_key = ?1 AND command = ?2",
        params![cwd_key, command],
    )?)
}

fn trim_global_table(
    tx: &Transaction<'_>,
    limit: usize,
) -> Result<(), HistoryError> {
    tx.execute(
        "\
        DELETE FROM commands_global
        WHERE id IN (
            SELECT id
            FROM commands_global
            ORDER BY submitted_millis DESC, id DESC
            LIMIT -1 OFFSET ?1
        )
        ",
        params![retention_offset(limit)],
    )?;
    Ok(())
}

fn retention_offset(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn system_time_to_millis(time: SystemTime) -> u64 {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    duration
        .as_secs()
        .saturating_mul(MILLIS_PER_SEC)
        .saturating_add(u64::from(duration.subsec_millis()))
}

fn millis_to_sql(millis: u64) -> Result<i64, HistoryError> {
    i64::try_from(millis).map_err(|_| HistoryError::TimestampOutOfRange(millis))
}

fn sql_millis_to_system_time(millis: i64) -> SystemTime {
    let millis = u64::try_from(millis).unwrap_or(0);
    let secs = millis / MILLIS_PER_SEC;
    let submillis = millis % MILLIS_PER_SEC;
    UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(submillis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_reads_cwd_history_before_global_fallback() {
        let root = temp_root("stores_and_reads_cwd_history_before_global_fallback");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        store_at(&store, "global only", root.join("b"), 1);
        store_at(&store, "cwd first", root.join("a"), 2);

        let entries = recent_commands(&store, HistoryQuery::cwd(root.join("a"), 4)).unwrap();

        assert_eq!(
            entries
                .iter()
                .map(|entry| (&entry.command, entry.source))
                .collect::<Vec<_>>(),
            vec![
                (&"cwd first".to_owned(), HistoryEntrySource::Cwd),
                (
                    &"global only".to_owned(),
                    HistoryEntrySource::GlobalFallback
                ),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn suppresses_adjacent_duplicate_for_same_cwd() {
        let root = temp_root("suppresses_adjacent_duplicate_for_same_cwd");
        fs::create_dir_all(root.join("a")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        store_at(&store, "cargo test", root.join("a"), 1);
        store_at(&store, "cargo test", root.join("a"), 2);

        let entries = recent_commands(&store, HistoryQuery::cwd(root.join("a"), 10)).unwrap();

        assert_eq!(commands(entries), ["cargo test"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn retention_is_local_to_each_table() {
        let root = temp_root("retention_is_local_to_each_table");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        let retention = HistoryRetention {
            max_entries_per_cwd: 2,
            max_global_entries: 3,
        };
        for i in 0..4 {
            store_with_retention(&store, format!("a{i}"), root.join("a"), i, retention);
        }
        store_with_retention(&store, "b4", root.join("b"), 4, retention);

        let cwd_entries = recent_commands(
            &store,
            HistoryQuery {
                cwd: root.join("a"),
                limit: 10,
                include_global_fallback: false,
            },
        )
        .unwrap();
        let global_entries =
            recent_commands(&store, HistoryQuery::cwd(root.join("b"), 10)).unwrap();

        assert_eq!(commands(cwd_entries), ["a3", "a2"]);
        assert_eq!(commands(global_entries), ["b4", "a3", "a2"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn filters_empty_and_leading_space_commands() {
        let root = temp_root("filters_empty_and_leading_space_commands");
        fs::create_dir_all(root.join("a")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        store_at(&store, "  ", root.join("a"), 1);
        store_at(&store, " secret", root.join("a"), 2);
        store_at(&store, "visible", root.join("a"), 3);

        let entries = recent_commands(&store, HistoryQuery::cwd(root.join("a"), 10)).unwrap();

        assert_eq!(commands(entries), ["visible"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn multiple_store_handles_can_use_the_same_database() {
        let root = temp_root("multiple_store_handles_can_use_the_same_database");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let db_path = root.join("history.sqlite3");
        let first = open(&db_path).unwrap();
        let second = open(&db_path).unwrap();

        store_at(&first, "from first", root.join("a"), 1);
        store_at(&second, "from second", root.join("b"), 2);

        let entries = recent_commands(&first, HistoryQuery::cwd(root.join("a"), 10)).unwrap();

        assert_eq!(commands(entries), ["from first", "from second"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn all_commands_returns_persistent_entries_in_recent_order() {
        let root = temp_root("all_commands_returns_persistent_entries_in_recent_order");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        store_at(&store, "first", root.join("a"), 1);
        store_at(&store, "second", root.join("b"), 2);

        let entries = all_commands(&store).unwrap();

        assert_eq!(stored_commands(&entries), ["second", "first"]);
        assert_eq!(entries[0].cwd, root.join("b"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clear_all_removes_cwd_and_global_history() {
        let root = temp_root("clear_all_removes_cwd_and_global_history");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        store_at(&store, "first", root.join("a"), 1);
        store_at(&store, "second", root.join("b"), 2);

        clear_all(&store).unwrap();

        assert!(all_commands(&store).unwrap().is_empty());
        assert!(
            recent_commands(&store, HistoryQuery::cwd(root.join("a"), 10))
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clear_cwd_removes_directory_entries_from_cwd_and_global_tables() {
        let root = temp_root("clear_cwd_removes_directory_entries_from_cwd_and_global_tables");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        store_at(&store, "a command", root.join("a"), 1);
        store_at(&store, "b command", root.join("b"), 2);

        clear_cwd(&store, &root.join("a")).unwrap();

        assert_eq!(
            stored_commands(&all_commands(&store).unwrap()),
            ["b command"]
        );
        assert_eq!(
            commands(recent_commands(&store, HistoryQuery::cwd(root.join("a"), 10)).unwrap()),
            ["b command"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_entries_removes_matching_commands_only() {
        let root = temp_root("delete_entries_removes_matching_commands_only");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let store = open(&root.join("history.sqlite3")).unwrap();

        store_at(&store, "remove me", root.join("a"), 1);
        store_at(&store, "keep me", root.join("b"), 2);
        let entries = all_commands(&store).unwrap();
        let remove_key = entries
            .iter()
            .find(|entry| entry.command == "remove me")
            .unwrap()
            .key
            .clone();

        delete_entries(&store, &[remove_key]).unwrap();

        assert_eq!(stored_commands(&all_commands(&store).unwrap()), ["keep me"]);
        assert_eq!(
            commands(recent_commands(&store, HistoryQuery::cwd(root.join("a"), 10)).unwrap()),
            ["keep me"]
        );
        let _ = fs::remove_dir_all(root);
    }

    fn store_at(
        store: &HistoryStore,
        command: impl Into<String>,
        cwd: PathBuf,
        seconds: u64,
    ) {
        store_with_retention(store, command, cwd, seconds, HistoryRetention::default());
    }

    fn store_with_retention(
        store: &HistoryStore,
        command: impl Into<String>,
        cwd: PathBuf,
        seconds: u64,
        retention: HistoryRetention,
    ) {
        store_command(
            store,
            StoreCommandRequest {
                command: command.into(),
                cwd,
                submitted_at: UNIX_EPOCH + Duration::from_secs(seconds),
                retention,
                ignore_leading_space: true,
            },
        )
        .unwrap();
    }

    fn commands(entries: Vec<HistoryEntry>) -> Vec<String> {
        entries.into_iter().map(|entry| entry.command).collect()
    }

    fn stored_commands(entries: &[StoredHistoryEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.command.clone()).collect()
    }

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("term41-history41-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }
}
