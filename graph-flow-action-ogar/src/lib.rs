//! `graph-flow-action-ogar` — **uplink** OGAR's hard ActionHandler into the
//! hard-gated dispatch.
//!
//! OGAR owns the **executor** (the `CapabilityExecutor` that actually runs a
//! capability — `ogar-action-handler::NativeCommandExecutor` and friends).
//! `graph-flow-action` owns the **hard gate**: [`dispatch_via`] runs the cold
//! floor (`ActionInvocation::commit_via`: def-match → **RBAC** (`ClassRbac`
//! grant) → **state-guard** (Libet do/don't) → **MUL** impact) and reaches the
//! hot path (`handle`) **only** when that returns `Committed`.
//!
//! This crate is the seam between them: [`GatedOgarHandler`] wraps an OGAR
//! `CapabilityExecutor` in a `graph-flow-action::ActionHandler`, so the executor
//! runs **only after the contract lands**. An unauthorized, guard-vetoed, or
//! MUL-blocked action never reaches the OGAR executor — proven by the tests
//! (`take_result()` is `None` whenever the gate refused).
//!
//! Dependency hygiene: `graph-flow-action`'s dep list is intentionally
//! `contract`-only (`I-ACTIONHANDLER-IS-KGV-NOT-CHOKEPOINT`). The OGAR coupling
//! lives **here**, not there. OGAR's `ogar-from-schema` carries no
//! `lance-graph` dependency, so the two sides meet only at this crate's API —
//! no second `lance-graph-contract` in the graph.

#![forbid(unsafe_code)]

use std::cell::RefCell;

use graph_flow_action::{dispatch_via, ActionHandler, HandlerOutcome};
use lance_graph_contract::action::{ActionDef, ActionInvocation};
use lance_graph_contract::mul::GateDecision;
use lance_graph_contract::rbac::{ActorId, ClassRbac};
use ogar_from_schema::action_ws::CapabilityExecutor;

/// The `auth_store` classid (`0x0B01`) the handler authorizes through — its
/// `Configuration` membrane (HIRO `ActionHandler → Configuration`).
const AUTH_STORE: u32 = 0x0000_0B01;

/// The outcome of running a capability: the bound `resultParameters` on success,
/// or an error message (an OGAR [`CapabilityExecutor::execute`] result).
pub type ExecResult = Result<Vec<(String, String)>, String>;

/// Wraps an OGAR [`CapabilityExecutor`] as a `graph-flow-action::ActionHandler`,
/// so the executor runs behind the hard gate.
///
/// `handle` (the hot path) is reached **only** after `dispatch_via`'s cold floor
/// commits, so the OGAR executor runs exactly when — and only when — the
/// contract (`commit_via`: RBAC ∧ state-guard ∧ MUL) has landed. The execution
/// result is captured (interior mutability, since `handle` takes `&self`) and
/// read back via [`take_result`](Self::take_result).
pub struct GatedOgarHandler<'p, E: CapabilityExecutor> {
    executor: E,
    capability: String,
    bound: &'p [(String, String)],
    result: RefCell<Option<ExecResult>>,
}

impl<'p, E: CapabilityExecutor> GatedOgarHandler<'p, E> {
    /// Build a gated handler for one capability invocation: the OGAR `executor`,
    /// the `capability` name (matched against [`ActionDef::predicate`]), and the
    /// already-bound `(name, value)` parameters.
    pub fn new(executor: E, capability: impl Into<String>, bound: &'p [(String, String)]) -> Self {
        Self {
            executor,
            capability: capability.into(),
            bound,
            result: RefCell::new(None),
        }
    }

    /// The execution result, available after a `dispatch_via`. **`None` iff the
    /// hard gate refused** (the executor never ran) — the structural proof the
    /// contract lands before execution. `Some(Ok(..))` on success;
    /// `Some(Err(..))` on a post-commit executor failure.
    #[must_use]
    pub fn take_result(&self) -> Option<ExecResult> {
        self.result.borrow_mut().take()
    }
}

impl<E: CapabilityExecutor> ActionHandler for GatedOgarHandler<'_, E> {
    fn configuration(&self) -> u32 {
        AUTH_STORE
    }

    fn capability(&self, action: &ActionDef) -> bool {
        // Routing only (HIRO ActionCapability) — "am I the executor for this?".
        // Authorization is the cold floor's, not here.
        self.capability == action.predicate
    }

    fn applicability(&self, _inv: &ActionInvocation) -> bool {
        true
    }

    fn handle(&self, _action: &ActionDef, _inv: &mut ActionInvocation) -> HandlerOutcome {
        // Reached ONLY post-commit — the contract already landed. Run the OGAR
        // executor (the real command) and stash its result.
        let result = self.executor.execute(&self.capability, self.bound);
        let ok = result.is_ok();
        *self.result.borrow_mut() = Some(result);
        if ok {
            HandlerOutcome::Done
        } else {
            // A post-acceptance executor failure is escalated (not Denied — the
            // action WAS authorized; it failed at the hot path).
            HandlerOutcome::Escalated
        }
    }
}

