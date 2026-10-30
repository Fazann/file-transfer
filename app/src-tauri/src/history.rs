//! Finished transfers, kept in SQLite at `paths::data_dir()/history.db`.
//!
//! Every function blocks on SQLite; call them inside `tokio::task::spawn_blocking`.

use std::{
    path::Path,
    sync::{Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection};
use serde::Serialize;

/// File name inside `paths::data_dir()`.
pub const HISTORY_FILE_NAME: &str = "history.db";

/// Most entries returned by [`History::list`].
const LIST_LIMIT: u32 = 500;

/// How a transfer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// It finished.
    Completed,
    /// It failed.
    Failed,
    /// Someone cancelled it.
    Cancelled,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

/// One finished transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// Row id.
    pub id: i64,
    /// `send` or `receive`.
    pub direction: String,
    /// The other device's name.
    pub peer_name: String,
    /// The file's name.
    pub file_name: String,
    /// The file's size in bytes.
    pub size: u64,
    /// How it ended.
    pub status: Status,
    /// The `ErrorCode` name for failures.
    pub error_code: Option<String>,
    /// Where a received file was saved.
    pub path: Option<String>,
    /// When it ended, in milliseconds since 1970-01-01 UTC (what JavaScript's `Date` uses).
    pub finished_at: u64,
}

/// The history database.
pub struct History(Mutex<Connection>);

impl History {
    /// Opens (or creates) the database at `path`.
    ///
    /// # Errors
    ///
    /// When the file cannot be opened or the table cannot be created.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS transfers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                direction TEXT NOT NULL,
                peer_name TEXT NOT NULL,
                file_name TEXT NOT NULL,
                size INTEGER NOT NULL,
                status TEXT NOT NULL,
                error_code TEXT,
                path TEXT,
                finished_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self(Mutex::new(connection)))
    }

    /// Adds a finished transfer; `entry.id` and `entry.finished_at` are filled in here.
    ///
    /// # Errors
    ///
    /// When the row cannot be written.
    pub fn record(&self, entry: &HistoryEntry) -> rusqlite::Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        self.lock().execute(
            "INSERT INTO transfers (direction, peer_name, file_name, size, status, error_code, path, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.direction,
                entry.peer_name,
                entry.file_name,
                i64::try_from(entry.size).unwrap_or(i64::MAX),
                entry.status.as_str(),
                entry.error_code,
                entry.path,
                i64::try_from(now).unwrap_or(i64::MAX),
            ],
        )?;
        Ok(())
    }

    /// The most recent transfers, newest first.
    ///
    /// # Errors
    ///
    /// When the table cannot be read.
    pub fn list(&self) -> rusqlite::Result<Vec<HistoryEntry>> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT id, direction, peer_name, file_name, size, status, error_code, path, finished_at
             FROM transfers ORDER BY finished_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([LIST_LIMIT], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                direction: row.get(1)?,
                peer_name: row.get(2)?,
                file_name: row.get(3)?,
                size: u64::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                status: Status::parse(&row.get::<_, String>(5)?),
                error_code: row.get(6)?,
                path: row.get(7)?,
                finished_at: u64::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
            })
        })?;
        rows.collect()
    }

    /// Deletes every entry.
    ///
    /// # Errors
    ///
    /// When the table cannot be written.
    pub fn clear(&self) -> rusqlite::Result<()> {
        self.lock().execute("DELETE FROM transfers", []).map(|_deleted| ())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves SQLite itself consistent, so keep going.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file_name: &str, status: Status) -> HistoryEntry {
        HistoryEntry {
            id: 0,
            direction: "send".into(),
            peer_name: "Desk".into(),
            file_name: file_name.into(),
            size: 42,
            status,
            error_code: (status == Status::Failed).then(|| "CONNECTION_LOST".into()),
            path: None,
            finished_at: 0,
        }
    }

    #[test]
    fn entries_are_listed_newest_first_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(&dir.path().join(HISTORY_FILE_NAME)).unwrap();
        history.record(&entry("a.txt", Status::Completed)).unwrap();
        history.record(&entry("b.txt", Status::Failed)).unwrap();
        let listed = history.list().unwrap();
        assert_eq!(listed.iter().map(|entry| entry.file_name.as_str()).collect::<Vec<_>>(), ["b.txt", "a.txt"]);
        assert_eq!(listed[0].error_code.as_deref(), Some("CONNECTION_LOST"));
        assert_eq!(listed[1].status, Status::Completed);
        assert!(listed[0].finished_at > 0);
        history.clear().unwrap();
        assert_eq!(history.list().unwrap(), Vec::new());
    }
}
