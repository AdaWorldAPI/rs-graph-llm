//! A REST [`CapabilityExecutor`] target (`feature = "rest"`) — the arago
//! HTTP-callout ActionHandler shape.
//!
//! Where [`ogar_action_handler::NativeCommandExecutor`] runs a local command,
//! [`RestExecutor`] POSTs the bound action parameters to a configured HTTP
//! endpoint and returns the response as `resultParameters` (`status` + `body`).
//! It is a sync, pure-Rust executor (`ureq`), so it slots into the same sync
//! [`CapabilityExecutor`] seam the gate and the daemon already drive — behind the
//! hard gate exactly like the native one (the gate runs `commit_via` first; this
//! executor only ever runs post-commit).
//!
//! **Trust model:** identical to the native executor — the executor performs the
//! call verbatim once reached; authorization (`commit_via`: RBAC ∧ state-guard ∧
//! MUL) is the gate's job, upstream. A `RestExecutor` should be wired behind
//! [`crate::run_gated`] / the [`crate::daemon::Daemon`], never called raw on
//! untrusted input.
//!
//! **Runtime note:** like the native executor, this does blocking I/O. The daemon
//! runs executors inline today (fine for short callouts); a `spawn_blocking`
//! offload is the hardening path if a slow endpoint must not stall the loop.

use std::collections::BTreeMap;

use ogar_from_schema::action_ws::CapabilityExecutor;

/// A [`CapabilityExecutor`] that calls an HTTP endpoint: the bound action
/// parameters become a JSON request body; the response `status` + `body` come
/// back as `resultParameters`.
///
/// Any completed HTTP response (including 4xx/5xx) is a successful
/// `resultParameters` (status carried in the `status` key) — only a transport
/// failure (connection refused, DNS) is an executor `Err`. This mirrors how an
/// arago REST handler reports the callee's response rather than swallowing it.
///
/// `Clone` (the inner `ureq::Agent` is `Arc`-backed) so it composes into
/// [`crate::daemon::Daemon`] / [`crate::run_gated`] as a gated capability route,
/// exactly like the native executor.
#[derive(Clone)]
pub struct RestExecutor {
    endpoint: String,
    agent: ureq::Agent,
}

impl RestExecutor {
    /// Build a REST executor that POSTs to `endpoint`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        // http_status_as_error(false): a 4xx/5xx is a response to report, not an
        // executor error — we read its status + body like any other.
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build();
        Self {
            endpoint: endpoint.into(),
            agent: config.into(),
        }
    }
}

impl CapabilityExecutor for RestExecutor {
    fn execute(
        &self,
        _capability: &str,
        bound: &[(String, String)],
    ) -> Result<Vec<(String, String)>, String> {
        // The capability is not re-checked here: the route/daemon already matched
        // it to this executor. The bound params become the JSON request body.
        let body: BTreeMap<&str, &str> = bound
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let body = serde_json::to_string(&body).map_err(|e| e.to_string())?;

        let mut response = self
            .agent
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .send(body.as_str())
            .map_err(|e| format!("transport: {e}"))?;

        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;

        Ok(vec![
            ("status".to_owned(), status.to_string()),
            ("body".to_owned(), text),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Spawn a one-shot mock HTTP server that replies `200` with `body`; returns
    /// its address. The thread joins after serving a single request.
    fn mock_http_once(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf); // consume the request
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            let _ = stream.flush();
        });
        (format!("http://{addr}/action"), handle)
    }

    #[test]
    fn posts_params_and_returns_status_and_body() {
        let (url, server) = mock_http_once(r#"{"result":"ok"}"#);
        let exec = RestExecutor::new(url);
        let result = exec
            .execute("HttpCall", &[("host".to_owned(), "node-1".to_owned())])
            .expect("transport ok");
        server.join().unwrap();

        assert_eq!(result[0], ("status".to_owned(), "200".to_owned()));
        assert!(
            result[1].1.contains(r#""result":"ok""#),
            "body: {}",
            result[1].1
        );
    }

    #[test]
    fn transport_failure_is_an_executor_error() {
        // Nothing is listening on this port → connection refused → Err.
        let exec = RestExecutor::new("http://127.0.0.1:1/action");
        let err = exec
            .execute("HttpCall", &[("k".to_owned(), "v".to_owned())])
            .unwrap_err();
        assert!(err.starts_with("transport:"), "got: {err}");
    }

    /// The parity point: a REST capability runs behind the SAME hard gate as the
    /// native one. Authorized → `run_gated` reaches the REST call; unauthorized →
    /// `Denied`, and the endpoint is never hit (the executor never ran).
    #[test]
    fn rest_executor_runs_only_behind_the_gate() {
        use crate::run_gated;
        use graph_flow_action::HandlerOutcome;
        use lance_graph_contract::action::{ActionDef, ActionInvocation};
        use lance_graph_contract::canonical_node::NodeGuid;
        use lance_graph_contract::kanban::ExecTarget;
        use lance_graph_contract::mul::GateDecision;
        use lance_graph_contract::rbac::{ActorId, ClassId, ClassRbac, Operation, RoleId};

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

        let action = ActionDef {
            predicate: "HttpCall",
            object_class: MARS_MACHINE,
            exec: ExecTarget::Native,
            guard: None,
            required_role: Some("automation_operator"),
            overrides: None,
        };
        let bound = vec![("host".to_owned(), "node-1".to_owned())];

        // Authorized: the gate commits, the REST call fires (mock 200).
        let (url, server) = mock_http_once(r#"{"result":"ok"}"#);
        let mut inv = ActionInvocation::pending(
            MARS_MACHINE,
            "HttpCall",
            NodeGuid::new(MARS_MACHINE, 0, 0, 0, 0, 0),
            1,
            0,
            0,
        );
        let (outcome, result) = run_gated(
            RestExecutor::new(url),
            "HttpCall",
            &bound,
            &OpsRbac,
            "ops-1",
            &GateDecision::Flow,
            &action,
            &mut inv,
            None,
            1000,
        );
        server.join().unwrap();
        assert_eq!(outcome, HandlerOutcome::Done);
        assert_eq!(result.unwrap().unwrap()[0].0, "status");

        // Unauthorized: Denied at the gate; the endpoint (1) is never hit, so the
        // unreachable-port transport error never even occurs.
        let mut inv2 = ActionInvocation::pending(
            MARS_MACHINE,
            "HttpCall",
            NodeGuid::new(MARS_MACHINE, 0, 0, 0, 0, 0),
            1,
            0,
            0,
        );
        let (outcome2, result2) = run_gated(
            RestExecutor::new("http://127.0.0.1:1/action"),
            "HttpCall",
            &bound,
            &OpsRbac,
            "intruder",
            &GateDecision::Flow,
            &action,
            &mut inv2,
            None,
            1000,
        );
        assert_eq!(outcome2, HandlerOutcome::Denied);
        assert!(result2.is_none(), "the gate must block the REST call");
    }
}
