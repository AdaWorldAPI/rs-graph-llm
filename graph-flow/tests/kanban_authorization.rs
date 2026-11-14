//! G0: an orchestration status is not a lifecycle authorization.
//!
//! `ExecutionStatus` describes what a graph-flow run did. The Rubicon columns
//! of `lance_graph_contract::kanban` describe a mailbox's lifecycle, and a
//! column changes only through the contract's own transition rules. These
//! tests pin that saving a session, with or without a status, records no
//! lifecycle move.

#![cfg(feature = "kanban")]

use graph_flow::{
    Context, ExecutionStatus, KanbanSessionStorage, LifecycleRefusal, MoveCursor, Session,
    SessionStorage,
};
use lance_graph_contract::kanban::{KanbanColumn, LIBET_COMMIT_WINDOW_US};
use lance_graph_contract::mul::{FlowState, GateDecision, TrustTexture};
use lance_graph_contract::revision::{CounterfactualVerdict, EvidentialEffect, RevisionVerdict};

const HOLD: GateDecision = GateDecision::Hold {
    texture: TrustTexture::Uncertain,
    flow: FlowState::Transition,
};
const BLOCK: GateDecision = GateDecision::Block {
    texture: TrustTexture::Overconfident,
    flow: FlowState::Anxiety,
};
const ACCEPT: RevisionVerdict = RevisionVerdict {
    effect: EvidentialEffect::IncreaseEligible,
    counterfactual: CounterfactualVerdict::Necessary,
};

/// A saved session walked to `Evaluation` by two `Flow` gates.
async fn at_evaluation(storage: &KanbanSessionStorage, id: &str) -> MoveCursor {
    storage.save(session_at(id, "a")).await.unwrap();
    for _ in 0..2 {
        let cursor = storage.cursor(id).await.unwrap();
        storage
            .apply_gate(id, cursor, &GateDecision::Flow)
            .await
            .unwrap()
            .expect("a Flow gate moves before Evaluation");
    }
    let cursor = storage.cursor(id).await.unwrap();
    assert_eq!(cursor.column, KanbanColumn::Evaluation);
    cursor
}

fn session_at(id: &str, task_id: &str) -> Session {
    Session {
        id: id.to_string(),
        graph_id: "g".to_string(),
        current_task_id: task_id.to_string(),
        status_message: None,
        context: Context::new(),
    }
}

fn paused() -> ExecutionStatus {
    ExecutionStatus::Paused {
        next_task_id: "a".to_string(),
        reason: "no outgoing edge".to_string(),
    }
}

#[tokio::test]
async fn completed_from_planning_records_no_commit() {
    let storage = KanbanSessionStorage::new(1);
    storage.save(session_at("s", "a")).await.unwrap();
    storage
        .save_with_status(session_at("s", "a"), &ExecutionStatus::Completed)
        .await
        .unwrap();

    let moves = storage.moves("s").await;
    assert!(
        moves.iter().all(|m| m.to != KanbanColumn::Commit),
        "a Completed status recorded {moves:?}"
    );
    assert!(moves.is_empty(), "a status save recorded {moves:?}");
}

#[tokio::test]
async fn paused_or_waiting_from_planning_records_no_evaluation() {
    for status in [paused(), ExecutionStatus::WaitingForInput] {
        let storage = KanbanSessionStorage::new(1);
        storage.save(session_at("s", "a")).await.unwrap();
        storage
            .save_with_status(session_at("s", "a"), &status)
            .await
            .unwrap();

        let moves = storage.moves("s").await;
        assert!(moves.is_empty(), "{status:?} recorded {moves:?}");
    }
}

#[tokio::test]
async fn an_error_status_records_no_prune() {
    let storage = KanbanSessionStorage::new(1);
    storage.save(session_at("s", "a")).await.unwrap();
    storage.save(session_at("s", "b")).await.unwrap();
    storage
        .save_with_status(session_at("s", "b"), &ExecutionStatus::Error("x".into()))
        .await
        .unwrap();

    let moves = storage.moves("s").await;
    assert!(moves.is_empty(), "an Error status recorded {moves:?}");
}

