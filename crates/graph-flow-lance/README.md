# graph-flow-lance

graph-flow Tasks over the lance-graph fold algebra, on the same handle seam
z8run flows use. Orchestration lives in z8run and graph-flow; lanes, masks,
folds and their merge laws live in lance-graph. The seam between them carries
one thing, a handle:

```json
{"$kind":"lance-abi","role":"plan","handle":17,"generation":4,"source":9}
```

The seam is `z8run-lance`'s: its registry, its envelope and its export. This
crate keeps no copy of them, so a handle a z8run flow mints resolves inside a
graph-flow graph and the other way round. A `ReportPlan` lowers into
`lance-graph-quack`'s `Query` and runs on `lance-graph-mask-risc`'s one
evaluator, so z8run, graph-flow and Quack callers share one representation.

## Where each piece sits

| role | here | owns |
|---|---|---|
| n8n | z8run | flows, triggers, external edges, human handoff |
| LangGraph | rs-graph-llm `graph-flow` | stateful graphs, sessions, pauses |
| LangChain | Rig, behind `llm::Completion` | models, tools, retrieval |
| the engine under all three | lance-graph-report, Quack, mask-risc | DuckDB-shaped plans, mask and fold execution |

## LangGraph concepts, mask- and fold-shaped

| LangGraph | here |
|---|---|
| state channel with a reducer | a measure: fold states with an identity and an associative, commutative merge |
| `Send` fan-out, then reduce | an `axis` task over `CoordSpec::MaskSet`: one member per branch, one fold for all branches, the reduce is the merge along that axis |
| checkpointer | graph-flow `SessionStorage` holding handles, checked against the source generation on resume |
| `interrupt` | `NextAction::WaitForInput`; inside z8run, a session handle on the node's `waiting` port |
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

## The `graph-flow` z8run node

Register it with `register_graph_flow_node(engine, host)`, where the
`GraphFlowHost` names the graphs a flow may run and where paused sessions
wait.

| payload `$kind` | the node |
|---|---|
| `lance-abi` | starts the configured graph with the handle in its `Context` |
| `graph-flow-session` | resumes that session after writing `input` into its `Context` |
| anything else | refuses it |

On completion it emits whatever handle the graph left, with the model's answer
and the export, if any, in the message metadata. When the graph waits for
input it saves the session and emits a session handle on its `waiting` port.
A later message carrying that handle and an `input` object, from a webhook or
a human-handoff step, resumes it. Only handles cross z8run.

## Tests

`cargo test --manifest-path crates/graph-flow-lance/Cargo.toml`, with
`AdaWorldAPI/lance-graph`, `AdaWorldAPI/ndarray` and `AdaWorldAPI/z8run`
checked out beside this repository. Every expected number comes from a plain
loop over the raw vectors the source was built from.

`tests/seam.rs`:

| test | what it shows |
|---|---|
| `a_chain_folds_like_a_plain_loop` | filter, axis and five measures through a graph equal the loop, per label and in total |
| `a_saved_session_holds_handles_not_rows` | at 1,000 and 100,000 rows the persisted sessions are byte-identical and under 1 KB, while the folds saw both populations |
| `send_is_one_mask_set_fold` | five overlapping branches: one mask-set fold equals five task-pool folds equals the loop; the reduce is right for the mean too, where merging finalized branch means is not |
| `a_resumed_session_answers_the_same_and_a_republished_source_fails_closed` | a session persisted as JSON resumes on a new graph instance with the same answer; after a republish, a stale result does not resolve and a stale plan does not run |
| `a_rotated_plan_reuses_the_fold` | rotation costs no second fold |
| `the_model_sees_only_the_export` | the prompt holds the export and no handle; without a materialize step the model gets nothing |
| `a_misspelt_source_fails_when_the_graph_is_built` | names resolve when the graph is built |

`tests/z8run_flow.rs`:

| test | what it shows |
|---|---|
| `a_z8run_flow_hands_its_plan_to_a_graph_flow_graph` | a plan built by z8run-lance nodes runs in a graph that folds, exports and asks a model; every message before it is a handle under 200 bytes |
| `a_graph_flow_plan_feeds_z8run_lance_nodes` | a plan a graph builds is executed and exported by z8run-lance nodes |
| `a_paused_graph_crosses_z8run_as_a_session_handle` | the pause leaves z8run as a session handle; the saved session holds only the plan handle; a webhook message resumes it with the right answer, a session of another graph is refused, and a finished session is not kept |
| `a_graph_flow_node_takes_handles_never_data` | a payload of rows is refused, and so is an unknown graph |

Each guard was disabled once and its test went red:

| disabled | red |
|---|---|
| `filter` made a no-op | `a_chain_folds_like_a_plain_loop` |
| z8run-lance's generation check | `a_resumed_session_answers_the_same_and_a_republished_source_fails_closed` |
| z8run-lance's fold cache | `a_rotated_plan_reuses_the_fold` |
| the model reads the envelope key | `the_model_sees_only_the_export` |
| `source` also stores the row count | `a_saved_session_holds_handles_not_rows` |
| the node emits the session instead of its handle | `a_paused_graph_crosses_z8run_as_a_session_handle` |
| the node ignores the resume input | `a_paused_graph_crosses_z8run_as_a_session_handle` |
| the node passes data through | `a_graph_flow_node_takes_handles_never_data` |
| the node drops the result handle | the three z8run flow tests that read it |

With the generation check off, a paused plan still fails closed:
`lance-graph-report` refuses it again with `StaleSource`. A finished result
has only the registry's check, which is why the test also resolves one.

## Not built

- A wire identity. A handle is a counter local to one registry, so it can't
  leave the process. An external record needs an identity derived from what
  it names: source, generation and the plan's role-free physical key.
- z8run's own AI nodes still stand alone: `llm` and `ai_agent` call
  providers directly, `vector_store`, `conversation_memory` and
  `human_handoff` keep in-process maps, and `ai_agent` passes its whole
  conversation through messages. Routing them to Rig, lance-graph and
  graph-flow sessions is the next step on the z8run side.
- Suspension as bytes, readiness scheduled by loco, and folding model output
  as evidence before it becomes state, from lance-graph's 2026-10-05 working
  model and OGAR's loco gap list.
- Retrieval through Quack plans. Rig's LanceDB store adapter queries Lance
  directly today.
