//! Falsifiers for the graph-flow / lance-graph seam.
//!
//! Every expected number comes from a plain loop over the same raw vectors
//! the source was built from; no oracle calls into lance-graph.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use graph_flow::{
    Context, ExecutionStatus, Graph, GraphBuilder, NextAction, Session, Task, TaskResult,
};
use graph_flow_lance::{
    Completion, Envelope, LanceRegistry, LanceTask, LlmTask, RegistryStats, Role, DEFAULT_KEY,
    MATERIALIZED_KEY,
};
use lance_graph_report::boundary::Catalog;
use lance_graph_report::{
    AbiBatch, AxisRole, CellValue, CmpOp, Column, CoordSpec, FieldId, LaneData, MaskId, Measure,
    MeasureKind, PlannerPolicy, ReportResult, Scalar, Selection, SourceId,
};
use serde_json::Value;

const A: FieldId = FieldId(1);
const V: FieldId = FieldId(2);
const BRANCH_BASE: u32 = 10;
const BRANCHES: u32 = 5;
const SOURCE: SourceId = SourceId(5);

/// The raw vectors a source was built from: the oracle's only input.
struct Raw {
    a: Vec<u32>,
    v: Vec<i32>,
    /// `branches[i][row]`: row is in branch `i`.
    branches: Vec<Vec<bool>>,
}

fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    }
}

/// Branch `i` holds a row with probability `(i + 1) / 8`, so branches overlap
/// and differ in size, which is what makes a mean of means wrong.
fn raw(n: usize, seed: u64) -> (Raw, Vec<String>) {
    let mut next = xorshift(seed);
    let labels = ["a0", "a1", "a2", "a3"];
    let a_raw: Vec<String> = (0..n)
        .map(|_| labels[(next() % 4) as usize].to_string())
        .collect();
    let v: Vec<i32> = (0..n).map(|_| (next() % 100) as i32).collect();
    let branches = (0..BRANCHES)
        .map(|i| (0..n).map(|_| next() % 8 <= u64::from(i)).collect())
        .collect();
    (
        Raw {
            a: Vec::new(),
            v,
            branches,
        },
        a_raw,
    )
}

fn words(bits: &[bool]) -> Arc<[u64]> {
    let mut w = vec![0u64; bits.len().div_ceil(64)];
    for (r, &b) in bits.iter().enumerate() {
        if b {
            w[r / 64] |= 1 << (r % 64);
        }
    }
    w.into()
}

fn batch(reg: &LanceRegistry, raw: &mut Raw, a_raw: &[String], generation: u32) -> AbiBatch {
    let (a, da) = {
        let mut cam = reg.cam.write().unwrap();
        let mut kv = reg.kv.write().unwrap();
        let a = cam.canonicalize(A, a_raw.iter().map(String::as_str), &mut kv);
        (a, cam.domain(A))
    };
    raw.a = a.to_vec();
    let mut b = AbiBatch::new(SOURCE, generation, raw.v.len())
        .with_column(Column::coordinate(A, a, da))
        .unwrap()
        .with_column(Column::value(V, LaneData::I32(raw.v.clone().into())))
        .unwrap();
    for (i, bits) in raw.branches.iter().enumerate() {
        b = b
            .with_mask(MaskId(BRANCH_BASE + i as u32), words(bits))
            .unwrap();
    }
    b
}

fn fixture(n: usize, seed: u64) -> (Arc<LanceRegistry>, Raw) {
    let reg = Arc::new(LanceRegistry::new(PlannerPolicy::default()));
    let (mut raw, a_raw) = raw(n, seed);
    *reg.catalog.write().unwrap() = Catalog::default().with("a", A).with("v", V);
    let b = batch(&reg, &mut raw, &a_raw, 1);
    reg.publish("osint", b).unwrap();
    (reg, raw)
}

/// Count, sum, min, max over the rows a predicate keeps. `None` when empty.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Agg {
    count: i64,
    sum: i64,
    min: i64,
    max: i64,
}

fn agg(raw: &Raw, keep: impl Fn(usize) -> bool) -> Option<Agg> {
    let mut out: Option<Agg> = None;
    for r in 0..raw.v.len() {
        if !keep(r) {
            continue;
        }
        let v = i64::from(raw.v[r]);
        out = Some(match out {
            None => Agg {
                count: 1,
                sum: v,
                min: v,
                max: v,
            },
            Some(a) => Agg {
                count: a.count + 1,
                sum: a.sum + v,
                min: a.min.min(v),
                max: a.max.max(v),
            },
        });
    }
    out
}

