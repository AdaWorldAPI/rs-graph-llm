//! SoA-ownership sanity probes for the outer orchestration seam
//! (operator-requested, 2026-07-17).
//!
//! graph-flow sits OUTSIDE the reasoning compartments. These probes pin the
//! two properties that keep it an orchestrator instead of a soup:
//!
//! 1. **No soup.** N mailboxes = N storages, each attributing its moves to
//!    exactly one `MailboxId`. Interleaving sessions across storages changes
//!    nothing: every witness entry carries its own storage's mailbox, one
//!    storage cannot see another's sessions, and a session interleaved with
//!    a foreign mailbox replays identically to a solo run.
//! 2. **Ownership rides the storage boundary, never the Session.** The SAME
//!    `Session` value saved through two storages yields two witness logs
//!    stamped with the two different mailboxes — proof the DTO carries no
//!    ownership (V3 DTO purity) and attribution comes solely from the
//!    on-behalf boundary.
//!
//! Plus the sanctioned positive arm:
//!
//! 3. **Outside meta-awareness through the episodic witness.** An outer
//!    observer computes cross-mailbox awareness from CLONED witness values
//!    (`moves()` returns owned data) — a function that takes only
//!    `&[(MailboxId, Vec<KanbanMove>)]` and no storage handle. Observation
//!    is detached from any write capability by construction: the witness is
//!    a value you read, never a handle into the compartment.

#![cfg(feature = "kanban")]

use graph_flow::{Context, ExecutionStatus, KanbanSessionStorage, Session, SessionStorage};
use lance_graph_contract::collapse_gate::MailboxId;
use lance_graph_contract::kanban::{KanbanColumn, KanbanMove};

fn session_at(id: &str, task_id: &str) -> Session {
    Session {
        id: id.to_string(),
        graph_id: "g".to_string(),
        current_task_id: task_id.to_string(),
        status_message: None,
        context: Context::new(),
    }
}

/// Drive a session through the same fixed lifecycle on the given storage:
/// seed -> task hop (CognitiveWork) -> completed (Commit) -> error (Prune via
/// a second hop first). Deterministic, so two runs are comparable.
async fn drive_lifecycle(storage: &KanbanSessionStorage, id: &str) {
    storage.save(session_at(id, "a")).await.unwrap(); // first save: Planning spawn
    storage.save(session_at(id, "b")).await.unwrap(); // hop -> CognitiveWork
    storage
        .save_with_status(session_at(id, "b"), &ExecutionStatus::Completed)
        .await
        .unwrap(); // -> Commit
    storage
        .save_with_status(session_at(id, "b"), &ExecutionStatus::Error("x".into()))
        .await
        .unwrap(); // -> Prune
}

// --- probe 1: no soup ---------------------------------------------------

#[tokio::test]
async fn no_soup_interleaved_mailboxes_never_blend() {
    let mb_a: MailboxId = 3;
    let mb_b: MailboxId = 9;
    let storage_a = KanbanSessionStorage::new(mb_a);
    let storage_b = KanbanSessionStorage::new(mb_b);

    // Interleave: alternate saves across the two storages.
    storage_a.save(session_at("sa", "a")).await.unwrap();
    storage_b.save(session_at("sb", "a")).await.unwrap();
    storage_a.save(session_at("sa", "b")).await.unwrap();
    storage_b.save(session_at("sb", "b")).await.unwrap();
    storage_a
        .save_with_status(session_at("sa", "b"), &ExecutionStatus::Completed)
        .await
        .unwrap();
    storage_b
        .save_with_status(session_at("sb", "b"), &ExecutionStatus::Error("x".into()))
        .await
        .unwrap();

    // Every witness entry is stamped with its OWN storage's mailbox.
    let moves_a = storage_a.moves("sa").await;
    let moves_b = storage_b.moves("sb").await;
    assert!(!moves_a.is_empty() && !moves_b.is_empty());
    assert!(moves_a.iter().all(|m| m.mailbox == mb_a), "A stays A");
    assert!(moves_b.iter().all(|m| m.mailbox == mb_b), "B stays B");

    // One storage cannot see the other's sessions at all.
    assert!(storage_a.get("sb").await.unwrap().is_none(), "A can't see B");
    assert!(storage_b.get("sa").await.unwrap().is_none(), "B can't see A");
    assert!(storage_a.moves("sb").await.is_empty());

    // Interleaving changed nothing: a solo run of the same lifecycle on a
    // fresh storage produces the identical witness-column sequence.
    let solo = KanbanSessionStorage::new(mb_a);
    solo.save(session_at("sa", "a")).await.unwrap();
    solo.save(session_at("sa", "b")).await.unwrap();
    solo.save_with_status(session_at("sa", "b"), &ExecutionStatus::Completed)
        .await
        .unwrap();
    let solo_cols: Vec<KanbanColumn> = solo.moves("sa").await.iter().map(|m| m.to).collect();
    let inter_cols: Vec<KanbanColumn> = moves_a.iter().map(|m| m.to).collect();
    assert_eq!(solo_cols, inter_cols, "foreign mailbox traffic is invisible");
}

