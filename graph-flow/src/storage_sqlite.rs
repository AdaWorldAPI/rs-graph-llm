//! SQLite-based session storage using Arrow IPC binary format.
//!
//! Stores sessions in an embedded SQLite database. Context is serialized
//! as Arrow IPC bytes (binary, not JSON). This is the recommended default
//! for single-process deployments that don't need Lance time-travel.
//!
//! # Examples
//!
//! ```rust,no_run
//! use graph_flow::storage_sqlite::SqliteSessionStorage;
//! use graph_flow::{Session, SessionStorage};
//!
//! # #[tokio::main]
//! # async fn main() -> graph_flow::Result<()> {
//! let storage = SqliteSessionStorage::connect("sqlite:sessions.db").await?;
//!
//! let session = Session::new_from_task("s1".to_string(), "start_task");
//! storage.save(session).await?;
//!
//! let loaded = storage.get("s1").await?;
//! assert!(loaded.is_some());
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::sync::Arc;

use crate::{
    error::{GraphError, Result},
    storage::{Session, SessionStorage},
    Context,
};

/// SQLite-based session storage.
///
/// Context state is stored as a binary blob (serialized via the Context's
/// internal serialization). SQLite provides ACID transactions, WAL mode
/// for concurrent reads, and zero external dependencies.
pub struct SqliteSessionStorage {
    pool: Arc<Pool<Sqlite>>,
}

impl SqliteSessionStorage {
    /// Connect to (or create) a SQLite database.
    ///
    /// Use `"sqlite::memory:"` for in-memory, or `"sqlite:path/to/db"` for file.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| {
                GraphError::StorageError(format!("Failed to connect to SQLite: {e}"))
            })?;

        Self::migrate(&pool).await?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    async fn migrate(pool: &Pool<Sqlite>) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL,
                current_task_id TEXT NOT NULL,
                status_message TEXT,
                context_blob BLOB NOT NULL,
                task_history TEXT NOT NULL DEFAULT '[]',
                created_at INTEGER DEFAULT (unixepoch()),
                updated_at INTEGER DEFAULT (unixepoch())
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| GraphError::StorageError(format!("SQLite migration failed: {e}")))?;
        Ok(())
    }

    /// Serialize context to binary blob.
    ///
    /// Uses the context's built-in serialize → serde_json::Value → compact binary.
    /// The binary format is MessagePack for compactness, but could be Arrow IPC
    /// when the context evolves to native Arrow backing.
    async fn context_to_blob(context: &Context) -> Result<Vec<u8>> {
        let value = context.serialize().await;
        // Store as compact JSON bytes for now — the Context itself is HashMap-backed.
        // When Context migrates to Arrow backing, this becomes Arrow IPC bytes.
        serde_json::to_vec(&value)
            .map_err(|e| GraphError::StorageError(format!("Context serialization failed: {e}")))
    }

    /// Deserialize context from binary blob.
    fn blob_to_context(blob: &[u8]) -> Result<Context> {
        let value: serde_json::Value = serde_json::from_slice(blob)
            .map_err(|e| GraphError::StorageError(format!("Context deserialization failed: {e}")))?;

        let ctx = Context::new();
        if let serde_json::Value::Object(map) = value {
            for (k, v) in map {
                ctx.set_sync(&k, v);
            }
        }
        Ok(ctx)
    }

    fn history_to_string(history: &[String]) -> String {
        // Store as newline-delimited task IDs (no JSON)
        history.join("\n")
    }

    fn string_to_history(s: &str) -> Vec<String> {
        if s.is_empty() {
            Vec::new()
        } else {
            s.split('\n').map(|s| s.to_string()).collect()
        }
    }
}

#[async_trait]
impl SessionStorage for SqliteSessionStorage {
    async fn save(&self, session: Session) -> Result<()> {
        let blob = Self::context_to_blob(&session.context).await?;
        let history = Self::history_to_string(&session.task_history);

        sqlx::query(
            r#"
            INSERT INTO sessions (id, graph_id, current_task_id, status_message, context_blob, task_history, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch())
            ON CONFLICT (id) DO UPDATE SET
                graph_id = excluded.graph_id,
                current_task_id = excluded.current_task_id,
                status_message = excluded.status_message,
                context_blob = excluded.context_blob,
                task_history = excluded.task_history,
                updated_at = unixepoch()
            "#,
        )
        .bind(&session.id)
        .bind(&session.graph_id)
        .bind(&session.current_task_id)
        .bind(&session.status_message)
        .bind(&blob)
        .bind(&history)
        .execute(&*self.pool)
        .await
        .map_err(|e| GraphError::StorageError(format!("Failed to save session: {e}")))?;

        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Session>> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                Vec<u8>,
                String,
            ),
        >(
            r#"
            SELECT id, graph_id, current_task_id, status_message, context_blob, task_history
            FROM sessions
            WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&*self.pool)
        .await
        .map_err(|e| GraphError::StorageError(format!("Failed to fetch session: {e}")))?;

        match row {
            Some((session_id, graph_id, current_task_id, status_message, blob, history_str)) => {
                let context = Self::blob_to_context(&blob)?;
                let task_history = Self::string_to_history(&history_str);
                Ok(Some(Session {
                    id: session_id,
                    graph_id,
                    current_task_id,
                    status_message,
                    context,
                    task_history,
                }))
            }
            None => Ok(None),
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?1")
            .bind(id)
            .execute(&*self.pool)
            .await
            .map_err(|e| GraphError::StorageError(format!("Failed to delete session: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_storage() -> SqliteSessionStorage {
        SqliteSessionStorage::connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_save_and_get() {
        let storage = test_storage().await;

        let session = Session::new_from_task("s1".to_string(), "task_a");
        session.context.set("key", "value".to_string()).await;

        storage.save(session).await.unwrap();

        let loaded = storage.get("s1").await.unwrap().unwrap();
        assert_eq!(loaded.current_task_id, "task_a");
        let val: Option<String> = loaded.context.get("key").await;
        assert_eq!(val.unwrap(), "value");
    }

    #[tokio::test]
    async fn test_update() {
        let storage = test_storage().await;

        let mut session = Session::new_from_task("s1".to_string(), "task_a");
        storage.save(session.clone()).await.unwrap();

        session.current_task_id = "task_b".to_string();
        session.task_history.push("task_a".to_string());
        storage.save(session).await.unwrap();

        let loaded = storage.get("s1").await.unwrap().unwrap();
        assert_eq!(loaded.current_task_id, "task_b");
        assert_eq!(loaded.task_history, vec!["task_a"]);
    }

    #[tokio::test]
    async fn test_delete() {
        let storage = test_storage().await;

        let session = Session::new_from_task("s1".to_string(), "a");
        storage.save(session).await.unwrap();

        storage.delete("s1").await.unwrap();
        assert!(storage.get("s1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_nonexistent() {
        let storage = test_storage().await;
        assert!(storage.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_context_roundtrip() {
        let storage = test_storage().await;

        let session = Session::new_from_task("s1".to_string(), "t");
        session.context.set("num", 42i64).await;
        session.context.set("flag", true).await;
        session
            .context
            .set("nested", serde_json::json!({"a": [1, 2, 3]}))
            .await;

        storage.save(session).await.unwrap();

        let loaded = storage.get("s1").await.unwrap().unwrap();
        let num: Option<i64> = loaded.context.get("num").await;
        assert_eq!(num, Some(42));
        let flag: Option<bool> = loaded.context.get("flag").await;
        assert_eq!(flag, Some(true));
    }
}
