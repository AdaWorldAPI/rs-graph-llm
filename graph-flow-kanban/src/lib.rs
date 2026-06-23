//! `graph-flow-kanban` — the **outer planning-phase envelope** for one mailbox's
//! Rubicon cognitive cycle.
//!
//! # Where this sits
//!
//! `rs-graph-llm` orchestrates cognition as a **kanban board** over the 4-phase
//! Rubicon lifecycle ([`KanbanColumn`]): `Planning → CognitiveWork → Evaluation
//! → { Commit | Plan | Prune }`. This crate is the **envelope** that carries ONE
//! mailbox's cycle through that board:
//!
//! - it holds the current [`KanbanColumn`] (board position),
//! - it [`advance`](KanbanPlanEnvelope::advance)s on a MUL [`GateDecision`] —
//!   `Flow` walks the forward arc, `Block` vetoes to `Prune` (the Libet "free
//!   won't"), `Hold` stays put,
//! - it logs each step as a [`KanbanMove`] (the contract's transition record),
//! - it carries the precipitated [`PlanResult`] — and that plan holds the
//!   resolved **thinking style (i4 32-D)** and **Rung** depth in its
//!   [`ThinkingContext`].
//!
//! # The two siblings (`graph-flow-kanban` ⟂ `graph-flow-action`)
//!
//! These are the two crates the cognitive orchestrator needs, and they are
//! **siblings, not nested** — `rs-graph-llm`'s `graph-flow` composes them:
//!
//! | crate | scope | unit | layer |
//! |---|---|---|---|
//! | `graph-flow-kanban` (this) | **outer** | one **cycle** | the planning envelope / lifecycle |
//! | `graph-flow-action` | **inner** | one **action** | the hot-path executor (kgV `ActionHandler`) |
//!
//! When a card sits in [`KanbanColumn::CognitiveWork`], the individual actions
//! inside that phase dispatch through `graph-flow-action`; when the card crosses
//! the Rubicon to [`KanbanColumn::Commit`], that is the same cold-path commit
//! `graph-flow-action`'s `dispatch` reaches. Both are **thin over the
//! contract** — symmetric `lance-graph-contract`-only dependency lists.
//!
//! # The pass-through invariant (do not flatten the style or the exec target)
//!
//! The envelope is a *carrier*, never a *compiler*. The **thinking style (i4
//! 32-D)** rides inside [`PlanResult::thinking`]; the **[`ExecTarget`]**
//! (`Native` / `Jit` / `SurrealQl` / `Elixir`) rides on every [`KanbanMove`] and
//! on the envelope itself. That `ExecTarget` is the **JITson ↔ Elixir-low-code ↔
//! SurrealQL** dispatch seam: `Jit` hands the style to JITson/Cranelift, `Elixir`
//! to the low-code declarative template path, `SurrealQl` lowers into the
//! substrate. The envelope keeps all of them visible and untouched — flattening
//! the style into a scalar, or collapsing the `ExecTarget` to a default, would
//! be the lossy move this crate exists to prevent (cf. the lossless-DO rule and
//! `I-ACTIONHANDLER-IS-KGV-NOT-CHOKEPOINT`: thinking stays wide and upstream).

#![forbid(unsafe_code)]

use lance_graph_contract::collapse_gate::MailboxId;
use lance_graph_contract::kanban::{ExecTarget, KanbanColumn, KanbanMove, RubiconTransitionError};
use lance_graph_contract::mul::GateDecision;
use lance_graph_contract::plan::{PlanResult, ThinkingContext};

/// One mailbox's planning-phase cycle as a kanban card.
///
/// Constructed at spawn in [`KanbanColumn::Planning`]; advanced through the
/// Rubicon lifecycle by MUL gate decisions ([`advance`](Self::advance)) or by
/// explicit legal transitions ([`try_transition`](Self::try_transition), used
/// for the `Evaluation → Plan → Planning` re-deliberation that the gate path
/// alone cannot reach). It accumulates a [`KanbanMove`] log and carries the
/// precipitated plan.
///
/// This struct is intentionally **not** `Copy` (it owns a `Vec` and a
/// `PlanResult`) — it is the heap-side envelope, distinct from the ≤16-byte
/// `Copy` [`KanbanMove`] baton it emits.
#[derive(Debug, Clone)]
pub struct KanbanPlanEnvelope {
    /// The mailbox whose lifecycle this envelope tracks.
    pub mailbox: MailboxId,
    /// Current board column. Starts at [`KanbanColumn::Planning`].
    pub column: KanbanColumn,
    /// The execution backend selected for this cycle's work — the JITson ↔
    /// Elixir-low-code ↔ SurrealQL dispatch seam. Stamped onto every emitted
    /// [`KanbanMove`]; never collapsed to a default by the envelope.
    pub exec: ExecTarget,
    /// Deliberation cycle counter. Incremented each time the card re-enters
    /// [`Planning`](KanbanColumn::Planning) via the `Plan` re-deliberation exit.
    /// Stamped into each move's `witness_chain_position` (the contract's
    /// "monotonic cycle stamp" convention, [`KanbanMove::cycle`]).
    pub cycle: u32,
    /// The precipitated plan for this cycle, once the planner has produced one.
    /// Holds the i4-32-D thinking style + Rung in its [`ThinkingContext`].
    pub plan: Option<PlanResult>,
    /// The lifecycle transition log — the audit trail of every column move.
    pub moves: Vec<KanbanMove>,
}

