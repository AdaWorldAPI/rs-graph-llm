//! z8run flows and graph-flow graphs on one seam.
//!
//! The z8run side is driven node by node through z8run-lance's own node
//! factories; the graph-flow side runs inside a `graph-flow` z8run node. Both
//! use one `LanceRegistry`. Expected numbers come from plain loops.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use graph_flow::{
    Context, Graph, GraphBuilder, InMemorySessionStorage, NextAction, SessionStorage, Task,
    TaskResult,
};
use graph_flow_lance::z8run::factory as graph_flow_factory;
use graph_flow_lance::{
    Completion, Envelope, GraphFlowHost, LanceRegistry, LanceTask, LlmTask, Role, DEFAULT_KEY,
    SESSION_KIND,
};
use lance_graph_report::boundary::Catalog;
use lance_graph_report::{
    AbiBatch, AxisRole, CellValue, Column, CoordSpec, FieldId, LaneData, Measure, PlannerPolicy,
    ReportResult, SourceId,
};
use serde_json::{json, Value};
use uuid::Uuid;
use z8run_core::engine::NodeExecutorFactory;
use z8run_core::FlowMessage;
use z8run_lance::nodes::factory as lance_factory;

const A: FieldId = FieldId(1);
const V: FieldId = FieldId(2);

struct Raw {
    a: Vec<u32>,
    v: Vec<i32>,
}

fn fixture(n: usize, seed: u64) -> (Arc<LanceRegistry>, Raw) {
    let reg = Arc::new(LanceRegistry::new(PlannerPolicy::default()));
    let mut s = seed;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let labels = ["a0", "a1", "a2", "a3"];
    let a_raw: Vec<String> = (0..n)
        .map(|_| labels[(next() % 4) as usize].to_string())
        .collect();
    let v: Vec<i32> = (0..n).map(|_| (next() % 100) as i32).collect();
    let (a, da) = {
        let mut cam = reg.cam.write().unwrap();
        let mut kv = reg.kv.write().unwrap();
        let a = cam.canonicalize(A, a_raw.iter().map(String::as_str), &mut kv);
        (a, cam.domain(A))
    };
    *reg.catalog.write().unwrap() = Catalog::default().with("a", A).with("v", V);
    let raw = Raw {
        a: a.to_vec(),
        v: v.clone(),
    };
    let batch = AbiBatch::new(SourceId(5), 1, n)
        .with_column(Column::coordinate(A, a, da))
        .unwrap()
        .with_column(Column::value(V, LaneData::I32(v.into())))
        .unwrap();
    reg.publish("osint", batch).unwrap();
    (reg, raw)
}

fn start() -> FlowMessage {
    FlowMessage::new(Uuid::now_v7(), "trigger", json!({}), Uuid::now_v7())
}

async fn lance(
    reg: &Arc<LanceRegistry>,
    node: &'static str,
    cfg: Value,
    msg: FlowMessage,
) -> FlowMessage {
    let n = lance_factory(node, reg).create(cfg).await.unwrap();
    let mut out = n.process(msg).await.unwrap();
    assert_eq!(out.len(), 1);
    out.pop().unwrap()
}

async fn graph_flow(
    host: &Arc<GraphFlowHost>,
    cfg: Value,
    msg: FlowMessage,
) -> z8run_core::Z8Result<FlowMessage> {
    let n = graph_flow_factory(host).create(cfg).await?;
    let mut out = n.process(msg).await?;
    assert_eq!(out.len(), 1);
    Ok(out.pop().unwrap())
}

/// source, `v >= 10`, rows by `a`, count and sum: built by z8run nodes.
async fn z8run_plan(reg: &Arc<LanceRegistry>) -> Vec<FlowMessage> {
    let mut msgs = vec![lance(reg, "lance-source", json!({"source": "osint"}), start()).await];
    for (node, cfg) in [
        (
            "lance-filter",
            json!({"field": "v", "op": "ge", "value": 10}),
        ),
        ("lance-axis", json!({"coord": "a", "role": "row"})),
        ("lance-measure", json!({"op": "count"})),
        ("lance-measure", json!({"op": "sum", "field": "v"})),
    ] {
        let m = lance(reg, node, cfg, msgs.last().unwrap().clone()).await;
        msgs.push(m);
    }
    msgs
}

fn assert_handle(m: &FlowMessage, role: &str) {
    assert_eq!(m.payload["$kind"], "lance-abi", "{}", m.payload);
    assert_eq!(m.payload["role"], role);
    assert!(m.payload.to_string().len() < 200, "{}", m.payload);
}

