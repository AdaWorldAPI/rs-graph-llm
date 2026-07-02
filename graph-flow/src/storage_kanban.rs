//! Kanban-move-log-backed [`SessionStorage`] — D-V3-W3b.
//!
//! Makes every graph-flow execution **replayable**: the kanban move log is
//! both the WAL and the state (M25: "the board is BOTH the WAL and the
//! state"). Every [`Session`] snapshot is upserted alongside a
//! [`lance_graph_contract::kanban::KanbanMove`] log derived from the observed
//! transition, so a killed-mid-graph session can be resumed from the same
//! storage and its lifecycle audited via [`KanbanSessionStorage::moves`].
//!
//! Behind the `kanban` feature — it pulls the sibling `graph-flow-kanban`
//! envelope crate and `lance-graph-contract`, so it is off by default.
//!
//! `graph-flow` is the **composer**: this module hosts a
//! [`graph_flow_kanban::KanbanPlanEnvelope`] per session as the kanban-state
//! carrier (column + move log), matching that crate's own description
//! ("`rs-graph-llm`'s `graph-flow` composes this outer envelope"). See the
//! private `save_inner` method below for the one documented deviation from
//! the envelope's gated `advance`/`try_transition` API.

use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::RwLock;

use graph_flow_kanban::KanbanPlanEnvelope;
use lance_graph_contract::collapse_gate::MailboxId;
use lance_graph_contract::kanban::{ExecTarget, KanbanColumn, KanbanMove};

use crate::{error::Result, graph::ExecutionStatus, storage::SessionStorage, Session};

/// One session's kanban-tracked state: the latest [`Session`] snapshot plus
/// the [`KanbanPlanEnvelope`] (column + move log) derived from its save
/// history.
struct KanbanSessionRecord {
    snapshot: Session,
    envelope: KanbanPlanEnvelope,
}

/// A [`SessionStorage`] backed by an in-memory kanban move log, one
/// [`KanbanPlanEnvelope`] per session, all attributed to a single `mailbox`.
///
/// The `mailbox` is a field of the **storage** (the storage acts on behalf
/// of one mailbox), never of the [`Session`] — sessions carry no
/// owner/mailbox/tenant field (V3 DTO purity).
pub struct KanbanSessionStorage {
    mailbox: MailboxId,
    inner: RwLock<HashMap<String, KanbanSessionRecord>>,
}

