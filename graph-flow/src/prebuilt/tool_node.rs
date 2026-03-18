//! Full-featured ToolNode with request interception and state injection.
//!
//! Maps to LangGraph's `langgraph.prebuilt.tool_node` module.
//!
//! # Examples
//!
//! ```rust
//! use graph_flow::prebuilt::tool_node::{ToolNode, ToolCallRequest};
//! use graph_flow::{Task, TaskResult, NextAction, Context};
//! use async_trait::async_trait;
//! use std::sync::Arc;
//!
//! struct Calculator;
//!
//! #[async_trait]
//! impl Task for Calculator {
//!     fn id(&self) -> &str { "calculator" }
//!     async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
//!         Ok(TaskResult::new(Some("42".to_string()), NextAction::Continue))
//!     }
//! }
//!
//! let node = ToolNode::new(vec![Arc::new(Calculator) as Arc<dyn Task>]);
//! assert!(node.has_tool("calculator"));
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    context::Context,
    error::{GraphError, Result},
    task::{NextAction, Task, TaskResult},
};

/// A request to call a tool, with optional overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// Name of the tool to invoke.
    pub tool_name: String,
    /// Arguments to pass.
    pub args: serde_json::Value,
    /// Optional request ID for tracking.
    pub id: Option<String>,
}

