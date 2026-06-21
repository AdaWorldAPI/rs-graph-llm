//! # template-task — graph-flow Tasks for the Cognitive Compilation loop
//!
//! The existing Rubicon / kanbanview orchestration already exists in this
//! workspace; this crate adds the *one missing wire*: graph-flow [`Task`]s that
//! drive the **Elixir-shaped template** half of the loop (the new idea — see
//! `AdaWorldAPI/lance-graph` crates `elixir-template` / `template-runtime` /
//! `cognitive-compiler`).
//!
//! Two nodes, both Rubicon-`Execution`/`Evaluation` shaped:
//!
//! - [`RunCompiledTemplateTask`] — the **reflex**: if a compiled template
//!   matched the task (a `template_id` is in context), run it deterministically
//!   (no LLM) and continue. Otherwise wait — there is nothing to run until a
//!   match exists.
//! - [`SynthesizeTemplateTask`] — the **escalation/Evaluation** node: when only
//!   a recorded trace exists, synthesis (trace → template) is deferred to the
//!   lance-graph `cognitive-compiler`; this node records that and waits rather
//!   than fabricating a template (§18: no trace → no template, no LLM in the hot
//!   path once a template passes).
//!
//! This is a thin choreography layer: the actual template execution and
//! synthesis live in lance-graph. Kept dependency-light and self-contained so it
//! is a clean cherry-pick after an upstream reset of this repo.

use async_trait::async_trait;
use graph_flow::{Context, NextAction, Result, Task, TaskResult};

/// Context key holding the matched compiled template id (set by the basin match
/// / Rubicon `Commitment` node upstream).
pub const KEY_TEMPLATE_ID: &str = "template_id";
/// Context key holding the input payload the template runs over.
pub const KEY_PAYLOAD: &str = "payload";
/// Context key this task writes its deterministic output to.
pub const KEY_TEMPLATE_OUTPUT: &str = "template_output";
/// Context key holding a recorded execution-trace id (for synthesis).
pub const KEY_TRACE_ID: &str = "trace_id";
/// Context key this task writes the synthesis status to.
pub const KEY_SYNTHESIS_STATUS: &str = "synthesis_status";

/// Run the matched compiled Elixir-shaped template as the deterministic reflex.
///
/// Wiring target: `template_runtime::ReflexExecutor::execute` over the
/// `elixir_template::ElixirTemplate` identified by `template_id`. Until that
/// cross-repo wire lands, this node passes the payload through and continues so
/// the surrounding Rubicon graph is exercisable end-to-end.
pub struct RunCompiledTemplateTask;

#[async_trait]
impl Task for RunCompiledTemplateTask {
    fn id(&self) -> &str {
        "run_compiled_template"
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        let template_id: Option<String> = context.get(KEY_TEMPLATE_ID).await;
        let Some(template_id) = template_id else {
            // No matched template ⇒ nothing to run as a reflex yet.
            return Ok(TaskResult::new(
                Some("no compiled template matched; awaiting a basin match".to_string()),
                NextAction::WaitForInput,
            ));
        };
        let payload: String = context.get(KEY_PAYLOAD).await.unwrap_or_default();
        // Placeholder for `template_runtime::ReflexExecutor::execute(...)`.
        let output = format!("ran template `{template_id}` over {} byte(s) of payload", payload.len());
        context.set(KEY_TEMPLATE_OUTPUT, output.clone()).await;
        Ok(TaskResult::new(Some(output), NextAction::Continue))
    }
}

/// Synthesize a template from a recorded trace (Rubicon `Evaluation`).
///
/// Wiring target: `cognitive_compiler::TraceCompiler::synthesize`. Synthesis is
/// the deferred logic; this node records the intent and waits (escalates) rather
/// than emitting an unvalidated template.
pub struct SynthesizeTemplateTask;

#[async_trait]
impl Task for SynthesizeTemplateTask {
    fn id(&self) -> &str {
        "synthesize_template"
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        let trace_id: Option<String> = context.get(KEY_TRACE_ID).await;
        let (status, msg) = match trace_id {
            Some(t) => (
                "deferred",
                format!("trace `{t}` present; synthesis deferred to lance-graph cognitive-compiler"),
            ),
            None => ("no_trace", "no trace recorded; cannot synthesize (§18: no trace → no template)".to_string()),
        };
        context.set(KEY_SYNTHESIS_STATUS, status.to_string()).await;
        // Either way, hand back to the operator/critic loop rather than guess.
        Ok(TaskResult::new(Some(msg), NextAction::WaitForInput))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reflex_continues_when_template_matched() {
        let ctx = Context::new();
        ctx.set(KEY_TEMPLATE_ID, "source_ranking_v1".to_string()).await;
        ctx.set(KEY_PAYLOAD, "articles".to_string()).await;
        let r = RunCompiledTemplateTask.run(ctx.clone()).await.unwrap();
        assert!(matches!(r.next_action, NextAction::Continue));
        let out: Option<String> = ctx.get(KEY_TEMPLATE_OUTPUT).await;
        assert!(out.unwrap().contains("source_ranking_v1"));
    }

    #[tokio::test]
    async fn reflex_waits_without_a_match() {
        let ctx = Context::new();
        let r = RunCompiledTemplateTask.run(ctx).await.unwrap();
        assert!(matches!(r.next_action, NextAction::WaitForInput));
    }

    #[tokio::test]
    async fn synthesis_defers_with_trace_and_records_status() {
        let ctx = Context::new();
        ctx.set(KEY_TRACE_ID, "trace-1".to_string()).await;
        let r = SynthesizeTemplateTask.run(ctx.clone()).await.unwrap();
        assert!(matches!(r.next_action, NextAction::WaitForInput));
        let status: Option<String> = ctx.get(KEY_SYNTHESIS_STATUS).await;
        assert_eq!(status.as_deref(), Some("deferred"));
    }
}