/// Whatever sequence of saves and statuses a run produces, no recorded move
/// may leave the contract's lifecycle DAG.
#[tokio::test]
async fn no_save_sequence_records_an_illegal_edge() {
    let statuses = [
        None,
        Some(ExecutionStatus::Completed),
        Some(paused()),
        Some(ExecutionStatus::WaitingForInput),
        Some(ExecutionStatus::Error("x".into())),
    ];
    let tasks = ["a", "a", "b", "b", "c"];
    let mut illegal = Vec::new();
    for first in 0..statuses.len() {
        for second in 0..statuses.len() {
            let storage = KanbanSessionStorage::new(1);
            storage.save(session_at("s", "a")).await.unwrap();
            for (step, pick) in [first, second, first].into_iter().enumerate() {
                let session = session_at("s", tasks[(step + pick) % tasks.len()]);
                match &statuses[pick] {
                    Some(status) => storage.save_with_status(session, status).await.unwrap(),
                    None => storage.save(session).await.unwrap(),
                }
            }
            for m in storage.moves("s").await {
                if !m.from.can_transition_to(m.to) {
                    illegal.push((m.from, m.to));
                }
            }
        }
    }
    assert!(illegal.is_empty(), "illegal edges recorded: {illegal:?}");
}

/// Ordinary persistence keeps working and stays persistence: the latest
/// snapshot is returned, and a task hop records nothing.
#[tokio::test]
async fn save_and_load_record_no_move() {
    let storage = KanbanSessionStorage::new(1);
    storage.save(session_at("s", "a")).await.unwrap();
    storage.save(session_at("s", "b")).await.unwrap();

    let loaded = storage.get("s").await.unwrap().expect("saved session");
    assert_eq!(loaded.current_task_id, "b");
    assert!(storage.moves("s").await.is_empty());

    storage.delete("s").await.unwrap();
    assert!(storage.get("s").await.unwrap().is_none());
}

/// At `Evaluation`, `Commit` is a legal successor. Neither that, nor a
/// `Completed` status, nor a forward gate, nor an unadjudicated verdict
/// reaches it.
#[tokio::test]
async fn a_legal_successor_is_not_a_commit() {
    let storage = KanbanSessionStorage::new(1);
    let cursor = at_evaluation(&storage, "s").await;
    assert!(cursor.column.can_transition_to(KanbanColumn::Commit));

    storage
        .save_with_status(session_at("s", "a"), &ExecutionStatus::Completed)
        .await
        .unwrap();
    assert_eq!(storage.cursor("s").await, Some(cursor));

    assert_eq!(
        storage.apply_gate("s", cursor, &GateDecision::Flow).await,
        Err(LifecycleRefusal::RevisionRequired)
    );
    assert_eq!(storage.apply_gate("s", cursor, &HOLD).await, Ok(None));
    assert_eq!(storage.cursor("s").await, Some(cursor));

    let unadjudicated = RevisionVerdict::unadjudicated(EvidentialEffect::IncreaseEligible);
    let moved = storage
        .apply_revision("s", cursor, unadjudicated)
        .await
        .unwrap()
        .expect("an eligible, unattacked revision re-deliberates");
    assert_eq!(moved.to, KanbanColumn::Plan);
    assert!(storage
        .moves("s")
        .await
        .iter()
        .all(|m| m.to != KanbanColumn::Commit));
}

/// The adjudicated verdict is what reaches `Commit`, so the test above is
/// not passing because `Commit` is unreachable.
#[tokio::test]
async fn an_adjudicated_revision_commits() {
    let storage = KanbanSessionStorage::new(1);
    let cursor = at_evaluation(&storage, "s").await;
    let moved = storage
        .apply_revision("s", cursor, ACCEPT)
        .await
        .unwrap()
        .expect("an accepted revision moves");
    assert_eq!(
        (moved.from, moved.to),
        (KanbanColumn::Evaluation, KanbanColumn::Commit)
    );
}

