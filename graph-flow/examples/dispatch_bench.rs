//! dispatch_bench — measures graph-flow per-task-step dispatch cost.
//!
//! Added as static/inventory artifact for the lance-graph W2e/W3b evaluation
//! (graph-flow as replayable kanban-integrated langgraph handler). Runs
//! standalone against InMemorySessionStorage — no network, no external deps
//! beyond the graph-flow default feature set.
//!
//! NOTE ON RUNNING THIS FILE IN THIS WORKSPACE: `rs-graph-llm` is a Cargo
//! workspace, and `cargo run --example dispatch_bench -p graph-flow` from
//! this checkout resolves a *workspace-wide* Cargo.lock. Other members
//! (`crates/episodic-arc-task` -> `lance-graph-contract` git dep;
//! `graph-flow`'s own optional `surreal-lance` feature -> `AdaWorldAPI/surrealdb`
//! -> `AdaWorldAPI/ndarray` -> a `burn` git submodule) pull git sources during
//! lockfile generation regardless of whether those features are active for
//! THIS binary. In a network-sandboxed environment those git fetches 403 and
//! the whole-workspace build fails before this example is ever reached. The
//! numbers quoted in the accompanying report were produced by copying this
//! exact file into an isolated single-package crate (`[workspace]` header +
//! a `graph-flow = { path = "..." }` dep, default features only) so cargo
//! never needs to touch the git-sourced optional deps. Once this workspace
//! has network access (or a checked-in Cargo.lock), `cargo run --release
//! --example dispatch_bench -p graph-flow` from the repo root reproduces the
//! same numbers directly.
//!
//! Two execution paths through a linear 4-task no-op graph:
//!   (a) "continue_and_execute" — a single `FlowRunner::run()` call drives the
//!       whole 4-task chain to completion in one in-process recursion
//!       (`Graph::execute_session` recursing via `NextAction::ContinueAndExecute`).
//!       Storage cost: exactly 1x SessionStorage::get + 1x SessionStorage::save
//!       per full chain, regardless of chain length.
//!   (b) "continue_stepwise" — every task returns `NextAction::Continue`, so the
//!       caller must call `FlowRunner::run()` once PER task. Storage cost:
//!       1x get + 1x save PER STEP (4x per chain for a 4-task graph).
//!
//! This isolates the storage-roundtrip overhead FlowRunner::run() pays per call,
//! which is exactly the overhead a kanban-board-backed SessionStorage impl would
//! also pay on every step in stepwise mode.

use async_trait::async_trait;
use graph_flow::{
    Context, ExecutionStatus, FlowRunner, GraphBuilder, InMemorySessionStorage, NextAction,
    Session, SessionStorage, Task, TaskResult,
};
use std::sync::Arc;
use std::time::Instant;

/// A task that does nothing but report the configured NextAction.
/// No context access — isolates Graph/FlowRunner dispatch cost from
/// Context (DashMap + serde_json) cost.
struct NoOpTask {
    id: String,
    next: NextAction,
}

#[async_trait]
impl Task for NoOpTask {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, _context: Context) -> graph_flow::Result<TaskResult> {
        Ok(TaskResult::new(None, self.next.clone()))
    }
}

fn build_chain(prefix: &str, step_action: NextAction) -> (Arc<graph_flow::Graph>, String) {
    let ids: Vec<String> = (0..4).map(|i| format!("{prefix}_t{i}")).collect();
    let tasks: Vec<Arc<NoOpTask>> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let next = if i == 3 {
                NextAction::End
            } else {
                step_action.clone()
            };
            Arc::new(NoOpTask {
                id: id.clone(),
                next,
            })
        })
        .collect();

    let mut builder = GraphBuilder::new(format!("{prefix}_graph"));
    for t in &tasks {
        builder = builder.add_task(t.clone());
    }
    for w in ids.windows(2) {
        builder = builder.add_edge(&w[0], &w[1]);
    }
    let graph = Arc::new(builder.build());
    (graph, ids[0].clone())
}

/// Run the "continue_and_execute" chain once via a single FlowRunner::run() call.
/// Returns elapsed wall time for the whole 4-task chain.
async fn run_continue_and_execute_once(
    runner: &FlowRunner,
    storage: &InMemorySessionStorage,
    session_id: &str,
    start_task: &str,
) -> std::time::Duration {
    let session = Session::new_from_task(session_id.to_string(), start_task);
    storage.save(session).await.unwrap();

    let t0 = Instant::now();
    let result = runner.run(session_id).await.unwrap();
    let elapsed = t0.elapsed();
    assert!(matches!(result.status, ExecutionStatus::Completed));
    elapsed
}

