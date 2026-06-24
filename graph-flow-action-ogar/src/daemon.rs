//! B2-transport — the **transport-agnostic** gated-action daemon.
//!
//! HIRO distributes actions over more than one wire: a handler-facing WebSocket
//! (`action-ws`, the documented external-handler protocol) **and** an internal
//! **Kafka** bus that legacy handlers consume directly. The wire differs; the
//! dispatch does not. So this module factors the daemon into three pieces:
//!
//! - [`Daemon`] — the transport-agnostic core. [`Daemon::react`] turns one
//!   inbound `action-ws` message (a JSON frame) into the outbound frames it
//!   warrants, **running the hard gate and the executor in between**. No I/O.
//! - [`Transport`] — the swappable edge. One trait, `recv` + `send` of frames.
//!   [`WsTransport`] is the live `action-ws` WebSocket impl (`feature = "ws"`);
//!   a `KafkaTransport` over the same trait is the reserved second edge.
//! - [`Auth`] — the connection identity, shaped after OGIT `NTO/Auth/Configuration`
//!   (the `auth_store` class, OGAR `0x0B01`): the **same** principal the transport
//!   authenticates as is the actor the gate authorizes (`accountId` → actor).
//!
//! [`Daemon::serve`] is the loop, generic over any [`Transport`]: `recv` a frame,
//! `react`, `send` each reply. The WebSocket and Kafka edges share it verbatim.
//!
//! The dispatch each `submitAction` drives:
//!
//! ```text
//!   submitAction ─► validate id ─► (unknown cap / bad id ⇒ negativeAcknowledged)
//!                 ─► acknowledged{200}
//!                 ─► bind parameters (against the B2-lift signature)
//!                 ─► run_gated: commit_via (RBAC ∧ state-guard ∧ MUL) ─► executor
//!                 ─► sendActionResult{ result: resultParameters | error }
//! ```
//!
//! The gate is [`crate::run_gated`] — the executor runs only after the cold floor
//! commits. A denied / blocked action is acked (the id was valid) then reported
//! as an error in `sendActionResult` (the post-ack convention OGAR's
//! `handle_submit` uses), and the executor never ran.

#![cfg(feature = "daemon")]

use std::collections::BTreeMap;

use serde::Deserialize;

use graph_flow_action::HandlerOutcome;
use lance_graph_contract::action::{ActionDef, ActionInvocation};
use lance_graph_contract::canonical_node::NodeGuid;
use lance_graph_contract::mul::GateDecision;
use lance_graph_contract::rbac::ClassRbac;
use ogar_from_schema::action_ws::{
    bind_parameters, validate_id, CapabilityExecutor, MAX_RESULT_LEN,
};
use ogar_from_schema::do_arm::ActionParam;

use crate::run_gated;

/// The connection identity, shaped after OGIT `NTO/Auth/Configuration` (the
/// `auth_store` class — OGAR codebook `0x0B01`, "keyed by `organizationId` /
/// `accountId` / `applicationId` / `scopeId`"). One value carries both the
/// credential the [`Transport`] presents **and** the principal the gate
/// authorizes as — they must be the same identity, and OGIT's Auth type already
/// unifies them (`auth_store` maps `sub` → actor `0x0104`, org/tenant → scope).
///
/// A future producer-side lift (`auth_from_ogit(entity)`) would populate this
/// from a real `NTO/Auth/Configuration` node; today it is constructed directly.
#[derive(Debug, Clone)]
pub struct Auth {
    /// `accountId` — the principal (`sub`). **The actor the gate authorizes as**
    /// (`auth_store`: `sub` → actor). RBAC grants are checked against this.
    pub account: String,
    /// The bearer credential the transport presents. On `action-ws` it becomes
    /// the `token-$TOKEN` WebSocket subprotocol.
    pub token: String,
    /// `scopeId` — the tenant / organization scope (optional).
    pub scope: Option<String>,
    /// `applicationId` — the registered application this handler serves (optional).
    pub application: Option<String>,
}

impl Auth {
    /// The minimal identity: an `accountId` principal + its bearer `token`
    /// (no scope / application).
    pub fn new(account: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            account: account.into(),
            token: token.into(),
            scope: None,
            application: None,
        }
    }
}

/// One capability the daemon serves: the [`ActionDef`] to gate against (its
/// `predicate` must equal the wire `capability`) plus the concrete
/// [`ActionParam`] signature (from B2-lift's `lift_registration`).
pub struct Route {
    /// The gated action definition. `predicate` is matched against the wire
    /// `capability`; `object_class` / `guard` / `required_role` drive the gate.
    pub action: ActionDef,
    /// The capability's parameter signature — what `bind_parameters` validates
    /// the engine's `parameters` against (mandatory present, defaults filled).
    pub signature: Vec<ActionParam>,
}

