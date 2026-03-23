use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{Context, error::{GraphError, Result}, graph::Graph};

/// A checkpoint captures a session snapshot at a point in time.
///
/// Maps to LangGraph's checkpoint concept — each checkpoint records the full
/// session state so that execution can be resumed or inspected later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Unique checkpoint identifier.
    pub checkpoint_id: String,
    /// Thread (session) this checkpoint belongs to.
    pub thread_id: String,
    /// Optional namespace for multi-tenant isolation.
    pub checkpoint_ns: Option<String>,
    /// The full session state at this point.
    pub session: Session,
    /// ISO-8601 timestamp of when this checkpoint was created.
    pub created_at: String,
}

/// Session information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub graph_id: String,
    pub current_task_id: String,
    /// Optional status message from the last executed task
    pub status_message: Option<String>,
    pub context: crate::context::Context,
    /// Task history stack for GoBack navigation.
    /// Each time a task completes, its ID is pushed here.
    #[serde(default)]
    pub task_history: Vec<String>,
}

impl Session {
    pub fn new_from_task(sid: String, task_name: &str) -> Self {
        Self {
            id: sid,
            graph_id: "default".to_string(),
            current_task_id: task_name.to_string(),
            status_message: None,
            context: Context::new(),
            task_history: Vec::new(),
        }
    }

    /// Create a deep copy of this session, including a fresh context
    /// with the same key-value data. Unlike `clone()`, which shares the
    /// underlying `Arc`-backed context, this produces an independent copy
    /// suitable for checkpointing.
    pub async fn snapshot(&self) -> Self {
        let new_ctx = Context::new();
        let data = self.context.serialize().await;
        if let serde_json::Value::Object(map) = data {
            for (k, v) in map {
                new_ctx.set_sync(&k, v);
            }
        }
        Self {
            id: self.id.clone(),
            graph_id: self.graph_id.clone(),
            current_task_id: self.current_task_id.clone(),
            status_message: self.status_message.clone(),
            context: new_ctx,
            task_history: self.task_history.clone(),
        }
    }

    /// Push the current task onto history and advance to a new task.
    pub fn advance_to(&mut self, next_task_id: String) {
        self.task_history.push(self.current_task_id.clone());
        self.current_task_id = next_task_id;
    }

    /// Go back to the previous task. Returns the previous task ID if available.
    pub fn go_back(&mut self) -> Option<String> {
        if let Some(prev) = self.task_history.pop() {
            self.current_task_id = prev.clone();
            Some(prev)
        } else {
            None
        }
    }
}

/// Trait for storing and retrieving graphs
#[async_trait]
pub trait GraphStorage: Send + Sync {
    async fn save(&self, id: String, graph: Arc<Graph>) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Arc<Graph>>>;
    async fn delete(&self, id: &str) -> Result<()>;
}

/// Trait for storing and retrieving sessions
#[async_trait]
pub trait SessionStorage: Send + Sync {
    async fn save(&self, session: Session) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Session>>;
    async fn delete(&self, id: &str) -> Result<()>;

    // ── Checkpoint support (LangGraph parity) ──────────────────────────

    /// Save a checkpoint for the given thread.
    ///
    /// The `checkpoint_id` inside `checkpoint` must be unique within the thread.
    /// Backends that do not support checkpointing return a `StorageError`.
    async fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        let _ = checkpoint;
        Err(GraphError::StorageError(
            "Checkpointing is not supported by this storage backend".into(),
        ))
    }

    /// Load a specific checkpoint by thread ID and checkpoint ID.
    async fn get_checkpoint(
        &self,
        thread_id: &str,
        checkpoint_id: &str,
    ) -> Result<Option<Checkpoint>> {
        let _ = (thread_id, checkpoint_id);
        Err(GraphError::StorageError(
            "Checkpointing is not supported by this storage backend".into(),
        ))
    }

    /// List all checkpoints for a thread, ordered oldest-first.
    async fn list_checkpoints(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        let _ = thread_id;
        Err(GraphError::StorageError(
            "Checkpointing is not supported by this storage backend".into(),
        ))
    }
}

/// In-memory implementation of GraphStorage
pub struct InMemoryGraphStorage {
    graphs: Arc<DashMap<String, Arc<Graph>>>,
}

impl Default for InMemoryGraphStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryGraphStorage {
    pub fn new() -> Self {
        Self {
            graphs: Arc::new(DashMap::new()),
        }
    }
}

#[async_trait]
impl GraphStorage for InMemoryGraphStorage {
    async fn save(&self, id: String, graph: Arc<Graph>) -> Result<()> {
        self.graphs.insert(id, graph);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Arc<Graph>>> {
        Ok(self.graphs.get(id).map(|entry| entry.clone()))
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.graphs.remove(id);
        Ok(())
    }
}

/// In-memory implementation of SessionStorage
pub struct InMemorySessionStorage {
    sessions: Arc<DashMap<String, Session>>,
    /// thread_id -> Vec<Checkpoint>, ordered by creation time.
    checkpoints: Arc<DashMap<String, Vec<Checkpoint>>>,
}

impl Default for InMemorySessionStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySessionStorage {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            checkpoints: Arc::new(DashMap::new()),
        }
    }
}

