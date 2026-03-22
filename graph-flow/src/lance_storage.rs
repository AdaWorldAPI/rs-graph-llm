//! Lance-based session storage with time travel support.
//!
//! This module provides [`LanceSessionStorage`] which persists sessions as
//! Lance dataset rows. Each save appends a new row, enabling time travel
//! (loading sessions at any previous version).
//!
//! # Overview
//!
//! Lance is a columnar storage format with automatic versioning. Every write
//! operation creates a new version, and old versions are retained. This means
//! you can load any previous state of a session — enabling time travel debugging.
//!
//! # Examples
//!
//! ```rust,no_run
//! use graph_flow::lance_storage::LanceSessionStorage;
//! use graph_flow::SessionStorage;
//!
//! # #[tokio::main]
//! # async fn main() -> graph_flow::Result<()> {
//! let storage = LanceSessionStorage::new("/tmp/sessions.lance");
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "lance-store")]
mod real_impl {
    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    use arrow::array::{
        Int64Array, RecordBatch, RecordBatchIterator, StringArray, UInt64Array,
        LargeBinaryArray,
    };
    use arrow::datatypes::{DataType, Field, Schema};
    use lance::dataset::{WriteMode, WriteParams};
    use lance::Dataset;
    use tokio::sync::RwLock;

    use crate::{
        error::{GraphError, Result},
        storage::{Session, SessionStorage},
        Context,
    };

