//! # notebook-kernel
//!
//! Jupyter kernel wire protocol client, implemented from the kernel-protocol spec.
//! Only needed for R (IRkernel). Everything else runs in-process.
//!
//! Implements:
//! - ZMQ socket management (shell, iopub, control, heartbeat)
//! - HMAC-SHA256 message signing
//! - execute_request / execute_reply / display_data / stream / status
//! - Connection file parsing
//! - Arrow IPC for DataFrame exchange with R

pub mod protocol;
pub mod connection;
pub mod zmq_client;
pub mod r_bridge;

/// A kernel client that can execute code in an external kernel (e.g., IRkernel).
pub struct KernelClient {
    /// Connection configuration.
    pub connection: connection::ConnectionInfo,
    /// Whether the kernel is connected.
    connected: bool,
}

impl KernelClient {
    /// Create a new kernel client from a connection file path.
    pub fn from_connection_file(path: &str) -> Result<Self, KernelError> {
        let connection = connection::ConnectionInfo::from_file(path)?;
        Ok(Self {
            connection,
            connected: false,
        })
    }

    /// Check if the kernel is alive via heartbeat.
    pub fn is_alive(&self) -> bool {
        self.connected
    }
}

/// Kernel protocol error.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("Connection error: {0}")]
    Connection(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Execution error: {ename}: {evalue}")]
    Execution {
        ename: String,
        evalue: String,
        traceback: Vec<String>,
    },
}
