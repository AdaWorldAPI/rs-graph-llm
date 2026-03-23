//! Cypher query engine — Bolt protocol client for Neo4j/FalkorDB.

use crate::{QueryEngine, QueryError, QueryResult};
use async_trait::async_trait;

/// Cypher query executor using Bolt protocol.
pub struct CypherEngine {
    /// Bolt connection URI (e.g., "bolt://localhost:7687").
    pub uri: String,
    /// Username for authentication.
    pub username: Option<String>,
    /// Password for authentication.
    pub password: Option<String>,
    /// Database name (Neo4j 4+).
    pub database: Option<String>,
}

impl CypherEngine {
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            username: None,
            password: None,
            database: None,
        }
    }

    pub fn with_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }
}

#[async_trait]
impl QueryEngine for CypherEngine {
    async fn execute(&self, query: &str) -> Result<QueryResult, QueryError> {
        // TODO: Implement Bolt protocol client
        // For now, return an error indicating this is not yet implemented
        Err(QueryError::Unsupported(
            "Bolt protocol client not yet implemented. Use local engine for Cypher.".into(),
        ))
    }
}