    /// A session snapshot at a particular version.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct VersionedSession {
        /// The session data
        pub session: Session,
        /// The version number (auto-incremented on each save)
        pub version: u64,
        /// Timestamp of this version (epoch millis)
        pub timestamp: i64,
    }

    /// Lance-based session storage with time travel support.
    ///
    /// Each `save()` appends a new row to the Lance dataset with an incrementing
    /// version number. Previous versions can be retrieved using `get_at_version()`.
    ///
    /// Supports local filesystem, S3, and any Lance-compatible object store:
    /// ```rust,no_run
    /// # use graph_flow::lance_storage::LanceSessionStorage;
    /// // Local:
    /// let local = LanceSessionStorage::new("/data/sessions.lance");
    /// // S3 (Lance handles transparently):
    /// let s3 = LanceSessionStorage::new("s3://my-bucket/sessions.lance");
    /// ```
    pub struct LanceSessionStorage {
        /// Path to the Lance dataset (local or S3).
        pub dataset_path: String,
        /// Cached dataset handle.
        dataset: RwLock<Option<Dataset>>,
    }

    fn sessions_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("session_id", DataType::Utf8, false),
            Field::new("graph_id", DataType::Utf8, false),
            Field::new("current_task_id", DataType::Utf8, false),
            Field::new("status_message", DataType::Utf8, true),
            Field::new("context_bytes", DataType::LargeBinary, false),
            Field::new("task_history", DataType::Utf8, false),
            Field::new("version", DataType::UInt64, false),
            Field::new("timestamp", DataType::Int64, false),
        ]))
    }

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// Serialize context to compact JSON bytes.
    async fn context_to_bytes(ctx: &Context) -> Result<Vec<u8>> {
        let value = ctx.serialize().await;
        serde_json::to_vec(&value)
            .map_err(|e| GraphError::StorageError(format!("context serialize: {e}")))
    }

    /// Deserialize context from JSON bytes.
    fn bytes_to_context(bytes: &[u8]) -> Result<Context> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| GraphError::StorageError(format!("context deserialize: {e}")))?;
        let ctx = Context::new();
        if let serde_json::Value::Object(map) = value {
            for (k, v) in map {
                ctx.set_sync(&k, v);
            }
        }
        Ok(ctx)
    }

    impl LanceSessionStorage {
        /// Create a new Lance session storage.
        ///
        /// The path can be local (`/data/sessions.lance`) or S3 (`s3://bucket/sessions.lance`).
        pub fn new(dataset_path: impl Into<String>) -> Self {
            Self {
                dataset_path: dataset_path.into(),
                dataset: RwLock::new(None),
            }
        }

        /// Open existing dataset or return None.
        async fn open_dataset(&self) -> Result<Option<Dataset>> {
            match Dataset::open(&self.dataset_path).await {
                Ok(ds) => Ok(Some(ds)),
                Err(_) => Ok(None),
            }
        }

        /// Write a RecordBatch (create or append).
        async fn write_batch(&self, batch: RecordBatch, mode: WriteMode) -> Result<()> {
            let params = WriteParams {
                mode,
                ..Default::default()
            };
            let reader = RecordBatchIterator::new(vec![Ok(batch)], sessions_schema());
            let ds = Dataset::write(reader, &self.dataset_path, Some(params))
                .await
                .map_err(|e| GraphError::StorageError(format!("Lance write: {e}")))?;
            *self.dataset.write().await = Some(ds);
            Ok(())
        }

        /// Read all rows matching a session_id (all versions).
        async fn read_versions(&self, session_id: &str) -> Result<Vec<VersionedSession>> {
            // Refresh dataset handle
            let ds = match self.open_dataset().await? {
                Some(ds) => ds,
                None => return Ok(Vec::new()),
            };

            let batches = ds
                .scan()
                .try_into_stream()
                .await
                .map_err(|e| GraphError::StorageError(format!("Lance scan: {e}")))?;

            use futures::TryStreamExt;
            let batches: Vec<RecordBatch> = batches
                .try_collect()
                .await
                .map_err(|e| GraphError::StorageError(format!("Lance collect: {e}")))?;

            let mut versions = Vec::new();
            for batch in &batches {
                let sid_col = batch.column_by_name("session_id").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let gid_col = batch.column_by_name("graph_id").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let tid_col = batch.column_by_name("current_task_id").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let msg_col = batch.column_by_name("status_message").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let ctx_col = batch.column_by_name("context_bytes").unwrap()
                    .as_any().downcast_ref::<LargeBinaryArray>().unwrap();
                let hist_col = batch.column_by_name("task_history").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let ver_col = batch.column_by_name("version").unwrap()
                    .as_any().downcast_ref::<UInt64Array>().unwrap();
                let ts_col = batch.column_by_name("timestamp").unwrap()
                    .as_any().downcast_ref::<Int64Array>().unwrap();

                for i in 0..batch.num_rows() {
                    let sid = sid_col.value(i);
                    if sid != session_id {
                        continue;
                    }

                    let context = bytes_to_context(ctx_col.value(i))?;
                    let history_str = hist_col.value(i);
                    let task_history = if history_str.is_empty() {
                        Vec::new()
                    } else {
                        history_str.split('\n').map(String::from).collect()
                    };
                    let status_message = if msg_col.is_null(i) {
                        None
                    } else {
                        let s = msg_col.value(i);
                        if s.is_empty() { None } else { Some(s.to_string()) }
                    };

                    versions.push(VersionedSession {
                        session: Session {
                            id: sid.to_string(),
                            graph_id: gid_col.value(i).to_string(),
                            current_task_id: tid_col.value(i).to_string(),
                            status_message,
                            context,
                            task_history,
                        },
                        version: ver_col.value(i),
                        timestamp: ts_col.value(i),
                    });
                }
            }

            versions.sort_by_key(|v| v.version);
            Ok(versions)
        }

        /// Next version number for a session (max existing + 1, or 1).
        async fn next_version(&self, session_id: &str) -> Result<u64> {
            let versions = self.read_versions(session_id).await?;
            Ok(versions.last().map(|v| v.version + 1).unwrap_or(1))
        }

        /// Build a RecordBatch from a session + version.
        async fn session_to_batch(session: &Session, version: u64) -> Result<RecordBatch> {
            let ctx_bytes = context_to_bytes(&session.context).await?;
            let history = session.task_history.join("\n");
            let status = session.status_message.as_deref().unwrap_or("");
            let ts = now_ms();

            RecordBatch::try_new(
                sessions_schema(),
                vec![
                    Arc::new(StringArray::from(vec![session.id.as_str()])),
                    Arc::new(StringArray::from(vec![session.graph_id.as_str()])),
                    Arc::new(StringArray::from(vec![session.current_task_id.as_str()])),
                    Arc::new(StringArray::from(vec![status])),
                    Arc::new(LargeBinaryArray::from_vec(vec![&ctx_bytes])),
                    Arc::new(StringArray::from(vec![history.as_str()])),
                    Arc::new(UInt64Array::from(vec![version])),
                    Arc::new(Int64Array::from(vec![ts])),
                ],
            )
            .map_err(|e| GraphError::StorageError(format!("Arrow batch: {e}")))
        }

        // --- Public time-travel API ---

        /// Get a session at a specific version.
        pub async fn get_at_version(
            &self,
            session_id: &str,
            version: u64,
        ) -> Result<Option<Session>> {
            let versions = self.read_versions(session_id).await?;
            Ok(versions
                .into_iter()
                .find(|v| v.version == version)
                .map(|v| v.session))
        }

        /// Get all versions of a session, ordered by version number.
        pub async fn get_versions(&self, session_id: &str) -> Result<Vec<VersionedSession>> {
            self.read_versions(session_id).await
        }

        /// Get the version history (version numbers and timestamps) for a session.
        pub async fn get_version_history(
            &self,
            session_id: &str,
        ) -> Result<Vec<(u64, i64)>> {
            let versions = self.read_versions(session_id).await?;
            Ok(versions.iter().map(|v| (v.version, v.timestamp)).collect())
        }

        /// Revert a session to a specific version.
        ///
        /// Creates a NEW version with the state from the specified version (append-only).
        pub async fn revert_to_version(
            &self,
            session_id: &str,
            version: u64,
        ) -> Result<Session> {
            let old_session = self
                .get_at_version(session_id, version)
                .await?
                .ok_or_else(|| {
                    GraphError::SessionNotFound(format!(
                        "Session '{}' version {} not found",
                        session_id, version
                    ))
                })?;
            self.save(old_session.clone()).await?;
            Ok(old_session)
        }

        /// Get the current (latest) version number for a session.
        pub async fn current_version(&self, session_id: &str) -> Option<u64> {
            self.read_versions(session_id)
                .await
                .ok()
                .and_then(|vs| vs.last().map(|v| v.version))
        }

        /// Save a session with checkpoint namespace.
        pub async fn save_namespaced(
            &self,
            session: Session,
            namespace: &str,
        ) -> Result<()> {
            let mut namespaced = session;
            namespaced.id = format!("{}:{}", namespace, namespaced.id);
            self.save(namespaced).await
        }

        /// Get a session from a specific namespace.
        pub async fn get_namespaced(
            &self,
            session_id: &str,
            namespace: &str,
        ) -> Result<Option<Session>> {
            let key = format!("{}:{}", namespace, session_id);
            self.get(&key).await.map(|opt| {
                opt.map(|mut s| {
                    if let Some(stripped) = s.id.strip_prefix(&format!("{}:", namespace)) {
                        s.id = stripped.to_string();
                    }
                    s
                })
            })
        }
    }

    #[async_trait]
    impl SessionStorage for LanceSessionStorage {
        async fn save(&self, session: Session) -> Result<()> {
            let version = self.next_version(&session.id).await?;
            let batch = Self::session_to_batch(&session, version).await?;

            // Append mode: each save adds a new row (new version).
            let mode = if self.open_dataset().await?.is_some() {
                WriteMode::Append
            } else {
                WriteMode::Create
            };

            self.write_batch(batch, mode).await
        }

        async fn get(&self, id: &str) -> Result<Option<Session>> {
            let versions = self.read_versions(id).await?;
            Ok(versions.into_iter().last().map(|v| v.session))
        }

        async fn delete(&self, id: &str) -> Result<()> {
            // Read all rows, filter out the session, rewrite.
            // This is the simple approach; Lance delete API could also be used.
            let ds = match self.open_dataset().await? {
                Some(ds) => ds,
                None => return Ok(()),
            };

            let batches = ds
                .scan()
                .try_into_stream()
                .await
                .map_err(|e| GraphError::StorageError(format!("Lance scan: {e}")))?;

            use futures::TryStreamExt;
            let all_batches: Vec<RecordBatch> = batches
                .try_collect()
                .await
                .map_err(|e| GraphError::StorageError(format!("Lance collect: {e}")))?;

            // Collect all rows NOT matching the session id
            let mut sids = Vec::new();
            let mut gids = Vec::new();
            let mut tids = Vec::new();
            let mut msgs = Vec::new();
            let mut ctxs: Vec<Vec<u8>> = Vec::new();
            let mut hists = Vec::new();
            let mut vers = Vec::new();
            let mut tss = Vec::new();

            for batch in &all_batches {
                let sid_col = batch.column_by_name("session_id").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let gid_col = batch.column_by_name("graph_id").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let tid_col = batch.column_by_name("current_task_id").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let msg_col = batch.column_by_name("status_message").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let ctx_col = batch.column_by_name("context_bytes").unwrap()
                    .as_any().downcast_ref::<LargeBinaryArray>().unwrap();
                let hist_col = batch.column_by_name("task_history").unwrap()
                    .as_any().downcast_ref::<StringArray>().unwrap();
                let ver_col = batch.column_by_name("version").unwrap()
                    .as_any().downcast_ref::<UInt64Array>().unwrap();
                let ts_col = batch.column_by_name("timestamp").unwrap()
                    .as_any().downcast_ref::<Int64Array>().unwrap();

                for i in 0..batch.num_rows() {
                    if sid_col.value(i) == id {
                        continue;
                    }
                    sids.push(sid_col.value(i).to_string());
                    gids.push(gid_col.value(i).to_string());
                    tids.push(tid_col.value(i).to_string());
                    msgs.push(if msg_col.is_null(i) { String::new() } else { msg_col.value(i).to_string() });
                    ctxs.push(ctx_col.value(i).to_vec());
                    hists.push(hist_col.value(i).to_string());
                    vers.push(ver_col.value(i));
                    tss.push(ts_col.value(i));
                }
            }

            if sids.is_empty() {
                // All rows belonged to this session — delete the dataset
                if std::path::Path::new(&self.dataset_path).exists() {
                    tokio::fs::remove_dir_all(&self.dataset_path)
                        .await
                        .map_err(|e| GraphError::StorageError(format!("remove dataset: {e}")))?;
                }
                *self.dataset.write().await = None;
                return Ok(());
            }

            let ctx_refs: Vec<&[u8]> = ctxs.iter().map(|v| v.as_slice()).collect();
            let sid_refs: Vec<&str> = sids.iter().map(|s| s.as_str()).collect();
            let gid_refs: Vec<&str> = gids.iter().map(|s| s.as_str()).collect();
            let tid_refs: Vec<&str> = tids.iter().map(|s| s.as_str()).collect();
            let msg_refs: Vec<&str> = msgs.iter().map(|s| s.as_str()).collect();
            let hist_refs: Vec<&str> = hists.iter().map(|s| s.as_str()).collect();

            let batch = RecordBatch::try_new(
                sessions_schema(),
                vec![
                    Arc::new(StringArray::from(sid_refs)),
                    Arc::new(StringArray::from(gid_refs)),
                    Arc::new(StringArray::from(tid_refs)),
                    Arc::new(StringArray::from(msg_refs)),
                    Arc::new(LargeBinaryArray::from_vec(ctx_refs)),
                    Arc::new(StringArray::from(hist_refs)),
                    Arc::new(UInt64Array::from(vers)),
                    Arc::new(Int64Array::from(tss)),
                ],
            )
            .map_err(|e| GraphError::StorageError(format!("Arrow batch: {e}")))?;

            self.write_batch(batch, WriteMode::Overwrite).await
        }
    }
}

