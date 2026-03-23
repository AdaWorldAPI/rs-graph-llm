//! Non-MCP HTTP routes.
//!
//! Only `/health` is served outside MCP (for probes / load-balancers).

use axum::Json;
use serde::Serialize;

/// Health check endpoint.
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

#[derive(Serialize)]
pub struct HealthResponse {
    status: String,
    version: String,
}
