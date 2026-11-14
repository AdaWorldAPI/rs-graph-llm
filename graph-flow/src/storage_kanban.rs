//! Session persistence plus a session-local Rubicon move log.
//!
//! Two things live here and they are kept apart:
//!
//! - **Orchestration.** [`SessionStorage::save`] and
//!   [`KanbanSessionStorage::save_with_status`] persist a [`Session`]
//!   snapshot. An [`ExecutionStatus`] says what a graph-flow run did. It is
//!   remembered as the last status and nothing else: no save, with or without
//!   a status, moves a column or records a [`KanbanMove`].
//! - **Lifecycle.** A column moves only through
//!   [`KanbanSessionStorage::apply_gate`] and
//!   [`KanbanSessionStorage::apply_revision`]. Both take the evidence value
//!   the contract routes on (a MUL [`GateDecision`], a [`RevisionVerdict`])
//!   and delegate the choice of successor to
//!   [`KanbanColumn::advance_on_gate`] and
//!   [`KanbanColumn::advance_on_revision`]. Both also take the
//!   [`MoveCursor`] the approval was issued against and refuse when the log
//!   has moved since, so an approval applies at most once.
//!
//! The log is local to one session's [`KanbanPlanEnvelope`]. The canonical
//! mailbox lifecycle is owned by `MailboxSoaOwner::try_advance_phase` in
//! `lance-graph-contract`, driven by the cycle driver of
//! `lance-graph-supervisor`. A move in this log is an audit record of what
//! the given evidence routed to. It is not a permit: nothing here checks
//! action definition, RBAC or state guard (that is
//! `ActionInvocation::commit_via`, used by
//! [`graph_flow_kanban::orchestrate::run_cycle`]), and the gate and verdict
//! values are supplied by the caller, not certified by this module.
//!
//! Behind the `kanban` feature.

use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::RwLock;

use graph_flow_kanban::KanbanPlanEnvelope;
use lance_graph_contract::collapse_gate::MailboxId;
use lance_graph_contract::kanban::{ExecTarget, KanbanColumn, KanbanMove};
use lance_graph_contract::mul::GateDecision;
use lance_graph_contract::revision::RevisionVerdict;

use crate::{error::Result, graph::ExecutionStatus, storage::SessionStorage, Session};

/// Where a session's move log stood when an approval was issued: the column
/// and the number of moves recorded so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveCursor {
    /// Column the session occupied.
    pub column: KanbanColumn,
    /// Number of moves in the log.
    pub position: u32,
}

/// Why a lifecycle call recorded nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleRefusal {
    /// No session with that id has been saved.
    UnknownSession,
    /// The approval was issued against an earlier state of the log.
    StaleApproval {
        /// The cursor the caller presented.
        presented: MoveCursor,
        /// The log's cursor now.
        current: MoveCursor,
    },
    /// A forward gate was presented at `Evaluation`. Leaving `Evaluation`
    /// forward is the Rubicon exit and takes a [`RevisionVerdict`].
    RevisionRequired,
    /// A revision verdict was presented outside `Evaluation`.
    NotAtEvaluation(KanbanColumn),
}

struct KanbanSessionRecord {
    snapshot: Session,
    last_status: Option<ExecutionStatus>,
    envelope: KanbanPlanEnvelope,
}

impl KanbanSessionRecord {
    fn cursor(&self) -> MoveCursor {
        MoveCursor {
            column: self.envelope.column,
            position: self.envelope.moves.len() as u32,
        }
    }

    fn check(&self, presented: MoveCursor) -> std::result::Result<(), LifecycleRefusal> {
        let current = self.cursor();
        if presented == current {
            Ok(())
        } else {
            Err(LifecycleRefusal::StaleApproval { presented, current })
        }
    }
}

/// A [`SessionStorage`] that also keeps one [`KanbanPlanEnvelope`] per
/// session, all attributed to a single `mailbox`.
///
/// The `mailbox` belongs to the storage, never to the [`Session`].
pub struct KanbanSessionStorage {
    mailbox: MailboxId,
    inner: RwLock<HashMap<String, KanbanSessionRecord>>,
}