/// The swappable transport edge: a duplex stream of `action-ws` JSON frames.
/// [`WsTransport`] is the live WebSocket impl; a Kafka edge implements the same
/// two methods over its action / result topics.
#[allow(async_fn_in_trait)]
pub trait Transport {
    /// Edge error type (handshake / I/O).
    type Error;
    /// The next inbound frame, or `None` when the transport closes.
    async fn recv(&mut self) -> Option<String>;
    /// Send one outbound frame.
    async fn send(&mut self, frame: String) -> Result<(), Self::Error>;
}

/// The transport-agnostic gated-action daemon: a capability routing table + the
/// gate inputs (RBAC, the MUL decision, the acting identity from [`Auth`]),
/// driving the OGAR executor behind [`crate::run_gated`].
///
/// [`react`](Self::react) is pure (frame in → frames out); [`serve`](Self::serve)
/// drives it over any [`Transport`]. The executor is cloned per action (it is the
/// only piece that runs real I/O, and `run_gated` consumes it by value).
pub struct Daemon<R: ClassRbac, E: CapabilityExecutor + Clone> {
    routes: BTreeMap<String, Route>,
    rbac: R,
    executor: E,
    actor: String,
    gate: GateDecision,
    now_millis: u64,
}

impl<R: ClassRbac, E: CapabilityExecutor + Clone> Daemon<R, E> {
    /// Build a daemon. The acting identity is taken from `auth.account` (the OGIT
    /// `accountId` principal) — the gate authorizes every action as that actor,
    /// the same identity the transport authenticated. `gate` is the MUL decision
    /// applied to every action (wire a real homeostasis provider for production;
    /// a fixed `GateDecision::Flow` is the no-veto default). `now_millis` stamps
    /// the invocations (the `serve` loop can refresh it per message).
    pub fn new(rbac: R, executor: E, auth: &Auth, gate: GateDecision, now_millis: u64) -> Self {
        Self {
            routes: BTreeMap::new(),
            rbac,
            executor,
            actor: auth.account.clone(),
            gate,
            now_millis,
        }
    }

    /// Register a capability route (builder style). The route's
    /// `action.predicate` is the wire `capability` the engine will send.
    #[must_use]
    pub fn with_route(mut self, route: Route) -> Self {
        self.routes.insert(route.action.predicate.to_owned(), route);
        self
    }

    /// Serve the daemon over a [`Transport`] until it closes: `recv` each frame,
    /// [`react`](Self::react), `send` every reply. The WebSocket and Kafka edges
    /// share this loop unchanged.
    ///
    /// # Errors
    /// Propagates the transport's send error.
    pub async fn serve<T: Transport>(&self, mut transport: T) -> Result<(), T::Error> {
        while let Some(frame) = transport.recv().await {
            for reply in self.react(&frame) {
                transport.send(reply).await?;
            }
        }
        Ok(())
    }

    /// React to one inbound `action-ws` JSON frame, returning the outbound JSON
    /// frames to send back (zero or more). Pure — no I/O. Unknown / non-actionable
    /// message types (`acknowledged`, `error`, `configChanged`) yield no frames.
    #[must_use]
    pub fn react(&self, frame: &str) -> Vec<String> {
        let value: serde_json::Value = match serde_json::from_str(frame) {
            Ok(v) => v,
            // A malformed frame can't be correlated to an id — drop it (the live
            // loop logs; the engine will time out and retry).
            Err(_) => return Vec::new(),
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("submitAction") => match serde_json::from_value::<WireSubmit>(value) {
                Ok(submit) => self.on_submit(submit),
                Err(_) => Vec::new(),
            },
            // configChanged ⇒ the handler should re-GET its registration; the
            // live edge handles the re-fetch, the pure core has nothing to emit.
            // acknowledged / negativeAcknowledged / error are engine→handler
            // receipts with no reply.
            _ => Vec::new(),
        }
    }

    fn on_submit(&self, submit: WireSubmit) -> Vec<String> {
        let Some(route) = self.routes.get(&submit.capability) else {
            // Reject before ack: we don't serve this capability.
            return vec![nack(&submit.id, 400, "unknown capability")];
        };
        dispatch_action(
            &submit.id,
            &submit.capability,
            submit.parameters,
            &route.action,
            &route.signature,
            self.executor.clone(),
            &self.rbac,
            self.actor.as_str(),
            &self.gate,
            self.now_millis,
        )
    }
}

