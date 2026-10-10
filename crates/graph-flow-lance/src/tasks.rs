//! The lance task set. Every task but `materialize` maps an envelope in the
//! `Context` to an envelope; configuration (names, labels) is resolved to ids
//! once, when the task is built.
//!
//! | task | on a plan handle | on a result handle |
//! |---|---|---|
//! | `source` | emits a fresh plan over the named source | |
//! | `filter` | ANDs a selection | refused |
//! | `axis` | adds a coordinate in a role | refused |
//! | `measure` | adds a measure | refused |
//! | `rotate` | swaps rows and columns, metadata only | re-views the same cells |
//! | `execute` | folds, or reuses a cached fold, into a result | refused |
//! | `materialize` | refused | the data-export terminal |
//!
//! A LangGraph `Send` fan-out is not a task here. It is an `axis` task with a
//! [`CoordSpec::MaskSet`] coordinate: one member per branch, every branch
//! folded in the same pass, and the reduce is the merge along that axis.

use std::sync::Arc;

use async_trait::async_trait;
use graph_flow::{Context, GraphError, NextAction, Result, Task, TaskResult};
use lance_graph_report::render::{Grid, Terminal};
use lance_graph_report::{
    AxisRole, CellValue, CoordSpec, Measure, ReportPlan, Selection, SourceId, SourceRef,
};
use serde_json::{json, Value};

use crate::registry::{Envelope, LanceRegistry, Role};

/// The `Context` key lance tasks read and write by default.
pub const DEFAULT_KEY: &str = "lance";

/// The `Context` key `materialize` writes by default. It is the only key that
/// ever holds data rather than a handle.
pub const MATERIALIZED_KEY: &str = "lance.materialized";

fn failed(m: impl Into<String>) -> GraphError {
    GraphError::TaskExecutionFailed(m.into())
}

/// What a built task does. Ids only: every name was resolved at build time.
#[derive(Debug, Clone)]
pub enum LanceOp {
    /// Start a plan over a published source.
    Source(SourceId),
    /// AND a selection into the plan.
    Filter(Selection),
    /// Add a coordinate in a role.
    Axis(CoordSpec, AxisRole),
    /// Add a measure.
    Measure(Measure),
    /// Swap rows and columns.
    Rotate,
    /// Fold the plan into a result.
    Execute,
    /// Export a result as JSON: the terminal boundary.
    Materialize,
}

/// One graph-flow task over the lance-graph fold algebra.
pub struct LanceTask {
    id: String,
    op: LanceOp,
    reg: Arc<LanceRegistry>,
    key: String,
    out_key: String,
    next: NextAction,
}

impl LanceTask {
    fn build(id: impl Into<String>, op: LanceOp, reg: &Arc<LanceRegistry>) -> Self {
        Self {
            id: id.into(),
            op,
            reg: Arc::clone(reg),
            key: DEFAULT_KEY.to_string(),
            out_key: MATERIALIZED_KEY.to_string(),
            next: NextAction::ContinueAndExecute,
        }
    }

    /// Start a plan over the source published as `name`. The name is resolved
    /// now, so a misspelt source fails when the graph is built, not mid-run.
    pub fn source(id: impl Into<String>, reg: &Arc<LanceRegistry>, name: &str) -> Result<Self> {
        Ok(Self::build(id, LanceOp::Source(reg.source_id(name)?), reg))
    }

    /// AND a selection into the plan.
    pub fn filter(id: impl Into<String>, reg: &Arc<LanceRegistry>, sel: Selection) -> Self {
        Self::build(id, LanceOp::Filter(sel), reg)
    }

    /// Add a coordinate in a role. A [`CoordSpec::MaskSet`] here is a fan-out.
    pub fn axis(
        id: impl Into<String>,
        reg: &Arc<LanceRegistry>,
        coord: CoordSpec,
        role: AxisRole,
    ) -> Self {
        Self::build(id, LanceOp::Axis(coord, role), reg)
    }

    /// Add a measure, which becomes one or more mergeable fold states.
    pub fn measure(id: impl Into<String>, reg: &Arc<LanceRegistry>, m: Measure) -> Self {
        Self::build(id, LanceOp::Measure(m), reg)
    }

    /// Swap rows and columns. On a result this re-views the same cells.
    pub fn rotate(id: impl Into<String>, reg: &Arc<LanceRegistry>) -> Self {
        Self::build(id, LanceOp::Rotate, reg)
    }

    /// Fold the plan, or reuse the cached fold of a plan with the same
    /// physical key.
    pub fn execute(id: impl Into<String>, reg: &Arc<LanceRegistry>) -> Self {
        Self::build(id, LanceOp::Execute, reg)
    }

    /// Export the result as JSON into [`MATERIALIZED_KEY`].
    pub fn materialize(id: impl Into<String>, reg: &Arc<LanceRegistry>) -> Self {
        Self::build(id, LanceOp::Materialize, reg)
    }

