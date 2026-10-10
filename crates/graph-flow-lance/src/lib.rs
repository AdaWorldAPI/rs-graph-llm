//! # graph-flow-lance: graph-flow over the lance-graph fold algebra
//!
//! Three roles, one engine underneath:
//!
//! | role | here | owns |
//! |---|---|---|
//! | n8n | z8run | flows, triggers, external edges, human handoff |
//! | LangGraph | rs-graph-llm `graph-flow` | stateful graphs, sessions, pauses |
//! | LangChain | Rig, behind [`llm::Completion`] | models, tools, retrieval |
//! | the engine | lance-graph-report, Quack, mask-risc | DuckDB-shaped plans, mask and fold execution |
//!
//! The seam between orchestration and the engine carries one thing, a handle:
//!
//! ```json
//! {"$kind":"lance-abi","role":"plan","handle":17,"generation":4,"source":9}
//! ```
//!
//! The seam is `z8run-lance`'s: its registry, its envelope and its export.
//! This crate does not keep a second copy, so a handle a z8run flow mints
//! resolves inside a graph-flow graph and the other way round, and a
//! [`z8run`] node can run a graph-flow graph inside a z8run flow.
//!
//! No task here puts a row, a lane, a mask or a fold state into a `Context`.
//! A plan task rewrites an immutable plan and stores a new handle; `execute`
//! folds inside lance-graph and stores a result handle; only `materialize`,
//! the explicit terminal, writes data. A saved session is therefore the same
//! size whatever the population, and a resumed session cannot answer from
//! data that changed underneath it: handles fail closed on a newer source
//! generation.
//!
//! ## LangGraph, mask- and fold-shaped
//!
//! | LangGraph | here |
//! |---|---|
//! | `StateGraph` nodes and edges | graph-flow `Task`s and edges |
//! | state channel with a reducer | a measure: fold states with an identity and an associative, commutative merge |
//! | `Send` fan-out, then reduce | an axis over a [`lance_graph_report::CoordSpec::MaskSet`]: one member per branch, one fold for all branches, the reduce is the merge along that axis |
//! | checkpointer | graph-flow `SessionStorage` holding handles, checked against the source generation on resume |
//! | `interrupt` | `NextAction::WaitForInput`; inside z8run, a session handle on the node's `waiting` port |
//! | node that calls a model | [`llm::LlmTask`]: the model sees the materialized export only |
//!
//! ## What is not here
//!
//! Suspension as bytes, readiness scheduled by loco instead of `GoTo`, and
//! folding model output as evidence before it becomes state are lance-graph's
//! 2026-10-05 working model and OGAR's loco gap list. None of them is built.

#![forbid(unsafe_code)]

pub mod llm;
pub mod tasks;
pub mod z8run;

pub use llm::{Completion, LlmTask};
pub use tasks::{LanceOp, LanceTask, DEFAULT_KEY, MATERIALIZED_KEY};
pub use z8run::{register_graph_flow_node, GraphFlowHost, NODE_TYPE, SESSION_KIND};
pub use z8run_lance::nodes::grid_value;
pub use z8run_lance::{Envelope, LanceRegistry, RegistryStats, Role};