fn measures() -> Vec<Measure> {
    vec![
        Measure::count(),
        Measure::of(MeasureKind::Sum, V),
        Measure::of(MeasureKind::Min, V),
        Measure::of(MeasureKind::Max, V),
        Measure::of(MeasureKind::Mean, V),
    ]
}

fn int(c: CellValue) -> i64 {
    match c {
        CellValue::Int(i) => i,
        other => panic!("expected an integer cell, got {other:?}"),
    }
}

fn real(c: CellValue) -> f64 {
    match c {
        CellValue::Real(r) => r,
        CellValue::Int(i) => i as f64,
        other => panic!("expected a numeric cell, got {other:?}"),
    }
}

/// Assert a cell group equals the oracle's aggregate, mean included.
fn assert_cells(r: &ReportResult, cell: impl Fn(&Measure) -> CellValue, want: Option<Agg>) {
    let ms = r.measures().to_vec();
    match want {
        None => assert_eq!(int(cell(&ms[0])), 0, "empty group must count 0"),
        Some(w) => {
            assert_eq!(int(cell(&ms[0])), w.count, "count");
            assert_eq!(int(cell(&ms[1])), w.sum, "sum");
            assert_eq!(int(cell(&ms[2])), w.min, "min");
            assert_eq!(int(cell(&ms[3])), w.max, "max");
            assert_eq!(real(cell(&ms[4])), w.sum as f64 / w.count as f64, "mean");
        }
    }
}

/// A linear graph over `tasks`, every task continuing into the next, the last
/// one ending.
fn chain(tasks: Vec<LanceTask>) -> (Graph, String) {
    let n = tasks.len();
    let mut ids = Vec::new();
    let mut b = GraphBuilder::new("lance");
    for (i, t) in tasks.into_iter().enumerate() {
        let t = if i + 1 == n {
            t.with_next(NextAction::End)
        } else {
            t
        };
        ids.push(t.id().to_string());
        b = b.add_task(t.into_task());
    }
    for w in ids.windows(2) {
        b = b.add_edge(w[0].clone(), w[1].clone());
    }
    let start = ids[0].clone();
    (b.set_start_task(start.clone()).build(), start)
}

/// Source, then the five measures.
fn plan_tasks(reg: &Arc<LanceRegistry>) -> Vec<LanceTask> {
    let mut t = vec![LanceTask::source("source", reg, "osint").unwrap()];
    for (i, m) in measures().into_iter().enumerate() {
        t.push(LanceTask::measure(format!("measure{i}"), reg, m));
    }
    t
}

async fn run_to_end(graph: &Graph, start: &str) -> Session {
    let mut s = Session::new_from_task("s1".to_string(), start);
    let out = graph.execute_session(&mut s).await.unwrap();
    assert!(
        matches!(out.status, ExecutionStatus::Completed),
        "{:?}",
        out.status
    );
    s
}

async fn result_of(reg: &LanceRegistry, ctx: &Context) -> ReportResult {
    let v: Value = ctx.get(DEFAULT_KEY).await.expect("an envelope");
    let e = Envelope::from_json(&v).unwrap();
    assert_eq!(e.role, Role::Result);
    reg.result(&e).unwrap()
}

#[tokio::test]
async fn a_chain_folds_like_a_plain_loop() {
    let (reg, raw) = fixture(20_000, 7);
    let mut tasks = vec![
        LanceTask::source("source", &reg, "osint").unwrap(),
        LanceTask::filter(
            "filter",
            &reg,
            Selection::cmp(V, CmpOp::Ge, Scalar::Int(10)),
        ),
        LanceTask::axis("by_a", &reg, CoordSpec::Field(A), AxisRole::Row),
    ];
    for (i, m) in measures().into_iter().enumerate() {
        tasks.push(LanceTask::measure(format!("measure{i}"), &reg, m));
    }
    tasks.push(LanceTask::execute("execute", &reg));
    let (graph, start) = chain(tasks);
    let s = run_to_end(&graph, &start).await;
    let r = result_of(&reg, &s.context).await;

    let keys = r.row_keys();
    assert_eq!(keys.len(), 4, "four labels on the row axis");
    for key in &keys {
        let a = key[0];
        let want = agg(&raw, |row| raw.a[row] == a && raw.v[row] >= 10);
        assert_cells(&r, |m| r.value(m, &[], key, &[]), want);
    }
    let all = agg(&raw, |row| raw.v[row] >= 10);
    assert_cells(&r, |m| r.grand_total(m), all);
    assert_eq!(RegistryStats::get(&reg.stats.executions), 1);
}

