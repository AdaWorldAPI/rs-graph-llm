//! Gremlin query engine — WebSocket client for Gremlin Server.
//!
//! Protocol: WebSocket + GraphSON v3 (JSON framing).
//! The Gremlin Server accepts a JSON envelope with `requestId`, `op`, `processor`,
//! and `args.gremlin` containing the Gremlin traversal string.

use crate::{GraphData, GraphEdge, GraphNode, QueryEngine, QueryError, QueryMetadata, QueryResult};
use async_trait::async_trait;
use serde_json::json;
use std::time::Instant;

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

    pub fn with_auth(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    pub fn with_traversal_source(mut self, source: impl Into<String>) -> Self {
        self.traversal_source = source.into();
        self
    }
}

#[async_trait]
impl QueryEngine for GremlinEngine {
    async fn execute(&self, query: &str) -> Result<QueryResult, QueryError> {
        let start = Instant::now();
        let request_id = uuid::Uuid::new_v4().to_string();

        // Build the GraphSON v3 request envelope.
        let request = json!({
            "requestId": request_id,
            "op": "eval",
            "processor": "",
            "args": {
                "gremlin": query,
                "bindings": {},
                "language": "gremlin-groovy",
                "aliases": {
                    "g": self.traversal_source
                }
            }
        });

        // Connect via WebSocket.
        let (mut ws_stream, _) =
            tokio_tungstenite::connect_async(&self.uri)
                .await
                .map_err(|e| QueryError::Connection(format!("WebSocket connect failed: {e}")))?;

        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // Send the request.
        let payload = serde_json::to_string(&request)
            .map_err(|e| QueryError::Execution(e.to_string()))?;
        ws_stream
            .send(Message::Text(payload.into()))
            .await
            .map_err(|e| QueryError::Connection(format!("WebSocket send failed: {e}")))?;

        // Collect all response frames (Gremlin Server may send multiple).
        let mut rows: Vec<serde_json::Value> = Vec::new();
        let mut graph_nodes: Vec<GraphNode> = Vec::new();
        let mut graph_edges: Vec<GraphEdge> = Vec::new();

        while let Some(msg) = ws_stream.next().await {
            let msg = msg.map_err(|e| QueryError::Connection(e.to_string()))?;
            match msg {
                Message::Text(text) => {
                    let text_str: &str = &text;
                    let frame: serde_json::Value = serde_json::from_str(text_str)
                        .map_err(|e| QueryError::Execution(format!("Invalid JSON response: {e}")))?;

                    // Check for errors.
                    let status_code = frame
                        .pointer("/status/code")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(200);

                    if status_code >= 400 {
                        let message = frame
                            .pointer("/status/message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Unknown error");
                        return Err(QueryError::Execution(format!(
                            "Gremlin error {status_code}: {message}"
                        )));
                    }

                    // Extract result data from the response.
                    if let Some(data) = frame.pointer("/result/data") {
                        // GraphSON v3 wraps in @type/@value; extract the values.
                        let items = extract_graphson_items(data);
                        for item in &items {
                            // Try to extract graph elements (vertices/edges).
                            extract_graph_elements(item, &mut graph_nodes, &mut graph_edges);
                        }
                        rows.extend(items);
                    }

                    // Status 206 means partial content — more frames coming.
                    // Status 200 means final frame.
                    if status_code == 200 {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {} // Ignore ping/pong/binary
            }
        }

        // Close the WebSocket.
        let _ = ws_stream.close(None).await;

        let duration = start.elapsed();
        let row_count = rows.len();

        let graph = if graph_nodes.is_empty() && graph_edges.is_empty() {
            None
        } else {
            Some(GraphData {
                nodes: graph_nodes,
                edges: graph_edges,
            })
        };

        Ok(QueryResult {
            rows,
            graph,
            metadata: QueryMetadata {
                duration_ms: duration.as_secs_f64() * 1000.0,
                plan: None,
                row_count,
            },
        })
    }
}

/// Extract items from a GraphSON v3 response, unwrapping @type/@value wrappers.
fn extract_graphson_items(data: &serde_json::Value) -> Vec<serde_json::Value> {
    // GraphSON v3 wraps everything: {"@type": "g:List", "@value": [...]}
    if let Some(type_str) = data.get("@type").and_then(|v| v.as_str()) {
        if let Some(value) = data.get("@value") {
            match type_str {
                "g:List" => {
                    if let Some(arr) = value.as_array() {
                        return arr.iter().map(|v| unwrap_graphson(v)).collect();
                    }
                }
                "g:Map" => {
                    return vec![unwrap_graphson_map(value)];
                }
                _ => {
                    return vec![unwrap_graphson(data)];
                }
            }
        }
    }

    // Plain JSON array (GraphSON v1 or already unwrapped).
    if let Some(arr) = data.as_array() {
        return arr.iter().cloned().collect();
    }

    vec![data.clone()]
}

/// Recursively unwrap GraphSON v3 @type/@value wrappers to plain JSON.
fn unwrap_graphson(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            if let (Some(type_val), Some(inner)) = (map.get("@type"), map.get("@value")) {
                let type_str = type_val.as_str().unwrap_or("");
                match type_str {
                    "g:Int32" | "g:Int64" | "g:Float" | "g:Double" => {
                        return inner.clone();
                    }
                    "g:List" => {
                        if let Some(arr) = inner.as_array() {
                            return serde_json::Value::Array(
                                arr.iter().map(unwrap_graphson).collect(),
                            );
                        }
                    }
                    "g:Map" => {
                        return unwrap_graphson_map(inner);
                    }
                    "g:Vertex" | "g:Edge" | "g:Path" | "g:Property"
                    | "g:VertexProperty" => {
                        return unwrap_graphson(inner);
                    }
                    "g:UUID" | "g:Date" | "g:Timestamp" => {
                        return inner.clone();
                    }
                    _ => {
                        return unwrap_graphson(inner);
                    }
                }
            }
            // Regular object — unwrap each value.
            let mut result = serde_json::Map::new();
            for (k, v) in map {
                result.insert(k.clone(), unwrap_graphson(v));
            }
            serde_json::Value::Object(result)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(unwrap_graphson).collect())
        }
        other => other.clone(),
    }
}