impl KanbanPlanEnvelope {
    /// Spawn a fresh envelope in [`KanbanColumn::Planning`] (the readiness-
    /// potential window, `t < -550 ms`) with the chosen execution backend.
    #[must_use]
    pub fn new(mailbox: MailboxId, exec: ExecTarget) -> Self {
        Self {
            mailbox,
            column: KanbanColumn::Planning,
            exec,
            cycle: 0,
            plan: None,
            moves: Vec::new(),
        }
    }

    /// Attach the precipitated plan (builder form).
    #[must_use]
    pub fn with_plan(mut self, plan: PlanResult) -> Self {
        self.plan = Some(plan);
        self
    }

    /// The resolved thinking context for this cycle — the **i4 32-D thinking
    /// style**, the [`crate`]-external `RungLevel` depth, the semiring/inference
    /// selection. Returns `None` until a plan has precipitated. The envelope
    /// hands this through untouched; JITson / Elixir / SurrealQL (selected by
    /// [`Self::exec`]) consume it downstream.
    #[must_use]
    pub fn thinking(&self) -> Option<&ThinkingContext> {
        self.plan.as_ref()?.thinking.as_ref()
    }

    /// Has the cycle reached an **absorbing** column ([`KanbanColumn::Commit`] or
    /// [`KanbanColumn::Prune`])? The ractor lifecycle driver tombstones the
    /// mailbox iff this is true. `Plan` is terminal-as-decision but NOT absorbing
    /// (it re-deliberates), so this stays `false` there.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.column.is_absorbing()
    }

    /// Advance on a MUL [`GateDecision`] — the one soft window driving the hard
    /// Rubicon DAG. Delegates the legal-successor choice to
    /// [`KanbanColumn::advance_on_gate`], so an out-of-DAG transition is
    /// structurally impossible:
    ///
    /// - [`GateDecision::Flow`] → the forward successor (never `Prune`),
    /// - [`GateDecision::Block`] → `Prune` iff a legal veto edge exists here
    ///   (Libet "free won't" at `Planning` / `Evaluation`),
    /// - [`GateDecision::Hold`] → `None` (stay in place, re-assess next cycle).
    ///
    /// Returns the column entered, or `None` if the gate produced no move.
    pub fn advance(&mut self, gate: &GateDecision) -> Option<KanbanColumn> {
        let to = self.column.advance_on_gate(gate)?;
        self.record_move(to);
        Some(to)
    }

    /// Apply an **explicit** legal Rubicon transition (the `Evaluation → Plan →
    /// Planning` re-deliberation arc the gate path cannot express, or any
    /// caller-driven step). Fails closed with [`RubiconTransitionError`] if
    /// `to` is not a legal successor of the current column.
    pub fn try_transition(
        &mut self,
        to: KanbanColumn,
    ) -> Result<KanbanColumn, RubiconTransitionError> {
        if self.column.can_transition_to(to) {
            self.record_move(to);
            Ok(to)
        } else {
            Err(RubiconTransitionError {
                from: self.column,
                to,
            })
        }
    }

    /// Emit the [`KanbanMove`], move the column, and bump the cycle on a
    /// re-deliberation re-entry. Stamps the Libet Σ-commit anchor (`-550 ms`)
    /// on the `Planning → CognitiveWork` crossing, `0` otherwise — matching the
    /// contract's [`KanbanMove::libet_offset_us`] convention.
    fn record_move(&mut self, to: KanbanColumn) {
        let from = self.column;
        let libet_offset_us = if from == KanbanColumn::Planning && to == KanbanColumn::CognitiveWork
        {
            -550_000
        } else {
            0
        };
        self.moves.push(KanbanMove {
            mailbox: self.mailbox,
            from,
            to,
            // Contract convention (KanbanMove::cycle): the monotonic cycle stamp
            // stands in for the witness-chain index until the A3 witness-arc
            // column lands. Stamp the CURRENT cycle, then advance.
            witness_chain_position: self.cycle,
            libet_offset_us,
            exec: self.exec,
        });
        self.column = to;
        if to == KanbanColumn::Planning {
            // Re-deliberation: a new cycle begins (the "act differently next
            // time" exit carried the witness back into Planning).
            self.cycle += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBX: MailboxId = 42;

    #[test]
    fn new_starts_in_planning_with_no_moves() {
        let env = KanbanPlanEnvelope::new(MBX, ExecTarget::Native);
        assert_eq!(env.column, KanbanColumn::Planning);
        assert_eq!(env.cycle, 0);
        assert!(env.moves.is_empty());
        assert!(env.plan.is_none());
        assert!(!env.is_settled());
    }

    #[test]
    fn flow_walks_the_forward_rubicon_arc_to_commit() {
        let mut env = KanbanPlanEnvelope::new(MBX, ExecTarget::Jit);
        assert_eq!(
            env.advance(&GateDecision::Flow),
            Some(KanbanColumn::CognitiveWork)
        );
        assert_eq!(
            env.advance(&GateDecision::Flow),
            Some(KanbanColumn::Evaluation)
        );
        assert_eq!(env.advance(&GateDecision::Flow), Some(KanbanColumn::Commit));
        assert!(env.is_settled());
        // Absorbing — no further forward move.
        assert_eq!(env.advance(&GateDecision::Flow), None);
        assert_eq!(env.moves.len(), 3);
        // Libet Σ-commit anchor only on the Planning → CognitiveWork crossing.
        assert_eq!(env.moves[0].libet_offset_us, -550_000);
        assert_eq!(env.moves[1].libet_offset_us, 0);
        // ExecTarget rides through every move, never flattened.
        assert!(env.moves.iter().all(|m| m.exec == ExecTarget::Jit));
    }

    #[test]
    fn block_at_planning_vetoes_to_prune() {
        let mut env = KanbanPlanEnvelope::new(MBX, ExecTarget::Native);
        let out = env.advance(&GateDecision::Block {
            reason: "free won't".into(),
        });
        assert_eq!(out, Some(KanbanColumn::Prune));
        assert!(env.is_settled());
    }

    #[test]
    fn block_mid_cognitive_work_has_no_veto_edge() {
        let mut env = KanbanPlanEnvelope::new(MBX, ExecTarget::Native);
        env.advance(&GateDecision::Flow); // → CognitiveWork
                                          // No legal veto edge mid-CognitiveWork: gate yields no move (hold).
        let out = env.advance(&GateDecision::Block {
            reason: "too late".into(),
        });
        assert_eq!(out, None);
        assert_eq!(env.column, KanbanColumn::CognitiveWork);
    }

    #[test]
    fn hold_stays_in_place() {
        let mut env = KanbanPlanEnvelope::new(MBX, ExecTarget::Native);
        let out = env.advance(&GateDecision::Hold {
            reason: "uncertain".into(),
        });
        assert_eq!(out, None);
        assert_eq!(env.column, KanbanColumn::Planning);
        assert!(env.moves.is_empty());
    }

    #[test]
    fn explicit_replan_re_enters_planning_and_bumps_cycle() {
        let mut env = KanbanPlanEnvelope::new(MBX, ExecTarget::Elixir);
        env.advance(&GateDecision::Flow); // Planning → CognitiveWork
        env.advance(&GateDecision::Flow); // CognitiveWork → Evaluation
                                          // The Plan re-deliberation exit (gate path can't reach Plan: Flow picks
                                          // Commit first) — driven explicitly.
        assert_eq!(
            env.try_transition(KanbanColumn::Plan),
            Ok(KanbanColumn::Plan)
        );
        assert_eq!(env.cycle, 0); // Plan itself doesn't bump...
        assert_eq!(
            env.try_transition(KanbanColumn::Planning),
            Ok(KanbanColumn::Planning)
        );
        assert_eq!(env.cycle, 1); // ...the re-entry into Planning does.
        assert!(!env.is_settled());
    }

    #[test]
    fn illegal_transition_fails_closed() {
        let mut env = KanbanPlanEnvelope::new(MBX, ExecTarget::Native);
        // Planning → Evaluation skips CognitiveWork — not a legal edge.
        let err = env.try_transition(KanbanColumn::Evaluation).unwrap_err();
        assert_eq!(err.from, KanbanColumn::Planning);
        assert_eq!(err.to, KanbanColumn::Evaluation);
        assert_eq!(env.column, KanbanColumn::Planning); // untouched
    }

    #[test]
    fn move_cycle_stamp_tracks_envelope_cycle() {
        let mut env = KanbanPlanEnvelope::new(MBX, ExecTarget::Native);
        env.advance(&GateDecision::Flow); // cycle 0 move
        env.advance(&GateDecision::Flow);
        env.try_transition(KanbanColumn::Plan).unwrap();
        env.try_transition(KanbanColumn::Planning).unwrap(); // now cycle 1
        env.advance(&GateDecision::Flow); // cycle 1 move
                                          // First move stamped at cycle 0, last after re-deliberation at cycle 1.
        assert_eq!(env.moves.first().unwrap().cycle(), 0);
        assert_eq!(env.moves.last().unwrap().cycle(), 1);
    }
}