// When lance-store feature is NOT enabled, provide the in-memory mock
// so the rest of the crate compiles without lance.
#[cfg(not(feature = "lance-store"))]
mod mock_impl {
    use async_trait::async_trait;
    use dashmap::DashMap;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::{
        error::{GraphError, Result},
        storage::{Session, SessionStorage},
    };

    /// A session snapshot at a particular version.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct VersionedSession {
        pub session: Session,
        pub version: u64,
        pub timestamp: i64,
    }

    /// Lance-based session storage (in-memory mock when `lance-store` feature is disabled).
    ///
    /// Enable the `lance-store` feature for real Lance persistence.
    pub struct LanceSessionStorage {
        pub dataset_path: String,
        sessions: Arc<DashMap<String, Vec<VersionedSession>>>,
        version_counter: Arc<AtomicU64>,
    }

    impl LanceSessionStorage {
        pub fn new(dataset_path: impl Into<String>) -> Self {
            Self {
                dataset_path: dataset_path.into(),
                sessions: Arc::new(DashMap::new()),
                version_counter: Arc::new(AtomicU64::new(1)),
            }
        }

        pub async fn get_at_version(
            &self,
            session_id: &str,
            version: u64,
        ) -> Result<Option<Session>> {
            Ok(self
                .sessions
                .get(session_id)
                .and_then(|versions| {
                    versions.iter().find(|v| v.version == version).map(|v| v.session.clone())
                }))
        }

        pub async fn get_versions(&self, session_id: &str) -> Result<Vec<VersionedSession>> {
            Ok(self.sessions.get(session_id).map(|v| v.clone()).unwrap_or_default())
        }

        pub async fn get_version_history(
            &self,
            session_id: &str,
        ) -> Result<Vec<(u64, i64)>> {
            Ok(self
                .sessions
                .get(session_id)
                .map(|versions| versions.iter().map(|v| (v.version, v.timestamp)).collect())
                .unwrap_or_default())
        }

        pub async fn revert_to_version(
            &self,
            session_id: &str,
            version: u64,
        ) -> Result<Session> {
            let old_session = self
                .get_at_version(session_id, version)
                .await?
                .ok_or_else(|| {
                    GraphError::SessionNotFound(format!(
                        "Session '{}' version {} not found",
                        session_id, version
                    ))
                })?;
            self.save(old_session.clone()).await?;
            Ok(old_session)
        }

        pub async fn current_version(&self, session_id: &str) -> Option<u64> {
            self.sessions
                .get(session_id)
                .and_then(|versions| versions.last().map(|v| v.version))
        }

        pub async fn save_namespaced(
            &self,
            session: Session,
            namespace: &str,
        ) -> Result<()> {
            let mut namespaced = session;
            namespaced.id = format!("{}:{}", namespace, namespaced.id);
            self.save(namespaced).await
        }

        pub async fn get_namespaced(
            &self,
            session_id: &str,
            namespace: &str,
        ) -> Result<Option<Session>> {
            let key = format!("{}:{}", namespace, session_id);
            self.get(&key).await.map(|opt| {
                opt.map(|mut s| {
                    if let Some(stripped) = s.id.strip_prefix(&format!("{}:", namespace)) {
                        s.id = stripped.to_string();
                    }
                    s
                })
            })
        }
    }

    #[async_trait]
    impl SessionStorage for LanceSessionStorage {
        async fn save(&self, session: Session) -> Result<()> {
            let version = self.version_counter.fetch_add(1, Ordering::SeqCst);
            let versioned = VersionedSession {
                session: session.clone(),
                version,
                timestamp: chrono::Utc::now().timestamp_millis(),
            };
            self.sessions
                .entry(session.id.clone())
                .or_default()
                .push(versioned);
            Ok(())
        }

        async fn get(&self, id: &str) -> Result<Option<Session>> {
            Ok(self
                .sessions
                .get(id)
                .and_then(|versions| versions.last().map(|v| v.session.clone())))
        }

        async fn delete(&self, id: &str) -> Result<()> {
            self.sessions.remove(id);
            Ok(())
        }
    }
}