fn int(c: CellValue) -> i64 {
    match c {
        CellValue::Int(i) => i,
        other => panic!("expected an integer cell, got {other:?}"),
    }
}

/// Per `a`: rows with `v >= 10`, and their sum.
fn assert_counts_and_sums(r: &ReportResult, raw: &Raw) {
    let ms = r.measures().to_vec();
    assert_eq!(r.row_keys().len(), 4);
    for key in r.row_keys() {
        let (mut count, mut sum) = (0i64, 0i64);
        for row in 0..raw.v.len() {
            if raw.a[row] == key[0] && raw.v[row] >= 10 {
                count += 1;
                sum += i64::from(raw.v[row]);
            }
        }
        assert_eq!(
            int(r.value(&ms[0], &[], &key, &[])),
            count,
            "count for {key:?}"
        );
        assert_eq!(int(r.value(&ms[1], &[], &key, &[])), sum, "sum for {key:?}");
    }
}

#[derive(Default)]
struct Double {
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl Completion for Double {
    async fn complete(&self, prompt: String) -> graph_flow::Result<String> {
        self.seen.lock().unwrap().push(prompt);
        Ok("a3 leads".to_string())
    }
}

fn linear(id: &str, tasks: Vec<Arc<dyn Task>>) -> Arc<Graph> {
    let mut b = GraphBuilder::new(id);
    let ids: Vec<String> = tasks.iter().map(|t| t.id().to_string()).collect();
    for t in tasks {
        b = b.add_task(t);
    }
    for w in ids.windows(2) {
        b = b.add_edge(w[0].clone(), w[1].clone());
    }
    Arc::new(b.set_start_task(ids[0].clone()).build())
}

#[tokio::test]
async fn a_z8run_flow_hands_its_plan_to_a_graph_flow_graph() {
    let (reg, raw) = fixture(20_000, 7);
    let model = Arc::new(Double::default());
    let report = linear(
        "report",
        vec![
            LanceTask::execute("execute", &reg).into_task(),
            LanceTask::materialize("materialize", &reg).into_task(),
            LlmTask::new("ask", model.clone(), "Which label leads?").into_task(),
        ],
    );
    let host = Arc::new(GraphFlowHost::in_memory().with_graph("report", report));

    let msgs = z8run_plan(&reg).await;
    for m in &msgs {
        assert_handle(m, "plan");
    }
    let out = graph_flow(
        &host,
        json!({"graph": "report"}),
        msgs.last().unwrap().clone(),
    )
    .await
    .unwrap();
    assert_eq!(out.source_port, "output");
    assert_handle(&out, "result");
    let r = reg
        .result(&Envelope::from_json(&out.payload).unwrap())
        .unwrap();
    assert_counts_and_sums(&r, &raw);

    assert_eq!(out.metadata["graph-flow.response"], "a3 leads");
    let export = &out.metadata["graph-flow.export"];
    let seen = model.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].contains(&export.to_string()),
        "the model saw the export"
    );
    assert!(!seen[0].contains("lance-abi"), "and no handle");
}

#[tokio::test]
async fn a_graph_flow_plan_feeds_z8run_lance_nodes() {
    let (reg, raw) = fixture(10_000, 13);
    let plan = linear(
        "plan",
        vec![
            LanceTask::source("source", &reg, "osint")
                .unwrap()
                .into_task(),
            LanceTask::filter(
                "filter",
                &reg,
                lance_graph_report::Selection::cmp(
                    V,
                    lance_graph_report::CmpOp::Ge,
                    lance_graph_report::Scalar::Int(10),
                ),
            )
            .into_task(),
            LanceTask::axis("by_a", &reg, CoordSpec::Field(A), AxisRole::Row).into_task(),
            LanceTask::measure("count", &reg, Measure::count()).into_task(),
            LanceTask::measure(
                "sum",
                &reg,
                Measure::of(lance_graph_report::MeasureKind::Sum, V),
            )
            .with_next(NextAction::End)
            .into_task(),
        ],
    );
    let host = Arc::new(GraphFlowHost::in_memory().with_graph("plan", plan));
    // A graph needs a handle to start from; the source task replaces it.
    let seed = lance(&reg, "lance-source", json!({"source": "osint"}), start()).await;
    let planned = graph_flow(&host, json!({"graph": "plan"}), seed)
        .await
        .unwrap();
    assert_handle(&planned, "plan");

    // z8run-lance nodes execute and export the plan the graph built.
    let executed = lance(&reg, "lance-execute", json!({}), planned).await;
    assert_handle(&executed, "result");
    let r = reg
        .result(&Envelope::from_json(&executed.payload).unwrap())
        .unwrap();
    assert_counts_and_sums(&r, &raw);
    let exported = lance(
        &reg,
        "lance-materialize",
        json!({"format": "json"}),
        executed,
    )
    .await;
    assert_eq!(exported.payload["format"], "json");
}

