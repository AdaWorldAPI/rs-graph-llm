//! State snapshots for graph execution inspection.
//!
//! Maps to LangGraph's `StateSnapshot` type.

use serde::{Deserialize, Serialize};

/// A point-in-time snapshot of graph execution state.
///
/// Maps to LangGraph's `StateSnapshot` NamedTuple.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Current state values (serialized context).
    pub values: serde_json::Value,
    /// IDs of tasks that will execute next.
    pub next: Vec<String>,
    /// Session/thread ID.
    pub thread_id: String,
    /// Current task ID.
    pub current_task: String,
    /// Task execution history.
    pub task_history: Vec<String>,
    /// When this snapshot was created.
    pub created_at: String,
    /// Optional metadata.
    pub metadata: serde_json::Value,
}

impl StateSnapshot {
    /// Create a snapshot from a session.
    pub async fn from_session(session: &crate::storage::Session) -> Self {
        let values = session.context.serialize().await;
        let now = chrono::Utc::now().to_rfc3339();

        Self {
            values,
            next: Vec::new(), // Populated by caller
            thread_id: session.id.clone(),
            current_task: session.current_task_id.clone(),
            task_history: session.task_history.clone(),
            created_at: now,
            metadata: serde_json::json!({}),
        }
    }

    /// Create a snapshot with next task information.
    pub fn with_next(mut self, next: Vec<String>) -> Self {
        self.next = next;
        self
    }

    /// Create a snapshot with metadata.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    #[tokio::test]
    async fn test_snapshot_from_session() {
        let session = Session::new_from_task("t1".to_string(), "task_a");
        session.context.set("key", "value").await;

        let snap = StateSnapshot::from_session(&session).await;
        assert_eq!(snap.thread_id, "t1");
        assert_eq!(snap.current_task, "task_a");
        assert_eq!(snap.values["key"], "value");
    }

    #[tokio::test]
    async fn test_snapshot_with_next() {
        let session = Session::new_from_task("t1".to_string(), "a");
        let snap = StateSnapshot::from_session(&session)
            .await
            .with_next(vec!["b".into(), "c".into()]);
        assert_eq!(snap.next, vec!["b", "c"]);
    }
}
