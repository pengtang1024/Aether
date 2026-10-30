//! SQLite persistence for tasks, files, plugin settings and task logs.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::{Checksum, ChecksumAlgorithm, FileInfo};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Canceled,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(value: &str) -> Result<Self, StoreError> {
        Ok(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            other => return Err(StoreError::Invalid(format!("unknown task state {other}"))),
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

#[derive(Debug, Clone)]
pub struct NewTask {
    pub id: String,
    pub url: String,
    pub plugin_id: String,
    pub filename: Option<String>,
    pub settings: Value,
    pub state: TaskState,
    /// Queue priority: higher runs first, equal values stay FIFO.
    pub priority: i32,
    /// Where finalized files land. `None` means the daemon's download root;
    /// absolute paths let the UI implement "save as".
    pub target_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTask {
    pub id: String,
    pub url: String,
    pub plugin_id: String,
    pub filename: Option<String>,
    pub state: TaskState,
    /// Queue priority: higher runs first, equal values stay FIFO.
    pub priority: i32,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed_bps: u64,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub error_retryable: bool,
    pub settings: Value,
    pub plugin_state: Option<Value>,
    pub target_dir: Option<String>,
    /// Failed attempts so far; drives the exponential backoff and its cap.
    pub retry_attempts: u32,
    /// Wall-clock deadline of a pending automatic retry (epoch ms).
    pub retry_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
    pub files: Vec<StoredFile>,
}

/// One `task_logs` row, oldest first when returned from [`TaskStore::logs`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLog {
    pub level: String,
    pub message: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFile {
    pub relative_path: String,
    pub size: u64,
    pub checksum: Option<String>,
}

impl StoredFile {
    pub fn from_sdk(file: &FileInfo) -> Result<Self, StoreError> {
        let checksum = match &file.checksum {
            Some(checksum) if checksum.algorithm == ChecksumAlgorithm::Sha256 => {
                Some(checksum.value.clone())
            }
            Some(_) => {
                return Err(StoreError::Invalid(
                    "only SHA-256 checksums can be stored".into(),
                ))
            }
            None => None,
        };
        Ok(Self {
            relative_path: file.relative_path.clone(),
            size: file.size,
            checksum,
        })
    }

    pub fn to_sdk(&self) -> FileInfo {
        FileInfo {
            relative_path: self.relative_path.clone(),
            size: self.size,
            checksum: self.checksum.as_ref().map(|value| Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: value.clone(),
            }),
        }
    }
}

/// Log lines kept per task; older lines are pruned on append.
pub const TASK_LOG_LIMIT: i64 = 500;

pub struct TaskStore {
    connection: Connection,
}

impl TaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        Self::configure(&connection)?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        Self::configure(&connection)?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    fn configure(connection: &Connection) -> Result<(), StoreError> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(())
    }

    fn migrate(connection: &Connection) -> Result<(), StoreError> {
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 1 {
            connection.execute_batch(
                r#"
                BEGIN;
                CREATE TABLE IF NOT EXISTS tasks (
                    id TEXT PRIMARY KEY,
                    url TEXT NOT NULL,
                    plugin_id TEXT NOT NULL,
                    filename TEXT,
                    state TEXT NOT NULL,
                    downloaded INTEGER NOT NULL DEFAULT 0,
                    total INTEGER,
                    speed_bps INTEGER NOT NULL DEFAULT 0,
                    error_kind TEXT,
                    error_message TEXT,
                    error_retryable INTEGER NOT NULL DEFAULT 0,
                    settings TEXT NOT NULL DEFAULT '{}',
                    plugin_state TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    completed_at INTEGER
                );
                CREATE INDEX IF NOT EXISTS tasks_state_idx ON tasks(state, created_at);
                CREATE TABLE IF NOT EXISTS task_files (
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    relative_path TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    checksum TEXT,
                    PRIMARY KEY (task_id, relative_path)
                );
                CREATE TABLE IF NOT EXISTS plugin_settings (
                    plugin_id TEXT PRIMARY KEY,
                    settings TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS task_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    level TEXT NOT NULL,
                    message TEXT NOT NULL,
                    fields TEXT,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS task_logs_task_idx ON task_logs(task_id, id);
                PRAGMA user_version = 1;
                COMMIT;
                "#,
            )?;
        }
        if version < 2 {
            // v2: per-task destination directory ("save as").
            connection.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE tasks ADD COLUMN target_dir TEXT;
                PRAGMA user_version = 2;
                COMMIT;
                "#,
            )?;
        }
        if version < 3 {
            // v3: durable automatic-retry bookkeeping across restarts.
            connection.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE tasks ADD COLUMN retry_attempts INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE tasks ADD COLUMN retry_at INTEGER;
                PRAGMA user_version = 3;
                COMMIT;
                "#,
            )?;
        }
        if version < 4 {
            // v4: queue priority (higher runs first).
            connection.execute_batch(
                r#"
                BEGIN;
                ALTER TABLE tasks ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
                PRAGMA user_version = 4;
                COMMIT;
                "#,
            )?;
        }
        if version < 5 {
            // v5: plugins the user switched off (survives restarts).
            connection.execute_batch(
                r#"
                BEGIN;
                CREATE TABLE IF NOT EXISTS disabled_plugins (
                    plugin_id TEXT PRIMARY KEY,
                    created_at INTEGER NOT NULL
                );
                PRAGMA user_version = 5;
                COMMIT;
                "#,
            )?;
        }
        Ok(())
    }

    pub fn create_task(&mut self, task: &NewTask) -> Result<(), StoreError> {
        let now = now_ms();
        self.connection.execute(
            "INSERT INTO tasks (id, url, plugin_id, filename, state, settings, target_dir,
                                priority, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                task.id,
                task.url,
                task.plugin_id,
                task.filename,
                task.state.as_str(),
                serde_json::to_string(&task.settings)?,
                task.target_dir,
                task.priority,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn task(&self, id: &str) -> Result<Option<StoredTask>, StoreError> {
        let mut task = self
            .connection
            .query_row(
                "SELECT id, url, plugin_id, filename, state, downloaded, total, speed_bps,
                        error_kind, error_message, error_retryable, settings, plugin_state,
                        created_at, updated_at, completed_at, target_dir,
                        retry_attempts, retry_at, priority
                 FROM tasks WHERE id = ?1",
                [id],
                row_to_task,
            )
            .optional()?;
        if let Some(task) = &mut task {
            task.files = self.files(id)?;
        }
        Ok(task)
    }

    pub fn list_tasks(&self, limit: usize) -> Result<Vec<StoredTask>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, url, plugin_id, filename, state, downloaded, total, speed_bps,
                    error_kind, error_message, error_retryable, settings, plugin_state,
                    created_at, updated_at, completed_at, target_dir,
                    retry_attempts, retry_at, priority
             FROM tasks ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], row_to_task)?;
        let mut tasks = Vec::new();
        for row in rows {
            let mut task = row?;
            task.files = self.files(&task.id)?;
            tasks.push(task);
        }
        Ok(tasks)
    }

    pub fn recoverable_tasks(&self) -> Result<Vec<StoredTask>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, url, plugin_id, filename, state, downloaded, total, speed_bps,
                    error_kind, error_message, error_retryable, settings, plugin_state,
                    created_at, updated_at, completed_at, target_dir,
                    retry_attempts, retry_at, priority
             FROM tasks WHERE state IN ('queued', 'running', 'paused') ORDER BY created_at",
        )?;
        let rows = statement.query_map([], row_to_task)?;
        let mut tasks = Vec::new();
        for row in rows {
            let mut task = row?;
            task.files = self.files(&task.id)?;
            tasks.push(task);
        }
        Ok(tasks)
    }

    fn files(&self, task_id: &str) -> Result<Vec<StoredFile>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT relative_path, size, checksum FROM task_files WHERE task_id = ?1 ORDER BY relative_path",
        )?;
        let rows = statement.query_map([task_id], |row| {
            Ok(StoredFile {
                relative_path: row.get(0)?,
                size: row.get::<_, i64>(1)? as u64,
                checksum: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_progress(
        &mut self,
        id: &str,
        downloaded: u64,
        total: Option<u64>,
        speed_bps: u64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE tasks SET downloaded = ?2, total = ?3, speed_bps = ?4,
                    state = 'running', error_kind = NULL, error_message = NULL,
                    updated_at = ?5
             WHERE id = ?1",
            params![
                id,
                downloaded as i64,
                total.map(|value| value as i64),
                speed_bps as i64,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn set_state(&mut self, id: &str, state: TaskState) -> Result<(), StoreError> {
        let completed_at = state.is_terminal().then(now_ms);
        self.connection.execute(
            "UPDATE tasks SET state = ?2, updated_at = ?3, completed_at = COALESCE(?4, completed_at)
             WHERE id = ?1",
            params![id, state.as_str(), now_ms(), completed_at],
        )?;
        Ok(())
    }

    pub fn fail_task(
        &mut self,
        id: &str,
        kind: &str,
        message: &str,
        retryable: bool,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE tasks SET state = 'failed', error_kind = ?2, error_message = ?3,
                    error_retryable = ?4, updated_at = ?5, completed_at = ?5
             WHERE id = ?1",
            params![id, kind, message, retryable as i64, now_ms()],
        )?;
        Ok(())
    }

    pub fn complete_task(&mut self, id: &str, files: &[FileInfo]) -> Result<(), StoreError> {
        let stored = files
            .iter()
            .map(StoredFile::from_sdk)
            .collect::<Result<Vec<_>, _>>()?;
        let total: u64 = stored.iter().map(|file| file.size).sum();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE tasks SET state = 'completed', downloaded = ?2, total = ?2,
                    updated_at = ?3, completed_at = ?3,
                    speed_bps = 0, error_kind = NULL, error_message = NULL,
                    retry_attempts = 0, retry_at = NULL
             WHERE id = ?1",
            params![id, total as i64, now_ms()],
        )?;
        for file in &stored {
            transaction.execute(
                "INSERT OR REPLACE INTO task_files (task_id, relative_path, size, checksum)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, file.relative_path, file.size as i64, file.checksum],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_task(&mut self, id: &str) -> Result<(), StoreError> {
        self.connection
            .execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn save_plugin_settings(
        &mut self,
        plugin_id: &str,
        settings: &Value,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO plugin_settings (plugin_id, settings) VALUES (?1, ?2)
             ON CONFLICT(plugin_id) DO UPDATE SET settings = excluded.settings",
            params![plugin_id, serde_json::to_string(settings)?],
        )?;
        Ok(())
    }

    pub fn plugin_settings(&self, plugin_id: &str) -> Result<Option<Value>, StoreError> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT settings FROM plugin_settings WHERE plugin_id = ?1",
                [plugin_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value
            .map(|value| serde_json::from_str(&value))
            .transpose()?)
    }

    pub fn append_log(
        &mut self,
        task_id: &str,
        level: &str,
        message: &str,
        fields: Option<&Value>,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO task_logs (task_id, level, message, fields, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                task_id,
                level,
                message,
                fields.map(serde_json::to_string).transpose()?,
                now_ms()
            ],
        )?;
        // Bound the log table: a chatty plugin must not grow the database
        // forever. Keep the newest lines per task; the UI reads the same tail.
        self.connection.execute(
            "DELETE FROM task_logs WHERE task_id = ?1 AND id NOT IN (
                 SELECT id FROM task_logs WHERE task_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![task_id, TASK_LOG_LIMIT],
        )?;
        Ok(())
    }

    /// Copy the database to `path` with SQLite's online backup API — safe
    /// while the daemon keeps writing (readers never block it for long).
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut destination = Connection::open(path)?;
        let backup = rusqlite::backup::Backup::new(&self.connection, &mut destination)?;
        backup.run_to_completion(256, std::time::Duration::from_millis(5), None)?;
        Ok(())
    }

    /// SQLite's own consistency check: `"ok"` or the first reported problem.
    pub fn integrity_check(&self) -> Result<String, StoreError> {
        Ok(self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
    }

    /// The queued task that should run next. Priority is aged: every five
    /// minutes in the queue earns one point, capped at 100, so a starved
    /// low-priority task eventually overtakes a freshly queued high-priority
    /// one (a +100 head start lasts ~8.3 hours) while ties fall to the
    /// oldest task.
    pub fn next_queued(&self) -> Result<Option<StoredTask>, StoreError> {
        let mut task = self
            .connection
            .query_row(
                "SELECT id, url, plugin_id, filename, state, downloaded, total, speed_bps,
                        error_kind, error_message, error_retryable, settings, plugin_state,
                        created_at, updated_at, completed_at, target_dir,
                        retry_attempts, retry_at, priority
                 FROM tasks WHERE state = 'queued'
                 ORDER BY (priority + MIN(100, (strftime('%s','now') - created_at / 1000) / 300)) DESC,
                          created_at ASC, rowid ASC LIMIT 1",
                [],
                row_to_task,
            )
            .optional()?;
        if let Some(task) = &mut task {
            task.files = self.files(&task.id)?;
        }
        Ok(task)
    }

    /// Record a pending automatic retry: `attempts` failures so far and the
    /// wall-clock deadline. Survives a daemon restart.
    pub fn set_retry(
        &mut self,
        task_id: &str,
        attempts: u32,
        retry_at: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE tasks SET retry_attempts = ?1, retry_at = ?2 WHERE id = ?3",
            params![attempts as i64, retry_at, task_id],
        )?;
        Ok(())
    }

    /// The retry is about to run: keep the attempt counter, drop the deadline.
    pub fn clear_pending_retry(&mut self, task_id: &str) -> Result<(), StoreError> {
        self.connection
            .execute("UPDATE tasks SET retry_at = NULL WHERE id = ?1", [task_id])?;
        Ok(())
    }

    /// Forget retry bookkeeping entirely (manual resume, cancel, completion).
    pub fn reset_retry(&mut self, task_id: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE tasks SET retry_attempts = 0, retry_at = NULL WHERE id = ?1",
            [task_id],
        )?;
        Ok(())
    }

    /// Failed tasks with a pending automatic retry: `(task_id, attempts, at)`.
    pub fn pending_retries(&self) -> Result<Vec<(String, u32, i64)>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, retry_attempts, retry_at FROM tasks
             WHERE state = 'failed' AND retry_at IS NOT NULL ORDER BY retry_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as u32,
                row.get::<_, i64>(2)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sum and count of finalized files for tasks completed since `since_ms`.
    pub fn completed_since(&self, since_ms: i64) -> Result<(u64, u64), StoreError> {
        let row = self.connection.query_row(
            "SELECT COALESCE(SUM(f.size), 0), COUNT(f.task_id)
             FROM task_files f JOIN tasks t ON t.id = f.task_id
             WHERE t.state = 'completed' AND t.completed_at >= ?1",
            [since_ms],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )?;
        Ok(row)
    }

    /// `(completed_at, size, relative_path)` for every finalized file whose
    /// task completed since `since_ms` — the per-day and per-type breakdowns.
    pub fn completed_files_since(
        &self,
        since_ms: i64,
    ) -> Result<Vec<(i64, u64, String)>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT t.completed_at, f.size, f.relative_path
             FROM task_files f JOIN tasks t ON t.id = f.task_id
             WHERE t.state = 'completed' AND t.completed_at >= ?1",
        )?;
        let rows = statement.query_map([since_ms], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, String>(2)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// `(completed, failed)` task counts for the dashboard.
    pub fn outcome_counts(&self) -> Result<(u64, u64), StoreError> {
        let row = self.connection.query_row(
            "SELECT
                 SUM(CASE WHEN state = 'completed' THEN 1 ELSE 0 END),
                 SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END)
             FROM tasks",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                ))
            },
        )?;
        Ok(row)
    }

    /// Plugin ids the user disabled; applied to the registry at startup.
    pub fn disabled_plugins(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT plugin_id FROM disabled_plugins ORDER BY plugin_id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn set_plugin_disabled(
        &mut self,
        plugin_id: &str,
        disabled: bool,
    ) -> Result<(), StoreError> {
        if disabled {
            self.connection.execute(
                "INSERT OR IGNORE INTO disabled_plugins (plugin_id, created_at) VALUES (?1, ?2)",
                params![plugin_id, now_ms()],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM disabled_plugins WHERE plugin_id = ?1",
                [plugin_id],
            )?;
        }
        Ok(())
    }

    /// The last `limit` log lines for a task, oldest first so callers can print
    /// them in reading order without reversing.
    pub fn logs(&self, task_id: &str, limit: usize) -> Result<Vec<TaskLog>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT level, message, fields, created_at FROM task_logs
             WHERE task_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![task_id, limit as i64], |row| {
            let fields: Option<String> = row.get(2)?;
            Ok(TaskLog {
                level: row.get(0)?,
                message: row.get(1)?,
                fields: fields.and_then(|raw| serde_json::from_str(&raw).ok()),
                created_at: row.get(3)?,
            })
        })?;
        let mut logs = rows.collect::<Result<Vec<_>, _>>()?;
        logs.reverse();
        Ok(logs)
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTask> {
    let settings: String = row.get(11)?;
    let plugin_state: Option<String> = row.get(12)?;
    let state: String = row.get(4)?;
    Ok(StoredTask {
        id: row.get(0)?,
        url: row.get(1)?,
        plugin_id: row.get(2)?,
        filename: row.get(3)?,
        state: TaskState::parse(&state).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error.to_string(),
                )),
            )
        })?,
        downloaded: row.get::<_, i64>(5)? as u64,
        total: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
        speed_bps: row.get::<_, i64>(7)? as u64,
        error_kind: row.get(8)?,
        error_message: row.get(9)?,
        error_retryable: row.get::<_, i64>(10)? != 0,
        settings: serde_json::from_str(&settings).unwrap_or(Value::Null),
        plugin_state: plugin_state.and_then(|value| serde_json::from_str(&value).ok()),
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        completed_at: row.get(15)?,
        target_dir: row.get(16)?,
        retry_attempts: row.get::<_, i64>(17)? as u32,
        retry_at: row.get(18)?,
        priority: row.get::<_, i64>(19)? as i32,
        files: Vec::new(),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid store data: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_round_trips_a_task() {
        let mut store = TaskStore::open_in_memory().expect("memory store");
        store.create_task(&new_task("t-backup")).expect("create");
        let path = std::env::temp_dir().join(format!(
            "aether-backup-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        store.backup_to(&path).expect("backup");
        let restored = TaskStore::open(&path).expect("backup opens");
        assert_eq!(restored.list_tasks(10).expect("list").len(), 1);
        assert_eq!(restored.integrity_check().expect("check"), "ok");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn logs_are_capped_per_task() {
        let mut store = TaskStore::open_in_memory().expect("memory store");
        store.create_task(&new_task("t-logs")).expect("create");
        for index in 0..(TASK_LOG_LIMIT + 20) {
            store
                .append_log("t-logs", "info", &format!("line {index}"), None)
                .expect("append");
        }
        let logs = store.logs("t-logs", 10_000).expect("logs");
        assert_eq!(logs.len(), TASK_LOG_LIMIT as usize, "old lines are pruned");
        // logs() is oldest-first: the first kept line is 20 of 0..=519.
        assert!(logs.first().unwrap().message.contains("line 20"));
        assert!(logs
            .last()
            .unwrap()
            .message
            .contains(&format!("line {}", TASK_LOG_LIMIT + 19)));
    }

    fn new_task(id: &str) -> NewTask {
        NewTask {
            id: id.into(),
            url: "https://example.com/file.bin".into(),
            plugin_id: "http".into(),
            filename: Some("file.bin".into()),
            settings: serde_json::json!({"connections": 4}),
            state: TaskState::Queued,
            priority: 0,
            target_dir: None,
        }
    }

    #[test]
    fn queue_priority_ages_so_low_priority_tasks_are_not_starved() {
        let mut store = TaskStore::open_in_memory().unwrap();
        let mut high = new_task("high");
        high.priority = 100;
        store.create_task(&high).unwrap();
        let mut low = new_task("low");
        low.priority = 0;
        store.create_task(&low).unwrap();

        // Fresh: the high-priority task runs first.
        let next = store.next_queued().unwrap().unwrap();
        assert_eq!(next.id, "high");

        // Backdate low to ~10h of waiting: its boost is capped at 100, so
        // the effective priorities tie at 100 and the oldest task wins.
        store
            .connection
            .execute(
                "UPDATE tasks SET created_at = ?1 WHERE id = 'low'",
                params![now_ms() - 10 * 3600 * 1000],
            )
            .unwrap();
        let next = store.next_queued().unwrap().unwrap();
        assert_eq!(
            next.id, "low",
            "aged low-priority task must overtake the fresh high one"
        );

        // A brand-new high-priority task cannot cut ahead of the aged one.
        let mut high2 = new_task("high2");
        high2.priority = 100;
        store.create_task(&high2).unwrap();
        let next = store.next_queued().unwrap().unwrap();
        assert_eq!(next.id, "low");
    }

    #[test]
    fn creates_updates_and_completes_a_task() {
        let mut store = TaskStore::open_in_memory().unwrap();
        store.create_task(&new_task("t1")).unwrap();
        store.update_progress("t1", 512, Some(1024), 100).unwrap();
        let task = store.task("t1").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Running);
        assert_eq!(task.downloaded, 512);
        assert_eq!(task.total, Some(1024));

        let files = vec![FileInfo {
            relative_path: "file.bin".into(),
            size: 1024,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: "abc".into(),
            }),
        }];
        store.complete_task("t1", &files).unwrap();
        let task = store.task("t1").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.files.len(), 1);
        assert_eq!(task.files[0].checksum.as_deref(), Some("abc"));
    }

    #[test]
    fn persists_to_a_file_and_lists_recoverable_tasks() {
        let path = std::env::temp_dir().join(format!(
            "aether-store-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        {
            let mut store = TaskStore::open(&path).unwrap();
            store.create_task(&new_task("t1")).unwrap();
            store.set_state("t1", TaskState::Running).unwrap();
        }
        let store = TaskStore::open(&path).unwrap();
        let tasks = store.recoverable_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Running);
        drop(store);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn stores_plugin_settings() {
        let mut store = TaskStore::open_in_memory().unwrap();
        let settings = serde_json::json!({"connections": 8});
        store.save_plugin_settings("http", &settings).unwrap();
        assert_eq!(store.plugin_settings("http").unwrap(), Some(settings));
    }

    #[test]
    fn fails_and_logs() {
        let mut store = TaskStore::open_in_memory().unwrap();
        store.create_task(&new_task("t1")).unwrap();
        store.fail_task("t1", "timeout", "timed out", true).unwrap();
        store.append_log("t1", "error", "timed out", None).unwrap();
        let task = store.task("t1").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Failed);
        assert!(task.error_retryable);
        assert_eq!(store.logs("t1", 10).unwrap().len(), 1);
    }

    #[test]
    fn migrates_a_v1_database_and_keeps_tasks() {
        let path = std::env::temp_dir().join(format!(
            "aether-store-v1-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        fs::remove_file(&path).ok();
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    r#"
                    CREATE TABLE tasks (
                        id TEXT PRIMARY KEY,
                        url TEXT NOT NULL,
                        plugin_id TEXT NOT NULL,
                        filename TEXT,
                        state TEXT NOT NULL,
                        downloaded INTEGER NOT NULL DEFAULT 0,
                        total INTEGER,
                        speed_bps INTEGER NOT NULL DEFAULT 0,
                        error_kind TEXT,
                        error_message TEXT,
                        error_retryable INTEGER NOT NULL DEFAULT 0,
                        settings TEXT NOT NULL DEFAULT '{}',
                        plugin_state TEXT,
                        created_at INTEGER NOT NULL,
                        updated_at INTEGER NOT NULL,
                        completed_at INTEGER
                    );
                    CREATE TABLE task_files (
                        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                        relative_path TEXT NOT NULL,
                        size INTEGER NOT NULL,
                        checksum TEXT,
                        PRIMARY KEY (task_id, relative_path)
                    );
                    CREATE TABLE plugin_settings (
                        plugin_id TEXT PRIMARY KEY,
                        settings TEXT NOT NULL
                    );
                    CREATE TABLE task_logs (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                        level TEXT NOT NULL,
                        message TEXT NOT NULL,
                        fields TEXT,
                        created_at INTEGER NOT NULL
                    );
                    PRAGMA user_version = 1;
                    INSERT INTO tasks (id, url, plugin_id, state, created_at, updated_at)
                    VALUES ('old', 'https://example.com/old.bin', 'http', 'completed', 1, 1);
                    "#,
                )
                .unwrap();
        }

        let mut store = TaskStore::open(&path).unwrap();
        let old = store.task("old").unwrap().unwrap();
        assert_eq!(old.state, TaskState::Completed);
        assert_eq!(old.target_dir, None);
        assert_eq!(old.retry_attempts, 0);
        assert_eq!(old.retry_at, None);

        // The new column is usable after the migration.
        let mut task = new_task("new");
        task.target_dir = Some("/tmp/elsewhere".into());
        store.create_task(&task).unwrap();
        assert_eq!(
            store.task("new").unwrap().unwrap().target_dir.as_deref(),
            Some("/tmp/elsewhere")
        );

        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        drop(store);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn promotes_by_priority_then_fifo() {
        let mut store = TaskStore::open_in_memory().unwrap();
        for (id, priority) in [("low", -5), ("normal", 0), ("high", 10), ("high2", 10)] {
            let mut task = new_task(id);
            task.priority = priority;
            store.create_task(&task).unwrap();
        }
        // Same-millisecond inserts must still be FIFO among equal priorities.
        assert_eq!(store.next_queued().unwrap().unwrap().id, "high");
        store.set_state("high", TaskState::Running).unwrap();
        assert_eq!(store.next_queued().unwrap().unwrap().id, "high2");
        store.set_state("high2", TaskState::Completed).unwrap();
        assert_eq!(store.next_queued().unwrap().unwrap().id, "normal");
        store.set_state("normal", TaskState::Queued).unwrap();
        store.set_state("normal", TaskState::Canceled).unwrap();
        assert_eq!(store.next_queued().unwrap().unwrap().id, "low");
        store.set_state("low", TaskState::Canceled).unwrap();
        assert!(store.next_queued().unwrap().is_none());
    }

    #[test]
    fn tracks_disabled_plugins_and_stats() {
        let mut store = TaskStore::open_in_memory().unwrap();
        store.set_plugin_disabled("bt", true).unwrap();
        store.set_plugin_disabled("ftp", true).unwrap();
        store.set_plugin_disabled("ftp", false).unwrap();
        assert_eq!(store.disabled_plugins().unwrap(), vec!["bt".to_string()]);

        store.create_task(&new_task("t1")).unwrap();
        store
            .complete_task(
                "t1",
                &[aether_plugin_sdk::FileInfo {
                    relative_path: "a.bin".into(),
                    size: 2048,
                    checksum: None,
                }],
            )
            .unwrap();
        let task = store.task("t1").unwrap().unwrap();
        let completed_at = task.completed_at.unwrap();
        assert_eq!(
            store.completed_since(completed_at - 1000).unwrap(),
            (2048, 1)
        );
        assert_eq!(store.completed_since(completed_at + 1000).unwrap(), (0, 0));
        assert_eq!(store.outcome_counts().unwrap(), (1, 0));
        assert_eq!(
            store.completed_files_since(completed_at - 1000).unwrap(),
            vec![(completed_at, 2048, "a.bin".to_string())]
        );
        assert!(store
            .completed_files_since(completed_at + 1000)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn tracks_pending_retries() {
        let mut store = TaskStore::open_in_memory().unwrap();
        store.create_task(&new_task("t1")).unwrap();
        store.fail_task("t1", "timeout", "timed out", true).unwrap();
        store.set_retry("t1", 2, 1_700_000_000_000).unwrap();
        assert_eq!(
            store.pending_retries().unwrap(),
            vec![("t1".to_string(), 2, 1_700_000_000_000)]
        );
        let task = store.task("t1").unwrap().unwrap();
        assert_eq!(task.retry_attempts, 2);
        assert_eq!(task.retry_at, Some(1_700_000_000_000));

        // The retry is running: the deadline goes, the counter stays.
        store.clear_pending_retry("t1").unwrap();
        assert!(store.pending_retries().unwrap().is_empty());
        let task = store.task("t1").unwrap().unwrap();
        assert_eq!(task.retry_attempts, 2);
        assert_eq!(task.retry_at, None);

        // Manual action forgets everything.
        store.set_retry("t1", 3, 42).unwrap();
        store.reset_retry("t1").unwrap();
        let task = store.task("t1").unwrap().unwrap();
        assert_eq!(task.retry_attempts, 0);
        assert_eq!(task.retry_at, None);

        // Completion clears any leftover schedule.
        store.set_retry("t1", 4, 43).unwrap();
        store
            .complete_task(
                "t1",
                &[aether_plugin_sdk::FileInfo {
                    relative_path: "file.bin".into(),
                    size: 1,
                    checksum: None,
                }],
            )
            .unwrap();
        let task = store.task("t1").unwrap().unwrap();
        assert_eq!(task.retry_attempts, 0);
        assert_eq!(task.retry_at, None);
        assert!(store.pending_retries().unwrap().is_empty());
    }

    #[test]
    fn returns_logs_oldest_first_with_fields() {
        let mut store = TaskStore::open_in_memory().unwrap();
        store.create_task(&new_task("t1")).unwrap();
        store.append_log("t1", "info", "first", None).unwrap();
        store
            .append_log(
                "t1",
                "warn",
                "second",
                Some(&serde_json::json!({"attempt": 2})),
            )
            .unwrap();
        store.append_log("t1", "error", "third", None).unwrap();

        let logs = store.logs("t1", 10).unwrap();
        let messages: Vec<_> = logs.iter().map(|log| log.message.as_str()).collect();
        assert_eq!(messages, ["first", "second", "third"]);
        assert_eq!(logs[1].level, "warn");
        assert_eq!(logs[1].fields, Some(serde_json::json!({"attempt": 2})));
        assert_eq!(logs[0].fields, None);

        // The limit keeps the newest lines, still in reading order.
        let recent = store.logs("t1", 2).unwrap();
        let messages: Vec<_> = recent.iter().map(|log| log.message.as_str()).collect();
        assert_eq!(messages, ["second", "third"]);
    }
}