    /// Read and write the envelope under `key` instead of [`DEFAULT_KEY`].
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }

    /// Write the materialized export under `key` instead of
    /// [`MATERIALIZED_KEY`].
    pub fn with_out_key(mut self, key: impl Into<String>) -> Self {
        self.out_key = key.into();
        self
    }

    /// Return `next` instead of `NextAction::ContinueAndExecute`.
    pub fn with_next(mut self, next: NextAction) -> Self {
        self.next = next;
        self
    }

    /// Finish building.
    pub fn into_task(self) -> Arc<dyn Task> {
        Arc::new(self)
    }

    async fn envelope(&self, ctx: &Context) -> Result<Envelope> {
        let v: Value = ctx
            .get(&self.key)
            .await
            .ok_or_else(|| GraphError::ContextError(format!("no envelope under '{}'", self.key)))?;
        Envelope::from_json(&v)
    }

    async fn step(&self, ctx: &Context) -> Result<String> {
        if let LanceOp::Source(id) = self.op {
            let b = self.reg.batch(id)?;
            let env = self.reg.put_plan(ReportPlan::over(SourceRef {
                id,
                generation: b.generation(),
            }))?;
            ctx.set(self.key.clone(), env.to_json()).await;
            return Ok(format!("plan {} over source {}", env.handle, id));
        }
        let env = self.envelope(ctx).await?;
        match (env.role, &self.op) {
            (Role::Result, LanceOp::Rotate) => {
                let r = self.reg.result(&env)?.rotate();
                let plan_like = ReportPlan::over(SourceRef {
                    id: env.source,
                    generation: env.generation,
                });
                let out = self.reg.put_result(&plan_like, r)?;
                ctx.set(self.key.clone(), out.to_json()).await;
                Ok(format!("result {} re-viewed as {}", env.handle, out.handle))
            }
            (Role::Result, LanceOp::Materialize) => {
                let r = self.reg.result(&env)?;
                // The boundary stores are behind std locks; render inside this
                // block so every guard is dropped before the next await.
                let body = {
                    let cam = self
                        .reg
                        .cam
                        .read()
                        .map_err(|_| failed("cam lock poisoned"))?;
                    let kv = self.reg.kv.read().map_err(|_| failed("kv lock poisoned"))?;
                    let cat = self
                        .reg
                        .catalog
                        .read()
                        .map_err(|_| failed("catalog lock poisoned"))?;
                    let t = Terminal {
                        cam: &cam,
                        kv: &kv,
                        catalog: &cat,
                    };
                    grid_value(&t.grid(&r))?
                };
                ctx.set(self.out_key.clone(), body).await;
                Ok(format!("result {} materialized", env.handle))
            }
            (Role::Result, _) => Err(failed(format!(
                "task '{}' does not accept a result handle",
                self.id
            ))),
            (Role::Plan, LanceOp::Materialize) => Err(failed(
                "materialize needs a result handle; place an execute task before it",
            )),
            (Role::Plan, LanceOp::Execute) => {
                let plan = self.reg.plan(&env)?;
                let res = self.reg.execute(&plan)?;
                let out = self.reg.put_result(&plan, res)?;
                ctx.set(self.key.clone(), out.to_json()).await;
                Ok(format!(
                    "plan {} folded into result {}",
                    env.handle, out.handle
                ))
            }
            (Role::Plan, op) => {
                let plan = (*self.reg.plan(&env)?).clone();
                let plan = match op.clone() {
                    LanceOp::Filter(s) => plan.filter(s),
                    LanceOp::Axis(c, r) => plan.axis(c, r),
                    LanceOp::Measure(m) => plan.measure(m),
                    LanceOp::Rotate => plan.rotate(),
                    LanceOp::Source(_) | LanceOp::Execute | LanceOp::Materialize => {
                        unreachable!("handled above")
                    }
                };
                let out = self.reg.put_plan(plan)?;
                ctx.set(self.key.clone(), out.to_json()).await;
                Ok(format!("plan {} rewritten as {}", env.handle, out.handle))
            }
        }
    }
}

#[async_trait]
impl Task for LanceTask {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        let summary = self.step(&context).await?;
        Ok(TaskResult::new_with_status(
            None,
            self.next.clone(),
            Some(summary),
        ))
    }
}

/// The presented grid as one JSON value. Ported from `z8run-lance`
/// (`grid_value`): built directly from the grid, never rendered to text and
/// parsed back, so every cell keeps its exact value.
pub fn grid_value(g: &Grid) -> Result<Value> {
    let strs = |v: &[String]| Value::from(v.to_vec());
    let vals = |v: &[CellValue]| v.iter().map(|&c| cell_value(c)).collect::<Result<Vec<_>>>();
    let pages = g
        .pages
        .iter()
        .map(|(p, rows)| {
            let rows = rows
                .iter()
                .map(|row| {
                    let cells = row
                        .cells
                        .iter()
                        .map(|c| vals(c).map(Value::from))
                        .collect::<Result<Vec<_>>>()?;
                    Ok(json!({ "row": strs(&row.labels), "cells": cells, "total": vals(&row.total)? }))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(json!({ "page": strs(p), "rows": rows }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "columns": g.columns.iter().map(|c| strs(c)).collect::<Vec<_>>(),
        "measures": strs(&g.measures),
        "pages": pages,
        "grand_total": vals(&g.grand_total)?,
    }))
}

/// One cell, exactly. A non-finite real has no JSON form and is refused.
fn cell_value(v: CellValue) -> Result<Value> {
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;
    Ok(match v {
        CellValue::Null => Value::Null,
        CellValue::Int(i) => Value::from(i),
        CellValue::Real(r) if !r.is_finite() => {
            return Err(failed(format!(
                "non-finite cell {r} has no JSON representation"
            )))
        }
        CellValue::Real(r)
            if r.fract() == 0.0
                && (0.0..2.0 * TWO_63).contains(&r)
                && !(r == 0.0 && r.is_sign_negative()) =>
        {
            Value::from(r as u64)
        }
        CellValue::Real(r) if r.fract() == 0.0 && (-TWO_63..0.0).contains(&r) => {
            Value::from(r as i64)
        }
        CellValue::Real(r) => Value::from(r),
    })
}