/// The shared gated-dispatch core for one routed `submitAction`: validate id →
/// ack → bind against `signature` → `run_gated` (`commit_via`: RBAC ∧ guard ∧
/// MUL) → `sendActionResult`. Both the static [`Daemon`] (executor wired) and the
/// late-binding [`ResolvingDaemon`] (executor resolved from the classid) funnel
/// here — they differ only in how they pick `action` / `signature` / `executor`.
#[allow(clippy::too_many_arguments)]
fn dispatch_action<E: CapabilityExecutor + Clone, R: ClassRbac>(
    id: &str,
    capability: &str,
    parameters: BTreeMap<String, String>,
    action: &ActionDef,
    signature: &[ActionParam],
    executor: E,
    rbac: &R,
    actor: &str,
    gate: &GateDecision,
    now_millis: u64,
) -> Vec<String> {
    if validate_id(id).is_err() {
        return vec![nack(id, 400, "invalid action id")];
    }

    // The id is valid and routed — acknowledge, then report the outcome as a
    // sendActionResult (post-ack convention: failures go in the result).
    let mut frames = vec![ack(id)];

    let supplied: Vec<(String, String)> = parameters.into_iter().collect();
    let bound = match bind_parameters(&supplied, signature) {
        Ok(bound) => bound,
        Err(err) => {
            frames.push(result_frame(id, &error_result(&err.to_string())));
            return frames;
        }
    };

    // The Rubicon invocation for the gate (object resolved from object_class; a
    // real node store would resolve the concrete target — None guard value until
    // one is wired).
    let mut inv = ActionInvocation::pending(
        action.object_class,
        action.predicate,
        NodeGuid::new(action.object_class, 0, 0, 0, 0, 0),
        1,
        0,
        0,
    );

    let (outcome, exec_result) = run_gated(
        executor,
        capability.to_owned(),
        &bound,
        rbac,
        actor,
        gate,
        action,
        &mut inv,
        None,
        now_millis,
    );

    let result = match (outcome, exec_result) {
        (HandlerOutcome::Done, Some(Ok(params))) => ok_result(&params),
        // Post-commit executor failure (authorized, but the command failed).
        (_, Some(Err(message))) => error_result(&message),
        // The gate refused — the executor never ran.
        (HandlerOutcome::Denied, _) => error_result("denied: RBAC grant missing"),
        (HandlerOutcome::Postponed, _) => error_result("postponed: MUL hold"),
        (HandlerOutcome::Escalated, _) => error_result("escalated: MUL block or guard veto"),
        (HandlerOutcome::NotApplicable, _) => error_result("not applicable to this node"),
        (HandlerOutcome::Done, None) => error_result("internal: committed without a result"),
    };

    frames.push(result_frame(id, &cap_result_len(result)));
    frames
}

// ──────────────────────────────────── class-late-bound dispatch ──
//
// The third axis of agnosticism. The static `Daemon` wires the action class +
// executor at build time (its `routes` map + single `executor`). The
// `ResolvingDaemon` wires neither: it resolves the action class from the target
// node's **classid** per action (`ClassResolver`), and picks the executor from
// what that class resolves to (`RunnerKind` → `ExecutorRegistry`). One binary
// then serves every class on every transport; a new capability / class / runner
// is a registry entry, never a daemon change — OGAR's "the key prerenders the
// node; classid → ClassView" applied to the action arm.

/// Which concrete executor runs a resolved action — the runner the class resolves
/// to. Distinct from the planner's `ExecTarget` (how a *plan* executes); this is
/// *which runner* performs the side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    /// Local command (e.g. OGAR `NativeCommandExecutor`).
    Native,
    /// Remote command over SSH (e.g. OGAR `SshExecutor`).
    Ssh,
    /// HTTP callout (e.g. `RestExecutor`).
    Rest,
    /// An application-defined runner, keyed by a small tag.
    Other(u16),
}

/// What a [`ClassResolver`] yields for a `(target, capability)`: the gated action
/// definition, its parameter signature, and the runner its class uses — all read
/// from the resolved class, none wired in the daemon.
pub struct ResolvedAction<'a> {
    /// The gated action definition (its `object_class` is the resolved classid).
    pub action: &'a ActionDef,
    /// The capability's parameter signature (from B2-lift's registration).
    pub signature: &'a [ActionParam],
    /// The runner the resolved class executes on.
    pub runner: RunnerKind,
}

/// Resolve the action for a target (the node the action runs on — the resolver
/// reads its classid) + capability, **late**. This is the holy-grail axis: the
/// class is chosen at dispatch time from the key, not wired at build time. A
/// resolver is typically backed by OGAR's `classid → ClassActions` surface,
/// populated from a deployed handler's B2-lift registration.
pub trait ClassResolver {
    /// Resolve `(target, capability)` → the action, or `None` if this daemon
    /// serves no such class/capability (→ `negativeAcknowledged`).
    fn resolve(&self, target: Option<&str>, capability: &str) -> Option<ResolvedAction<'_>>;
}

