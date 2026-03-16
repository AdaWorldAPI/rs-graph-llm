//! HTTP API server for graph-flow.
//!
//! Provides a LangGraph-compatible REST API wrapping graph-flow execution.
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | POST | `/threads` | Create a new session (thread) |
//! | POST | `/threads/{id}/runs` | Execute one graph step |
//! | GET | `/threads/{id}/state` | Get current session state |
//! | GET | `/threads/{id}/history` | Get version history (Lance time travel) |
//! | DELETE | `/threads/{id}` | Delete a session |
//!
//! # Examples
//!
//! ```rust,no_run
//! use graph_flow_server::create_router;
//! use graph_flow::{Graph, InMemorySessionStorage};
//! use std::sync::Arc;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let graph = Arc::new(Graph::new("my_workflow"));
//! let storage = Arc::new(InMemorySessionStorage::new());
//! let app = create_router(graph, storage);
//!
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
//! axum::serve(listener, app).await.unwrap();
//! # }
//! ```

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::{delete, get, post},
};
use graph_flow::{
    ExecutionStatus, FlowRunner, Graph, Session, SessionStorage,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use futures::stream;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub runner: FlowRunner,
    pub storage: Arc<dyn SessionStorage>,
}

/// Request body for creating a new thread.
#[derive(Debug, Deserialize)]
pub struct CreateThreadRequest {
    /// Starting task ID.
    pub start_task: String,
    /// Optional initial context values.
    #[serde(default)]
    pub context: serde_json::Map<String, serde_json::Value>,
}

/// Response for thread creation.
#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadResponse {
    pub thread_id: String,
    pub current_task: String,
}

/// Response for execution results.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunResponse {
    pub response: Option<String>,
    pub status: String,
    pub next_task: Option<String>,
}

/// Response for thread state.
#[derive(Debug, Serialize, Deserialize)]
pub struct StateResponse {
    pub thread_id: String,
    pub current_task: String,
    pub context: serde_json::Value,
}

/// A single entry in thread history.
#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub task_id: String,
    pub index: usize,
}

/// Response for thread history.
#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub thread_id: String,
    pub entries: Vec<HistoryEntry>,
    pub current_task: String,
}

/// Create the Axum router with all endpoints.
pub fn create_router(
    graph: Arc<Graph>,
    storage: Arc<dyn SessionStorage>,
) -> Router {
    let runner = FlowRunner::new(graph, storage.clone());
    let state = AppState { runner, storage };

    Router::new()
        .route("/threads", post(create_thread))
        .route("/threads/{id}/runs", post(run_thread))
        .route("/threads/{id}/runs/stream", post(run_thread_stream))
        .route("/threads/{id}/state", get(get_state))
        .route("/threads/{id}/history", get(get_history))
        .route("/threads/{id}", delete(delete_thread))
        .with_state(state)
}

async fn create_thread(
    State(state): State<AppState>,
    Json(req): Json<CreateThreadRequest>,
) -> Result<(StatusCode, Json<ThreadResponse>), (StatusCode, String)> {
    let thread_id = uuid::Uuid::new_v4().to_string();
    let session = Session::new_from_task(thread_id.clone(), &req.start_task);

    // Set initial context
    for (key, value) in req.context {
        session.context.set_sync(&key, value);
    }

    state
        .storage
        .save(session)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(ThreadResponse {
            thread_id,
            current_task: req.start_task,
        }),
    ))
}