/// Run an OGAR `executor` through the hard gate in one call: build a
/// [`GatedOgarHandler`], drive `dispatch_via` (routing → cold floor → hot path),
/// and return the dispatch [`HandlerOutcome`] plus the execution result.
///
/// The result is `None` whenever the gate refused (RBAC `Denied`, MUL `Hold`
/// `Postponed`, MUL `Block` / guard veto `Escalated`, or routing
/// `NotApplicable`) — the executor never ran. `Some(Ok/Err)` only after a
/// committed action reached the OGAR executor.
#[allow(clippy::too_many_arguments)]
pub fn run_gated<E: CapabilityExecutor, R: ClassRbac>(
    executor: E,
    capability: impl Into<String>,
    bound: &[(String, String)],
    rbac: &R,
    actor_id: ActorId<'_>,
    gate: &GateDecision,
    action: &ActionDef,
    inv: &mut ActionInvocation,
    guard_field_value: Option<&str>,
    now_millis: u64,
) -> (HandlerOutcome, Option<ExecResult>) {
    let handler = GatedOgarHandler::new(executor, capability, bound);
    let outcome = dispatch_via(
        &handler,
        rbac,
        actor_id,
        gate,
        action,
        inv,
        guard_field_value,
        now_millis,
    );
    (outcome, handler.take_result())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lance_graph_contract::action::ActionDef;
    use lance_graph_contract::canonical_node::NodeGuid;
    use lance_graph_contract::kanban::ExecTarget;
    use lance_graph_contract::rbac::{ClassId, Operation, RoleId};
    use ogar_action_handler::NativeCommandExecutor;

    /// `mars_machine` concept (0x0C04) — the MARS node an ExecuteCommand targets.
    const MARS_MACHINE: u32 = 0x0000_0C04;

    /// Grants `automation_operator` → ACT on `mars_machine`.
    struct OpsRbac;
    impl ClassRbac for OpsRbac {
        fn actor_roles(&self, actor: ActorId<'_>) -> &[RoleId] {
            match actor {
                "ops-1" => &["automation_operator"],
                _ => &[],
            }
        }
        fn grant_permits(&self, role: RoleId, class: ClassId, op: &Operation<'_>) -> bool {
            role == "automation_operator"
                && class as u16 == MARS_MACHINE as u16
                && matches!(op, Operation::Act { .. })
        }
    }

    fn def() -> ActionDef {
        ActionDef {
            predicate: "ExecuteCommand",
            object_class: MARS_MACHINE,
            exec: ExecTarget::Native,
            guard: None,
            required_role: Some("automation_operator"),
            overrides: None,
        }
    }

    fn inv() -> ActionInvocation {
        ActionInvocation::pending(
            MARS_MACHINE,
            "ExecuteCommand",
            NodeGuid::new(MARS_MACHINE, 0, 0, 0, 0, 0),
            1,
            0,
            0,
        )
    }

    #[test]
    fn authorized_action_passes_the_gate_and_runs_the_command() {
        let bound = vec![("command".to_owned(), "echo gated".to_owned())];
        let mut i = inv();
        let (outcome, result) = run_gated(
            NativeCommandExecutor,
            "ExecuteCommand",
            &bound,
            &OpsRbac,
            "ops-1",
            &GateDecision::Flow,
            &def(),
            &mut i,
            None,
            1000,
        );
        assert_eq!(outcome, HandlerOutcome::Done);
        let ran = result.expect("executor ran").expect("ok");
        assert_eq!(ran[0], ("output".to_owned(), "gated".to_owned()));
    }

    /// The load-bearing test: an unauthorized actor is **Denied at the gate**,
    /// and the OGAR executor **never runs** — the hard contract lands first.
    #[test]
    fn unauthorized_action_is_blocked_before_execution() {
        let bound = vec![("command".to_owned(), "echo SHOULD_NOT_RUN".to_owned())];
        let mut i = inv();
        let (outcome, result) = run_gated(
            NativeCommandExecutor,
            "ExecuteCommand",
            &bound,
            &OpsRbac,
            "intruder", // no roles → no grant
            &GateDecision::Flow,
            &def(),
            &mut i,
            None,
            1000,
        );
        assert_eq!(outcome, HandlerOutcome::Denied);
        assert!(
            result.is_none(),
            "the hard gate must block execution — the OGAR executor never ran"
        );
    }

    #[test]
    fn mul_block_vetoes_before_execution() {
        let bound = vec![("command".to_owned(), "echo nope".to_owned())];
        let mut i = inv();
        let (outcome, result) = run_gated(
            NativeCommandExecutor,
            "ExecuteCommand",
            &bound,
            &OpsRbac,
            "ops-1", // authorized…
            &GateDecision::Block {
                reason: "human veto".into(),
            }, // …but MUL blocks
            &def(),
            &mut i,
            None,
            1000,
        );
        assert_eq!(outcome, HandlerOutcome::Escalated);
        assert!(
            result.is_none(),
            "MUL Block must veto before the executor runs"
        );
    }
}
