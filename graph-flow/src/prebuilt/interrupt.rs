//! Structured human-in-the-loop interrupt types.
//!
//! Maps to LangGraph's `langgraph.prebuilt.interrupt` module.
//!
//! # Examples
//!
//! ```rust
//! use graph_flow::prebuilt::interrupt::{HumanInterrupt, HumanResponse, ActionRequest};
//!
//! let interrupt = HumanInterrupt::new("approve_claim")
//!     .with_description("Please review this insurance claim")
//!     .with_action(ActionRequest::new("approve", "Approve the claim"))
//!     .with_action(ActionRequest::new("reject", "Reject the claim"));
//!
//! let response = HumanResponse::action("approve", serde_json::json!({"reason": "valid"}));
//! assert_eq!(response.action_name(), Some("approve"));
//! ```

use serde::{Deserialize, Serialize};

/// An action that a human can take in response to an interrupt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRequest {
    /// Unique name for this action.
    pub action: String,
    /// Human-readable description.
    pub description: String,
    /// Optional arguments schema or default values.
    pub args: Option<serde_json::Value>,
}

impl ActionRequest {
    pub fn new(action: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            description: description.into(),
            args: None,
        }
    }

    pub fn with_args(mut self, args: serde_json::Value) -> Self {
        self.args = Some(args);
        self
    }
}

/// Configuration for a human interrupt point.
///
/// Describes what the human needs to do and what actions are available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanInterrupt {
    /// Unique identifier for this interrupt point.
    pub id: String,
    /// Human-readable description of what's needed.
    pub description: Option<String>,
    /// Available actions the human can take.
    pub actions: Vec<ActionRequest>,
    /// Whether free-text response is allowed.
    pub allow_text_response: bool,
    /// Context data to show the human.
    pub context: Option<serde_json::Value>,
}

impl HumanInterrupt {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: None,
            actions: Vec::new(),
            allow_text_response: true,
            context: None,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn with_action(mut self, action: ActionRequest) -> Self {
        self.actions.push(action);
        self
    }

    pub fn with_context(mut self, ctx: serde_json::Value) -> Self {
        self.context = Some(ctx);
        self
    }

    pub fn no_text_response(mut self) -> Self {
        self.allow_text_response = false;
        self
    }
}

/// A human's response to an interrupt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HumanResponse {
    /// Selected one of the provided actions.
    Action {
        action: String,
        args: Option<serde_json::Value>,
    },
    /// Provided free-text input.
    Text(String),
    /// Cancelled / dismissed the interrupt.
    Cancel,
}

impl HumanResponse {
    pub fn action(name: impl Into<String>, args: serde_json::Value) -> Self {
        Self::Action {
            action: name.into(),
            args: Some(args),
        }
    }

    pub fn text(input: impl Into<String>) -> Self {
        Self::Text(input.into())
    }

    pub fn action_name(&self) -> Option<&str> {
        match self {
            Self::Action { action, .. } => Some(action),
            _ => None,
        }
    }

    pub fn is_cancel(&self) -> bool {
        matches!(self, Self::Cancel)
    }
}

/// A node-level interrupt that can be raised during task execution.
///
/// Maps to LangGraph's `NodeInterrupt`. Store in context to signal
/// that a specific task needs human input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInterrupt {
    /// The interrupt configuration.
    pub interrupt: HumanInterrupt,
    /// The task that raised the interrupt.
    pub task_id: String,
}

impl NodeInterrupt {
    pub fn new(task_id: impl Into<String>, interrupt: HumanInterrupt) -> Self {
        Self {
            task_id: task_id.into(),
            interrupt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_interrupt_builder() {
        let interrupt = HumanInterrupt::new("approve")
            .with_description("Approve this?")
            .with_action(ActionRequest::new("yes", "Approve"))
            .with_action(ActionRequest::new("no", "Reject"))
            .with_context(serde_json::json!({"amount": 1000}))
            .no_text_response();

        assert_eq!(interrupt.id, "approve");
        assert_eq!(interrupt.actions.len(), 2);
        assert!(!interrupt.allow_text_response);
        assert!(interrupt.context.is_some());
    }

    #[test]
    fn test_human_response() {
        let resp = HumanResponse::action("approve", serde_json::json!({"note": "ok"}));
        assert_eq!(resp.action_name(), Some("approve"));
        assert!(!resp.is_cancel());

        let resp = HumanResponse::text("I approve this");
        assert_eq!(resp.action_name(), None);

        let resp = HumanResponse::Cancel;
        assert!(resp.is_cancel());
    }

    #[test]
    fn test_node_interrupt() {
        let ni = NodeInterrupt::new(
            "review_task",
            HumanInterrupt::new("review").with_description("Review needed"),
        );
        assert_eq!(ni.task_id, "review_task");
        assert_eq!(ni.interrupt.id, "review");
    }

    #[test]
    fn test_action_request_with_args() {
        let action = ActionRequest::new("submit", "Submit form")
            .with_args(serde_json::json!({"required": ["name", "email"]}));
        assert!(action.args.is_some());
    }
}