#[tokio::test]
async fn a_replayed_approval_records_nothing() {
    let storage = KanbanSessionStorage::new(1);
    let cursor = at_evaluation(&storage, "s").await;
    storage.apply_revision("s", cursor, ACCEPT).await.unwrap();
    let after = storage.moves("s").await;
    let current = storage.cursor("s").await.unwrap();

    assert_eq!(
        storage.apply_revision("s", cursor, ACCEPT).await,
        Err(LifecycleRefusal::StaleApproval {
            presented: cursor,
            current
        })
    );
    assert_eq!(storage.moves("s").await, after);
}

/// An approval issued in one deliberation cycle does not apply in the next,
/// although the column is `Evaluation` again.
#[tokio::test]
async fn a_stale_approval_from_an_earlier_cycle_records_nothing() {
    let storage = KanbanSessionStorage::new(1);
    let first = at_evaluation(&storage, "s").await;

    let revise = RevisionVerdict::unadjudicated(EvidentialEffect::NoIncrease);
    storage.apply_revision("s", first, revise).await.unwrap();
    for _ in 0..3 {
        let cursor = storage.cursor("s").await.unwrap();
        storage
            .apply_gate("s", cursor, &GateDecision::Flow)
            .await
            .unwrap();
    }
    let second = storage.cursor("s").await.unwrap();
    assert_eq!(second.column, first.column);
    assert_ne!(second, first);

    let before = storage.moves("s").await;
    assert!(matches!(
        storage.apply_revision("s", first, ACCEPT).await,
        Err(LifecycleRefusal::StaleApproval { .. })
    ));
    assert_eq!(storage.moves("s").await, before);
}

/// Every gate and verdict, applied from every column a session can reach,
/// records only edges of the contract's DAG.
#[tokio::test]
async fn lifecycle_calls_record_only_legal_edges() {
    let gates = [GateDecision::Flow, HOLD, BLOCK];
    let verdicts = [
        ACCEPT,
        RevisionVerdict::unadjudicated(EvidentialEffect::IncreaseEligible),
        RevisionVerdict::unadjudicated(EvidentialEffect::NoIncrease),
        RevisionVerdict::unadjudicated(EvidentialEffect::Suspend),
    ];
    let mut recorded = 0;
    for a in 0..7 {
        for b in 0..7 {
            for c in 0..7 {
                let storage = KanbanSessionStorage::new(1);
                storage.save(session_at("s", "a")).await.unwrap();
                for pick in [0, 0, a, 0, b, 0, 0, c, a] {
                    let cursor = storage.cursor("s").await.unwrap();
                    let _ = if pick < 3 {
                        storage.apply_gate("s", cursor, &gates[pick]).await
                    } else {
                        storage
                            .apply_revision("s", cursor, verdicts[pick - 3])
                            .await
                    };
                }
                for m in storage.moves("s").await {
                    assert!(m.from.can_transition_to(m.to), "{:?} -> {:?}", m.from, m.to);
                    recorded += 1;
                }
            }
        }
    }
    assert!(recorded > 1000, "only {recorded} moves exercised");
}

/// The Libet window is read off the `Planning -> CognitiveWork` crossing.
#[tokio::test]
async fn the_libet_window_is_derived_from_the_crossing() {
    let storage = KanbanSessionStorage::new(1);
    at_evaluation(&storage, "s").await;
    let windows: Vec<_> = storage
        .moves("s")
        .await
        .iter()
        .map(|m| m.libet_window_us())
        .collect();
    assert_eq!(windows, vec![Some(LIBET_COMMIT_WINDOW_US), None]);
}

#[tokio::test]
async fn lifecycle_calls_on_an_unknown_session_are_refused() {
    let storage = KanbanSessionStorage::new(1);
    let cursor = MoveCursor {
        column: KanbanColumn::Planning,
        position: 0,
    };
    assert_eq!(
        storage
            .apply_gate("nope", cursor, &GateDecision::Flow)
            .await,
        Err(LifecycleRefusal::UnknownSession)
    );
    assert_eq!(
        storage.apply_revision("nope", cursor, ACCEPT).await,
        Err(LifecycleRefusal::UnknownSession)
    );
}
