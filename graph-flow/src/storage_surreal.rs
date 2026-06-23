//! SurrealDB-on-Lance (kv-lance) backed [`SessionStorage`].
//!
//! Persists graph-flow [`Session`]s — the kanban cards — as documents in a
//! SurrealDB instance running on the AdaWorldAPI Lance (kv-lance) backend, so
//! session/orchestration state lives on the same columnar substrate as the
//! cold blob + vector stores rig drives. The hot scan path never sees the
//! serialized session; it is fetched by key on demand, exactly mirroring
//! [`crate::storage_postgres::PostgresSessionStorage`].
//!
//! Behind the `surreal-lance` feature — it pulls the AdaWorldAPI/surrealdb fork
//! and the Lance tree, so it is off by default to keep the core build light.
//!
//! ```no_run
//! # async fn demo() -> graph_flow::Result<()> {
//! use graph_flow::storage_surreal::SurrealSessionStorage;
//! use graph_flow::SessionStorage;
//! let store = SurrealSessionStorage::connect("./data/graphflow.lance").await?;
//! let loaded = store.get("some-session-id").await?;
//! # let _ = loaded; Ok(())
//! # }
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use surrealdb::engine::local::{Db, Lance};
use surrealdb::Surreal;

use crate::{
    error::{GraphError, Result},
    storage::SessionStorage,
    Session,
};

const NS: &str = "graphflow";
const DB: &str = "sessions";
const TABLE: &str = "sessions";

/// A [`SessionStorage`] backed by SurrealDB on the Lance (kv-lance) engine.
pub struct SurrealSessionStorage {
    db: Arc<Surreal<Db>>,
}

/// Wrapper record: the full `Session` JSON in one `data` field, so the
/// `Session.id` string never collides with SurrealDB's reserved record id
/// (a `Thing`). The record's SurrealDB id is `(sessions, session.id)`; `data`
/// carries the serialized session. On `select`, SurrealDB's extra `id` field is
/// ignored by serde (the struct has no matching field).
#[derive(Debug, Serialize, Deserialize)]
struct SessionRecord {
    data: serde_json::Value,
}

impl SurrealSessionStorage {
    /// Open (or create) a SurrealDB instance on the Lance backend at `path`.
    ///
    /// `path` is the Lance dataset directory, e.g. `./data/graphflow.lance`.
    pub async fn connect(path: &str) -> Result<Self> {
        let db = Surreal::new::<Lance>(path).await.map_err(|e| {
            GraphError::StorageError(format!("Failed to open Lance-backed SurrealDB: {e}"))
        })?;
        db.use_ns(NS)
            .use_db(DB)
            .await
            .map_err(|e| GraphError::StorageError(format!("use_ns/use_db failed: {e}")))?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Borrow the underlying SurrealDB handle — for the cold blob / vector
    /// tables that share this same kv-lance instance.
    pub fn inner(&self) -> &Surreal<Db> {
        &self.db
    }
}

#[async_trait]
impl SessionStorage for SurrealSessionStorage {
    async fn save(&self, session: Session) -> Result<()> {
        let id = session.id.clone();
        let data = serde_json::to_value(&session).map_err(|e| {
            GraphError::StorageError(format!("Session serialization failed: {e}"))
        })?;
        let _: Option<SessionRecord> = self
            .db
            .upsert((TABLE, id.as_str()))
            .content(SessionRecord { data })
            .await
            .map_err(|e| GraphError::StorageError(format!("Failed to save session: {e}")))?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Session>> {
        let rec: Option<SessionRecord> = self
            .db
            .select((TABLE, id))
            .await
            .map_err(|e| GraphError::StorageError(format!("Failed to fetch session: {e}")))?;
        match rec {
            Some(r) => {
                let session: Session = serde_json::from_value(r.data).map_err(|e| {
                    GraphError::StorageError(format!("Session deserialization failed: {e}"))
                })?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let _: Option<SessionRecord> = self
            .db
            .delete((TABLE, id))
            .await
            .map_err(|e| GraphError::StorageError(format!("Failed to delete session: {e}")))?;
        Ok(())
    }
}