async fn run_thread(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunResponse>, (StatusCode, String)> {
    let result = state
        .runner
        .run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (status, next_task) = match &result.status {
        ExecutionStatus::Completed => ("completed".to_string(), None),
        ExecutionStatus::WaitingForInput => ("waiting_for_input".to_string(), None),
        ExecutionStatus::Paused {
            next_task_id,
            reason,
        } => (format!("paused: {}", reason), Some(next_task_id.clone())),
        ExecutionStatus::Error(e) => (format!("error: {}", e), None),
    };

    Ok(Json(RunResponse {
        response: result.response,
        status,
        next_task,
    }))
}

async fn get_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<StateResponse>, (StatusCode, String)> {
    let session = state
        .storage
        .get(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Thread {} not found", id)))?;

    let context = session.context.serialize().await;

    Ok(Json(StateResponse {
        thread_id: id,
        current_task: session.current_task_id,
        context,
    }))
}

async fn delete_thread(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .storage
        .delete(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

/// SSE streaming endpoint — runs the graph step by step, emitting events.
async fn run_thread_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>>, (StatusCode, String)> {
    // Verify session exists
    let _session = state
        .storage
        .get(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Thread {} not found", id)))?;

    let runner = state.runner.clone();
    let storage = state.storage.clone();
    let thread_id = id.clone();

    let event_stream = stream::unfold(
        (runner, storage, thread_id, false),
        |(runner, storage, thread_id, done)| async move {
            if done {
                return None;
            }
            match runner.run(&thread_id).await {
                Ok(result) => {
                    let is_done = matches!(result.status, ExecutionStatus::Completed | ExecutionStatus::Error(_));
                    let event_data = serde_json::json!({
                        "response": result.response,
                        "status": format!("{:?}", result.status),
                    });
                    let event = Event::default()
                        .data(event_data.to_string())
                        .event("step");
                    Some((Ok(event), (runner, storage, thread_id, is_done)))
                }
                Err(e) => {
                    let event = Event::default()
                        .data(serde_json::json!({"error": e.to_string()}).to_string())
                        .event("error");
                    Some((Ok(event), (runner, storage, thread_id, true)))
                }
            }
        },
    );

    Ok(Sse::new(event_stream))
}

/// Get thread execution history.
async fn get_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<HistoryResponse>, (StatusCode, String)> {
    let session = state
        .storage
        .get(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Thread {} not found", id)))?;

    let entries: Vec<HistoryEntry> = session
        .task_history
        .iter()
        .enumerate()
        .map(|(i, task_id)| HistoryEntry {
            task_id: task_id.clone(),
            index: i,
        })
        .collect();

    Ok(Json(HistoryResponse {
        thread_id: id,
        entries,
        current_task: session.current_task_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use async_trait::async_trait;
    use graph_flow::{GraphBuilder, InMemorySessionStorage, NextAction, Task, TaskResult, Context};
    use tower::ServiceExt;

    struct EchoTask;

    #[async_trait]
    impl Task for EchoTask {
        fn id(&self) -> &str {
            "echo"
        }
        async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
            let input: String = ctx.get("input").await.unwrap_or_default();
            Ok(TaskResult::new(
                Some(format!("Echo: {}", input)),
                NextAction::End,
            ))
        }
    }

    fn test_app() -> Router {
        let graph = Arc::new(
            GraphBuilder::new("test")
                .add_task(Arc::new(EchoTask) as Arc<dyn Task>)
                .build(),
        );
        let storage = Arc::new(InMemorySessionStorage::new());
        create_router(graph, storage)
    }

    #[tokio::test]
    async fn test_create_and_run_thread() {
        let app = test_app();

        // Create thread
        let req = Request::builder()
            .method("POST")
            .uri("/threads")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "start_task": "echo",
                    "context": {"input": "hello"}
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let thread: ThreadResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(thread.current_task, "echo");

        // Run thread
        let req = Request::builder()
            .method("POST")
            .uri(format!("/threads/{}/runs", thread.thread_id))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let run: RunResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(run.response, Some("Echo: hello".to_string()));
        assert_eq!(run.status, "completed");
    }

    #[tokio::test]
    async fn test_get_state() {
        let app = test_app();

        // Create thread
        let req = Request::builder()
            .method("POST")
            .uri("/threads")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "start_task": "echo",
                    "context": {"key": "value"}
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let thread: ThreadResponse = serde_json::from_slice(&body).unwrap();

        // Get state
        let req = Request::builder()
            .method("GET")
            .uri(format!("/threads/{}/state", thread.thread_id))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let state: StateResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(state.context["key"], "value");
    }
}
