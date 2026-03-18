use async_trait::async_trait;
use sqlx::{postgres::PgPoolOptions, Pool, Postgres};
use std::sync::Arc;

use crate::{Session, Context, error::{Result, GraphError}, storage::SessionStorage};

pub struct PostgresSessionStorage {
    pool: Arc<Pool<Postgres>>,
}

impl PostgresSessionStorage {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| GraphError::StorageError(format!("Failed to connect to Postgres: {e}")))?;

        Self::migrate(&pool).await?;
        Ok(Self { pool: Arc::new(pool) })
    }

    async fn migrate(pool: &Pool<Postgres>) -> Result<()> {
        // Create table with BYTEA for context and TEXT for task_history
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL,
                current_task_id TEXT NOT NULL,
                status_message TEXT,
                context_bytes BYTEA NOT NULL,
                task_history TEXT NOT NULL DEFAULT '',
                created_at TIMESTAMPTZ DEFAULT NOW(),
                updated_at TIMESTAMPTZ DEFAULT NOW()
            );
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| GraphError::StorageError(format!("Migration failed: {e}")))?;

        // Migration: if old JSONB `context` column exists, add new columns
        // and backfill. This is idempotent — safe to run on both old and new schemas.
        let has_old_column: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM information_schema.columns
                WHERE table_name = 'sessions' AND column_name = 'context'
                AND data_type = 'jsonb'
            )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap_or(false);

        if has_old_column {
            // Add new columns if missing (idempotent via IF NOT EXISTS equivalent)
            sqlx::query(
                r#"
                DO $$ BEGIN
                    IF NOT EXISTS (
                        SELECT 1 FROM information_schema.columns
                        WHERE table_name = 'sessions' AND column_name = 'context_bytes'
                    ) THEN
                        ALTER TABLE sessions ADD COLUMN context_bytes BYTEA;
                        ALTER TABLE sessions ADD COLUMN task_history TEXT NOT NULL DEFAULT '';
                        -- Backfill: convert JSONB to BYTEA (JSON bytes)
                        UPDATE sessions SET context_bytes = convert_to(context::text, 'UTF8')
                        WHERE context_bytes IS NULL;
                        -- Make NOT NULL after backfill
                        ALTER TABLE sessions ALTER COLUMN context_bytes SET NOT NULL;
                    END IF;
                END $$;
                "#,
            )
            .execute(pool)
            .await
            .map_err(|e| GraphError::StorageError(format!("JSONB migration failed: {e}")))?;
        }

        Ok(())
    }

    /// Serialize context to compact JSON bytes.
    async fn context_to_bytes(ctx: &Context) -> Result<Vec<u8>> {
        let value = ctx.serialize().await;
        serde_json::to_vec(&value)
            .map_err(|e| GraphError::StorageError(format!("Context serialization failed: {e}")))
    }

    /// Deserialize context from JSON bytes.
    fn bytes_to_context(bytes: &[u8]) -> Result<Context> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| GraphError::StorageError(format!("Context deserialization failed: {e}")))?;
        let ctx = Context::new();
        if let serde_json::Value::Object(map) = value {
            for (k, v) in map {
                ctx.set_sync(&k, v);
            }
        }
        Ok(ctx)
    }
}

#[async_trait]
impl SessionStorage for PostgresSessionStorage {
    async fn save(&self, session: Session) -> Result<()> {
        let context_bytes = Self::context_to_bytes(&session.context).await?;
        let task_history = session.task_history.join("\n");

        let mut tx = self.pool.begin().await
            .map_err(|e| GraphError::StorageError(format!("Failed to start transaction: {e}")))?;

        sqlx::query(
            r#"
            INSERT INTO sessions (id, graph_id, current_task_id, status_message,
                                  context_bytes, task_history, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, NOW())
            ON CONFLICT (id) DO UPDATE SET
                graph_id = EXCLUDED.graph_id,
                current_task_id = EXCLUDED.current_task_id,
                status_message = EXCLUDED.status_message,
                context_bytes = EXCLUDED.context_bytes,
                task_history = EXCLUDED.task_history,
                updated_at = NOW()
            "#,
        )
        .bind(&session.id)
        .bind(&session.graph_id)
        .bind(&session.current_task_id)
        .bind(&session.status_message)
        .bind(&context_bytes)
        .bind(&task_history)
        .execute(&mut *tx)
        .await
        .map_err(|e| GraphError::StorageError(format!("Failed to save session: {e}")))?;

        tx.commit().await
            .map_err(|e| GraphError::StorageError(format!("Failed to commit transaction: {e}")))?;

        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Session>> {
        let row = sqlx::query_as::<_, (String, String, String, Option<String>, Vec<u8>, String)>(
            r#"
            SELECT id, graph_id, current_task_id, status_message, context_bytes, task_history
            FROM sessions
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&*self.pool)
        .await
        .map_err(|e| GraphError::StorageError(format!("Failed to fetch session: {e}")))?;

        if let Some((session_id, graph_id, current_task_id, status_message, blob, history_str)) = row {
            let context = Self::bytes_to_context(&blob)?;
            let task_history = if history_str.is_empty() {
                Vec::new()
            } else {
                history_str.split('\n').map(String::from).collect()
            };
            Ok(Some(Session {
                id: session_id,
                graph_id,
                current_task_id,
                status_message,
                context,
                task_history,
            }))
        } else {
            Ok(None)
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id)
            .execute(&*self.pool)
            .await
            .map_err(|e| GraphError::StorageError(format!("Failed to delete session: {e}")))?;
        Ok(())
    }
}
