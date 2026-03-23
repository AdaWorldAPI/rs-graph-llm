//! MCP server over SSE transport.
//!
//! Implements the Model Context Protocol (MCP) so Claude Code can drive the
//! notebook as an agent.  Transport is Server-Sent Events on the existing axum
//! server.
//!
//! Endpoint: `GET /mcp/sse`  — SSE stream (one per session)
//! Endpoint: `POST /mcp/message?session_id=<id>` — JSON-RPC requests
//!
//! Claude Code connects with:
//! ```json
//! { "type": "url", "url": "http://localhost:2718/mcp/sse", "name": "notebook" }
//! ```

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

use crate::state::NotebookState;

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

/// State shared across all handlers via axum `State` extractor.
#[derive(Clone)]
pub struct AppState {
    /// The notebook (cells, DAG, runtime).
    pub notebook: Arc<Mutex<NotebookState>>,
    /// Active SSE sessions:  session_id → channel sender.
    pub sessions: Arc<DashMap<String, mpsc::Sender<String>>>,
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl JsonRpcResponse {
    fn ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn err(id: Option<serde_json::Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// MCP protocol constants
// ---------------------------------------------------------------------------

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "notebook";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// SSE endpoint — GET /mcp/sse
// ---------------------------------------------------------------------------

pub async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel::<String>(64);

    // Send the endpoint event through the channel so it's the first SSE event.
    let endpoint_url = format!("/mcp/message?session_id={session_id}");
    let _ = tx.send(endpoint_url.clone()).await;

    // Store the sender so the POST handler can reach it.
    state.sessions.insert(session_id.clone(), tx);

    tracing::info!("MCP session started: {session_id}");

    // Build the SSE stream. First event is `endpoint`, rest are `message`.
    let mut first = true;
    let stream = ReceiverStream::new(rx).map(move |data| {
        if first {
            first = false;
            Ok(Event::default().event("endpoint").data(data))
        } else {
            Ok(Event::default().event("message").data(data))
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// Message endpoint — POST /mcp/message
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SessionQuery {
    pub session_id: String,
}

pub async fn message_handler(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let session_id = &query.session_id;

    // Validate JSON-RPC version.
    if request.jsonrpc != "2.0" {
        return Json(JsonRpcResponse::err(
            request.id,
            -32600,
            "Invalid JSON-RPC version",
        ));
    }

    // Notifications (no id) — just acknowledge.
    if request.id.is_none() {
        // `notifications/initialized` etc. — no response needed.
        // Return empty 202 equivalent as JSON.
        return Json(JsonRpcResponse::ok(None, serde_json::json!({})));
    }

    // Dispatch.
    let response = dispatch(&state, &request).await;

    // Also send the response through the SSE channel.
    if let Some(tx) = state.sessions.get(session_id) {
        let json = serde_json::to_string(&response).unwrap_or_default();
        let _ = tx.send(json).await;
    }

    Json(response)
}

// ---------------------------------------------------------------------------
// JSON-RPC method dispatch
// ---------------------------------------------------------------------------

async fn dispatch(state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(req),
        "tools/list" => handle_tools_list(req),
        "tools/call" => handle_tools_call(state, req).await,
        "ping" => JsonRpcResponse::ok(req.id.clone(), serde_json::json!({})),
        _ => JsonRpcResponse::err(req.id.clone(), -32601, format!("Method not found: {}", req.method)),
    }
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

fn handle_initialize(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        }),
    )
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

fn handle_tools_list(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::json!({
            "tools": tool_definitions()
        }),
    )
}

fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "cell_execute",
            "description": "Execute a cell. Creates it if it doesn't exist. Language is auto-detected from code if `lang` is omitted or set to \"auto\". Returns result immediately.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code":    { "type": "string", "description": "Source code to execute" },
                    "lang":    { "type": "string", "description": "Language: cypher, gremlin, sparql, nars, rust, r, python, markdown. Omit or \"auto\" for auto-detection." },
                    "cell_id": { "type": "string", "description": "Optional cell ID. Auto-generates if omitted." }
                },
                "required": ["code"]
            }
        },
        {
            "name": "cell_get",
            "description": "Read a cell's current state including code, status, output, and MIME type.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cell_id": { "type": "string", "description": "Cell ID to look up" }
                },
                "required": ["cell_id"]
            }
        },
        {
            "name": "cells_list",
            "description": "List all cells ordered by position. Includes DAG state (defs, refs, status).",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "cell_create",
            "description": "Add a cell at a position. Language auto-detected if `lang` is omitted. Does not execute.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code":  { "type": "string", "description": "Source code" },
                    "lang":  { "type": "string", "description": "Language. Omit or \"auto\" for auto-detection." },
                    "after": { "type": "string", "description": "Insert after this cell ID" }
                },
                "required": ["code"]
            }
        },
        {
            "name": "cell_update",
            "description": "Modify cell code. Triggers reactive re-execution of downstream cells.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cell_id": { "type": "string", "description": "Cell to update" },
                    "code":    { "type": "string", "description": "New source code" }
                },
                "required": ["cell_id", "code"]
            }
        },
        {
            "name": "cell_delete",
            "description": "Remove a cell. Marks downstream cells as stale.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cell_id": { "type": "string", "description": "Cell to delete" }
                },
                "required": ["cell_id"]
            }
        },
        {
            "name": "detect_language",
            "description": "Detect the language of a code snippet without creating a cell. Returns the detected language and confidence.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Code to analyze" }
                },
                "required": ["code"]
            }
        },
        {
            "name": "dag_get",
            "description": "Get the dependency graph as nodes and edges.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "notebook_save",
            "description": "Serialize the full notebook state to a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to save to" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "notebook_load",
            "description": "Load a notebook from a file, replacing current state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to load from" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "notebook_export",
            "description": "Render the notebook to cockpit HTML (dense panels, interactive graph, sortable tables) or Markdown.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "format": { "type": "string", "description": "Output format: html, pdf, markdown" },
                    "path":   { "type": "string", "description": "Output file path" }
                },
                "required": ["format", "path"]
            }
        }
    ])
}