#[cfg(feature = "lance-store")]
pub use real_impl::*;

#[cfg(not(feature = "lance-store"))]
pub use mock_impl::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Context;
    use crate::storage::SessionStorage;

    fn make_session(id: &str, task: &str) -> crate::storage::Session {
        crate::storage::Session {
            id: id.to_string(),
            graph_id: "test".to_string(),
            current_task_id: task.to_string(),
            status_message: None,
            context: Context::new(),
            task_history: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_basic_save_and_get() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_basic.lance");

        let session = make_session("s1", "task_a");
        storage.save(session).await.unwrap();

        let loaded = storage.get("s1").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().current_task_id, "task_a");
    }

    #[tokio::test]
    async fn test_versioning() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_ver.lance");

        let mut session = make_session("sv1", "task_a");
        storage.save(session.clone()).await.unwrap();

        session.current_task_id = "task_b".to_string();
        storage.save(session.clone()).await.unwrap();

        session.current_task_id = "task_c".to_string();
        storage.save(session).await.unwrap();

        let latest = storage.get("sv1").await.unwrap().unwrap();
        assert_eq!(latest.current_task_id, "task_c");

        let versions = storage.get_versions("sv1").await.unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].session.current_task_id, "task_a");
        assert_eq!(versions[1].session.current_task_id, "task_b");
        assert_eq!(versions[2].session.current_task_id, "task_c");
    }

    #[tokio::test]
    async fn test_time_travel() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_tt.lance");

        let mut session = make_session("st1", "start");
        storage.save(session.clone()).await.unwrap();
        let v1 = storage.current_version("st1").await.unwrap();

        session.current_task_id = "middle".to_string();
        storage.save(session.clone()).await.unwrap();

        session.current_task_id = "end".to_string();
        storage.save(session).await.unwrap();

        let old = storage.get_at_version("st1", v1).await.unwrap().unwrap();
        assert_eq!(old.current_task_id, "start");
    }

    #[tokio::test]
    async fn test_revert() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_rev.lance");

        let mut session = make_session("sr1", "start");
        storage.save(session.clone()).await.unwrap();
        let v1 = storage.current_version("sr1").await.unwrap();

        session.current_task_id = "changed".to_string();
        storage.save(session).await.unwrap();

        let reverted = storage.revert_to_version("sr1", v1).await.unwrap();
        assert_eq!(reverted.current_task_id, "start");

        let latest = storage.get("sr1").await.unwrap().unwrap();
        assert_eq!(latest.current_task_id, "start");
    }

    #[tokio::test]
    async fn test_version_history() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_vh.lance");

        let session = make_session("sh1", "a");
        storage.save(session).await.unwrap();

        let session = make_session("sh1", "b");
        storage.save(session).await.unwrap();

        let history = storage.get_version_history("sh1").await.unwrap();
        assert_eq!(history.len(), 2);
        assert!(history[0].0 < history[1].0);
    }

    #[tokio::test]
    async fn test_delete() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_del.lance");

        let session = make_session("sd1", "a");
        storage.save(session).await.unwrap();

        storage.delete("sd1").await.unwrap();
        assert!(storage.get("sd1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_nonexistent_session() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_ne.lance");
        assert!(storage.get("nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_checkpoint_namespacing() {
        let storage = LanceSessionStorage::new("/tmp/test_lance_session_ns.lance");

        let session_a = make_session("sn1", "task_a");
        let session_b = make_session("sn1", "task_b");

        storage.save_namespaced(session_a, "ns1").await.unwrap();
        storage.save_namespaced(session_b, "ns2").await.unwrap();

        let a = storage.get_namespaced("sn1", "ns1").await.unwrap().unwrap();
        let b = storage.get_namespaced("sn1", "ns2").await.unwrap().unwrap();

        assert_eq!(a.current_task_id, "task_a");
        assert_eq!(b.current_task_id, "task_b");
        assert_eq!(a.id, "sn1");
        assert_eq!(b.id, "sn1");
    }
}
