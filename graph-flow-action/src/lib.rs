//! `graph-flow-action` — the **kgV `ActionHandler`** (the minimal executor plug)
//! plus the **cold-path → MUL → hot** dispatch chain.
//!
//! # Where this sits (see ada-docs `architecture/COLD_PATH_MUL_ACTIONHANDLER.md`)
//!
//! The full action governance is three layers, and **almost all of it already
//! lives in the contract**:
//!
//! - **Cold path (hard floor)** — the irreversible lifecycle gates
//!   `{ RBAC · Libet do/don't · Rubikon letzte Phase · SLA }`. These are
//!   `contract::action::ActionInvocation::commit`, which adjudicates, in order:
//!   def-match → **RBAC** (`ActionDef.required_role` vs `ActorContext`) →
//!   **state guard** (Libet do/don't) → **MUL impact** (the Rubicon crossing:
//!   `Flow → Committed`, `Hold → Pending`, `Block → Cancelled`).
//! - **MUL (the one soft window)** — `contract::mul::GateDecision`, passed as
//!   `impact`; deliberated *before* the Libet veto, by the planner / cognition.
//! - **Hot path (execution)** — [`ActionHandler::handle`], reached only when the
//!   cold path returns `Committed`.
//!
//! This crate adds the **kgV plug** ([`ActionHandler`]) and the thin
//! [`dispatch`] that wires *routing* (capability / applicability) → the cold
//! floor (`commit`) → the hot `handle`. HIRO / Bardioc / rig dock by writing
//! `impl ActionHandler`.
//!
//! # The invariant (`I-ACTIONHANDLER-IS-KGV-NOT-CHOKEPOINT`)
//!
//! `ActionHandler` is the **kleinste gemeinsame Vielfache** — the minimal
//! interface every executor fits into. **Thinking stays wide and upstream; it
//! does not crawl under this waist.** A `ThinkingStyle` (or any cognition type)
//! imported into this crate is a **structural violation** — the dependency list
//! is intentionally `contract`-only, and the governance lives in `dispatch`, not
//! in the trait, so an executor implements *only* `handle()` and never drags the
//! cold path or the MUL window.

#![forbid(unsafe_code)]

use lance_graph_contract::action::{ActionDef, ActionInvocation, ActionState};
use lance_graph_contract::auth::ActorContext;
use lance_graph_contract::mul::GateDecision;

/// The minimal executor port — the *kleinste gemeinsame Vielfache* every
/// executor (HIRO / Bardioc / rig) fits into. It carries only what **every**
/// executor needs; anything richer stops it being the kgV. The governance (cold
/// path + MUL) is in [`dispatch`], **not** here — so an `impl ActionHandler`
/// drags neither RBAC nor cognition.
pub trait ActionHandler {
    /// The `auth_store` classid (`0x0B01`) this handler authorizes through — its
    /// `Configuration` membrane (HIRO `ActionHandler → Configuration`). Pure
    /// provenance: the actual RBAC adjudication is the cold path's
    /// (`ActionDef.required_role` against the `ActorContext`).
    fn configuration(&self) -> u32;

    /// `ActionCapability` — *can this handler execute this `ActionDef` at all?*
    /// Routing, not authorization (e.g. "I am the ssh executor").
    fn capability(&self, action: &ActionDef) -> bool;

    /// `ActionApplicability` — *does this handler apply to this invocation's
    /// target?* Routing guard (e.g. "on `ogit/_id`").
    fn applicability(&self, inv: &ActionInvocation) -> bool;

    /// The hot path — the consummated act. Reached **only** after the cold path
    /// (`commit`) returns `Committed`. The executor runs the body here
    /// (lancedb / surreal-kv-lance / an LLM call / a shell command).
    fn handle(&self, action: &ActionDef, inv: &mut ActionInvocation) -> HandlerOutcome;
}

/// The outcome of a [`dispatch`] — each variant names which layer decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// The hot path ran (`Committed` → `handle`).
    Done,
    /// The cold path refused: def-mismatch or **RBAC** (`required_role`) — the
    /// invocation is `Failed`. Fail-closed.
    Denied,
    /// **MUL `Hold`** — the soft window deferred; the invocation stays `Pending`
    /// (re-assess next cycle). = HIRO `guardFailurePolicy = Postponable`.
    Postponed,
    /// **MUL `Block`** *or* the **state guard** (Libet do/don't) vetoed — the
    /// invocation is `Cancelled`; escalate (human / LLM resolves the tail).
    Escalated,
    /// The handler does not route here (`capability` / `applicability` false).
    /// The cold path was never consulted; the invocation is untouched.
    NotApplicable,
}