// ---------------------------------------------------------------------------
// tools/call
// ---------------------------------------------------------------------------

async fn handle_tools_call(state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let tool_name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = req.params.get("arguments").cloned().unwrap_or(serde_json::json!({}));

    let result = match tool_name {
        "cell_execute" => tool_cell_execute(state, &arguments).await,
        "cell_get" => tool_cell_get(state, &arguments).await,
        "cells_list" => tool_cells_list(state).await,
        "cell_create" => tool_cell_create(state, &arguments).await,
        "cell_update" => tool_cell_update(state, &arguments).await,
        "cell_delete" => tool_cell_delete(state, &arguments).await,
        "detect_language" => tool_detect_language(&arguments).await,
        "dag_get" => tool_dag_get(state).await,
        "notebook_save" => tool_notebook_save(state, &arguments).await,
        "notebook_load" => tool_notebook_load(state, &arguments).await,
        "notebook_export" => tool_notebook_export(state, &arguments).await,
        _ => Err(format!("Unknown tool: {tool_name}")),
    };

    match result {
        Ok(content) => JsonRpcResponse::ok(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&content).unwrap_or_default()
                }]
            }),
        ),
        Err(msg) => JsonRpcResponse::ok(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": msg
                }],
                "isError": true
            }),
        ),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

async fn tool_cell_execute(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = args.get("code").and_then(|v| v.as_str()).ok_or("Missing 'code'")?;
    let lang = args.get("lang").and_then(|v| v.as_str());
    let cell_id = args.get("cell_id").and_then(|v| v.as_str());

    let mut nb = state.notebook.lock().await;
    let result = nb.execute_cell(code, lang, cell_id).await?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

async fn tool_cell_get(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let cell_id = args.get("cell_id").and_then(|v| v.as_str()).ok_or("Missing 'cell_id'")?;

    let nb = state.notebook.lock().await;
    let info = nb.get_cell(cell_id)?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

async fn tool_cells_list(state: &AppState) -> Result<serde_json::Value, String> {
    let nb = state.notebook.lock().await;
    let cells = nb.list_cells();
    serde_json::to_value(serde_json::json!({ "cells": cells })).map_err(|e| e.to_string())
}

async fn tool_cell_create(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = args.get("code").and_then(|v| v.as_str()).ok_or("Missing 'code'")?;
    let lang = args.get("lang").and_then(|v| v.as_str());
    let after = args.get("after").and_then(|v| v.as_str());

    let mut nb = state.notebook.lock().await;
    let result = nb.create_cell(code, lang, after)?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

async fn tool_cell_update(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let cell_id = args.get("cell_id").and_then(|v| v.as_str()).ok_or("Missing 'cell_id'")?;
    let code = args.get("code").and_then(|v| v.as_str()).ok_or("Missing 'code'")?;

    let mut nb = state.notebook.lock().await;
    let result = nb.update_cell(cell_id, code).await?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

async fn tool_cell_delete(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let cell_id = args.get("cell_id").and_then(|v| v.as_str()).ok_or("Missing 'cell_id'")?;

    let mut nb = state.notebook.lock().await;
    let result = nb.delete_cell(cell_id)?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

async fn tool_detect_language(
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = args.get("code").and_then(|v| v.as_str()).ok_or("Missing 'code'")?;
    let result = NotebookState::detect_language(code);
    serde_json::to_value(result).map_err(|e| e.to_string())
}

async fn tool_dag_get(state: &AppState) -> Result<serde_json::Value, String> {
    let nb = state.notebook.lock().await;
    let dag = nb.dag();
    serde_json::to_value(dag).map_err(|e| e.to_string())
}

async fn tool_notebook_save(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or("Missing 'path'")?;

    let nb = state.notebook.lock().await;
    let result = nb.save(path)?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

async fn tool_notebook_load(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or("Missing 'path'")?;

    let mut nb = state.notebook.lock().await;
    let result = nb.load(path)?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

async fn tool_notebook_export(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let format = args.get("format").and_then(|v| v.as_str()).ok_or("Missing 'format'")?;
    let path = args.get("path").and_then(|v| v.as_str()).ok_or("Missing 'path'")?;

    let nb = state.notebook.lock().await;
    let result = nb.export(format, path)?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}
