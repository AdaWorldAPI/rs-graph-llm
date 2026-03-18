//! Three-tier session storage: Lance (hot) → PostgreSQL (warm) → S3 (cold).
//!
//! Write path:  `save()` → Lance (immediate) → PostgreSQL (write-through)
//! Read path:   `get()` → Lance (fast) → PostgreSQL (fallback) → S3 (archive)
//!
//! # Examples
//!
//! ```rust,no_run
//! use graph_flow::tiered_storage::TieredSessionStorage;
//!
//! # #[tokio::main]
//! # async fn main() -> graph_flow::Result<()> {
//! // Hot-only (Lance local):
//! let hot = TieredSessionStorage::hot_only("/data/sessions.lance");
//!
//! // Hot + warm (Lance + PostgreSQL):
//! // let hw = TieredSessionStorage::hot_warm("/data/sessions.lance", "postgres://...").await?;
//!
//! // Full tier (Lance + PostgreSQL + S3):
//! // let full = TieredSessionStorage::full("/data/sessions.lance", "postgres://...", "s3://bucket/archive/").await?;
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;

use crate::{
    error::Result,
    lance_storage::LanceSessionStorage,
    storage::{Session, SessionStorage},
    storage_postgres::PostgresSessionStorage,
};

/// Three-tier storage: Lance (hot) → PostgreSQL (warm) → S3 (cold).
///
/// - **Hot**: In-process Lance dataset. Sub-ms reads, append-only writes.
/// - **Warm**: PostgreSQL on Railway. Survives restarts, ACID transactions.
/// - **Cold**: S3-backed Lance dataset. Archival, seconds latency.
///
/// The hot tier always has the freshest data. Warm is a write-through
/// durability layer. Cold is for archival/replay.
pub struct TieredSessionStorage {
    hot: LanceSessionStorage,
    warm: Option<PostgresSessionStorage>,
    cold_path: Option<String>,
}

impl TieredSessionStorage {
    /// Hot-only: Lance local, no PostgreSQL, no S3.
    pub fn hot_only(lance_path: &str) -> Self {
        Self {
            hot: LanceSessionStorage::new(lance_path),
            warm: None,
            cold_path: None,
        }
    }

    /// Hot + warm: Lance local + PostgreSQL write-through.
    pub async fn hot_warm(lance_path: &str, pg_url: &str) -> Result<Self> {
        let warm = PostgresSessionStorage::connect(pg_url).await?;
        Ok(Self {
            hot: LanceSessionStorage::new(lance_path),
            warm: Some(warm),
            cold_path: None,
        })
    }

    /// Full tier: Lance local + PostgreSQL + S3 cold archive.
    pub async fn full(lance_path: &str, pg_url: &str, s3_path: &str) -> Result<Self> {
        let warm = PostgresSessionStorage::connect(pg_url).await?;
        Ok(Self {
            hot: LanceSessionStorage::new(lance_path),
            warm: Some(warm),
            cold_path: Some(s3_path.to_string()),
        })
    }

    /// Access the hot (Lance) tier directly for time-travel operations.
    pub fn hot(&self) -> &LanceSessionStorage {
        &self.hot
    }
}

#[async_trait]
impl SessionStorage for TieredSessionStorage {
    async fn save(&self, session: Session) -> Result<()> {
        // Always write to Lance (hot)
        self.hot.save(session.clone()).await?;

        // Write-through to PostgreSQL (warm) if configured
        if let Some(warm) = &self.warm {
            warm.save(session).await?;
        }

        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Session>> {
        // Try Lance first (hot, sub-ms)
        if let Some(session) = self.hot.get(id).await? {
            return Ok(Some(session));
        }

        // Fallback to PostgreSQL (warm, ms)
        if let Some(warm) = &self.warm
            && let Some(session) = warm.get(id).await? {
                // Backfill into Lance for next access
                let _ = self.hot.save(session.clone()).await;
                return Ok(Some(session));
            }

        // Cold path: open S3 Lance dataset (slow, seconds)
        if let Some(cold_path) = &self.cold_path {
            let cold = LanceSessionStorage::new(cold_path);
            if let Some(session) = cold.get(id).await? {
                // Backfill into hot + warm
                let _ = self.hot.save(session.clone()).await;
                if let Some(warm) = &self.warm {
                    let _ = warm.save(session.clone()).await;
                }
                return Ok(Some(session));
            }
        }

        Ok(None)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.hot.delete(id).await?;
        if let Some(warm) = &self.warm {
            warm.delete(id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Context;

    fn make_session(id: &str, task: &str) -> Session {
        Session {
            id: id.to_string(),
            graph_id: "test".to_string(),
            current_task_id: task.to_string(),
            status_message: None,
            context: Context::new(),
            task_history: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_hot_only() {
        let storage = TieredSessionStorage::hot_only("/tmp/test_tiered_hot.lance");

        let session = make_session("t1", "task_a");
        storage.save(session).await.unwrap();

        let loaded = storage.get("t1").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().current_task_id, "task_a");
    }

    #[tokio::test]
    async fn test_hot_time_travel() {
        let storage = TieredSessionStorage::hot_only("/tmp/test_tiered_tt.lance");

        let mut session = make_session("tt1", "start");
        storage.save(session.clone()).await.unwrap();

        session.current_task_id = "end".to_string();
        storage.save(session).await.unwrap();

        // Access time travel via hot tier
        let versions = storage.hot().get_versions("tt1").await.unwrap();
        assert_eq!(versions.len(), 2);
    }

    #[tokio::test]
    async fn test_delete() {
        let storage = TieredSessionStorage::hot_only("/tmp/test_tiered_del.lance");

        let session = make_session("td1", "a");
        storage.save(session).await.unwrap();

        storage.delete("td1").await.unwrap();
        assert!(storage.get("td1").await.unwrap().is_none());
    }
}
