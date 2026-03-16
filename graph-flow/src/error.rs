use thiserror::Error;

#[derive(Error, Debug)]
pub enum GraphError {
    #[error("Task execution failed: {0}")]
    TaskExecutionFailed(String),

    #[error("Graph not found: {0}")]
    GraphNotFound(String),

    #[error("Invalid edge: {0}")]
    InvalidEdge(String),

    #[error("Task not found: {0}")]
    TaskNotFound(String),

    #[error("Context error: {0}")]
    ContextError(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    /// Graph exceeded the configured recursion limit.
    /// Maps to LangGraph's `GraphRecursionError`.
    #[error("Graph recursion limit exceeded: {limit} steps (task: {task_id})")]
    RecursionLimitExceeded {
        limit: usize,
        task_id: String,
    },

    /// Graph execution was interrupted (human-in-the-loop).
    /// Maps to LangGraph's `GraphInterrupt`.
    #[error("Graph interrupted at task '{task_id}': {reason}")]
    GraphInterrupt {
        task_id: String,
        reason: String,
        /// Serialized interrupt data (e.g. HumanInterrupt).
        data: Option<serde_json::Value>,
    },

    /// Graph validation failed.
    #[error("Graph validation failed: {0}")]
    ValidationError(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, GraphError>;
