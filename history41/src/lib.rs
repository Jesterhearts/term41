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

use redb::Database;
use redb::MultimapTableDefinition;
use redb::ReadableDatabase;
use redb::ReadableMultimapTable;
use redb::TableDefinition;
use tether_map::LinkedHashMap;

const SCHEMA_VERSION: u64 = 1;
const MILLIS_PER_SEC: u64 = 1_000;
const DEFAULT_MAX_ENTRIES_PER_CWD: usize = 200;
const DEFAULT_MAX_GLOBAL_ENTRIES: usize = 4_000;

type CwdTimeKey<'a> = (&'a str, u64);
type CwdRecord<'a> = (&'a str, &'a str, u64);
type GlobalRecord<'a> = (&'a str, &'a str, &'a str, u64);
type OwnedCwdTimeKey = (String, u64);
type OwnedCwdRecord = (String, String, u64);
type OwnedGlobalRecord = (String, String, String, u64);
type CwdRemoval = (OwnedCwdTimeKey, OwnedCwdRecord);
type GlobalRemoval = (u64, OwnedGlobalRecord);
type HistoryEntryMap = LinkedHashMap<String, HistoryEntry>;

const METADATA: TableDefinition<&str, u64> = TableDefinition::new("metadata");
const COMMANDS_BY_CWD: MultimapTableDefinition<CwdTimeKey<'static>, CwdRecord<'static>> =
    MultimapTableDefinition::new("commands_by_cwd");
const COMMANDS_GLOBAL: MultimapTableDefinition<u64, GlobalRecord<'static>> =
    MultimapTableDefinition::new("commands_global");

#[derive(Debug, Clone)]
pub struct HistoryStore {
    db: Arc<Database>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryEntrySource {
    Cwd,
    GlobalFallback,
}

#[derive(Debug)]
pub enum HistoryError {
    Io(std::io::Error),
    Database(redb::Error),
    DatabaseOpen(redb::DatabaseError),
    Storage(redb::StorageError),
    Table(redb::TableError),
    Commit(redb::CommitError),
    Transaction(redb::TransactionError),
}

impl fmt::Display for HistoryError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "history I/O error: {error}"),
            Self::Database(error) => write!(f, "history database error: {error}"),
            Self::DatabaseOpen(error) => write!(f, "history database open error: {error}"),
            Self::Storage(error) => write!(f, "history storage error: {error}"),
            Self::Table(error) => write!(f, "history table error: {error}"),
            Self::Commit(error) => write!(f, "history commit error: {error}"),
            Self::Transaction(error) => write!(f, "history transaction error: {error}"),
        }
    }
}

impl Error for HistoryError {}

impl From<std::io::Error> for HistoryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<redb::Error> for HistoryError {
    fn from(error: redb::Error) -> Self {
        Self::Database(error)
    }
}

impl From<redb::DatabaseError> for HistoryError {
    fn from(error: redb::DatabaseError) -> Self {
        Self::DatabaseOpen(error)
    }
}

