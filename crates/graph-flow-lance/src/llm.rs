//! The model boundary, where Rig plugs in.
//!
//! A language model sees only what crossed the materialization boundary: the
//! export under [`MATERIALIZED_KEY`]. It never receives a handle, a lane, a
//! mask or a fold state, so nothing it says can be mistaken for a fact the
//! substrate holds.
//!
//! The answer goes into graph-flow's chat history, which `Context::get_rig_messages`
//! already turns into Rig messages for the next turn. It does not become
//! substrate state. Folding model output as evidence before it may become
//! state is the 2026-10-05 working model in lance-graph and is not built.

use std::sync::Arc;

use async_trait::async_trait;
use graph_flow::{Context, GraphError, NextAction, Result, Task, TaskResult};
use serde_json::Value;

use crate::tasks::MATERIALIZED_KEY;

/// Anything that turns a prompt into text. A Rig agent is the intended
/// implementor; tests use a deterministic double.
#[async_trait]
pub trait Completion: Send + Sync {
    /// Complete one prompt.
    async fn complete(&self, prompt: String) -> Result<String>;
}

/// Asks a model about the materialized export, and only about that.
pub struct LlmTask {
    id: String,
    model: Arc<dyn Completion>,
    instruction: String,
    in_key: String,
    next: NextAction,
}

impl LlmTask {
    /// A task that sends `instruction` plus the export to `model`.
    pub fn new(
        id: impl Into<String>,
        model: Arc<dyn Completion>,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            model,
            instruction: instruction.into(),
            in_key: MATERIALIZED_KEY.to_string(),
            next: NextAction::End,
        }
    }

    /// Read the export from `key` instead of [`MATERIALIZED_KEY`].
    pub fn with_in_key(mut self, key: impl Into<String>) -> Self {
        self.in_key = key.into();
        self
    }

    /// Return `next` instead of `NextAction::End`.
    pub fn with_next(mut self, next: NextAction) -> Self {
        self.next = next;
        self
    }

    /// Finish building.
    pub fn into_task(self) -> Arc<dyn Task> {
        Arc::new(self)
    }
}

#[async_trait]
impl Task for LlmTask {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        let export: Value = context.get(&self.in_key).await.ok_or_else(|| {
            GraphError::ContextError(format!(
                "nothing materialized under '{}'; place a materialize task before the model",
                self.in_key
            ))
        })?;
        let prompt = format!("{}\n\n{}", self.instruction, export);
        let answer = self.model.complete(prompt.clone()).await?;
        context.add_user_message(prompt).await;
        context.add_assistant_message(answer.clone()).await;
        Ok(TaskResult::new(Some(answer), self.next.clone()))
    }
}
