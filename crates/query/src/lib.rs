//! # notebook-query
//!
//! Graph query engines transcoded from graph-notebook (Python).
//!
//! Supports:
//! - Cypher (via Bolt protocol to Neo4j/FalkorDB, or local via lance-graph)
//! - Gremlin (via WebSocket to Gremlin Server)
//! - SPARQL (via HTTP POST to SPARQL endpoints)
//! - NARS (via HTTP to NARS endpoints)

pub mod cypher;
pub mod gremlin;
pub mod sparql;
pub mod local;
pub mod result;

use async_trait::async_trait;

/// Trait for all query engines.
#[async_trait]
pub trait QueryEngine: Send + Sync {
    /// Execute a query and return a result.
    async fn execute(&self, query: &str) -> Result<QueryResult, QueryError>;
}

/// Universal query result: rows (Arrow) + graph (nodes/edges for vis.js).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryResult {
    /// Tabular result as JSON (serialized Arrow RecordBatch).
    pub rows: Vec<serde_json::Value>,
    /// Graph visualization data (nodes + edges for vis.js).
    pub graph: Option<GraphData>,
    /// Query metadata (timing, plan, etc.).
    pub metadata: QueryMetadata,
}

/// Graph data for vis.js rendering.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// A node in the graph visualization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub properties: serde_json::Value,
}

/// An edge in the graph visualization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub properties: serde_json::Value,
}

/// Query execution metadata.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct QueryMetadata {
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub row_count: usize,
}

/// Query execution error.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("Connection error: {0}")]
    Connection(String),
    #[error("Query syntax error: {0}")]
    Syntax(String),
    #[error("Execution error: {0}")]
    Execution(String),
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
}