impl ToolCallRequest {
    pub fn new(tool_name: impl Into<String>, args: serde_json::Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            args,
            id: None,
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// Type alias for tool interceptor functions.
///
/// An interceptor receives a `ToolCallRequest` and can modify it,
/// skip it, or let it proceed.
pub type ToolInterceptor =
    Arc<dyn Fn(&ToolCallRequest, &Context) -> InterceptorAction + Send + Sync>;

/// What an interceptor decides to do with a tool call.
pub enum InterceptorAction {
    /// Let the call proceed (possibly modified).
    Proceed(ToolCallRequest),
    /// Skip this call entirely, using this value as the result.
    Skip(serde_json::Value),
    /// Block this call with an error.
    Block(String),
}

/// A routing node that dispatches tool calls to registered tools.
///
/// Maps to LangGraph's `ToolNode`. Reads `tool_calls` from context,
/// executes the appropriate tools, and writes results back.
pub struct ToolNode {
    tools: HashMap<String, Arc<dyn Task>>,
    interceptors: Vec<ToolInterceptor>,
    handle_errors: bool,
}

impl ToolNode {
    /// Create a ToolNode from a list of tool tasks.
    pub fn new(tools: Vec<Arc<dyn Task>>) -> Self {
        let map = tools
            .into_iter()
            .map(|t| (t.id().to_string(), t))
            .collect();
        Self {
            tools: map,
            interceptors: Vec::new(),
            handle_errors: true,
        }
    }

    /// Add an interceptor that runs before each tool call.
    pub fn with_interceptor(mut self, interceptor: ToolInterceptor) -> Self {
        self.interceptors.push(interceptor);
        self
    }

    /// Set whether to catch tool errors or propagate them.
    pub fn handle_errors(mut self, handle: bool) -> Self {
        self.handle_errors = handle;
        self
    }

    /// Check if a tool is registered.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Get tool names.
    pub fn tool_names(&self) -> Vec<&String> {
        self.tools.keys().collect()
    }
}

#[async_trait]
impl Task for ToolNode {
    fn id(&self) -> &str {
        "tool_node"
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        // Read tool calls from context
        let calls: Vec<ToolCallRequest> = context
            .get("tool_calls")
            .await
            .unwrap_or_default();

        if calls.is_empty() {
            return Ok(TaskResult::new(
                Some("No tool calls to execute".to_string()),
                NextAction::Continue,
            ));
        }

        let mut results = Vec::new();

        for call in calls {
            // Run interceptors
            let mut current_call = call;
            let mut skipped = false;

            for interceptor in &self.interceptors {
                match interceptor(&current_call, &context) {
                    InterceptorAction::Proceed(modified) => {
                        current_call = modified;
                    }
                    InterceptorAction::Skip(value) => {
                        results.push(serde_json::json!({
                            "tool": current_call.tool_name,
                            "result": value,
                            "skipped": true,
                        }));
                        skipped = true;
                        break;
                    }
                    InterceptorAction::Block(reason) => {
                        if self.handle_errors {
                            results.push(serde_json::json!({
                                "tool": current_call.tool_name,
                                "error": reason,
                                "blocked": true,
                            }));
                            skipped = true;
                            break;
                        } else {
                            return Err(GraphError::TaskExecutionFailed(format!(
                                "Tool call blocked: {}",
                                reason
                            )));
                        }
                    }
                }
            }

            if skipped {
                continue;
            }

            // Execute the tool
            let tool_name = &current_call.tool_name;
            if let Some(tool) = self.tools.get(tool_name) {
                // Set tool args in context for the tool to read
                context.set("tool_args", current_call.args.clone()).await;

                match tool.run(context.clone()).await {
                    Ok(result) => {
                        results.push(serde_json::json!({
                            "tool": tool_name,
                            "result": result.response,
                            "id": current_call.id,
                        }));
                    }
                    Err(e) => {
                        if self.handle_errors {
                            results.push(serde_json::json!({
                                "tool": tool_name,
                                "error": e.to_string(),
                                "id": current_call.id,
                            }));
                        } else {
                            return Err(e);
                        }
                    }
                }
            } else {
                let msg = format!("Tool '{}' not found", tool_name);
                if self.handle_errors {
                    results.push(serde_json::json!({
                        "tool": tool_name,
                        "error": msg,
                    }));
                } else {
                    return Err(GraphError::TaskNotFound(msg));
                }
            }
        }

        context.set("tool_results", results.clone()).await;

        let summary = results
            .iter()
            .map(|r| {
                r.get("tool")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown")
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(", ");

        Ok(TaskResult::new(
            Some(format!("Executed tools: {}", summary)),
            NextAction::Continue,
        ))
    }
}

/// Condition function for routing: returns true if there are pending tool calls.
///
/// Maps to LangGraph's `tools_condition()`.
pub fn tools_condition(ctx: &Context) -> bool {
    let calls: Option<Vec<ToolCallRequest>> = ctx.get_sync("tool_calls");
    calls.is_some_and(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;

    #[async_trait]
    impl Task for EchoTool {
        fn id(&self) -> &str {
            "echo"
        }
        async fn run(&self, ctx: Context) -> Result<TaskResult> {
            let args: serde_json::Value = ctx.get("tool_args").await.unwrap_or_default();
            Ok(TaskResult::new(
                Some(format!("Echo: {}", args)),
                NextAction::Continue,
            ))
        }
    }

    #[test]
    fn test_tool_node_creation() {
        let node = ToolNode::new(vec![Arc::new(EchoTool) as Arc<dyn Task>]);
        assert!(node.has_tool("echo"));
        assert!(!node.has_tool("missing"));
    }

    #[tokio::test]
    async fn test_tool_node_execution() {
        let node = ToolNode::new(vec![Arc::new(EchoTool) as Arc<dyn Task>]);
        let ctx = Context::new();
        ctx.set(
            "tool_calls",
            vec![ToolCallRequest::new("echo", serde_json::json!({"msg": "hi"}))],
        )
        .await;

        let result = node.run(ctx.clone()).await.unwrap();
        assert!(result.response.unwrap().contains("echo"));

        let results: Vec<serde_json::Value> = ctx.get("tool_results").await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_tool_node_missing_tool() {
        let node = ToolNode::new(vec![]);
        let ctx = Context::new();
        ctx.set(
            "tool_calls",
            vec![ToolCallRequest::new("missing", serde_json::json!({}))],
        )
        .await;

        let _result = node.run(ctx.clone()).await.unwrap();
        let results: Vec<serde_json::Value> = ctx.get("tool_results").await.unwrap();
        assert!(results[0].get("error").is_some());
    }

    #[tokio::test]
    async fn test_tool_node_with_interceptor() {
        let node = ToolNode::new(vec![Arc::new(EchoTool) as Arc<dyn Task>]).with_interceptor(
            Arc::new(|req: &ToolCallRequest, _ctx: &Context| {
                if req.tool_name == "echo" {
                    InterceptorAction::Skip(serde_json::json!("intercepted"))
                } else {
                    InterceptorAction::Proceed(req.clone())
                }
            }),
        );

        let ctx = Context::new();
        ctx.set(
            "tool_calls",
            vec![ToolCallRequest::new("echo", serde_json::json!({}))],
        )
        .await;

        node.run(ctx.clone()).await.unwrap();
        let results: Vec<serde_json::Value> = ctx.get("tool_results").await.unwrap();
        assert_eq!(results[0]["skipped"], true);
    }

    #[test]
    fn test_tools_condition() {
        let ctx = Context::new();
        assert!(!tools_condition(&ctx));

        ctx.set_sync(
            "tool_calls",
            vec![ToolCallRequest::new("t", serde_json::json!({}))],
        );
        assert!(tools_condition(&ctx));
    }
}
