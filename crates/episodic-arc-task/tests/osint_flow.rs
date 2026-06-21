//! End-to-end: the OSINT episodic-arc loop wired into a real graph-flow graph.
//!
//! retrieve_episodic → record_episode → cite_gate → commit, driven by `FlowRunner`
//! over `InMemorySessionStorage`. Proves the Tasks compose and execute (not just
//! unit-pass), and that the fail-closed cite-gate halts a flow with uncited claims
//! before it can commit. `commit` is a terminal node (`NextAction::End`) standing
//! in for the promote/persist step.

use std::sync::Arc;

use async_trait::async_trait;
use episodic_arc_task::{
    CiteGateTask, RecordEpisodeTask, RetrieveEpisodicTask, KEY_CITE_STATUS, KEY_CLAIMS,
    KEY_OBSERVATION, KEY_PUBLIC_INTEREST, KEY_QUERY, KEY_SOURCE_TEXT,
};
use graph_flow::{
    Context, ExecutionStatus, FlowRunner, GraphBuilder, InMemorySessionStorage, NextAction, Result,
    Session, SessionStorage, Task, TaskResult,
};

/// Terminal node: the gate passed, persist/promote and end the flow.
struct CommitTask;

#[async_trait]
impl Task for CommitTask {
    fn id(&self) -> &str {
        "commit"
    }
    async fn run(&self, _context: Context) -> Result<TaskResult> {
        Ok(TaskResult::new(Some("committed".to_string()), NextAction::End))
    }
}

fn osint_graph() -> Arc<graph_flow::Graph> {
    Arc::new(
        GraphBuilder::new("osint_episodic_arc")
            .add_task(Arc::new(RetrieveEpisodicTask))
            .add_task(Arc::new(RecordEpisodeTask))
            .add_task(Arc::new(CiteGateTask))
            .add_task(Arc::new(CommitTask))
            .add_edge("retrieve_episodic", "record_episode")
            .add_edge("record_episode", "cite_gate")
            .add_edge("cite_gate", "commit")
            .set_start_task("retrieve_episodic")
            .build(),
    )
}

/// Drive the flow from its start task until it stops (Completed / WaitingForInput
/// / Error), returning the terminal status. Guards against non-convergence.
async fn drive(runner: &FlowRunner, sid: &str) -> ExecutionStatus {
    let mut steps = 0;
    loop {
        let result = runner.run(sid).await.expect("run");
        steps += 1;
        assert!(steps < 12, "flow did not converge");
        match result.status {
            ExecutionStatus::Paused { .. } => continue,
            other => return other,
        }
    }
}

#[tokio::test]
async fn osint_flow_completes_when_cited_and_justified() {
    let storage: Arc<dyn SessionStorage> = Arc::new(InMemorySessionStorage::new());
    let session = Session::new_from_task("happy".into(), "retrieve_episodic");
    session.context.set(KEY_QUERY, "who met whom in Paris").await;
    session.context.set(KEY_OBSERVATION, "Alice met Bob").await;
    session.context.set(KEY_SOURCE_TEXT, "Alice met Bob in Paris.").await;
    session.context.set(KEY_PUBLIC_INTEREST, "rank a public official's claim").await;
    session.context.set(KEY_CLAIMS, "Bob was in Paris\t12345\t0\t13").await;
    storage.save(session).await.unwrap();

    let runner = FlowRunner::new(osint_graph(), storage.clone());
    let status = drive(&runner, "happy").await;

    assert!(matches!(status, ExecutionStatus::Completed), "got {status:?}");
    let session = storage.get("happy").await.unwrap().unwrap();
    assert_eq!(session.context.get::<String>(KEY_CITE_STATUS).await.as_deref(), Some("passed"));
}

#[tokio::test]
async fn osint_flow_halts_at_cite_gate_on_uncited_claim() {
    let storage: Arc<dyn SessionStorage> = Arc::new(InMemorySessionStorage::new());
    let session = Session::new_from_task("blocked".into(), "retrieve_episodic");
    session.context.set(KEY_QUERY, "rumor check").await;
    session.context.set(KEY_OBSERVATION, "heard a rumor").await;
    session.context.set(KEY_SOURCE_TEXT, "").await; // no source
    session.context.set(KEY_PUBLIC_INTEREST, "public interest").await;
    session.context.set(KEY_CLAIMS, "unsourced rumor\t0\t0\t0").await; // sentinel id ⇒ uncited
    storage.save(session).await.unwrap();

    let runner = FlowRunner::new(osint_graph(), storage.clone());
    let status = drive(&runner, "blocked").await;

    // The fail-closed gate escalates rather than completing — commit is never reached.
    assert!(matches!(status, ExecutionStatus::WaitingForInput), "got {status:?}");
    let session = storage.get("blocked").await.unwrap().unwrap();
    assert_eq!(
        session.context.get::<String>(KEY_CITE_STATUS).await.as_deref(),
        Some("blocked_uncited_claim")
    );
}
