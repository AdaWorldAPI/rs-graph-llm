//! SPARQL query engine — HTTP POST client for SPARQL endpoints.

use crate::{QueryEngine, QueryError, QueryResult, QueryMetadata};
use async_trait::async_trait;

/// SPARQL query executor using HTTP POST.
pub struct SparqlEngine {
    /// SPARQL endpoint URL (e.g., "http://localhost:8890/sparql").
    pub endpoint: String,
    /// HTTP client.
    client: reqwest::Client,
}

impl SparqlEngine {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl QueryEngine for SparqlEngine {
    async fn execute(&self, query: &str) -> Result<QueryResult, QueryError> {
        let response = self.client
            .post(&self.endpoint)
            .header("Content-Type", "application/sparql-query")
            .header("Accept", "application/sparql-results+json")
            .body(query.to_string())
            .send()
            .await
            .map_err(|e| QueryError::Connection(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(QueryError::Execution(
                format!("SPARQL endpoint returned {status}: {body}")
            ));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| QueryError::Execution(e.to_string()))?;

        // Parse SPARQL JSON results format
        let rows = parse_sparql_results(&body);

        Ok(QueryResult {
            rows,
            graph: None, // SPARQL SELECT doesn't produce graph data directly
            metadata: QueryMetadata {
                row_count: 0,
                ..Default::default()
            },
        })
    }
}

/// Parse SPARQL JSON results format into flat rows.
fn parse_sparql_results(body: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut rows = Vec::new();

    if let Some(results) = body.get("results").and_then(|r| r.get("bindings")).and_then(|b| b.as_array()) {
        for binding in results {
            let mut row = serde_json::Map::new();
            if let Some(obj) = binding.as_object() {
                for (key, val) in obj {
                    // Extract the "value" field from each binding
                    if let Some(v) = val.get("value") {
                        row.insert(key.clone(), v.clone());
                    }
                }
            }
            rows.push(serde_json::Value::Object(row));
        }
    }

    rows
}