/// Pauses until the context says `approved`.
struct Gate;

#[async_trait]
impl Task for Gate {
    fn id(&self) -> &str {
        "gate"
    }

    async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
        if ctx.get::<bool>("approved").await == Some(true) {
            Ok(TaskResult::new(None, NextAction::ContinueAndExecute))
        } else {
            Ok(TaskResult::new(
                Some("waiting".into()),
                NextAction::WaitForInput,
            ))
        }
    }
}

#[tokio::test]
async fn a_paused_graph_crosses_z8run_as_a_session_handle() {
    let (reg, raw) = fixture(10_000, 31);
    let approve = linear(
        "approve",
        vec![
            Arc::new(Gate) as Arc<dyn Task>,
            LanceTask::execute("execute", &reg)
                .with_next(NextAction::End)
                .into_task(),
        ],
    );
    let storage = Arc::new(InMemorySessionStorage::new());
    let host = Arc::new(
        GraphFlowHost::new(storage.clone() as Arc<dyn SessionStorage>)
            .with_graph("approve", approve),
    );
    let plan = z8run_plan(&reg).await.pop().unwrap();

    let waiting = graph_flow(&host, json!({"graph": "approve"}), plan)
        .await
        .unwrap();
    assert_eq!(waiting.source_port, "waiting");
    assert_eq!(waiting.payload["$kind"], SESSION_KIND);
    assert!(
        waiting.payload.to_string().len() < 200,
        "{}",
        waiting.payload
    );
    let id = waiting.payload["session"].as_str().unwrap().to_string();
    let saved = storage
        .get(&id)
        .await
        .unwrap()
        .expect("the session waits in storage");
    let bytes = serde_json::to_value(&saved).unwrap();
    let data = bytes["context"]["data"].as_object().unwrap();
    assert_eq!(data.len(), 1, "only the plan handle: {data:?}");
    assert_eq!(data[DEFAULT_KEY]["$kind"], "lance-abi");

    // The session belongs to one graph.
    let mut wrong = waiting.payload.clone();
    wrong["graph"] = json!("report");
    let resume =
        |payload: Value| FlowMessage::new(Uuid::now_v7(), "webhook", payload, Uuid::now_v7());
    assert!(
        graph_flow(&host, json!({"graph": "approve"}), resume(wrong))
            .await
            .is_err()
    );

    // Later, a webhook approves and the session resumes where it paused.
    let mut approve_msg = waiting.payload.clone();
    approve_msg["input"] = json!({"approved": true});
    let done = graph_flow(&host, json!({"graph": "approve"}), resume(approve_msg))
        .await
        .unwrap();
    assert_eq!(done.source_port, "output");
    assert_handle(&done, "result");
    let r = reg
        .result(&Envelope::from_json(&done.payload).unwrap())
        .unwrap();
    assert_eq!(
        Envelope::from_json(&done.payload).unwrap().role,
        Role::Result
    );
    assert_counts_and_sums(&r, &raw);
    assert!(
        storage.get(&id).await.unwrap().is_none(),
        "a finished session is not kept"
    );
}

#[tokio::test]
async fn a_graph_flow_node_takes_handles_never_data() {
    let (reg, _) = fixture(100, 3);
    let g = linear(
        "g",
        vec![LanceTask::execute("execute", &reg)
            .with_next(NextAction::End)
            .into_task()],
    );
    let host = Arc::new(GraphFlowHost::in_memory().with_graph("g", g));
    let rows = FlowMessage::new(
        Uuid::now_v7(),
        "trigger",
        json!({"rows": [1, 2, 3]}),
        Uuid::now_v7(),
    );
    let e = graph_flow(&host, json!({"graph": "g"}), rows)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("never data"), "{e}");

    let e = graph_flow_factory(&host)
        .create(json!({"graph": "nope"}))
        .await
        .err()
        .unwrap();
    assert!(e.to_string().contains("no graph registered"), "{e}");
}
