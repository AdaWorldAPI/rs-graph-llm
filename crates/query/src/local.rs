//! Local Cypher engine — executes Cypher via lance-graph (no network hop).

use crate::{GraphData, GraphNode, GraphEdge, QueryEngine, QueryError, QueryResult, QueryMetadata};
use async_trait::async_trait;
use arrow::array::RecordBatch;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Local Cypher query executor using lance-graph.
/// Queries execute in-process via the semiring planner — no network hop.
pub struct LocalCypherEngine {
    /// Graph configuration (schema mapping).
    config: lance_graph::GraphConfig,
    /// In-memory datasets (loaded Arrow RecordBatches).
    datasets: Arc<tokio::sync::RwLock<HashMap<String, RecordBatch>>>,
}

impl LocalCypherEngine {
    pub fn new(config: lance_graph::GraphConfig) -> Self {
        Self {
            config,
            datasets: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Load a dataset (Arrow RecordBatch) for a node label or relationship type.
    pub async fn load_dataset(&self, name: impl Into<String>, batch: RecordBatch) {
        self.datasets.write().await.insert(name.into(), batch);
    }
}

#[async_trait]
impl QueryEngine for LocalCypherEngine {
    async fn execute(&self, query: &str) -> Result<QueryResult, QueryError> {
        let start = Instant::now();

        // Parse and configure the Cypher query
        let cypher_query = lance_graph::CypherQuery::new(query)
            .map_err(|e| QueryError::Syntax(e.to_string()))?
            .with_config(self.config.clone());

        // Execute against in-memory datasets
        let datasets = self.datasets.read().await.clone();
        let batch = cypher_query
            .execute(datasets, None)
            .await
            .map_err(|e| QueryError::Execution(e.to_string()))?;

        let duration = start.elapsed();
        let row_count = batch.num_rows();

        // Convert RecordBatch to JSON rows
        let rows = record_batch_to_json(&batch);

        Ok(QueryResult {
            rows,
            graph: None, // TODO: extract graph structure from results
            metadata: QueryMetadata {
                duration_ms: duration.as_secs_f64() * 1000.0,
                plan: None,
                row_count,
            },
        })
    }
}

/// Convert an Arrow RecordBatch to a vector of JSON objects.
fn record_batch_to_json(batch: &RecordBatch) -> Vec<serde_json::Value> {
    let mut rows = Vec::with_capacity(batch.num_rows());
    let schema = batch.schema();

    for row_idx in 0..batch.num_rows() {
        let mut row = serde_json::Map::new();
        for (col_idx, field) in schema.fields().iter().enumerate() {
            let col = batch.column(col_idx);
            let value = arrow::array::cast::as_string_array(col)
                .value(row_idx);
            row.insert(
                field.name().clone(),
                serde_json::Value::String(value.to_string()),
            );
        }
        rows.push(serde_json::Value::Object(row));
    }

    rows
}
