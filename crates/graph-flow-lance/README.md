# graph-flow-lance

graph-flow Tasks over the lance-graph fold algebra. graph-flow owns
orchestration; lance-graph owns lanes, masks, folds and their merge laws. The
seam between them carries one thing, a handle:

```json
{"$kind":"lance-abi","role":"plan","handle":17,"generation":4,"source":9}
```

It is the same seam `z8run-lance` carries for z8run flows, over the same
`lance-graph-report` plans. A `ReportPlan` lowers into `lance-graph-quack`'s
`Query` and runs on `lance-graph-mask-risc`'s one evaluator, so graph-flow,
z8run and Quack callers share one representation.

## Where each piece sits

| Role | Here | Shape |
|---|---|---|
| LangGraph | rs-graph-llm `graph-flow` | Tasks, edges, sessions, pauses |
| LangChain | Rig, behind `llm::Completion` | models, tools, retrieval |
| n8n | z8run, through `z8run-lance` | visual flows |
| the engine under all three | lance-graph-report, Quack, mask-risc | DuckDB-shaped plans, mask and fold execution |

## LangGraph concepts, mask- and fold-shaped

| LangGraph | here |
|---|---|
| state channel with a reducer | a measure: fold states with an identity and an associative, commutative merge |
| `Send` fan-out, then reduce | an `axis` task over `CoordSpec::MaskSet`: one member per branch, one fold for all branches, the reduce is the merge along that axis |
| checkpointer | graph-flow `SessionStorage` holding handles, checked against the source generation on resume |
| `interrupt` | `NextAction::WaitForInput` |
| node that calls a model | `LlmTask`: the model sees the materialized export only |

## Tasks

| task | on a plan handle | on a result handle |
|---|---|---|
| `source` | emits a fresh plan over the named source | |
| `filter` | ANDs a selection | refused |
| `axis` | adds a coordinate in a role | refused |
| `measure` | adds a measure | refused |
| `rotate` | swaps rows and columns | re-views the same cells |
| `execute` | folds, or reuses a cached fold | refused |
| `materialize` | refused | writes the export, the only data a `Context` holds |

## Tests

`cargo test --manifest-path crates/graph-flow-lance/Cargo.toml`, with
`AdaWorldAPI/lance-graph` and `AdaWorldAPI/ndarray` checked out beside this
repository. Every expected number comes from a plain loop over the raw
vectors the source was built from.

| test | what it shows |
|---|---|
| `a_chain_folds_like_a_plain_loop` | filter, axis and five measures through a graph equal the loop, per label and in total |
| `a_saved_session_holds_handles_not_rows` | at 1,000 and 100,000 rows the persisted sessions are byte-identical and under 1 KB, while the folds saw both populations |
| `send_is_one_mask_set_fold` | five overlapping branches: one mask-set fold equals five task-pool folds equals the loop; the reduce is right for the mean too, where merging finalized branch means is not |
| `a_resumed_session_answers_the_same_and_a_republished_source_fails_closed` | a session persisted as JSON resumes on a new graph instance with the same answer; after a republish, a stale result does not resolve and a stale plan does not run |
| `a_rotated_plan_reuses_the_fold` | rotation costs no second fold |
| `the_model_sees_only_the_export` | the prompt holds the export and no handle; without a materialize step the model gets nothing |
| `a_misspelt_source_fails_when_the_graph_is_built` | names resolve when the graph is built |

Each guard was disabled once and its test went red:

| disabled | red |
|---|---|
| `filter` made a no-op | `a_chain_folds_like_a_plain_loop` |
| the registry's generation check | `a_resumed_session_answers_the_same_and_a_republished_source_fails_closed` |
| the fold cache | `a_rotated_plan_reuses_the_fold` |
| the model reads the envelope key | `the_model_sees_only_the_export` |
| `source` also stores the row count | `a_saved_session_holds_handles_not_rows` |

With the registry's generation check off, a paused plan still fails closed:
`lance-graph-report` refuses it again with `StaleSource`. A finished result
has only the registry's check, which is why the test also resolves one.

## Not built

- A wire identity. A handle is a counter local to one registry, so it can't
  leave the process. An external record needs an identity derived from what
  it names: source, generation and the plan's role-free physical key.
- Suspension as bytes, readiness scheduled by loco, and folding model output
  as evidence before it becomes state, from lance-graph's 2026-10-05 working
  model and OGAR's loco gap list.
- Retrieval through Quack plans. Rig's LanceDB store adapter queries Lance
  directly today.