impl KanbanSessionStorage {
    /// Construct a storage whose emitted [`KanbanMove`]s are all attributed
    /// to `mailbox`.
    #[must_use]
    pub fn new(mailbox: MailboxId) -> Self {
        Self {
            mailbox,
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// The full [`KanbanMove`] log for `session_id` — the auditable WAL
    /// trail. Empty if the session is unknown or has never crossed a kanban
    /// column boundary (e.g. it has only ever been saved once).
    pub async fn moves(&self, session_id: &str) -> Vec<KanbanMove> {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|record| record.envelope.moves.clone())
            .unwrap_or_default()
    }

    /// Replay `session_id`: the last saved [`Session`] snapshot. The move
    /// log ([`Self::moves`]) is the auditable trail of how it got there;
    /// v1 replay is "snapshot + log" per M25's design (no per-move
    /// context reconstruction yet).
    pub async fn replay(&self, session_id: &str) -> Option<Session> {
        let map = self.inner.read().await;
        map.get(session_id).map(|record| record.snapshot.clone())
    }

    /// Save `session`, deriving the kanban-column transition from the
    /// observed change AND (when available) the caller's
    /// [`ExecutionStatus`] — the precise terminal-column form of
    /// [`SessionStorage::save`], used when the caller has `status` in hand
    /// (e.g. from [`crate::runner::FlowRunner::run`]'s returned
    /// [`crate::graph::ExecutionResult`]).
    pub async fn save_with_status(&self, session: Session, status: &ExecutionStatus) -> Result<()> {
        self.save_inner(session, Some(status)).await
    }

    /// V1 Rubicon mapping (orchestrator-decided 2026-07-02; revisable, tests
    /// pin it):
    ///
    /// 1. first save of a session (no prior snapshot) -> `Planning`-equivalent
    ///    (the [`KanbanPlanEnvelope`] spawn state — no move is recorded, there
    ///    is no real predecessor to transition from);
    /// 2. else, if `current_task_id` CHANGED from the prior snapshot ->
    ///    `CognitiveWork`-equivalent (takes priority over the status-based
    ///    rules below — a task hop is cognitive work regardless of the
    ///    caller-supplied status);
    /// 3. else (task unchanged), a status of `WaitingForInput` / `Paused`-like
    ///    (or no status supplied — `save()`'s plain-trait path cannot see
    ///    one) -> `Evaluation`-equivalent (the safe "review" default);
    /// 4. else (task unchanged), a status of `Completed`-like ->
    ///    `Commit`-equivalent;
    /// 5. else (task unchanged), a status of `Error`-like -> `Prune`-equivalent.
    ///
    /// A move is appended to the envelope only when the computed target
    /// column differs from the envelope's current column (repeated task hops
    /// that both classify to `CognitiveWork`, for example, collapse to a
    /// single column occupancy — a [`KanbanMove`] is a *transition* record,
    /// not a per-save heartbeat).
    async fn save_inner(&self, session: Session, status: Option<&ExecutionStatus>) -> Result<()> {
        let mut map = self.inner.write().await;
        match map.get_mut(&session.id) {
            None => {
                // Rule 1: first save -> Planning, the envelope's spawn state.
                // No move recorded (no real predecessor), matching
                // `KanbanPlanEnvelope::new`'s own empty-moves spawn.
                let id = session.id.clone();
                let envelope = KanbanPlanEnvelope::new(self.mailbox, ExecTarget::Native);
                map.insert(
                    id,
                    KanbanSessionRecord {
                        snapshot: session,
                        envelope,
                    },
                );
            }
            Some(record) => {
                let to = classify_column(&record.snapshot.current_task_id, &session.current_task_id, status);
                if to != record.envelope.column {
                    let from = record.envelope.column;
                    record.envelope.moves.push(KanbanMove {
                        mailbox: self.mailbox,
                        from,
                        to,
                        // Deliberate V1 divergence from `KanbanPlanEnvelope::record_move`'s
                        // own "cycle stamp" convention: no re-deliberation-cycle counter
                        // exists at this layer, so `witness_chain_position` is simply the
                        // move-log length before this push — monotonic per session, per
                        // the brief's explicit instruction.
                        witness_chain_position: record.envelope.moves.len() as u32,
                        libet_offset_us: 0,
                        exec: ExecTarget::Native,
                    });
                    record.envelope.column = to;
                }
                record.snapshot = session;
            }
        }
        Ok(())
    }
}

/// See [`KanbanSessionStorage::save_inner`] doc comment for the full V1
/// Rubicon mapping this function implements (rules 2-5; rule 1, the
/// first-save case, is handled structurally by the envelope's spawn state
/// and never reaches this function).
fn classify_column(
    prior_task_id: &str,
    new_task_id: &str,
    status: Option<&ExecutionStatus>,
) -> KanbanColumn {
    if prior_task_id != new_task_id {
        return KanbanColumn::CognitiveWork;
    }
    match status {
        Some(ExecutionStatus::WaitingForInput) | Some(ExecutionStatus::Paused { .. }) | None => {
            KanbanColumn::Evaluation
        }
        Some(ExecutionStatus::Completed) => KanbanColumn::Commit,
        Some(ExecutionStatus::Error(_)) => KanbanColumn::Prune,
    }
}

#[async_trait]
impl SessionStorage for KanbanSessionStorage {
    async fn save(&self, session: Session) -> Result<()> {
        self.save_inner(session, None).await
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

    fn session_at(id: &str, task_id: &str) -> Session {
        Session {
            id: id.to_string(),
            graph_id: "g".to_string(),
            current_task_id: task_id.to_string(),
            status_message: None,
            context: Context::new(),
        }
    }

    // --- unit test 1: mapping-per-status ------------------------------

    #[test]
    fn v1_mapping_per_status() {
        // Task unchanged: status decides the target column.
        assert_eq!(classify_column("t", "t", None), KanbanColumn::Evaluation);
        assert_eq!(
            classify_column("t", "t", Some(&ExecutionStatus::WaitingForInput)),
            KanbanColumn::Evaluation
        );
        assert_eq!(
            classify_column(
                "t",
                "t",
                Some(&ExecutionStatus::Paused {
                    next_task_id: "t".to_string(),
                    reason: "no outgoing edge".to_string(),
                })
            ),
            KanbanColumn::Evaluation
        );
        assert_eq!(
            classify_column("t", "t", Some(&ExecutionStatus::Completed)),
            KanbanColumn::Commit
        );
        assert_eq!(
            classify_column("t", "t", Some(&ExecutionStatus::Error("boom".to_string()))),
            KanbanColumn::Prune
        );

        // Task changed: CognitiveWork wins regardless of status.
        assert_eq!(
            classify_column("t1", "t2", Some(&ExecutionStatus::Completed)),
            KanbanColumn::CognitiveWork
        );
        assert_eq!(
            classify_column("t1", "t2", Some(&ExecutionStatus::Error("x".to_string()))),
            KanbanColumn::CognitiveWork
        );
    }

    // --- unit test 2: moves() monotonic witness_chain_position --------

    #[tokio::test]
    async fn moves_witness_chain_position_is_monotonic() {
        let storage = KanbanSessionStorage::new(1);
        let id = "sess-mono";

        storage.save(session_at(id, "a")).await.unwrap(); // first save: no move
        storage.save(session_at(id, "b")).await.unwrap(); // task changed -> CognitiveWork (move 0)
        storage
            .save_with_status(session_at(id, "b"), &ExecutionStatus::Completed)
            .await
            .unwrap(); // unchanged, Completed -> Commit (move 1)
        storage
            .save_with_status(session_at(id, "c"), &ExecutionStatus::Error("x".to_string()))
            .await
            .unwrap(); // task changed -> CognitiveWork (move 2)

        let moves = storage.moves(id).await;
        assert_eq!(moves.len(), 3);
        let positions: Vec<u32> = moves.iter().map(|m| m.witness_chain_position).collect();
        assert_eq!(positions, vec![0, 1, 2]);
        let cols: Vec<KanbanColumn> = moves.iter().map(|m| m.to).collect();
        assert_eq!(
            cols,
            vec![
                KanbanColumn::CognitiveWork,
                KanbanColumn::Commit,
                KanbanColumn::CognitiveWork,
            ]
        );
    }

    // --- gate test: M25 kill-mid-graph replay --------------------------

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

    #[tokio::test]
    async fn m25_kill_mid_graph_replay_resumes_without_repeats_or_gaps() {
        let storage = Arc::new(KanbanSessionStorage::new(7));
        let session_id = "sess-m25".to_string();

        // Seed the session (this is the "first save" -> Planning).
        let session = Session::new_from_task(session_id.clone(), "task1");
        storage.save(session).await.unwrap();

        // Run task1 and task2 step-by-step, then "kill" by dropping the
        // FlowRunner and its graph.
        {
            let graph = Arc::new(build_graph());
            let runner = FlowRunner::new(graph, storage.clone());

            let result = runner.run(&session_id).await.unwrap();
            assert!(matches!(result.status, crate::graph::ExecutionStatus::Paused { .. }));

            let result = runner.run(&session_id).await.unwrap();
            assert!(matches!(result.status, crate::graph::ExecutionStatus::Paused { .. }));
            // `runner` and its `graph` are dropped at the end of this block —
            // the "kill".
        }

        // From the SAME storage state, a FRESH FlowRunner over a FRESH
        // (identically built) graph resumes the session to completion.
        let graph2 = Arc::new(build_graph());
        let runner2 = FlowRunner::new(graph2, storage.clone());
        let final_result = runner2.run(&session_id).await.unwrap();
        assert!(matches!(
            final_result.status,
            crate::graph::ExecutionStatus::Completed
        ));

        // The caller has `final_result.status` in hand (FlowRunner::run's
        // return value) — the natural real-world finalization call: refine
        // the plain-`save()` landing column (`Evaluation`, since the trait
        // path saw no status) into the precise terminal column.
        let settled = storage.get(&session_id).await.unwrap().unwrap();
        storage
            .save_with_status(settled, &final_result.status)
            .await
            .unwrap();

        // No repeats, no gaps: each task ran exactly once, in order.
        let final_session = storage.replay(&session_id).await.unwrap();
        let trace: Vec<String> = final_session.context.get("trace").await.unwrap();
        assert_eq!(trace, vec!["task1", "task2", "task3"]);

        // The recorded move-log column sequence matches the expected V1
        // mapping sequence exactly:
        //   task1->task2 hop            -> CognitiveWork
        //   task2->task3 hop            -> CognitiveWork again, collapsed
        //                                   (same column, no move appended)
        //   task3 End via plain save()  -> Evaluation (no status available)
        //   follow-up save_with_status  -> Commit (status now known)
        let moves = storage.moves(&session_id).await;
        let cols: Vec<KanbanColumn> = moves.iter().map(|m| m.to).collect();
        assert_eq!(
            cols,
            vec![
                KanbanColumn::CognitiveWork,
                KanbanColumn::Evaluation,
                KanbanColumn::Commit,
            ]
        );
    }
}