/// Run a capability on the concrete executor for a resolved [`RunnerKind`]. The
/// registry owns the executors (native / ssh / rest / …); the daemon never names
/// them — it asks the registry to run whatever the class resolved to.
pub trait ExecutorRegistry {
    /// Execute `capability` with `bound` params on the executor for `runner`.
    fn run(
        &self,
        runner: RunnerKind,
        capability: &str,
        bound: &[(String, String)],
    ) -> Result<Vec<(String, String)>, String>;
}

/// Adapter: a [`CapabilityExecutor`] that forwards to an [`ExecutorRegistry`] for
/// one `runner`. Lets `run_gated` (which wants a `CapabilityExecutor`) drive the
/// registry, so the concrete executor is chosen **post-commit** by the resolved
/// runner — the gate runs first, exactly as for a wired executor.
struct RegistryExecutor<'a, X: ExecutorRegistry> {
    registry: &'a X,
    runner: RunnerKind,
}

// Manual Clone/Copy so the impl does NOT require `X: Copy` (only `&X` + the tag,
// both Copy, are stored).
impl<X: ExecutorRegistry> Clone for RegistryExecutor<'_, X> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<X: ExecutorRegistry> Copy for RegistryExecutor<'_, X> {}

impl<X: ExecutorRegistry> CapabilityExecutor for RegistryExecutor<'_, X> {
    fn execute(
        &self,
        capability: &str,
        bound: &[(String, String)],
    ) -> Result<Vec<(String, String)>, String> {
        self.registry.run(self.runner, capability, bound)
    }
}

/// The class-late-bound daemon: the holy grail. It holds **no** static action
/// classes and **no** wired executor — only a [`ClassResolver`] (class chosen
/// from the target's classid) and an [`ExecutorRegistry`] (executor chosen from
/// the resolved [`RunnerKind`]). The same submitAction can dispatch to native,
/// SSH, or REST purely by what its target's classid resolves to, with zero
/// daemon change — and every action still passes the same hard gate.
///
/// Transport-agnostic ([`serve`](Self::serve) over any [`Transport`]),
/// class-agnostic (the resolver), executor-agnostic (the registry): the three
/// axes meet here, all keyed by the GUID.
pub struct ResolvingDaemon<R: ClassResolver, X: ExecutorRegistry, Rbac: ClassRbac> {
    resolver: R,
    registry: X,
    rbac: Rbac,
    actor: String,
    gate: GateDecision,
    now_millis: u64,
}

impl<R: ClassResolver, X: ExecutorRegistry, Rbac: ClassRbac> ResolvingDaemon<R, X, Rbac> {
    /// Build a resolving daemon from a `resolver` (class ← classid), a `registry`
    /// (executor ← runner), `rbac`, the [`Auth`] identity (the gate actor), the
    /// MUL `gate`, and the invocation clock.
    pub fn new(
        resolver: R,
        registry: X,
        rbac: Rbac,
        auth: &Auth,
        gate: GateDecision,
        now_millis: u64,
    ) -> Self {
        Self {
            resolver,
            registry,
            rbac,
            actor: auth.account.clone(),
            gate,
            now_millis,
        }
    }

    /// Serve over a [`Transport`] until it closes — identical loop to
    /// [`Daemon::serve`], dispatching through the resolver + registry.
    ///
    /// # Errors
    /// Propagates the transport's send error.
    pub async fn serve<T: Transport>(&self, mut transport: T) -> Result<(), T::Error> {
        while let Some(frame) = transport.recv().await {
            for reply in self.react(&frame) {
                transport.send(reply).await?;
            }
        }
        Ok(())
    }

