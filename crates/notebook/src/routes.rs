//! HTTP route handlers for the notebook binary.

use axum::Json;
use serde::{Deserialize, Serialize};

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

/// Execute a cell.
pub async fn execute(Json(req): Json<ExecuteRequest>) -> Json<ExecuteResponse> {
    // TODO: wire to notebook-runtime
    Json(ExecuteResponse {
        cell_id: req.cell_id,
        status: "ok".to_string(),
        output: None,
    })
}

#[derive(Deserialize)]
pub struct ExecuteRequest {
    pub cell_id: String,
    pub code: String,
    pub language: String,
}

#[derive(Serialize)]
pub struct ExecuteResponse {
    pub cell_id: String,
    pub status: String,
    pub output: Option<serde_json::Value>,
}

/// Kernel status endpoint.
pub async fn status() -> Json<StatusResponse> {
    Json(StatusResponse {
        kernels: vec![
            KernelInfo { language: "python".into(), status: "available".into() },
            KernelInfo { language: "cypher".into(), status: "available".into() },
            KernelInfo { language: "sparql".into(), status: "available".into() },
            KernelInfo { language: "gremlin".into(), status: "available".into() },
            KernelInfo { language: "r".into(), status: "requires_irkernel".into() },
        ],
    })
}

#[derive(Serialize)]
pub struct StatusResponse {
    kernels: Vec<KernelInfo>,
}

#[derive(Serialize)]
pub struct KernelInfo {
    language: String,
    status: String,
}
