//! The DO fold, on today's fold states.
//!
//! lance-graph's 2026-10-05 working model: the DO arm folds source
//! contributions per destination, a repeated add is idempotent, an add and a
//! remove on one destination conflict, and only the folded state per
//! destination may cross into an action. This test checks that the existing
//! report fold states already carry that law: `Max` is idempotent, so a
//! per-destination `Max` over an add flag and over a remove flag is the
//! mutation state, and the state guard is a selection over the population.

use std::collections::BTreeMap;
use std::sync::Arc;

use graph_flow_lance::LanceRegistry;
use lance_graph_report::boundary::Catalog;
use lance_graph_report::{
    AbiBatch, AxisRole, CellValue, CmpOp, Column, CoordSpec, FieldId, LaneData, Measure,
    MeasureKind, PlannerPolicy, ReportPlan, ReportResult, Scalar, Selection, SourceId, SourceRef,
};

const TARGET: FieldId = FieldId(1);
const STATE: FieldId = FieldId(2);
const ADD: FieldId = FieldId(3);
const REMOVE: FieldId = FieldId(4);

/// One contribution: which destination, its current state, add or remove.
#[derive(Clone)]
struct Contribution {
    target: String,
    state: &'static str,
    add: i32,
    remove: i32,
}

fn contributions(n: usize, seed: u64) -> Vec<Contribution> {
    let mut s = seed;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    (0..n)
        .map(|_| {
            let t = next() % 40;
            // A destination's state is a property of the destination.
            let state = if t % 3 == 0 { "stopped" } else { "running" };
            // Removes arrive for a quarter of the destinations only, so the
            // fixture holds clean adds as well as add/remove conflicts.
            let remove = i32::from(t % 4 == 1 && next() % 10 == 0);
            Contribution {
                target: format!("node-{t:03}"),
                state,
                add: 1 - remove,
                remove,
            }
        })
        .collect()
}

fn publish(reg: &LanceRegistry, name: &str, id: u32, rows: &[Contribution]) -> SourceRef {
    let (t, dt, st, ds) = {
        let mut cam = reg.cam.write().unwrap();
        let mut kv = reg.kv.write().unwrap();
        let t = cam.canonicalize(TARGET, rows.iter().map(|r| r.target.as_str()), &mut kv);
        let st = cam.canonicalize(STATE, rows.iter().map(|r| r.state), &mut kv);
        (t, cam.domain(TARGET), st, cam.domain(STATE))
    };
    let add: Vec<i32> = rows.iter().map(|r| r.add).collect();
    let remove: Vec<i32> = rows.iter().map(|r| r.remove).collect();
    let b = AbiBatch::new(SourceId(id), 1, rows.len())
        .with_column(Column::coordinate(TARGET, t, dt))
        .unwrap()
        .with_column(Column::coordinate(STATE, st, ds))
        .unwrap()
        .with_column(Column::value(ADD, LaneData::I32(add.into())))
        .unwrap()
        .with_column(Column::value(REMOVE, LaneData::I32(remove.into())))
        .unwrap();
    reg.publish(name, b).unwrap();
    SourceRef {
        id: SourceId(id),
        generation: 1,
    }
}

/// Per destination, among eligible contributions: any add, any remove.
fn oracle(rows: &[Contribution], guard: &str) -> BTreeMap<String, (bool, bool)> {
    let mut m: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.state == guard) {
        let e = m.entry(r.target.clone()).or_default();
        e.0 |= r.add == 1;
        e.1 |= r.remove == 1;
    }
    m
}

fn flag(c: CellValue) -> bool {
    match c {
        CellValue::Int(1) => true,
        CellValue::Int(0) | CellValue::Null => false,
        other => panic!("a flag is 0 or 1, got {other:?}"),
    }
}

/// The DO fold: guard as a selection, destination as the axis, the mutation
/// state as two idempotent `Max` states.
fn do_fold(reg: &LanceRegistry, src: SourceRef) -> ReportResult {
    let running = reg.cam.read().unwrap().ordinal(STATE, "running").unwrap();
    let plan = ReportPlan::over(src)
        .filter(Selection::cmp(STATE, CmpOp::Eq, Scalar::Ordinal(running)))
        .axis(CoordSpec::Field(TARGET), AxisRole::Row)
        .measure(Measure::of(MeasureKind::Max, ADD))
        .measure(Measure::of(MeasureKind::Max, REMOVE))
        .measure(Measure::count());
    reg.execute(&plan).unwrap()
}

/// Destination label to (add, remove, contributions).
fn states(reg: &LanceRegistry, r: &ReportResult) -> BTreeMap<String, (bool, bool, i64)> {
    let ms = r.measures().to_vec();
    let cam = reg.cam.read().unwrap();
    let kv = reg.kv.read().unwrap();
    let mut out = BTreeMap::new();
    for key in r.row_keys() {
        let count = match r.value(&ms[2], &[], &key, &[]) {
            CellValue::Int(c) => c,
            other => panic!("count, got {other:?}"),
        };
        if count == 0 {
            continue;
        }
        let label = cam.resolve(TARGET, key[0], &kv).unwrap().to_string();
        let add = flag(r.value(&ms[0], &[], &key, &[]));
        let remove = flag(r.value(&ms[1], &[], &key, &[]));
        out.insert(label, (add, remove, count));
    }
    out
}

#[test]
fn the_do_fold_runs_on_existing_fold_states() {
    let reg = Arc::new(LanceRegistry::new(PlannerPolicy::default()));
    *reg.catalog.write().unwrap() = Catalog::default()
        .with("target", TARGET)
        .with("state", STATE)
        .with("add", ADD)
        .with("remove", REMOVE);
    let rows = contributions(5_000, 17);
    let src = publish(&reg, "contrib", 7, &rows);
    let once = states(&reg, &do_fold(&reg, src));

    // Per destination: the flags equal the oracle, and the guard kept only
    // running destinations.
    let want = oracle(&rows, "running");
    assert_eq!(once.len(), want.len());
    for (t, (add, remove)) in &want {
        let got = once[t];
        assert_eq!((got.0, got.1), (*add, *remove), "{t}");
    }
    assert!(
        want.len() < 40,
        "the guard must exclude the stopped destinations"
    );

    // One action per destination, never one per contribution, and an add
    // with a remove on the same destination is a conflict, not an action.
    let actions = once.values().filter(|(a, r, _)| *a && !*r).count();
    let conflicts = once.values().filter(|(a, r, _)| *a && *r).count();
    let contributions: i64 = once.values().map(|(_, _, c)| c).sum();
    assert!(
        actions > 0 && conflicts > 0,
        "the fixture must exercise both"
    );
    assert!(
        contributions > (actions + conflicts) as i64 * 10,
        "many contributions per destination"
    );

    // Redelivering every contribution changes the counts and nothing else:
    // ADD+ADD is idempotent by the fold law, with no dedup cache.
    let twice: Vec<Contribution> = rows.iter().chain(rows.iter()).cloned().collect();
    let src2 = publish(&reg, "contrib-twice", 8, &twice);
    let again = states(&reg, &do_fold(&reg, src2));
    assert_eq!(again.len(), once.len());
    for (t, (add, remove, count)) in &once {
        let g = again[t];
        assert_eq!((g.0, g.1), (*add, *remove), "{t}: flags unchanged");
        assert_eq!(g.2, 2 * count, "{t}: every contribution counted twice");
    }
}
