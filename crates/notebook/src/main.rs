//! # notebook
//!
//! Polyglot reactive notebook — single binary.
//!
//! Combines:
//! - notebook-runtime: reactive cell execution (marimo transcode)
//! - notebook-query: graph query engines (graph-notebook transcode)
//! - notebook-kernel: Jupyter kernel protocol (for R via IRkernel)
//! - notebook-publish: document publisher (quarto transcode)
//!
//! The ONLY API is MCP over SSE.  Claude Code connects with:
//! ```json
//! { "type": "url", "url": "http://localhost:2718/mcp/sse", "name": "notebook" }
//! ```

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use dashmap::DashMap;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;

mod mcp;
mod routes;
mod state;

use mcp::AppState;
use state::NotebookState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "notebook=info".into()),
        )
        .init();

    let addr = std::env::var("NOTEBOOK_ADDR").unwrap_or_else(|_| "0.0.0.0:2718".to_string());

    tracing::info!("Starting polyglot notebook on {addr}");

    let shared = AppState {
        notebook: Arc::new(Mutex::new(NotebookState::new())),
        sessions: Arc::new(DashMap::new()),
    };

    let app = Router::new()
        // Health probe (the only non-MCP endpoint).
        .route("/health", get(routes::health))
        // MCP SSE transport.
        .route("/mcp/sse", get(mcp::sse_handler))
        .route("/mcp/message", post(mcp::message_handler))
        .with_state(shared)
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind");

    tracing::info!("Notebook ready at http://{addr}");
    tracing::info!("MCP endpoint: http://{addr}/mcp/sse");

    axum::serve(listener, app)
        .await
        .expect("Server error");
}