// --- probe 2: ownership rides the storage, never the Session -------------

#[tokio::test]
async fn ownership_comes_from_the_boundary_not_the_dto() {
    // The SAME Session value (no owner/mailbox/tenant field exists on it)
    // saved through two differently-owned storages: attribution follows the
    // storage, proving the DTO is ownership-free.
    let storage_3 = KanbanSessionStorage::new(3);
    let storage_9 = KanbanSessionStorage::new(9);

    for storage in [&storage_3, &storage_9] {
        drive_lifecycle(storage, "same-session").await;
    }

    let stamped_3: Vec<MailboxId> = storage_3
        .moves("same-session")
        .await
        .iter()
        .map(|m| m.mailbox)
        .collect();
    let stamped_9: Vec<MailboxId> = storage_9
        .moves("same-session")
        .await
        .iter()
        .map(|m| m.mailbox)
        .collect();

    assert!(stamped_3.iter().all(|&mb| mb == 3));
    assert!(stamped_9.iter().all(|&mb| mb == 9));
    assert_eq!(stamped_3.len(), stamped_9.len(), "identical lifecycles");
}

// --- probe 3: meta-awareness reads witness VALUES, holds no handle -------

/// The sanctioned outside-observer shape: takes cloned witness data only —
/// no storage reference, no session handle, nothing that could write into
/// any compartment. Observation detached from capability by construction.
fn meta_awareness(witnesses: &[(MailboxId, Vec<KanbanMove>)]) -> Option<MailboxId> {
    witnesses
        .iter()
        .max_by_key(|(_, moves)| {
            moves
                .iter()
                .filter(|m| m.to == KanbanColumn::Prune)
                .count()
        })
        .map(|(mb, _)| *mb)
}

#[tokio::test]
async fn episodic_witness_gives_meta_awareness_without_membership() {
    let storage_a = KanbanSessionStorage::new(1);
    let storage_b = KanbanSessionStorage::new(2);

    // Mailbox 1: clean lifecycle (ends Commit). Mailbox 2: ends in Prune.
    storage_a.save(session_at("s1", "a")).await.unwrap();
    storage_a.save(session_at("s1", "b")).await.unwrap();
    storage_a
        .save_with_status(session_at("s1", "b"), &ExecutionStatus::Completed)
        .await
        .unwrap();
    drive_lifecycle(&storage_b, "s2").await; // ends with an Error -> Prune

    // The observer clones the witnesses OUT — values, not handles — and
    // computes a cross-mailbox observation.
    let witnesses = vec![
        (1 as MailboxId, storage_a.moves("s1").await),
        (2 as MailboxId, storage_b.moves("s2").await),
    ];
    assert_eq!(
        meta_awareness(&witnesses),
        Some(2),
        "outside meta-awareness sees which compartment is struggling"
    );

    // And the observation could not have perturbed the compartments: the
    // witness logs re-read from the storages are unchanged.
    assert_eq!(storage_a.moves("s1").await.len(), witnesses[0].1.len());
    assert_eq!(storage_b.moves("s2").await.len(), witnesses[1].1.len());
}