#[async_trait]
impl SessionStorage for InMemorySessionStorage {
    async fn save(&self, session: Session) -> Result<()> {
        self.sessions.insert(session.id.clone(), session);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Session>> {
        Ok(self.sessions.get(id).map(|entry| entry.clone()))
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.sessions.remove(id);
        Ok(())
    }

    async fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        self.checkpoints
            .entry(checkpoint.thread_id.clone())
            .or_default()
            .push(checkpoint);
        Ok(())
    }

    async fn get_checkpoint(
        &self,
        thread_id: &str,
        checkpoint_id: &str,
    ) -> Result<Option<Checkpoint>> {
        Ok(self.checkpoints.get(thread_id).and_then(|cps| {
            cps.iter()
                .find(|cp| cp.checkpoint_id == checkpoint_id)
                .cloned()
        }))
    }

    async fn list_checkpoints(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        Ok(self
            .checkpoints
            .get(thread_id)
            .map(|cps| cps.clone())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_checkpoint(thread_id: &str, cp_id: &str, task: &str) -> Checkpoint {
        let session = Session::new_from_task(thread_id.to_string(), task);
        Checkpoint {
            checkpoint_id: cp_id.to_string(),
            thread_id: thread_id.to_string(),
            checkpoint_ns: None,
            session,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[tokio::test]
    async fn test_checkpoint_save_and_load() {
        let storage = InMemorySessionStorage::new();

        let cp = make_checkpoint("thread1", "cp1", "task_a");
        storage.save_checkpoint(cp).await.unwrap();

        let loaded = storage
            .get_checkpoint("thread1", "cp1")
            .await
            .unwrap()
            .expect("checkpoint should exist");
        assert_eq!(loaded.checkpoint_id, "cp1");
        assert_eq!(loaded.thread_id, "thread1");
        assert_eq!(loaded.session.current_task_id, "task_a");
    }

    #[tokio::test]
    async fn test_checkpoint_not_found() {
        let storage = InMemorySessionStorage::new();

        let result = storage.get_checkpoint("thread1", "nope").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_checkpoint_list_empty() {
        let storage = InMemorySessionStorage::new();

        let list = storage.list_checkpoints("thread1").await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_checkpoint_list_multiple() {
        let storage = InMemorySessionStorage::new();

        let cp1 = make_checkpoint("thread1", "cp1", "task_a");
        let cp2 = make_checkpoint("thread1", "cp2", "task_b");
        let cp3 = make_checkpoint("thread1", "cp3", "task_c");

        storage.save_checkpoint(cp1).await.unwrap();
        storage.save_checkpoint(cp2).await.unwrap();
        storage.save_checkpoint(cp3).await.unwrap();

        let list = storage.list_checkpoints("thread1").await.unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].checkpoint_id, "cp1");
        assert_eq!(list[1].checkpoint_id, "cp2");
        assert_eq!(list[2].checkpoint_id, "cp3");
    }

    #[tokio::test]
    async fn test_checkpoint_isolation_across_threads() {
        let storage = InMemorySessionStorage::new();

        let cp_a = make_checkpoint("thread_a", "cp1", "task_x");
        let cp_b = make_checkpoint("thread_b", "cp1", "task_y");

        storage.save_checkpoint(cp_a).await.unwrap();
        storage.save_checkpoint(cp_b).await.unwrap();

        let list_a = storage.list_checkpoints("thread_a").await.unwrap();
        let list_b = storage.list_checkpoints("thread_b").await.unwrap();

        assert_eq!(list_a.len(), 1);
        assert_eq!(list_b.len(), 1);
        assert_eq!(list_a[0].session.current_task_id, "task_x");
        assert_eq!(list_b[0].session.current_task_id, "task_y");
    }

    #[tokio::test]
    async fn test_checkpoint_with_namespace() {
        let storage = InMemorySessionStorage::new();

        let cp = Checkpoint {
            checkpoint_id: "cp1".to_string(),
            thread_id: "thread1".to_string(),
            checkpoint_ns: Some("prod".to_string()),
            session: Session::new_from_task("thread1".to_string(), "task_a"),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        storage.save_checkpoint(cp).await.unwrap();

        let loaded = storage
            .get_checkpoint("thread1", "cp1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.checkpoint_ns, Some("prod".to_string()));
    }

    #[tokio::test]
    async fn test_checkpoint_preserves_session_context() {
        let storage = InMemorySessionStorage::new();

        let session = Session::new_from_task("thread1".to_string(), "task_a");
        session.context.set("score", 42i64).await;
        session.context.set("name", "alice".to_string()).await;

        let cp = Checkpoint {
            checkpoint_id: "cp_ctx".to_string(),
            thread_id: "thread1".to_string(),
            checkpoint_ns: None,
            session,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        storage.save_checkpoint(cp).await.unwrap();

        let loaded = storage
            .get_checkpoint("thread1", "cp_ctx")
            .await
            .unwrap()
            .unwrap();
        let score: Option<i64> = loaded.session.context.get("score").await;
        let name: Option<String> = loaded.session.context.get("name").await;
        assert_eq!(score, Some(42));
        assert_eq!(name, Some("alice".to_string()));
    }
}