    /// React to one inbound `action-ws` frame (pure, no I/O), resolving the class
    /// from the target's classid and the executor from the resolved runner.
    #[must_use]
    pub fn react(&self, frame: &str) -> Vec<String> {
        let value: serde_json::Value = match serde_json::from_str(frame) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("submitAction") => match serde_json::from_value::<WireSubmit>(value) {
                Ok(submit) => self.on_submit(submit),
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn on_submit(&self, submit: WireSubmit) -> Vec<String> {
        let Some(resolved) = self
            .resolver
            .resolve(submit.node.as_deref(), &submit.capability)
        else {
            // No class/capability resolved for this target — reject before ack.
            return vec![nack(
                &submit.id,
                400,
                "unknown capability or unresolved class",
            )];
        };
        let executor = RegistryExecutor {
            registry: &self.registry,
            runner: resolved.runner,
        };
        dispatch_action(
            &submit.id,
            &submit.capability,
            submit.parameters,
            resolved.action,
            resolved.signature,
            executor,
            &self.rbac,
            self.actor.as_str(),
            &self.gate,
            self.now_millis,
        )
    }
}

/// A `submitAction` frame as it arrives on the wire (`parameters` is a JSON
/// object, unlike the action_ws domain type's pair list).
#[derive(Debug, Deserialize)]
struct WireSubmit {
    id: String,
    capability: String,
    #[serde(default)]
    #[allow(dead_code)]
    handler: String,
    #[serde(default)]
    #[allow(dead_code)]
    scope: Option<String>,
    /// The target node the action runs on (its id / GUID). A `ClassResolver`
    /// reads its classid to choose the action class late. Optional: a static
    /// [`Daemon`] ignores it (it routes by capability); a [`ResolvingDaemon`]
    /// passes it to the resolver.
    #[serde(default, alias = "target", alias = "targetNode")]
    node: Option<String>,
    #[serde(default)]
    parameters: BTreeMap<String, String>,
    #[serde(default, rename = "timeout")]
    #[allow(dead_code)]
    timeout_millis: Option<i64>,
}

fn ack(id: &str) -> String {
    serde_json::json!({ "type": "acknowledged", "id": id, "code": 200, "message": "accepted" })
        .to_string()
}

fn nack(id: &str, code: u16, message: &str) -> String {
    serde_json::json!({ "type": "negativeAcknowledged", "id": id, "code": code, "message": message })
        .to_string()
}

/// Build a `sendActionResult` frame. Per spec, `result` is a single STRING (the
/// `resultParameters` JSON-encoded), not a nested object.
fn result_frame(id: &str, result: &str) -> String {
    serde_json::json!({ "type": "sendActionResult", "id": id, "result": result }).to_string()
}

/// Encode the bound `resultParameters` as the result string (a JSON object).
fn ok_result(params: &[(String, String)]) -> String {
    let map: BTreeMap<&str, &str> = params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    serde_json::to_string(&map).unwrap_or_else(|_| r#"{"error":"result encode failed"}"#.to_owned())
}

/// Encode an error as the result string (the OGAR `{"error":…}` convention).
fn error_result(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

/// Enforce the spec's 1 MiB `result` bound.
fn cap_result_len(result: String) -> String {
    if result.len() > MAX_RESULT_LEN {
        error_result("result too large (exceeds 1 MiB)")
    } else {
        result
    }
}

// ─────────────────────────────────────────────── ws transport edge ──

/// The live `action-ws` WebSocket [`Transport`] (`feature = "ws"`).
#[cfg(feature = "ws")]
mod ws {
    use super::{Auth, Transport};
    use futures_util::{SinkExt, StreamExt};
    use ogar_from_schema::action_ws::{auth_subprotocol, ACTION_WS_PATH};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    /// A live action-ws transport error (connect / protocol / I/O).
    #[derive(Debug)]
    pub enum WsError {
        /// Building the connect request failed (bad URL / header).
        Request(String),
        /// The WebSocket handshake or I/O failed.
        Io(String),
    }

    impl std::fmt::Display for WsError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                WsError::Request(m) => write!(f, "action-ws request: {m}"),
                WsError::Io(m) => write!(f, "action-ws io: {m}"),
            }
        }
    }

    impl std::error::Error for WsError {}

    /// A connected `action-ws` WebSocket, presenting the [`Auth`] token as the
    /// `token-$TOKEN` subprotocol. Implements [`Transport`].
    pub struct WsTransport {
        socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    }

    impl WsTransport {
        /// The HIRO connect URL for a host (`wss://<host>{ACTION_WS_PATH}`).
        #[must_use]
        pub fn connect_url(host: &str) -> String {
            format!("wss://{host}{ACTION_WS_PATH}")
        }

        /// Connect to an `action-ws` endpoint at `url`, presenting `auth.token`
        /// as the `token-$TOKEN` subprotocol. `url` is `ws(s)://…{ACTION_WS_PATH}`
        /// (see [`connect_url`](Self::connect_url)); use `ws://` for a plaintext
        /// test server.
        ///
        /// # Errors
        /// Returns [`WsError`] on a bad request or a failed handshake.
        pub async fn connect(url: &str, auth: &Auth) -> Result<Self, WsError> {
            let mut request = url
                .into_client_request()
                .map_err(|e| WsError::Request(e.to_string()))?;
            request.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                auth_subprotocol(&auth.token)
                    .parse()
                    .map_err(|_| WsError::Request("bad token header".to_owned()))?,
            );
            let (socket, _response) = tokio_tungstenite::connect_async(request)
                .await
                .map_err(|e| WsError::Io(e.to_string()))?;
            Ok(Self { socket })
        }
    }

    impl Transport for WsTransport {
        type Error = WsError;

        async fn recv(&mut self) -> Option<String> {
            while let Some(message) = self.socket.next().await {
                match message {
                    Ok(Message::Text(text)) => return Some(text),
                    Ok(Message::Close(_)) | Err(_) => return None,
                    Ok(Message::Ping(payload)) => {
                        // Keepalive — answer and keep reading.
                        let _ = self.socket.send(Message::Pong(payload)).await;
                    }
                    Ok(_) => {}
                }
            }
            None
        }

