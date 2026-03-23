//! # notebook-runtime
//!
//! Reactive cell execution runtime, transcoded from marimo (Python).
//!
//! Cells have dependencies (refs/defs). When a cell's output changes,
//! all downstream cells re-execute. That's a DAG scheduler.

pub mod cell;
pub mod dataflow;
pub mod detect;
pub mod executor;

use uuid::Uuid;

/// Unique identifier for a cell.
pub type CellId = Uuid;

/// Cell execution status.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellStatus {
    /// Cell has run with latest inputs.
    Idle,
    /// Cell is waiting to execute.
    Queued,
    /// Cell is currently executing.
    Running,
    /// Cell is stale (inputs changed, not yet re-run).
    Stale,
    /// Cell is disabled.
    Disabled,
}

/// The output of a cell execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CellOutput {
    /// MIME type of the output (e.g., "text/plain", "text/html", "application/json").
    pub mime_type: String,
    /// Serialized output data.
    pub data: String,
}

/// A notification sent from the runtime to the frontend.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op")]
pub enum Notification {
    /// Cell execution result.
    #[serde(rename = "cell-op")]
    CellOp {
        cell_id: CellId,
        output: Option<CellOutput>,
        console: Vec<CellOutput>,
        status: CellStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        stale_inputs: Option<bool>,
        timestamp: f64,
    },
    /// Variables changed.
    #[serde(rename = "variables")]
    Variables {
        variables: Vec<VariableInfo>,
    },
    /// Kernel is ready.
    #[serde(rename = "kernel-ready")]
    KernelReady {
        cell_ids: Vec<CellId>,
    },
}

/// Information about a variable defined by a cell.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VariableInfo {
    pub name: String,
    pub defined_by: CellId,
    pub used_by: Vec<CellId>,
}
