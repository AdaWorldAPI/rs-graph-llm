//! Gremlin query engine — WebSocket client for Gremlin Server.

use crate::{QueryEngine, QueryError, QueryResult};
use async_trait::async_trait;

/// Gremlin query executor using WebSocket + GraphSON.
pub struct GremlinEngine {
    /// WebSocket URI (e.g., "ws://localhost:8182/gremlin").
    pub uri: String,
    /// Traversal source name (default: "g").
    pub traversal_source: String,
    /// Username for authentication.
    pub username: Option<String>,
    /// Password for authentication.
    pub password: Option<String>,
}

impl GremlinEngine {
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            traversal_source: "g".to_string(),
            username: None,
            password: None,
        }
    }
}

#[async_trait]
impl QueryEngine for GremlinEngine {
    async fn execute(&self, query: &str) -> Result<QueryResult, QueryError> {
        // TODO: Implement WebSocket + GraphSON client
        Err(QueryError::Unsupported(
            "Gremlin WebSocket client not yet implemented.".into(),
        ))
    }
}