#[tokio::test]
async fn a_saved_session_holds_handles_not_rows() {
    let mut checkpoints = Vec::new();
    let mut counts = Vec::new();
    for n in [1_000usize, 100_000] {
        let (reg, _) = fixture(n, 11);
        let mut tasks = plan_tasks(&reg);
        tasks.push(LanceTask::execute("execute", &reg));
        let (graph, start) = chain(tasks);
        let s = run_to_end(&graph, &start).await;
        let r = result_of(&reg, &s.context).await;
        counts.push(int(r.grand_total(&Measure::count())));
        checkpoints.push(serde_json::to_string(&s).unwrap());
    }
    // The populations differ a hundredfold and the folds saw that.
    assert_eq!(counts, vec![1_000, 100_000]);
    // The persisted sessions are byte-identical, and small.
    assert_eq!(checkpoints[0], checkpoints[1]);
    assert!(
        checkpoints[0].len() < 1024,
        "{} bytes",
        checkpoints[0].len()
    );
    let v: Value = serde_json::from_str(&checkpoints[0]).unwrap();
    let data = v["context"]["data"].as_object().unwrap();
    assert_eq!(data.len(), 1, "only the envelope key: {data:?}");
    assert_eq!(data[DEFAULT_KEY]["$kind"], "lance-abi");
}

/// One branch of the classic task-pool fan-out: filter the shared plan by
/// its mask, fold, and store a result handle under its own key.
struct Branch {
    id: String,
    i: u32,
    reg: Arc<LanceRegistry>,
}

#[async_trait]
impl Task for Branch {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
        let z8 =
            |e: z8run_core::Z8Error| graph_flow::GraphError::TaskExecutionFailed(e.to_string());
        let v: Value = ctx.get(DEFAULT_KEY).await.unwrap();
        let env = Envelope::from_json(&v).map_err(z8)?;
        let plan = (*self.reg.plan(&env).map_err(z8)?)
            .clone()
            .filter(Selection::Mask(MaskId(BRANCH_BASE + self.i)));
        let res = self.reg.execute(&plan).map_err(z8)?;
        let out = self.reg.put_result(&plan, res).map_err(z8)?;
        ctx.set(format!("branch.{}", self.i), out.to_json()).await;
        Ok(TaskResult::new(None, NextAction::End))
    }
}