        async fn send(&mut self, frame: String) -> Result<(), WsError> {
            self.socket
                .send(Message::Text(frame))
                .await
                .map_err(|e| WsError::Io(e.to_string()))
        }
    }
}

#[cfg(feature = "ws")]
pub use ws::{WsError, WsTransport};

#[cfg(test)]
mod tests {
    use super::*;
    use lance_graph_contract::kanban::ExecTarget;
    use lance_graph_contract::rbac::{ActorId, ClassId, Operation, RoleId};
    use ogar_action_handler::NativeCommandExecutor;

    const MARS_MACHINE: u32 = 0x0000_0C04;

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

    fn execute_command_route() -> Route {
        Route {
            action: ActionDef {
                predicate: "ExecuteCommand",
                object_class: MARS_MACHINE,
                exec: ExecTarget::Native,
                guard: None,
                required_role: Some("automation_operator"),
                overrides: None,
            },
            signature: vec![ActionParam {
                name: "command".to_owned(),
                mandatory: true,
                default: None,
            }],
        }
    }

    fn daemon(account: &str, gate: GateDecision) -> Daemon<OpsRbac, NativeCommandExecutor> {
        let auth = Auth::new(account, "test-token");
        Daemon::new(OpsRbac, NativeCommandExecutor, &auth, gate, 1000)
            .with_route(execute_command_route())
    }

    fn submit_frame(id: &str, command: &str) -> String {
        serde_json::json!({
            "type": "submitAction",
            "id": id,
            "capability": "ExecuteCommand",
            "handler": "h1",
            "parameters": { "command": command }
        })
        .to_string()
    }

    fn parse(frame: &str) -> serde_json::Value {
        serde_json::from_str(frame).expect("valid json frame")
    }

    #[test]
    fn authorized_submit_acks_then_returns_the_result() {
        let d = daemon("ops-1", GateDecision::Flow);
        let frames = d.react(&submit_frame("app:req-000001", "echo wired"));
        assert_eq!(frames.len(), 2);
        let ack = parse(&frames[0]);
        assert_eq!(ack["type"], "acknowledged");
        assert_eq!(ack["code"], 200);
        let res = parse(&frames[1]);
        assert_eq!(res["type"], "sendActionResult");
        assert_eq!(res["id"], "app:req-000001");
        // result is a STRING containing the resultParameters object.
        let inner: serde_json::Value =
            serde_json::from_str(res["result"].as_str().unwrap()).unwrap();
        assert_eq!(inner["output"], "wired");
    }

    #[test]
    fn unauthorized_submit_acks_then_reports_denied_without_running() {
        // intruder has no role → the gate Denies; the executor never runs, so
        // the result carries the error, not command output.
        let d = daemon("intruder", GateDecision::Flow);
        let frames = d.react(&submit_frame("app:req-000002", "echo SHOULD_NOT_RUN"));
        assert_eq!(frames.len(), 2);
        assert_eq!(parse(&frames[0])["type"], "acknowledged");
        let res = parse(&frames[1]);
        let inner: serde_json::Value =
            serde_json::from_str(res["result"].as_str().unwrap()).unwrap();
        assert!(
            inner["error"].as_str().unwrap().contains("denied"),
            "got: {inner}"
        );
        assert!(inner.get("output").is_none(), "executor must not have run");
    }

    #[test]
    fn mul_block_acks_then_reports_escalated_without_running() {
        let d = daemon(
            "ops-1",
            GateDecision::Block {
                reason: "human veto".to_owned(),
            },
        );
        let frames = d.react(&submit_frame("app:req-000003", "echo nope"));
        let res = parse(&frames[1]);
        let inner: serde_json::Value =
            serde_json::from_str(res["result"].as_str().unwrap()).unwrap();
        assert!(inner["error"].as_str().unwrap().contains("escalated"));
        assert!(inner.get("output").is_none());
    }

    #[test]
    fn unknown_capability_is_nacked_before_ack() {
        let d = daemon("ops-1", GateDecision::Flow);
        let frame = serde_json::json!({
            "type": "submitAction",
            "id": "app:req-000004",
            "capability": "RunScript",
            "parameters": {}
        })
        .to_string();
        let frames = d.react(&frame);
        assert_eq!(frames.len(), 1);
        let nack = parse(&frames[0]);
        assert_eq!(nack["type"], "negativeAcknowledged");
        assert_eq!(nack["code"], 400);
    }

    #[test]
    fn invalid_id_is_nacked_before_ack() {
        let d = daemon("ops-1", GateDecision::Flow);
        // id below the 12-char minimum.
        let frames = d.react(&submit_frame("short", "echo x"));
        assert_eq!(frames.len(), 1);
        assert_eq!(parse(&frames[0])["type"], "negativeAcknowledged");
    }

