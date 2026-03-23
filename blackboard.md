# Blackboard — rs-graph-llm

> Single-binary architecture: already Rust. Integrates as `crate::compiler` (graph workflow orchestration).

## What Exists

Rust-native LangGraph equivalent — graph-based workflow orchestration for multi-agent LLM applications. Level 2 (ORCHESTRATE) in the Ada ecosystem.

## Build Status

**Builds clean** as of 2026-03-23 (1 warning: unused `Movie` struct in recommendation-service).

## Core Architecture

### graph-flow crate (9,594 LOC)

```rust
// Task trait
#[async_trait]
pub trait Task: Send + Sync {
    fn id(&self) -> &str;
    async fn run(&self, context: Context) -> Result<TaskResult>;
}

// Control flow
pub enum NextAction {
    Continue, ContinueAndExecute, WaitForInput, End,
    GoTo(String), GoBack,
}

// Graph construction
let graph = GraphBuilder::new("workflow")
    .add_task(task1)
    .add_conditional_edge(task1.id(), |ctx| check(ctx), task2.id(), task3.id())
    .build();

// Execution
let runner = FlowRunner::new(graph, session_storage);
let result = runner.run(&session_id).await?;
```

### Storage Backends
- In-memory (default)
- PostgreSQL (`storage_postgres.rs`)
- SQLite (`storage_sqlite.rs`)
- Lance with time-travel versioning (`lance_storage.rs`)

### HTTP API (graph-flow-server)
| Endpoint | Method | Purpose |
|---|---|---|
| `/threads` | POST | Create session |
| `/threads/{id}/runs` | POST | Execute step |
| `/threads/{id}/runs/stream` | POST | SSE streaming |
| `/threads/{id}/state` | GET | Get state |

## Integration Points for Binary

- SCOPE A's runtime can delegate workflow cells to graph-flow
- LLM-driven graph queries: user intent → workflow → Cypher query → lance-graph
- Streaming support for real-time cell output

## Workspace Structure

```
rs-graph-llm/
├── graph-flow/              # Core library (9.6K LOC)
├── graph-flow-server/       # HTTP API (405 LOC)
├── insurance-claims-service/ # Example workflow
├── recommendation-service/  # Example RAG pipeline
├── medical-document-service/ # Example doc processing
└── examples/                # Learning examples
```

## Dependencies

tokio 1.40, axum 0.8.4, rig-core 0.19.0, sqlx 0.8.6, lance 2.0, arrow 57

## Key Files

| File | LOC | Purpose |
|---|---|---|
| `graph-flow/src/graph.rs` | 650 | Execution engine |
| `graph-flow/src/task.rs` | 530 | Task trait + NextAction |
| `graph-flow/src/context.rs` | 900 | Thread-safe state |
| `graph-flow/src/lance_storage.rs` | 900 | Time-travel storage |
| `graph-flow/src/streaming.rs` | 500 | Streaming tasks |
| `graph-flow/src/thinking.rs` | 500 | 10-layer cognitive stack |
| `graph-flow-server/src/lib.rs` | 405 | HTTP API |