/// Run the "continue_stepwise" chain: call FlowRunner::run() once per task,
/// each call doing its own storage get+save. Returns elapsed wall time for
/// the whole 4-task chain (sum of the 4 per-step runner.run() calls).
async fn run_continue_stepwise_once(
    runner: &FlowRunner,
    storage: &InMemorySessionStorage,
    session_id: &str,
    start_task: &str,
) -> std::time::Duration {
    let session = Session::new_from_task(session_id.to_string(), start_task);
    storage.save(session).await.unwrap();

    let t0 = Instant::now();
    loop {
        let result = runner.run(session_id).await.unwrap();
        match result.status {
            ExecutionStatus::Completed => break,
            ExecutionStatus::Paused { .. } => continue,
            other => panic!("unexpected status: {other:?}"),
        }
    }
    t0.elapsed()
}

const TASKS_PER_CHAIN: u64 = 4;

async fn bench_variant<F, Fut>(
    label: &str,
    batch_sizes: &[u64],
    warmup_iters: u64,
    mut run_once: F,
) where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = std::time::Duration>,
{
    println!("\n=== {label} ===");

    // Warm up: run a handful of iterations untimed first (allocator, DashMap
    // shard init, tokio task-local caches, etc. settle here — NOT included
    // in any reported number below).
    for i in 0..warmup_iters {
        run_once(u64::MAX - i).await;
    }

    println!(
        "{:>10} | {:>14} | {:>16} | {:>18}",
        "batch", "total_ns", "ns_per_chain", "ns_per_task_step"
    );
    for &batch in batch_sizes {
        let mut total = std::time::Duration::ZERO;
        for i in 0..batch {
            total += run_once(i).await;
        }
        let total_ns = total.as_nanos();
        let ns_per_chain = total_ns / batch as u128;
        let ns_per_step = total_ns / (batch as u128 * TASKS_PER_CHAIN as u128);
        println!(
            "{:>10} | {:>14} | {:>16} | {:>18}",
            batch, total_ns, ns_per_chain, ns_per_step
        );
    }
}

#[tokio::main]
async fn main() {
    let batch_sizes: [u64; 3] = [1, 64, 4096];

    // --- (a) continue_and_execute: 1 storage get + 1 storage save per whole chain ---
    {
        let (graph, start) = build_chain("cae", NextAction::ContinueAndExecute);
        let storage = Arc::new(InMemorySessionStorage::new());
        let runner = FlowRunner::new(graph, storage.clone());
        let start = start.clone();
        bench_variant(
            "continue_and_execute (1 get+save per 4-task chain)",
            &batch_sizes,
            200,
            |i| {
                let runner = runner.clone();
                let storage = storage.clone();
                let start = start.clone();
                async move {
                    let sid = format!("cae_session_{i}");
                    run_continue_and_execute_once(&runner, &storage, &sid, &start).await
                }
            },
        )
        .await;
    }

    // --- (b) continue_stepwise: 1 storage get + 1 storage save PER TASK STEP ---
    {
        let (graph, start) = build_chain("stw", NextAction::Continue);
        let storage = Arc::new(InMemorySessionStorage::new());
        let runner = FlowRunner::new(graph, storage.clone());
        let start = start.clone();
        bench_variant(
            "continue_stepwise (1 get+save per task step, 4x per chain)",
            &batch_sizes,
            200,
            |i| {
                let runner = runner.clone();
                let storage = storage.clone();
                let start = start.clone();
                async move {
                    let sid = format!("stw_session_{i}");
                    run_continue_stepwise_once(&runner, &storage, &sid, &start).await
                }
            },
        )
        .await;
    }

    println!(
        "\nNote: single tokio runtime for the whole process (#[tokio::main]) — \
         runtime bootstrap happens BEFORE any Instant::now() call above, so it \
         is NOT included in the batch=1 numbers. batch=1 numbers do still carry \
         first-call effects (cold DashMap shard allocation inside a *fresh* \
         InMemorySessionStorage would show here, but warmup above reuses the \
         same storage instance, so that's already amortized)."
    );
}
