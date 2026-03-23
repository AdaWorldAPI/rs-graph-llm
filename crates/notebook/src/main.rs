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
//! The binary:
//! 1. Serves marimo's JS/React frontend as static files
//! 2. Handles WebSocket connections for cell execution
//! 3. Dispatches cell code to the appropriate engine
//! 4. Publishes notebooks to PDF/HTML

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;

mod routes;

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "notebook=info".into()),
        )
        .init();

    let addr = std::env::var("NOTEBOOK_ADDR").unwrap_or_else(|_| "0.0.0.0:2718".to_string());

    tracing::info!("Starting polyglot notebook on {addr}");

    let app = Router::new()
        .route("/api/health", get(routes::health))
        .route("/api/kernel/execute", post(routes::execute))
        .route("/api/kernel/status", get(routes::status))
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind");

    tracing::info!("Notebook ready at http://{addr}");

    axum::serve(listener, app)
        .await
        .expect("Server error");
}