/// Run an action through **routing → cold floor → hot path**.
///
/// 1. **Routing** (kgV handler's own cheap checks): `capability` ∧
///    `applicability`. False ⇒ [`HandlerOutcome::NotApplicable`] — the cold path
///    is never consulted, the invocation is untouched.
/// 2. **Cold floor** ([`ActionInvocation::commit`]): the whole hard lifecycle —
///    def-match → RBAC (`required_role` vs `actor`) → state guard
///    (`guard_field_value`) → MUL impact (`gate`). Returns the [`ActionState`].
/// 3. **Hot path**: only on `Committed` do we call [`ActionHandler::handle`].
///
/// `gate` is the MUL [`GateDecision`] (the soft window, decided upstream);
/// `guard_field_value` is the current value of `action.guard.field` on the
/// target (the Core holds no object state — `I-VSA-IDENTITIES`); `now_millis`
/// stamps the commit (the SLA/HLC clock).
#[must_use]
pub fn dispatch<H: ActionHandler>(
    handler: &H,
    actor: &ActorContext,
    gate: &GateDecision,
    action: &ActionDef,
    inv: &mut ActionInvocation,
    guard_field_value: Option<&str>,
    now_millis: u64,
) -> HandlerOutcome {
    // 1. Routing — is this the right handler for this action/instance? Cheap,
    //    local, no authorization. (HIRO ActionCapability ∧ ActionApplicability.)
    if !handler.capability(action) || !handler.applicability(inv) {
        return HandlerOutcome::NotApplicable;
    }
    // 2. Cold floor — commit() IS { def-match · RBAC · Libet · Rubikon@SLA }.
    //    MUL's GateDecision rides in as `gate` (the one soft window).
    match inv.commit(action, actor, gate, guard_field_value, now_millis) {
        // 3. Hot path — only a committed (Rubicon-crossed) action executes.
        ActionState::Committed => handler.handle(action, inv),
        ActionState::Pending => HandlerOutcome::Postponed, // MUL Hold
        ActionState::Cancelled => HandlerOutcome::Escalated, // MUL Block / guard veto
        ActionState::Failed => HandlerOutcome::Denied,     // def-mismatch / RBAC
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lance_graph_contract::action::StateGuard;
    use lance_graph_contract::canonical_node::NodeGuid;
    use lance_graph_contract::kanban::ExecTarget;

    const PATIENT: u32 = 0x0901;

    /// A trivial executor that records nothing — `handle` returns `Done`.
    struct EchoHandler {
        can: bool,
    }
    impl ActionHandler for EchoHandler {
        fn configuration(&self) -> u32 {
            0x0B01 // auth_store
        }
        fn capability(&self, _a: &ActionDef) -> bool {
            self.can
        }
        fn applicability(&self, _inv: &ActionInvocation) -> bool {
            true
        }
        fn handle(&self, _a: &ActionDef, _inv: &mut ActionInvocation) -> HandlerOutcome {
            HandlerOutcome::Done
        }
    }

    fn def(required_role: Option<&'static str>, guard: Option<StateGuard>) -> ActionDef {
        ActionDef {
            predicate: "approve",
            object_class: PATIENT,
            exec: ExecTarget::Native,
            guard,
            required_role,
            overrides: None,
        }
    }

    fn inv() -> ActionInvocation {
        ActionInvocation::pending(
            PATIENT,
            "approve",
            NodeGuid::new(PATIENT, 0, 0, 0, 0, 0),
            1,
            0,
            0,
        )
    }

    fn actor(roles: &[&str]) -> ActorContext {
        ActorContext::new(
            "user-1".into(),
            0,
            roles.iter().map(|s| (*s).to_string()).collect(),
        )
    }

    #[test]
    fn authorized_flow_commits_and_handles() {
        let h = EchoHandler { can: true };
        let mut i = inv();
        let out = dispatch(
            &h,
            &actor(&["physician"]),
            &GateDecision::Flow,
            &def(Some("physician"), None),
            &mut i,
            None,
            1000,
        );
        assert_eq!(out, HandlerOutcome::Done);
        assert_eq!(i.state, ActionState::Committed);
    }

    #[test]
    fn cold_path_rbac_denies_wrong_role() {
        let h = EchoHandler { can: true };
        let mut i = inv();
        let out = dispatch(
            &h,
            &actor(&["cashier"]), // lacks "physician"
            &GateDecision::Flow,
            &def(Some("physician"), None),
            &mut i,
            None,
            1000,
        );
        assert_eq!(out, HandlerOutcome::Denied);
        assert_eq!(i.state, ActionState::Failed);
    }

    #[test]
    fn mul_hold_postpones() {
        let h = EchoHandler { can: true };
        let mut i = inv();
        let out = dispatch(
            &h,
            &actor(&[]),
            &GateDecision::Hold {
                reason: "uncertain".into(),
            },
            &def(None, None),
            &mut i,
            None,
            1000,
        );
        assert_eq!(out, HandlerOutcome::Postponed);
        assert_eq!(i.state, ActionState::Pending);
    }

    #[test]
    fn mul_block_escalates() {
        let h = EchoHandler { can: true };
        let mut i = inv();
        let out = dispatch(
            &h,
            &actor(&[]),
            &GateDecision::Block {
                reason: "veto".into(),
            },
            &def(None, None),
            &mut i,
            None,
            1000,
        );
        assert_eq!(out, HandlerOutcome::Escalated);
        assert_eq!(i.state, ActionState::Cancelled);
    }

    #[test]
    fn libet_state_guard_unsatisfied_escalates() {
        let h = EchoHandler { can: true };
        let mut i = inv();
        // guard wants status == "ready"; we supply None → veto (Cancelled).
        let out = dispatch(
            &h,
            &actor(&[]),
            &GateDecision::Flow,
            &def(
                None,
                Some(StateGuard {
                    field: "status",
                    value: "ready",
                }),
            ),
            &mut i,
            None,
            1000,
        );
        assert_eq!(out, HandlerOutcome::Escalated);
    }

    #[test]
    fn not_applicable_never_consults_the_cold_path() {
        let h = EchoHandler { can: false }; // capability false → routing miss
        let mut i = inv();
        let out = dispatch(
            &h,
            &actor(&[]),
            &GateDecision::Flow,
            &def(None, None),
            &mut i,
            None,
            1000,
        );
        assert_eq!(out, HandlerOutcome::NotApplicable);
        assert_eq!(i.state, ActionState::Pending); // untouched — never committed
    }
}
