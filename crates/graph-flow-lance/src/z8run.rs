//! graph-flow graphs as z8run nodes: the n8n role hosting the LangGraph role.
//!
//! A z8run flow builds a plan with z8run-lance's nodes and hands its handle to
//! a `graph-flow` node. The node starts a registered graph with that handle in
//! its `Context`, runs it, and emits what the graph left under the lance key:
//! another handle. Both sides use z8run-lance's registry, so the handle a flow
//! minted resolves inside the graph.
//!
//! When the graph pauses for input, the node persists the session and emits a
//! session handle on its `waiting` port:
//!
//! ```json
//! {"$kind":"graph-flow-session","session":"…","graph":"approve","task":"gate"}
//! ```
//!
//! A later message carrying that handle plus an `input` object, from a webhook
//! or a human-handoff step, resumes the session. Nothing but handles crosses
//! z8run: never rows, and never a session's content.
//!
//! | payload `$kind` | the node |
//! |---|---|
//! | `lance-abi` | starts the configured graph with the handle under the lance key |
//! | `graph-flow-session` | resumes that session after writing `input` into its `Context` |
//! | anything else | refuses it |

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use graph_flow::{ExecutionStatus, Graph, InMemorySessionStorage, Session, SessionStorage};
use serde_json::{json, Value};
use uuid::Uuid;
use z8run_core::engine::{FlowEngine, NodeExecutor, NodeExecutorFactory};
use z8run_core::{FlowMessage, Z8Error, Z8Result};
use z8run_lance::Envelope;

use crate::tasks::{DEFAULT_KEY, MATERIALIZED_KEY};

/// The z8run node type.
pub const NODE_TYPE: &str = "graph-flow";

/// The `$kind` of a session handle.
pub const SESSION_KIND: &str = "graph-flow-session";

/// Steps a single message may run before the node gives up. A graph that
/// pauses with no outgoing edge would otherwise run its last task forever.
pub const DEFAULT_MAX_STEPS: u32 = 1_000;

fn err(m: impl Into<String>) -> Z8Error {
    Z8Error::Internal(m.into())
}

/// The graphs a z8run engine may run, and where paused sessions wait.
pub struct GraphFlowHost {
    graphs: HashMap<String, Arc<Graph>>,
    sessions: Arc<dyn SessionStorage>,
}

impl GraphFlowHost {
    /// A host whose paused sessions go to `sessions`.
    pub fn new(sessions: Arc<dyn SessionStorage>) -> Self {
        Self {
            graphs: HashMap::new(),
            sessions,
        }
    }

    /// A host with in-memory session storage.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(InMemorySessionStorage::new()))
    }

    /// Make `graph` runnable under `id`.
    pub fn with_graph(mut self, id: impl Into<String>, graph: Arc<Graph>) -> Self {
        self.graphs.insert(id.into(), graph);
        self
    }
}

/// One configured `graph-flow` node.
pub struct GraphFlowNode {
    host: Arc<GraphFlowHost>,
    graph_id: String,
    graph: Option<Arc<Graph>>,
    max_steps: u32,
}

impl GraphFlowNode {
    fn graph(&self) -> Z8Result<&Arc<Graph>> {
        self.graph
            .as_ref()
            .ok_or_else(|| err("graph-flow node is not configured"))
    }

    async fn run(&self, msg: &FlowMessage, mut session: Session) -> Z8Result<Vec<FlowMessage>> {
        let graph = self.graph()?;
        for _ in 0..self.max_steps {
            let out = graph
                .execute_session(&mut session)
                .await
                .map_err(|e| err(e.to_string()))?;
            match out.status {
                ExecutionStatus::Paused { .. } => continue,
                ExecutionStatus::WaitingForInput => {
                    let payload = json!({
                        "$kind": SESSION_KIND,
                        "session": session.id,
                        "graph": self.graph_id,
                        "task": session.current_task_id,
                    });
                    self.host
                        .sessions
                        .save(session)
                        .await
                        .map_err(|e| err(e.to_string()))?;
                    return Ok(vec![msg.derive(msg.source_node, "waiting", payload)]);
                }
                ExecutionStatus::Completed => {
                    let handle: Value = session
                        .context
                        .get(DEFAULT_KEY)
                        .await
                        .unwrap_or(Value::Null);
                    let mut m = msg.derive(msg.source_node, "output", handle);
                    if let Some(r) = out.response {
                        m = m.with_metadata("graph-flow.response", Value::String(r));
                    }
                    if let Some(export) = session.context.get::<Value>(MATERIALIZED_KEY).await {
                        m = m.with_metadata("graph-flow.export", export);
                    }
                    self.host
                        .sessions
                        .delete(&session.id)
                        .await
                        .map_err(|e| err(e.to_string()))?;
                    return Ok(vec![m]);
                }
                ExecutionStatus::Error(e) => return Err(err(e)),
            }
        }
        Err(err(format!(
            "graph '{}' did not finish or pause within {} steps",
            self.graph_id, self.max_steps
        )))
    }
}