#[tokio::test]
async fn send_is_one_mask_set_fold() {
    let (reg, raw) = fixture(30_000, 23);
    let want: Vec<Option<Agg>> = (0..BRANCHES as usize)
        .map(|i| agg(&raw, |row| raw.branches[i][row]))
        .collect();
    let some: Vec<Agg> = want.iter().map(|w| w.unwrap()).collect();
    let reduced = Agg {
        count: some.iter().map(|w| w.count).sum(),
        sum: some.iter().map(|w| w.sum).sum(),
        min: some.iter().map(|w| w.min).min().unwrap(),
        max: some.iter().map(|w| w.max).max().unwrap(),
    };

    // Mask-shaped: one axis over the branch masks, one fold.
    let mut tasks = plan_tasks(&reg);
    tasks.insert(
        1,
        LanceTask::axis(
            "branches",
            &reg,
            CoordSpec::MaskSet {
                base: MaskId(BRANCH_BASE),
                count: BRANCHES,
            },
            AxisRole::Row,
        ),
    );
    tasks.push(LanceTask::execute("execute", &reg));
    let (graph, start) = chain(tasks);
    let s = run_to_end(&graph, &start).await;
    let mask = result_of(&reg, &s.context).await;
    assert_eq!(
        RegistryStats::get(&reg.stats.executions),
        1,
        "one fold for every branch"
    );
    for (i, w) in want.iter().enumerate() {
        assert_cells(&mask, |m| mask.value(m, &[], &[i as u32], &[]), *w);
    }
    // The reduce is the merge along the branch axis, mean included.
    assert_cells(&mask, |m| mask.grand_total(m), Some(reduced));

    // Task-pool shaped, as LangGraph's Send runs it: one fold per branch.
    let before = RegistryStats::get(&reg.stats.executions);
    let children: Vec<Arc<dyn Task>> = (0..BRANCHES)
        .map(|i| {
            Arc::new(Branch {
                id: format!("branch{i}"),
                i,
                reg: Arc::clone(&reg),
            }) as Arc<dyn Task>
        })
        .collect();
    let fan = graph_flow::FanOutTask::new("fan", children).with_next_action(NextAction::End);
    let mut b = GraphBuilder::new("pool");
    let mut ids = Vec::new();
    for t in plan_tasks(&reg) {
        ids.push(t.id().to_string());
        b = b.add_task(t.into_task());
    }
    b = b.add_task(fan);
    ids.push("fan".to_string());
    for w in ids.windows(2) {
        b = b.add_edge(w[0].clone(), w[1].clone());
    }
    let pool = b.set_start_task(ids[0].clone()).build();
    let s = run_to_end(&pool, &ids[0]).await;
    assert_eq!(
        RegistryStats::get(&reg.stats.executions) - before,
        u64::from(BRANCHES),
        "the task pool folds once per branch"
    );

    let mut means = Vec::new();
    for (i, w) in want.iter().enumerate() {
        let v: Value = s.context.get(&format!("branch.{i}")).await.unwrap();
        let r = reg.result(&Envelope::from_json(&v).unwrap()).unwrap();
        // Each branch agrees with the mask-set cell and with the oracle.
        assert_cells(&r, |m| r.grand_total(m), *w);
        means.push(real(r.grand_total(&measures()[4])));
    }
    // Merging finalized cells is right for count, sum, min and max, and wrong
    // for the mean: a mean of means is not the mean of the branch union.
    let mean_of_means = means.iter().sum::<f64>() / means.len() as f64;
    let true_mean = reduced.sum as f64 / reduced.count as f64;
    assert_ne!(mean_of_means, true_mean);
    assert_eq!(real(mask.grand_total(&measures()[4])), true_mean);
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

fn gated(reg: &Arc<LanceRegistry>) -> (Graph, String) {
    let mut b = GraphBuilder::new("gated");
    let mut ids = Vec::new();
    let mut tasks = plan_tasks(reg);
    tasks.insert(
        1,
        LanceTask::axis("by_a", reg, CoordSpec::Field(A), AxisRole::Row),
    );
    for t in tasks {
        ids.push(t.id().to_string());
        b = b.add_task(t.into_task());
    }
    b = b.add_task(Arc::new(Gate));
    ids.push("gate".to_string());
    let exec = LanceTask::execute("execute", reg).with_next(NextAction::End);
    ids.push("execute".to_string());
    b = b.add_task(exec.into_task());
    for w in ids.windows(2) {
        b = b.add_edge(w[0].clone(), w[1].clone());
    }
    (b.set_start_task(ids[0].clone()).build(), ids[0].clone())
}

/// Run until the gate pauses, and return the session as persisted bytes.
async fn pause(graph: &Graph, start: &str) -> String {
    let mut s = Session::new_from_task("s1".to_string(), start);
    let out = graph.execute_session(&mut s).await.unwrap();
    assert!(matches!(out.status, ExecutionStatus::WaitingForInput));
    assert_eq!(s.current_task_id, "gate");
    serde_json::to_string(&s).unwrap()
}

#[tokio::test]
async fn a_resumed_session_answers_the_same_and_a_republished_source_fails_closed() {
    let (reg, raw) = fixture(10_000, 31);
    let (graph, start) = gated(&reg);
    let saved = pause(&graph, &start).await;

    // A different graph instance resumes from the persisted bytes.
    let (graph2, _) = gated(&reg);
    let mut s: Session = serde_json::from_str(&saved).unwrap();
    s.context.set("approved", true).await;
    let out = graph2.execute_session(&mut s).await.unwrap();
    assert!(matches!(out.status, ExecutionStatus::Completed));
    let r = result_of(&reg, &s.context).await;
    for key in r.row_keys() {
        let a = key[0];
        assert_cells(
            &r,
            |m| r.value(m, &[], &key, &[]),
            agg(&raw, |row| raw.a[row] == a),
        );
    }
    let old_result: Value = s.context.get(DEFAULT_KEY).await.unwrap();

    // Pause again, then republish the source under a new generation.
    let saved = pause(&graph, &start).await;
    let (mut raw2, a_raw2) = raw_fresh(10_000, 99);
    let b2 = batch(&reg, &mut raw2, &a_raw2, 2);
    reg.publish("osint", b2).unwrap();

    // A finished result is guarded by the registry alone: it must not
    // resolve against the new generation.
    let err = reg
        .result(&Envelope::from_json(&old_result).unwrap())
        .unwrap_err();
    assert!(err.to_string().contains("stale lance handle"), "{err}");

    // A paused plan fails closed when resumed. The registry refuses it, and
    // lance-graph-report would refuse it again (`StaleSource`) if the
    // registry did not.
    let mut s: Session = serde_json::from_str(&saved).unwrap();
    s.context.set("approved", true).await;
    let err = graph2.execute_session(&mut s).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("stale lance handle") || msg.contains("StaleSource"),
        "{msg}"
    );
}

