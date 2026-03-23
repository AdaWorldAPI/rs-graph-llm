//! Jupyter kernel wire protocol message types.
//!
//! Reference: kernel-protocol/docs/messaging.rst (protocol v5.4)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Wire protocol delimiter.
pub const DELIMITER: &[u8] = b"<IDS|MSG>";

/// Message header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub msg_id: String,
    pub session: String,
    pub username: String,
    pub date: String,
    pub msg_type: String,
    pub version: String,
}

impl Header {
    pub fn new(msg_type: impl Into<String>, session: &str) -> Self {
        Self {
            msg_id: uuid::Uuid::new_v4().to_string(),
            session: session.to_string(),
            username: "notebook".to_string(),
            date: chrono_now(),
            msg_type: msg_type.into(),
            version: "5.4".to_string(),
        }
    }
}

/// A complete Jupyter message (deserialized).
#[derive(Debug, Clone)]
pub struct Message {
    pub header: Header,
    pub parent_header: serde_json::Value,
    pub metadata: serde_json::Value,
    pub content: serde_json::Value,
    pub buffers: Vec<Vec<u8>>,
}

impl Message {
    /// Create a new message with the given type and content.
    pub fn new(msg_type: &str, session: &str, content: serde_json::Value) -> Self {
        Self {
            header: Header::new(msg_type, session),
            parent_header: serde_json::json!({}),
            metadata: serde_json::json!({}),
            content,
            buffers: Vec::new(),
        }
    }
}

// --- Request types ---

/// execute_request content.
#[derive(Debug, Serialize)]
pub struct ExecuteRequest {
    pub code: String,
    pub silent: bool,
    pub store_history: bool,
    pub user_expressions: HashMap<String, String>,
    pub allow_stdin: bool,
    pub stop_on_error: bool,
}

impl ExecuteRequest {
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            silent: false,
            store_history: true,
            user_expressions: HashMap::new(),
            allow_stdin: false,
            stop_on_error: true,
        }
    }
}

// --- Reply types ---

/// execute_reply content (success).
#[derive(Debug, Deserialize)]
pub struct ExecuteReply {
    pub status: String,
    pub execution_count: i64,
}

/// display_data content.
#[derive(Debug, Deserialize)]
pub struct DisplayData {
    pub data: HashMap<String, serde_json::Value>,
    pub metadata: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub transient: HashMap<String, serde_json::Value>,
}

/// stream content (stdout/stderr).
#[derive(Debug, Deserialize)]
pub struct StreamOutput {
    pub name: String,
    pub text: String,
}

/// status content.
#[derive(Debug, Deserialize)]
pub struct StatusChange {
    pub execution_state: String,
}

/// error content.
#[derive(Debug, Deserialize)]
pub struct ErrorOutput {
    pub ename: String,
    pub evalue: String,
    pub traceback: Vec<String>,
}

/// kernel_info_reply content.
#[derive(Debug, Deserialize)]
pub struct KernelInfoReply {
    pub status: String,
    pub protocol_version: String,
    pub implementation: String,
    pub implementation_version: String,
    pub language_info: LanguageInfo,
    #[serde(default)]
    pub banner: String,
}

/// Language info in kernel_info_reply.
#[derive(Debug, Deserialize)]
pub struct LanguageInfo {
    pub name: String,
    pub version: String,
    pub mimetype: String,
    pub file_extension: String,
}

fn chrono_now() -> String {
    // ISO 8601 timestamp
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs())
}
