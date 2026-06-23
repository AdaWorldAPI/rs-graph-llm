//! `orchestrate` — the end-to-end spine: resolve an OGAR `ActionDef`, drive the
//! Rubicon [`KanbanPlanEnvelope`], authorize+commit through `commit_via`, and
//! map the resulting [`ActionState`] onto a terminal Kanban column.
//!
//! Generic over a **provided** action manifest (`&[ActionDef]`) and a **provided**
//! [`ClassRbac`] — it imports no thinking-style, no provider, no SoA column (the
//! kgV invariant). The lifecycle advances are deterministic (`Flow` progression
//! `Planning → CognitiveWork → Evaluation`); only the **commit** consults the MUL
//! `gate` to decide the Evaluation terminal.

use crate::KanbanPlanEnvelope;
use lance_graph_contract::action::ActionDef;
use lance_graph_contract::action::{ActionInvocation, ActionState};
use lance_graph_contract::canonical_node::NodeGuid;
use lance_graph_contract::collapse_gate::MailboxId;
use lance_graph_contract::kanban::{ExecTarget, KanbanColumn};
use lance_graph_contract::mul::GateDecision;
use lance_graph_contract::rbac::{ActorId, ClassRbac};

/// The result of one [`run_cycle`] — the terminal column reached plus the
/// envelope (its move-log is the audit trail).
#[derive(Debug, Clone)]
pub struct CycleOutcome {
    /// The terminal Kanban column: `Commit` / `Plan` / `Prune`.
    pub outcome: KanbanColumn,
    /// The driven envelope (carries the `KanbanMove` log + exec target).
    pub envelope: KanbanPlanEnvelope,
}

/// Run one cognitive cycle for `(classid, predicate)` against the provided
/// manifest + RBAC.
///
/// 1. Resolve the [`ActionDef`] by `(object_class, predicate)`. Unknown ⇒ the
///    envelope is vetoed straight to `Prune` (no action to run).
/// 2. Drive the envelope `Planning → CognitiveWork → Evaluation` (deterministic
///    `Flow` progression — entering the work is not the MUL decision).
/// 3. At Evaluation, `commit_via` adjudicates the action (def-match → RBAC →
///    guard → MUL `gate`); the resulting [`ActionState`] maps to the terminal:
///    `Committed → Commit`, `Pending → Plan` (re-deliberate), `Cancelled`/`Failed
///    → Prune`.
///
/// `object_instance` is the target node (its `classid()` must equal `classid`).
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn run_cycle(
    actions: &[ActionDef],
    rbac: &impl ClassRbac,
    actor_id: ActorId<'_>,
    classid: u32,
    predicate: &'static str,
    gate: &GateDecision,
    object_instance: NodeGuid,
    guard_value: Option<&str>,
    now_millis: u64,
) -> CycleOutcome {
    let mailbox: MailboxId = classid;
    let Some(def) = actions
        .iter()
        .find(|a| a.object_class == classid && a.predicate == predicate)
    else {
        // No such action — veto the cycle (Planning → Prune is a legal Libet veto).
        let mut envelope = KanbanPlanEnvelope::new(mailbox, ExecTarget::Native);
        envelope.try_transition(KanbanColumn::Prune).ok();
        return CycleOutcome {
            outcome: KanbanColumn::Prune,
            envelope,
        };
    };

    let mut envelope = KanbanPlanEnvelope::new(mailbox, def.exec);
    // Deterministic lifecycle progression to the Evaluation decision point.
    envelope.advance(&GateDecision::Flow); // Planning → CognitiveWork
    envelope.advance(&GateDecision::Flow); // CognitiveWork → Evaluation

    // The cold floor: commit_via IS { def-match · RBAC · Libet guard · Rubikon@MUL }.
    let mut inv =
        ActionInvocation::pending(classid, predicate, object_instance, envelope.cycle, 0, 0);
    let state = inv.commit_via(def, rbac, actor_id, gate, guard_value, now_millis);

    let outcome = match state {
        ActionState::Committed => KanbanColumn::Commit,
        ActionState::Pending => KanbanColumn::Plan,
        ActionState::Cancelled | ActionState::Failed => KanbanColumn::Prune,
    };
    envelope.try_transition(outcome).ok();
    CycleOutcome { outcome, envelope }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lance_graph_contract::rbac::{ActorId as Aid, ClassId, Operation, RoleId};

    const PATIENT: u32 = 0x0000_0901;

    fn manifest() -> Vec<ActionDef> {
        vec![ActionDef {
            predicate: "approve",
            object_class: PATIENT,
            exec: ExecTarget::Native,
            guard: None,
            required_role: Some("physician"),
            overrides: None,
        }]
    }

    // A ClassRbac fixture: "dr-house" holds "physician" which is granted Act.
    struct Rbac;
    impl ClassRbac for Rbac {
        fn actor_roles(&self, actor: Aid<'_>) -> &[RoleId] {
            if actor == "dr-house" {
                const R: &[RoleId] = &["physician"];
                R
            } else {
                &[]
            }
        }
        fn grant_permits(&self, role: RoleId, class: ClassId, op: &Operation<'_>) -> bool {
            role == "physician" && class == PATIENT && matches!(op, Operation::Act { .. })
        }
    }

    fn guid() -> NodeGuid {
        NodeGuid::new(PATIENT, 0, 0, 0, 0, 1)
    }

    #[test]
    fn authorized_flow_reaches_commit() {
        let m = manifest();
        let out = run_cycle(
            &m,
            &Rbac,
            "dr-house",
            PATIENT,
            "approve",
            &GateDecision::Flow,
            guid(),
            None,
            1000,
        );
        assert_eq!(out.outcome, KanbanColumn::Commit);
        assert_eq!(out.envelope.column, KanbanColumn::Commit);
    }

    #[test]
    fn unauthorized_actor_reaches_prune() {
        let m = manifest();
        let out = run_cycle(
            &m,
            &Rbac,
            "betty", // not a physician
            PATIENT,
            "approve",
            &GateDecision::Flow,
            guid(),
            None,
            1000,
        );
        assert_eq!(out.outcome, KanbanColumn::Prune);
    }

    #[test]
    fn mul_hold_reaches_plan() {
        let m = manifest();
        let out = run_cycle(
            &m,
            &Rbac,
            "dr-house",
            PATIENT,
            "approve",
            &GateDecision::Hold {
                reason: "uncertain".into(),
            },
            guid(),
            None,
            1000,
        );
        assert_eq!(out.outcome, KanbanColumn::Plan);
    }

    #[test]
    fn unknown_predicate_reaches_prune() {
        let m = manifest();
        let out = run_cycle(
            &m,
            &Rbac,
            "dr-house",
            PATIENT,
            "nonexistent",
            &GateDecision::Flow,
            guid(),
            None,
            1000,
        );
        assert_eq!(out.outcome, KanbanColumn::Prune);
    }
}