    #[test]
    fn missing_mandatory_param_acks_then_errors_in_result() {
        let d = daemon("ops-1", GateDecision::Flow);
        let frame = serde_json::json!({
            "type": "submitAction",
            "id": "app:req-000005",
            "capability": "ExecuteCommand",
            "parameters": {}
        })
        .to_string();
        let frames = d.react(&frame);
        assert_eq!(frames.len(), 2);
        assert_eq!(parse(&frames[0])["type"], "acknowledged");
        let res = parse(&frames[1]);
        let inner: serde_json::Value =
            serde_json::from_str(res["result"].as_str().unwrap()).unwrap();
        assert!(inner.get("error").is_some());
    }

    #[test]
    fn non_actionable_frames_yield_nothing() {
        let d = daemon("ops-1", GateDecision::Flow);
        assert!(d.react(r#"{"type":"configChanged"}"#).is_empty());
        assert!(d
            .react(r#"{"type":"acknowledged","id":"x","code":200,"message":"ok"}"#)
            .is_empty());
        assert!(d.react("not json").is_empty());
    }

    /// The live ws edge: a mock action-ws server sends a submitAction; the daemon
    /// connects via [`WsTransport`] and serves it — proving the transport-agnostic
    /// core drives a real socket through the [`Transport`] trait.
    #[cfg(feature = "ws")]
    #[tokio::test]
    #[allow(clippy::result_large_err)] // tungstenite's handshake callback Result shape
    async fn ws_roundtrip_against_a_mock_server() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
            let (stream, _) = listener.accept().await.unwrap();
            // Echo the client's `token-$TOKEN` subprotocol back (as real HIRO
            // does) — tungstenite's client rejects a handshake where it offered a
            // subprotocol and the server selected none.
            let echo_token = |req: &Request, mut response: Response| {
                if let Some(proto) = req.headers().get("sec-websocket-protocol") {
                    response
                        .headers_mut()
                        .insert("sec-websocket-protocol", proto.clone());
                }
                Ok(response)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, echo_token)
                .await
                .unwrap();
            // Engine → handler: submitAction.
            ws.send(Message::Text(submit_frame(
                "app:req-000099",
                "echo socketed",
            )))
            .await
            .unwrap();
            // Handler → engine: acknowledged, then sendActionResult.
            let ack = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(parse(&ack)["type"], "acknowledged");
            let result = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let res = parse(&result);
            assert_eq!(res["type"], "sendActionResult");
            let inner: serde_json::Value =
                serde_json::from_str(res["result"].as_str().unwrap()).unwrap();
            assert_eq!(inner["output"], "socketed");
            ws.close(None).await.unwrap();
        });

        let url = format!("ws://{addr}/");
        let auth = Auth::new("ops-1", "test-token");
        let transport = WsTransport::connect(&url, &auth).await.unwrap();
        daemon("ops-1", GateDecision::Flow)
            .serve(transport)
            .await
            .unwrap();
        server.await.unwrap();
    }

    // ───────────────────────── the holy grail: class-late-bound dispatch ──

    /// `mars_resource` concept (0x0C02) — a second class, to show the classid
    /// (not a wired route) drives which executor runs.
    const MARS_RESOURCE: u32 = 0x0000_0C02;

    /// Grants `automation_operator` ACT on BOTH mars_machine and mars_resource.
    struct GrailRbac;
    impl ClassRbac for GrailRbac {
        fn actor_roles(&self, actor: ActorId<'_>) -> &[RoleId] {
            match actor {
                "ops-1" => &["automation_operator"],
                _ => &[],
            }
        }
        fn grant_permits(&self, role: RoleId, class: ClassId, op: &Operation<'_>) -> bool {
            role == "automation_operator"
                && (class as u16 == MARS_MACHINE as u16 || class as u16 == MARS_RESOURCE as u16)
                && matches!(op, Operation::Act { .. })
        }
    }