impl From<redb::StorageError> for HistoryError {
    fn from(error: redb::StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<redb::TableError> for HistoryError {
    fn from(error: redb::TableError) -> Self {
        Self::Table(error)
    }
}

impl From<redb::CommitError> for HistoryError {
    fn from(error: redb::CommitError) -> Self {
        Self::Commit(error)
    }
}

impl From<redb::TransactionError> for HistoryError {
    fn from(error: redb::TransactionError) -> Self {
        Self::Transaction(error)
    }
}

pub fn open(path: &Path) -> Result<HistoryStore, HistoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let db = Database::create(path)?;
    let store = HistoryStore { db: Arc::new(db) };
    initialize_schema(&store)?;
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
    let key_time = reverse_time(submitted_at_millis);
    if most_recent_cwd_command(store, &cwd.key)? == Some(request.command.as_str().to_owned()) {
        return Ok(());
    }

    let write = store.db.begin_write()?;
    {
        let mut by_cwd = write.open_multimap_table(COMMANDS_BY_CWD)?;
        by_cwd.insert(
            &(cwd.key.as_str(), key_time),
            &(
                cwd.display.as_str(),
                request.command.as_str(),
                submitted_at_millis,
            ),
        )?;
    }
    {
        let mut global = write.open_multimap_table(COMMANDS_GLOBAL)?;
        global.insert(
            &key_time,
            &(
                cwd.key.as_str(),
                cwd.display.as_str(),
                request.command.as_str(),
                submitted_at_millis,
            ),
        )?;
    }
    trim_cwd_table(
        &write,
        &cwd.key,
        request.retention.max_entries_per_cwd.max(1),
    )?;
    trim_global_table(&write, request.retention.max_global_entries.max(1))?;
    write.commit()?;
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
    let read = store.db.begin_read()?;
    let by_cwd = read.open_multimap_table(COMMANDS_BY_CWD)?;
    let mut entries = recent_cwd_entries(&by_cwd, &cwd.key, query.limit)?;
    if query.include_global_fallback && entries.len() < query.limit {
        let global = read.open_multimap_table(COMMANDS_GLOBAL)?;
        append_global_fallback(&global, &cwd.key, query.limit, &mut entries)?;
    }
    Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}

fn initialize_schema(store: &HistoryStore) -> Result<(), HistoryError> {
    let write = store.db.begin_write()?;
    {
        let mut metadata = write.open_table(METADATA)?;
        metadata.insert("schema_version", SCHEMA_VERSION)?;
    }
    {
        write.open_multimap_table(COMMANDS_BY_CWD)?;
    }
    {
        write.open_multimap_table(COMMANDS_GLOBAL)?;
    }
    write.commit()?;
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
    store: &HistoryStore,
    cwd_key: &str,
) -> Result<Option<String>, HistoryError> {
    let read = store.db.begin_read()?;
    let table = read.open_multimap_table(COMMANDS_BY_CWD)?;
    let mut range = table.range::<CwdTimeKey<'_>>((cwd_key, 0)..=(cwd_key, u64::MAX))?;
    let Some(item) = range.next() else {
        return Ok(None);
    };
    let (_, values) = item?;
    let Some(value) = values.into_iter().next() else {
        return Ok(None);
    };
    let value = value?;
    let (_, command, _) = value.value();
    Ok(Some(command.to_owned()))
}

fn recent_cwd_entries(
    table: &impl ReadableMultimapTable<CwdTimeKey<'static>, CwdRecord<'static>>,
    cwd_key: &str,
    limit: usize,
) -> Result<HistoryEntryMap, HistoryError> {
    let mut out = HistoryEntryMap::with_capacity(limit);
    let range = table.range::<CwdTimeKey<'_>>((cwd_key, 0)..=(cwd_key, u64::MAX))?;
    for item in range {
        let (_, values) = item?;
        for value in values {
            let value = value?;
            let (display_cwd, command, submitted_at) = value.value();
            push_unique_entry(
                &mut out,
                HistoryEntry {
                    command: command.to_owned(),
                    cwd: PathBuf::from(display_cwd),
                    submitted_at: millis_to_system_time(submitted_at),
                    source: HistoryEntrySource::Cwd,
                },
            );
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

fn append_global_fallback(
    table: &impl ReadableMultimapTable<u64, GlobalRecord<'static>>,
    cwd_key: &str,
    limit: usize,
    out: &mut HistoryEntryMap,
) -> Result<(), HistoryError> {
    let range = table.range::<u64>(..)?;
    for item in range {
        let (_, values) = item?;
        for value in values {
            let value = value?;
            let (entry_cwd_key, display_cwd, command, submitted_at) = value.value();
            if entry_cwd_key == cwd_key {
                continue;
            }
            push_unique_entry(
                out,
                HistoryEntry {
                    command: command.to_owned(),
                    cwd: PathBuf::from(display_cwd),
                    submitted_at: millis_to_system_time(submitted_at),
                    source: HistoryEntrySource::GlobalFallback,
                },
            );
            if out.len() >= limit {
                return Ok(());
            }
        }
    }
    Ok(())
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
    write: &redb::WriteTransaction,
    cwd_key: &str,
    limit: usize,
) -> Result<(), HistoryError> {
    let mut table = write.open_multimap_table(COMMANDS_BY_CWD)?;
    let removals = cwd_retention_removals(&table, cwd_key, limit)?;
    for (key, value) in removals {
        let key = (key.0.as_str(), key.1);
        let value = (value.0.as_str(), value.1.as_str(), value.2);
        table.remove(&key, &value)?;
    }
    Ok(())
}

fn trim_global_table(
    write: &redb::WriteTransaction,
    limit: usize,
) -> Result<(), HistoryError> {
    let mut table = write.open_multimap_table(COMMANDS_GLOBAL)?;
    let removals = global_retention_removals(&table, limit)?;
    for (key, value) in removals {
        let value = (
            value.0.as_str(),
            value.1.as_str(),
            value.2.as_str(),
            value.3,
        );
        table.remove(&key, &value)?;
    }
    Ok(())
}

fn cwd_retention_removals(
    table: &impl ReadableMultimapTable<CwdTimeKey<'static>, CwdRecord<'static>>,
    cwd_key: &str,
    limit: usize,
) -> Result<Vec<CwdRemoval>, HistoryError> {
    let mut seen = 0;
    let mut removals = Vec::new();
    let range = table.range::<CwdTimeKey<'_>>((cwd_key, 0)..=(cwd_key, u64::MAX))?;
    for item in range {
        let (key, values) = item?;
        let (stored_cwd, reverse_time) = key.value();
        for value in values {
            seen += 1;
            let value = value?;
            let (display_cwd, command, submitted_at) = value.value();
            if seen > limit {
                removals.push((
                    (stored_cwd.to_owned(), reverse_time),
                    (display_cwd.to_owned(), command.to_owned(), submitted_at),
                ));
            }
        }
    }
    Ok(removals)
}

fn global_retention_removals(
    table: &impl ReadableMultimapTable<u64, GlobalRecord<'static>>,
    limit: usize,
) -> Result<Vec<GlobalRemoval>, HistoryError> {
    let mut seen = 0;
    let mut removals = Vec::new();
    let range = table.range::<u64>(..)?;
    for item in range {
        let (reverse_time, values) = item?;
        let reverse_time = reverse_time.value();
        for value in values {
            seen += 1;
            let value = value?;
            let (cwd_key, display_cwd, command, submitted_at) = value.value();
            if seen > limit {
                removals.push((
                    reverse_time,
                    (
                        cwd_key.to_owned(),
                        display_cwd.to_owned(),
                        command.to_owned(),
                        submitted_at,
                    ),
                ));
            }
        }
    }
    Ok(removals)
}

fn reverse_time(millis: u64) -> u64 {
    u64::MAX - millis
}

fn system_time_to_millis(time: SystemTime) -> u64 {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    duration
        .as_secs()
        .saturating_mul(MILLIS_PER_SEC)
        .saturating_add(u64::from(duration.subsec_millis()))
}

fn millis_to_system_time(millis: u64) -> SystemTime {
    let secs = millis / MILLIS_PER_SEC;
    let submillis = (millis % MILLIS_PER_SEC) as u32;
    UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(u64::from(submillis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_reads_cwd_history_before_global_fallback() {
        let root = temp_root("stores_and_reads_cwd_history_before_global_fallback");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        let store = open(&root.join("history.redb")).unwrap();

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
        let store = open(&root.join("history.redb")).unwrap();

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
        let store = open(&root.join("history.redb")).unwrap();

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
        let store = open(&root.join("history.redb")).unwrap();

        store_at(&store, "  ", root.join("a"), 1);
        store_at(&store, " secret", root.join("a"), 2);
        store_at(&store, "visible", root.join("a"), 3);

        let entries = recent_commands(&store, HistoryQuery::cwd(root.join("a"), 10)).unwrap();

        assert_eq!(commands(entries), ["visible"]);
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

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("term41-history41-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }
}
