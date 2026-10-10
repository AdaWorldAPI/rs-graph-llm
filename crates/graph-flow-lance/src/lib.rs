//! # graph-flow-lance: graph-flow over the lance-graph fold algebra
//!
//! graph-flow owns orchestration: the graph, its edges, sessions, pauses and
//! the human and model boundaries. lance-graph owns everything physical:
//! lanes, masks, folds and their merge laws. This crate is the seam, and the
//! seam carries one thing, a handle:
//!
//! ```json
//! {"$kind":"lance-abi","role":"plan","handle":17,"generation":4,"source":9}
//! ```
//!
//! No task here puts a row, a lane, a mask or a fold state into a `Context`.
//! A plan task rewrites an immutable plan and stores a new handle; `execute`
//! folds inside lance-graph and stores a result handle; only `materialize`,
//! the explicit terminal, writes data. A saved session is therefore the same
//! size whatever the population, and a resumed session cannot answer from
//! data that changed underneath it: handles fail closed on a newer source
//! generation.
//!
//! The seam is the one `z8run-lance` carries for z8run flows, over the same
//! `lance-graph-report` plans, so graph-flow, z8run and Quack callers share
//! one semantic representation.
//!
//! ## LangGraph, mask- and fold-shaped
//!
//! | LangGraph | here |
//! |---|---|
//! | `StateGraph` nodes and edges | graph-flow `Task`s and edges |
//! | state channel with a reducer | a measure: fold states with an identity and an associative, commutative merge |
//! | `Send` fan-out, then reduce | an axis over a [`lance_graph_report::CoordSpec::MaskSet`]: one member per branch, one fold for all branches, the reduce is the merge along that axis |
//! | checkpointer | graph-flow `SessionStorage` holding handles, checked against the source generation on resume |
//! | `interrupt` | `NextAction::WaitForInput` |
//! | node that calls a model | [`llm::LlmTask`]: the model sees the materialized export only |
//!
//! LangChain's role, models, tools and retrieval, is Rig's. Rig plugs in
//! behind [`llm::Completion`].
//!
//! ## What is not here
//!
//! Suspension as bytes, readiness scheduled by loco instead of `GoTo`, and
//! folding model output as evidence before it becomes state are lance-graph's
//! 2026-10-05 working model and OGAR's loco gap list. None of them is built.

#![forbid(unsafe_code)]

pub mod llm;
pub mod registry;
pub mod tasks;

pub use llm::{Completion, LlmTask};
pub use registry::{Envelope, LanceRegistry, RegistryStats, Role};
pub use tasks::{grid_value, LanceOp, LanceTask, DEFAULT_KEY, MATERIALIZED_KEY};