    /// Resolves the SAME capability (`ExecuteCommand`) to different classes +
    /// runners by the target node's classid — the late binding. (Here a name
    /// stands in for the target's classid; a real resolver reads the classid
    /// prefix of the target GUID via OGAR's `classid → ClassActions` surface.)
    struct DemoResolver {
        native: (ActionDef, Vec<ActionParam>),
        rest: (ActionDef, Vec<ActionParam>),
    }
    impl DemoResolver {
        fn new() -> Self {
            let sig = || {
                vec![ActionParam {
                    name: "command".to_owned(),
                    mandatory: true,
                    default: None,
                }]
            };
            let def = |object_class| ActionDef {
                predicate: "ExecuteCommand",
                object_class,
                exec: ExecTarget::Native,
                guard: None,
                required_role: Some("automation_operator"),
                overrides: None,
            };
            Self {
                native: (def(MARS_MACHINE), sig()),
                rest: (def(MARS_RESOURCE), sig()),
            }
        }
    }
    impl ClassResolver for DemoResolver {
        fn resolve(&self, target: Option<&str>, capability: &str) -> Option<ResolvedAction<'_>> {
            if capability != "ExecuteCommand" {
                return None;
            }
            match target {
                // host-A's classid is mars_machine → run it natively.
                Some("host-A") => Some(ResolvedAction {
                    action: &self.native.0,
                    signature: &self.native.1,
                    runner: RunnerKind::Native,
                }),
                // svc-B's classid is mars_resource → run it as a REST callout.
                Some("svc-B") => Some(ResolvedAction {
                    action: &self.rest.0,
                    signature: &self.rest.1,
                    runner: RunnerKind::Rest,
                }),
                _ => None,
            }
        }
    }

    /// Maps a resolved runner to a concrete executor. Native runs for real; Rest
    /// is faked (the real `RestExecutor` needs a live endpoint — proven in its own
    /// test). The daemon never names either; it asks the registry to run whatever
    /// the class resolved to.
    struct DemoRegistry;
    impl ExecutorRegistry for DemoRegistry {
        fn run(
            &self,
            runner: RunnerKind,
            capability: &str,
            bound: &[(String, String)],
        ) -> Result<Vec<(String, String)>, String> {
            match runner {
                RunnerKind::Native => NativeCommandExecutor.execute(capability, bound),
                RunnerKind::Rest => Ok(vec![("ran".to_owned(), "rest".to_owned())]),
                other => Err(format!("no executor for {other:?}")),
            }
        }
    }

    fn resolving_daemon(account: &str) -> ResolvingDaemon<DemoResolver, DemoRegistry, GrailRbac> {
        let auth = Auth::new(account, "tok");
        ResolvingDaemon::new(
            DemoResolver::new(),
            DemoRegistry,
            GrailRbac,
            &auth,
            GateDecision::Flow,
            1000,
        )
    }

    fn targeted_submit(id: &str, node: &str, command: &str) -> String {
        serde_json::json!({
            "type": "submitAction", "id": id, "capability": "ExecuteCommand",
            "node": node, "parameters": { "command": command }
        })
        .to_string()
    }

    /// THE GRAIL: one submitAction, one capability — but the executor is chosen
    /// late, from what the target's classid resolves to. host-A (mars_machine) →
    /// native runs the real command; svc-B (mars_resource) → the REST runner.
    /// Zero daemon change between the two; only the resolution differs.
    #[test]
    fn same_capability_dispatches_by_resolved_class_to_different_runners() {
        let d = resolving_daemon("ops-1");

        let fa = d.react(&targeted_submit("app:req-aaaaaa", "host-A", "echo grail"));
        assert_eq!(parse(&fa[0])["type"], "acknowledged");
        let res_a: serde_json::Value =
            serde_json::from_str(parse(&fa[1])["result"].as_str().unwrap()).unwrap();
        assert_eq!(
            res_a["output"], "grail",
            "host-A → native runner ran the command"
        );

        let fb = d.react(&targeted_submit("app:req-bbbbbb", "svc-B", "echo grail"));
        let res_b: serde_json::Value =
            serde_json::from_str(parse(&fb[1])["result"].as_str().unwrap()).unwrap();
        assert_eq!(res_b["ran"], "rest", "svc-B → REST runner, same capability");
    }

    /// The gate still rules: an unauthorized actor is `Denied` and NEITHER
    /// resolved runner executes — late binding does not weaken the hard floor.
    #[test]
    fn resolving_daemon_unauthorized_is_denied_without_running() {
        let d = resolving_daemon("intruder");
        let frames = d.react(&targeted_submit(
            "app:req-cccccc",
            "host-A",
            "echo SHOULD_NOT_RUN",
        ));
        assert_eq!(parse(&frames[0])["type"], "acknowledged");
        let res: serde_json::Value =
            serde_json::from_str(parse(&frames[1])["result"].as_str().unwrap()).unwrap();
        assert!(res["error"].as_str().unwrap().contains("denied"));
        assert!(
            res.get("output").is_none(),
            "the resolved executor never ran"
        );
    }

    /// A target whose classid resolves to no served class is rejected before ack.
    #[test]
    fn resolving_daemon_unresolved_target_is_nacked() {
        let d = resolving_daemon("ops-1");
        let frames = d.react(&targeted_submit("app:req-dddddd", "unknown-host", "echo x"));
        assert_eq!(frames.len(), 1);
        assert_eq!(parse(&frames[0])["type"], "negativeAcknowledged");
    }
}