impl KanbanSessionStorage {
    /// A storage whose recorded moves are attributed to `mailbox`.
    #[must_use]
    pub fn new(mailbox: MailboxId) -> Self {
        Self {
            mailbox,
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// The move log of `session_id`. Empty for an unknown session and for a
    /// session no lifecycle call has moved.
    pub async fn moves(&self, session_id: &str) -> Vec<KanbanMove> {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|record| record.envelope.moves.clone())
            .unwrap_or_default()
    }

    /// The last saved [`Session`] snapshot.
    pub async fn replay(&self, session_id: &str) -> Option<Session> {
        let map = self.inner.read().await;
        map.get(session_id).map(|record| record.snapshot.clone())
    }

    /// The status given to the most recent
    /// [`save_with_status`](Self::save_with_status), if any. Orchestration
    /// data only.
    pub async fn last_status(&self, session_id: &str) -> Option<ExecutionStatus> {
        let map = self.inner.read().await;
        map.get(session_id).and_then(|r| r.last_status.clone())
    }

    /// The session's current cursor. An approval is issued against this.
    pub async fn cursor(&self, session_id: &str) -> Option<MoveCursor> {
        let map = self.inner.read().await;
        map.get(session_id).map(KanbanSessionRecord::cursor)
    }

    /// Save `session` and remember `status`. Records no move.
    pub async fn save_with_status(&self, session: Session, status: &ExecutionStatus) -> Result<()> {
        self.persist(session, Some(status.clone())).await;
        Ok(())
    }

    /// Apply a MUL gate decision issued against `presented`.
    ///
    /// `Flow` walks `Planning → CognitiveWork → Evaluation` and is refused at
    /// `Evaluation`. `Block` vetoes to `Prune` where the contract has a veto
    /// edge. `Hold`, and any decision with no legal successor, records
    /// nothing and returns `Ok(None)`.
    pub async fn apply_gate(
        &self,
        session_id: &str,
        presented: MoveCursor,
        gate: &GateDecision,
    ) -> std::result::Result<Option<KanbanMove>, LifecycleRefusal> {
        let mut map = self.inner.write().await;
        let record = map
            .get_mut(session_id)
            .ok_or(LifecycleRefusal::UnknownSession)?;
        record.check(presented)?;
        if record.envelope.column == KanbanColumn::Evaluation && matches!(gate, GateDecision::Flow)
        {
            return Err(LifecycleRefusal::RevisionRequired);
        }
        Ok(record
            .envelope
            .advance(gate)
            .and_then(|_| record.envelope.moves.last().copied()))
    }

    /// Apply a revision verdict issued against `presented`, at `Evaluation`.
    ///
    /// The successor is the contract's
    /// [`KanbanColumn::advance_on_revision`]: `Commit` only for an eligible
    /// effect that survived the counterfactual, `Plan` otherwise, nothing for
    /// `Suspend`.
    pub async fn apply_revision(
        &self,
        session_id: &str,
        presented: MoveCursor,
        verdict: RevisionVerdict,
    ) -> std::result::Result<Option<KanbanMove>, LifecycleRefusal> {
        let mut map = self.inner.write().await;
        let record = map
            .get_mut(session_id)
            .ok_or(LifecycleRefusal::UnknownSession)?;
        record.check(presented)?;
        let column = record.envelope.column;
        if column != KanbanColumn::Evaluation {
            return Err(LifecycleRefusal::NotAtEvaluation(column));
        }
        let Some(to) = column.advance_on_revision(verdict) else {
            return Ok(None);
        };
        Ok(record
            .envelope
            .try_transition(to)
            .ok()
            .and_then(|_| record.envelope.moves.last().copied()))
    }

    async fn persist(&self, session: Session, status: Option<ExecutionStatus>) {
        let mut map = self.inner.write().await;
        match map.get_mut(&session.id) {
            None => {
                let id = session.id.clone();
                map.insert(
                    id,
                    KanbanSessionRecord {
                        snapshot: session,
                        last_status: status,
                        envelope: KanbanPlanEnvelope::new(self.mailbox, ExecTarget::Native),
                    },
                );
            }
            Some(record) => {
                record.snapshot = session;
                if status.is_some() {
                    record.last_status = status;
                }
            }
        }
    }
}

#[async_trait]
impl SessionStorage for KanbanSessionStorage {
    async fn save(&self, session: Session) -> Result<()> {
        self.persist(session, None).await;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Session>> {
        let map = self.inner.read().await;
        Ok(map.get(id).map(|record| record.snapshot.clone()))
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let mut map = self.inner.write().await;
        map.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::Context, graph::Graph, runner::FlowRunner, task::TaskResult, GraphBuilder,
        NextAction, Task,
    };
    use std::sync::Arc;

    struct CountingTask {
        task_id: &'static str,
        next: NextAction,
    }

    #[async_trait]
    impl Task for CountingTask {
        fn id(&self) -> &str {
            self.task_id
        }

        async fn run(&self, context: Context) -> crate::error::Result<TaskResult> {
            let mut trace: Vec<String> = context.get("trace").await.unwrap_or_default();
            trace.push(self.task_id.to_string());
            context.set("trace", trace).await;
            Ok(TaskResult::new(
                Some(format!("{} done", self.task_id)),
                self.next.clone(),
            ))
        }
    }

    /// Builds a fresh 3-task linear graph (task1 -> task2 -> task3, End at
    /// task3). Called twice in the gate test: once for the pre-kill steps,
    /// once (a fresh, identically-built instance) for the resume.
    fn build_graph() -> Graph {
        let t1 = Arc::new(CountingTask {
            task_id: "task1",
            next: NextAction::Continue,
        });
        let t2 = Arc::new(CountingTask {
            task_id: "task2",
            next: NextAction::Continue,
        });
        let t3 = Arc::new(CountingTask {
            task_id: "task3",
            next: NextAction::End,
        });
        GraphBuilder::new("kanban_replay_graph")
            .add_task(t1.clone())
            .add_task(t2.clone())
            .add_task(t3.clone())
            .add_edge(t1.id(), t2.id())
            .add_edge(t2.id(), t3.id())
            .build()
    }

    /// A killed run resumes from the same storage with no repeated and no
    /// skipped task, and the run records no lifecycle move.
    #[tokio::test]
    async fn m25_kill_mid_graph_replay_resumes_without_repeats_or_gaps() {
        let storage = Arc::new(KanbanSessionStorage::new(7));
        let session_id = "sess-m25".to_string();

        let session = Session::new_from_task(session_id.clone(), "task1");
        storage.save(session).await.unwrap();

        {
            let graph = Arc::new(build_graph());
            let runner = FlowRunner::new(graph, storage.clone());

            let result = runner.run(&session_id).await.unwrap();
            assert!(matches!(result.status, ExecutionStatus::Paused { .. }));

            let result = runner.run(&session_id).await.unwrap();
            assert!(matches!(result.status, ExecutionStatus::Paused { .. }));
        }

        let graph2 = Arc::new(build_graph());
        let runner2 = FlowRunner::new(graph2, storage.clone());
        let final_result = runner2.run(&session_id).await.unwrap();
        assert!(matches!(final_result.status, ExecutionStatus::Completed));

        let settled = storage.get(&session_id).await.unwrap().unwrap();
        storage
            .save_with_status(settled, &final_result.status)
            .await
            .unwrap();

        let final_session = storage.replay(&session_id).await.unwrap();
        let trace: Vec<String> = final_session.context.get("trace").await.unwrap();
        assert_eq!(trace, vec!["task1", "task2", "task3"]);

        assert!(matches!(
            storage.last_status(&session_id).await,
            Some(ExecutionStatus::Completed)
        ));
        assert_eq!(storage.moves(&session_id).await, Vec::new());
        assert_eq!(
            storage.cursor(&session_id).await,
            Some(MoveCursor {
                column: KanbanColumn::Planning,
                position: 0
            })
        );
    }
}
