//! G0: an orchestration status is not a lifecycle authorization.
//!
//! `ExecutionStatus` describes what a graph-flow run did. The Rubicon columns
//! of `lance_graph_contract::kanban` describe a mailbox's lifecycle, and a
//! column changes only through the contract's own transition rules. These
//! tests pin that saving a session, with or without a status, records no
//! lifecycle move.

#![cfg(feature = "kanban")]

use graph_flow::{Context, ExecutionStatus, KanbanSessionStorage, Session, SessionStorage};
use lance_graph_contract::kanban::KanbanColumn;

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