/// Unwrap a GraphSON v3 Map (@value is [key, val, key, val, ...]).
fn unwrap_graphson_map(value: &serde_json::Value) -> serde_json::Value {
    if let Some(arr) = value.as_array() {
        let mut map = serde_json::Map::new();
        let mut iter = arr.iter();
        while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
            let key = match unwrap_graphson(k) {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            map.insert(key, unwrap_graphson(v));
        }
        return serde_json::Value::Object(map);
    }
    value.clone()
}

/// Extract GraphNode/GraphEdge from a Gremlin result item.
fn extract_graph_elements(
    item: &serde_json::Value,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
) {
    if let Some(obj) = item.as_object() {
        // Vertex: has "id", "label", possibly "properties"
        if obj.contains_key("id") && obj.contains_key("label") {
            let id = obj
                .get("id")
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let label = obj
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // Check if it's an edge (has inV/outV or inVLabel/outVLabel)
            if obj.contains_key("inV") || obj.contains_key("inVLabel") {
                let from = obj
                    .get("outV")
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                let to = obj
                    .get("inV")
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();

                edges.push(GraphEdge {
                    from,
                    to,
                    label,
                    properties: obj
                        .get("properties")
                        .cloned()
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                });
            } else {
                // It's a vertex.
                let group = obj.get("label").and_then(|v| v.as_str()).map(String::from);
                let properties = obj
                    .get("properties")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                // Dedup by id.
                if !nodes.iter().any(|n| n.id == id) {
                    nodes.push(GraphNode {
                        id,
                        label,
                        group,
                        properties,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrap_graphson_v3_int() {
        let val = serde_json::json!({"@type": "g:Int32", "@value": 42});
        assert_eq!(unwrap_graphson(&val), serde_json::json!(42));
    }

    #[test]
    fn unwrap_graphson_v3_list() {
        let val = serde_json::json!({
            "@type": "g:List",
            "@value": [
                {"@type": "g:Int32", "@value": 1},
                {"@type": "g:Int32", "@value": 2}
            ]
        });
        assert_eq!(
            extract_graphson_items(&val),
            vec![serde_json::json!(1), serde_json::json!(2)]
        );
    }

    #[test]
    fn extract_vertex() {
        let item = serde_json::json!({
            "id": "v1",
            "label": "person",
            "properties": {"name": "Alice"}
        });
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        extract_graph_elements(&item, &mut nodes, &mut edges);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, "v1");
        assert_eq!(nodes[0].label, "person");
        assert!(edges.is_empty());
    }

    #[test]
    fn extract_edge() {
        let item = serde_json::json!({
            "id": "e1",
            "label": "knows",
            "outV": "v1",
            "inV": "v2",
            "properties": {"weight": 0.8}
        });
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        extract_graph_elements(&item, &mut nodes, &mut edges);
        assert!(nodes.is_empty());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "v1");
        assert_eq!(edges[0].to, "v2");
        assert_eq!(edges[0].label, "knows");
    }
}