#[async_trait]
impl NodeExecutor for GraphFlowNode {
    async fn process(&self, msg: FlowMessage) -> Z8Result<Vec<FlowMessage>> {
        let graph = self.graph()?;
        match msg.payload.get("$kind").and_then(Value::as_str) {
            Some("lance-abi") => {
                Envelope::from_json(&msg.payload)?;
                let start = graph
                    .start_task_id()
                    .ok_or_else(|| err(format!("graph '{}' has no start task", self.graph_id)))?;
                let session = Session::new_from_task(Uuid::now_v7().to_string(), &start);
                session.context.set(DEFAULT_KEY, msg.payload.clone()).await;
                self.run(&msg, session).await
            }
            Some(SESSION_KIND) => {
                let id = msg
                    .payload
                    .get("session")
                    .and_then(Value::as_str)
                    .ok_or_else(|| err("a session handle needs 'session'"))?;
                if msg.payload.get("graph").and_then(Value::as_str) != Some(self.graph_id.as_str())
                {
                    return Err(err(format!(
                        "session {id} belongs to another graph than '{}'",
                        self.graph_id
                    )));
                }
                let session = self
                    .host
                    .sessions
                    .get(id)
                    .await
                    .map_err(|e| err(e.to_string()))?
                    .ok_or_else(|| err(format!("no paused session {id}")))?;
                if let Some(input) = msg.payload.get("input").and_then(Value::as_object) {
                    for (k, v) in input {
                        session.context.set(k.clone(), v.clone()).await;
                    }
                }
                self.run(&msg, session).await
            }
            _ => Err(err(
                "graph-flow takes a lance-abi handle or a graph-flow-session handle, never data",
            )),
        }
    }

    async fn configure(&mut self, config: Value) -> Z8Result<()> {
        let id = config
            .get("graph")
            .and_then(Value::as_str)
            .ok_or_else(|| err("graph-flow needs 'graph'"))?;
        let graph = self
            .host
            .graphs
            .get(id)
            .cloned()
            .ok_or_else(|| err(format!("no graph registered as '{id}'")))?;
        self.graph_id = id.to_string();
        self.graph = Some(graph);
        self.max_steps = config
            .get("max_steps")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(DEFAULT_MAX_STEPS);
        Ok(())
    }

    async fn validate(&self) -> Z8Result<()> {
        self.graph().map(|_| ())
    }

    fn node_type(&self) -> &str {
        NODE_TYPE
    }
}

/// Creates `graph-flow` nodes over one host.
pub struct GraphFlowNodeFactory {
    host: Arc<GraphFlowHost>,
}

#[async_trait]
impl NodeExecutorFactory for GraphFlowNodeFactory {
    async fn create(&self, config: Value) -> Z8Result<Box<dyn NodeExecutor>> {
        let mut n = GraphFlowNode {
            host: Arc::clone(&self.host),
            graph_id: String::new(),
            graph: None,
            max_steps: DEFAULT_MAX_STEPS,
        };
        n.configure(config).await?;
        Ok(Box::new(n))
    }

    fn node_type(&self) -> &str {
        NODE_TYPE
    }
}

/// A factory over `host`, for embedders and tests that drive nodes directly.
pub fn factory(host: &Arc<GraphFlowHost>) -> GraphFlowNodeFactory {
    GraphFlowNodeFactory {
        host: Arc::clone(host),
    }
}

/// Register the `graph-flow` node type with a z8run engine.
pub async fn register_graph_flow_node(engine: &FlowEngine, host: Arc<GraphFlowHost>) {
    engine
        .register_node_type(Arc::new(factory(&host)) as Arc<dyn NodeExecutorFactory>)
        .await;
}