fn raw_fresh(n: usize, seed: u64) -> (Raw, Vec<String>) {
    raw(n, seed)
}

#[tokio::test]
async fn a_rotated_plan_reuses_the_fold() {
    let (reg, _) = fixture(10_000, 41);
    let branch_axis = CoordSpec::MaskSet {
        base: MaskId(BRANCH_BASE),
        count: BRANCHES,
    };
    let build = |rotate: bool| {
        let mut t = vec![
            LanceTask::source("source", &reg, "osint").unwrap(),
            LanceTask::axis("rows", &reg, CoordSpec::Field(A), AxisRole::Row),
            LanceTask::axis("cols", &reg, branch_axis.clone(), AxisRole::Column),
            LanceTask::measure("count", &reg, Measure::count()),
        ];
        if rotate {
            t.push(LanceTask::rotate("rotate", &reg));
        }
        t.push(LanceTask::execute("execute", &reg));
        chain(t)
    };
    let (g1, s1) = build(false);
    let r1 = result_of(&reg, &run_to_end(&g1, &s1).await.context).await;
    let (g2, s2) = build(true);
    let r2 = result_of(&reg, &run_to_end(&g2, &s2).await.context).await;
    assert_eq!(RegistryStats::get(&reg.stats.executions), 1, "one fold");
    assert_eq!(
        RegistryStats::get(&reg.stats.cache_hits),
        1,
        "the rotation re-viewed it"
    );
    let m = Measure::count();
    for a in 0..4u32 {
        for b in 0..BRANCHES {
            assert_eq!(r1.value(&m, &[], &[a], &[b]), r2.value(&m, &[], &[b], &[a]));
        }
    }
}

/// Records every prompt it is given.
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

#[tokio::test]
async fn the_model_sees_only_the_export() {
    let (reg, _) = fixture(5_000, 53);
    let model = Arc::new(Double::default());
    let mut b = GraphBuilder::new("ask");
    let mut ids = Vec::new();
    let mut tasks = plan_tasks(&reg);
    tasks.insert(
        1,
        LanceTask::axis("by_a", &reg, CoordSpec::Field(A), AxisRole::Row),
    );
    tasks.push(LanceTask::execute("execute", &reg));
    tasks.push(LanceTask::materialize("materialize", &reg));
    for t in tasks {
        ids.push(t.id().to_string());
        b = b.add_task(t.into_task());
    }
    let ask = LlmTask::new("ask", model.clone(), "Which label has the most rows?");
    b = b.add_task(ask.into_task());
    ids.push("ask".to_string());
    for w in ids.windows(2) {
        b = b.add_edge(w[0].clone(), w[1].clone());
    }
    let g = b.set_start_task(ids[0].clone()).build();
    let s = run_to_end(&g, &ids[0]).await;

    let export: Value = s.context.get(MATERIALIZED_KEY).await.unwrap();
    let seen = model.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].contains(&export.to_string()),
        "the export is in the prompt"
    );
    assert!(
        !seen[0].contains("lance-abi"),
        "no handle reaches the model"
    );
    assert_eq!(s.context.chat_history_len().await, 2);

    // Without a materialize step there is nothing the model may see.
    let (reg2, _) = fixture(100, 1);
    let mut t = plan_tasks(&reg2);
    t.push(LanceTask::execute("execute", &reg2).with_next(NextAction::ContinueAndExecute));
    let mut b = GraphBuilder::new("no-export");
    let mut ids = Vec::new();
    for x in t {
        ids.push(x.id().to_string());
        b = b.add_task(x.into_task());
    }
    b = b.add_task(LlmTask::new("ask", Arc::new(Double::default()), "?").into_task());
    ids.push("ask".to_string());
    for w in ids.windows(2) {
        b = b.add_edge(w[0].clone(), w[1].clone());
    }
    let g = b.set_start_task(ids[0].clone()).build();
    let mut s = Session::new_from_task("s".into(), &ids[0]);
    let err = g.execute_session(&mut s).await.unwrap_err();
    assert!(err.to_string().contains("nothing materialized"), "{err}");
}

#[test]
fn a_misspelt_source_fails_when_the_graph_is_built() {
    let (reg, _) = fixture(10, 3);
    assert!(LanceTask::source("s", &reg, "osnit").is_err());
}
