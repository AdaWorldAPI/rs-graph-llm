# LangGraph Parity Checklist

> What exists, what's missing, priority ranking for each gap.

Legend:
- **DONE** = Implemented and tested in Rust
- **PARTIAL** = Exists but incomplete vs Python equivalent
- **MISSING** = Not implemented
- Priority: **P0** (critical), **P1** (important), **P2** (nice-to-have), **P3** (low/skip)

---

## Core Graph Construction

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `GraphBuilder` (fluent API) | DONE | — | Fully working |
| `StateGraph` (LangGraph compat) | DONE | — | `compat.rs` |
| `START` / `END` constants | DONE | — | `compat.rs` |
| `add_node()` | DONE | — | Via `add_task()` |
| `add_edge()` | DONE | — | Direct + conditional |
| Binary conditional edges | DONE | — | `add_conditional_edge(from, cond, yes, no)` |
| N-way conditional edges (`path_map`) | DONE | — | `add_conditional_edges(from, path_fn, path_map)` |
| `add_sequence()` (ordered chain) | DONE | — | `GraphBuilder::add_sequence()` in graph.rs |
| `set_entry_point()` | DONE | — | `set_start_task()` |
| `set_conditional_entry_point()` | MISSING | P2 | Conditional start routing |
| `set_finish_point()` | MISSING | P3 | Implicit via `NextAction::End` |
| `compile()` | DONE | — | `build()` / `StateGraph::compile()` |
| `validate()` (graph validation) | DONE | — | `GraphBuilder::validate()` in graph.rs |

## Graph Execution

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| Step-by-step execution | DONE | — | `execute_session()` |
| Run to completion | DONE | — | `FlowRunner::run()` loop |
| Async execution | DONE | — | Native async/await |
| Task timeout | DONE | — | `Graph.task_timeout` |
| Recursion limit | DONE | — | `RunConfig.recursion_limit` |
| Breakpoints (interrupt_before) | DONE | — | `BreakpointConfig` |
| Breakpoints (interrupt_after) | DONE | — | `BreakpointConfig` |
| Dynamic breakpoints | DONE | — | Via `RunConfig` |
| Batch execution | DONE | — | `FlowRunner::run_batch()` |
| `invoke()` equivalent | DONE | — | `execute_session()` |
| `stream()` equivalent | DONE | — | `StreamingRunner::stream()` |
| Stream modes (values/updates/debug) | DONE | — | `StreamMode` enum |
| Stream mode: messages | PARTIAL | P1 | Basic chat history streaming |
| Stream mode: custom | MISSING | P2 | Custom stream channels |
| Stream mode: events | MISSING | P2 | LangSmith-style events |
| `ainvoke()` / `astream()` | DONE | — | All Rust is async-native |
| Tags / metadata on runs | DONE | — | `RunConfig` |

## State Management

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| Key-value context | DONE | — | `Context` with DashMap |
| Typed state (`TypedContext<S>`) | DONE | — | Generic state struct |
| Chat history | DONE | — | `ChatHistory` in Context |
| `add_messages` reducer | PARTIAL | P1 | Manual add, no dedup/update by ID |
| Context serialization | DONE | — | `Context::serialize()` |
| Sync + async access | DONE | — | `get_sync()` / `set_sync()` + async |
| State snapshots | DONE | — | `StateSnapshot` in state_snapshot.rs |
| State history / time travel | PARTIAL | P1 | `LanceSessionStorage::list_versions()` (mock — not real Lance I/O) |

## Channels

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `LastValue` channel | DONE | — | `ChannelReducer::LastValue` |
| `Topic` (append list) | DONE | — | `ChannelReducer::Append` |
| `BinaryOperatorAggregate` | DONE | — | `ChannelReducer::Custom(fn)` |
| `AnyValue` channel | MISSING | P3 | Rarely used |
| `EphemeralValue` channel | MISSING | P2 | Useful for one-shot data |
| `NamedBarrierValue` | MISSING | P3 | Synchronization primitive |
| `UntrackedValue` | MISSING | P3 | Rarely used |
| Channel checkpoint/restore | MISSING | P2 | From checkpoint support |
| Channel `is_available()` | MISSING | P3 | Availability tracking |
| Channel `consume()` / `finish()` | MISSING | P3 | Lifecycle methods |

## Checkpointing / Storage

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `SessionStorage` trait | DONE | — | `save()` / `get()` / `delete()` |
| In-memory storage | DONE | — | `InMemorySessionStorage` |
| PostgreSQL storage | DONE | — | `PostgresSessionStorage` |
| Lance-backed storage | DONE | — | `LanceSessionStorage` |
| Version history | DONE | — | `list_versions()` / `get_at_version()` |
| Checkpoint namespacing | DONE | — | `save_namespaced()` / `get_namespaced()` |
| `put_writes()` (partial writes) | MISSING | P2 | Write individual channels |
| `copy_thread()` | MISSING | P2 | Clone session state |
| `prune()` (cleanup old) | MISSING | P2 | Storage cleanup |
| `delete_for_runs()` | MISSING | P3 | Selective deletion |
| `CheckpointMetadata` | MISSING | P2 | Rich metadata per checkpoint |
| `get_next_version()` | MISSING | P3 | Auto-version numbering |
| Serde: msgpack | MISSING | P3 | JSON is sufficient |
| Serde: encrypted | MISSING | P2 | Sensitive data at rest |
| SQLite storage | DONE | — | `SqliteSessionStorage` in storage_sqlite.rs |

## Subgraphs

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `SubgraphTask` | DONE | — | Inner graph execution |
| Shared context | DONE | — | Parent/child share Context |
| Input/output mappings | DONE | — | `with_mappings()` |
| Max iteration guard | DONE | — | 1000 iteration limit |
| `get_subgraphs()` introspection | MISSING | P2 | Enumerate child graphs |

## Prebuilt Agents

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `create_react_agent()` | DONE | — | With iteration guard |
| Tool routing | DONE | — | `ToolRouterTask` |
| Tool aggregation | DONE | — | `ToolAggregatorTask` |
| `ToolNode` (full) | DONE | — | `ToolNode` with interceptors in prebuilt/tool_node.rs |
| `tools_condition()` | DONE | — | Conditional edge on `needs_tool` |
| `InjectedState` | MISSING | P1 | Tool state injection |
| `InjectedStore` | MISSING | P2 | Tool store injection |
| `ToolRuntime` | MISSING | P2 | Runtime injection to tools |
| `ToolCallRequest` / interceptors | DONE | — | `ToolCallRequest`, `InterceptorAction` in prebuilt/tool_node.rs |
| `ValidationNode` | MISSING | P2 | Schema validation for tool calls |
| Prompt / system message | DONE | — | `create_react_agent_with_prompt()` in react_agent.rs |
| Model selection (multi-model) | MISSING | P1 | Dynamic model per-call |
| `generate_structured_response()` | MISSING | P2 | Structured output mode |
| `HumanInterrupt` config | DONE | — | `HumanInterrupt` + `ActionRequest` in prebuilt/interrupt.rs |
| `HumanResponse` | DONE | — | `HumanResponse` enum (Action/Text/Cancel) in prebuilt/interrupt.rs |
| `post_model_hook` | MISSING | P2 | Post-inference processing |

## Agent Cards / YAML

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| Agent card YAML schema | DONE | — | `AgentCard` struct |
| `compile_agent_card()` | DONE | — | YAML → Graph |
| `TaskRegistry` | DONE | — | Real task bindings |
| Capability placeholders | DONE | — | `CapabilityTask` fallback |
| LangGraph JSON import | DONE | — | `import_langgraph_workflow()` |

## Error Handling

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `GraphError` enum | DONE | — | 7 variants |
| `ToolResult` (success/error/fallback) | DONE | — | Structured results |
| Retry policy | DONE | — | Fixed/Exponential/None |
| `GraphRecursionError` | DONE | — | `RecursionLimitExceeded` variant in error.rs |
| `NodeInterrupt` | DONE | — | `NodeInterrupt` struct in prebuilt/interrupt.rs |
| `GraphInterrupt` | DONE | — | `GraphInterrupt` variant with task_id/reason/data in error.rs |
| `InvalidUpdateError` | MISSING | P3 | Via TaskExecutionFailed |
| Error codes | MISSING | P3 | Enum-based codes |

## Store (Long-term Memory)

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `BaseStore` trait | DONE | — | `BaseStore` trait in store/mod.rs |
| `InMemoryStore` | DONE | — | `InMemoryStore` in store/memory.rs |
| `Item` / `SearchItem` | DONE | — | `Item`, `SearchItem` in store/mod.rs |
| `get()` / `put()` / `delete()` | DONE | — | Full CRUD in BaseStore trait |
| `search()` with embeddings | PARTIAL | P1 | Filter search done; vector/embedding search via LanceStore NOT YET |
| `list_namespaces()` | DONE | — | Implemented in InMemoryStore |
| `MatchCondition` | DONE | — | JSON path filtering in store/mod.rs |
| `IndexConfig` / `TTLConfig` | MISSING | P2 | Index + expiry config |
| `AsyncBatchedBaseStore` | MISSING | P2 | Batched async operations |
| `PostgresStore` | MISSING | P1 | Persistent store |

## Functional API

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `@task` decorator | MISSING | P2 | Macro could work (`#[task]`) |
| `@entrypoint` decorator | MISSING | P2 | Macro for graph entry |
| `SyncAsyncFuture` | N/A | — | Rust is natively async |

## Runtime

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| `Runtime` class | MISSING | P2 | Runtime context injection |
| `get_runtime()` | MISSING | P2 | Access current runtime |
| `Runtime.override()` | MISSING | P2 | Override runtime values |

## HTTP API / Server

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| Thread creation (POST) | DONE | — | `/threads` |
| Thread execution (POST) | DONE | — | `/threads/{id}/runs` |
| Thread state (GET) | DONE | — | `/threads/{id}/state` |
| Thread deletion (DELETE) | DONE | — | `/threads/{id}` |
| Thread history (GET) | DONE | — | `GET /threads/{id}/history` in server |
| SSE streaming endpoint | DONE | — | `POST /threads/{id}/runs/stream` SSE in server |
| Cron runs | MISSING | P3 | Scheduled execution |
| Assistants CRUD | MISSING | P2 | Multi-graph management |

## Visualization / Debugging

| Feature | Status | Priority | Notes |
|---------|--------|----------|-------|
| Mermaid diagram export | MISSING | P2 | `get_graph()` → Mermaid |
| Debug stream mode | DONE | — | `StreamMode::Debug` |
| Task history tracking | DONE | — | `Session::task_history` |

---

## Priority Summary (Updated 2026-03-18)

| Priority | Remaining | Description |
|----------|-----------|-------------|
| **P0** | 0 | Store/memory system — DONE |
| **P1** | 5 | Remaining: InjectedState, model selection, add_messages dedup, vector search in LanceStore, LanceSessionStorage real I/O |
| **P2** | 22 | Nice-to-have features (functional API, visualization, advanced channels) |
| **P3** | 12 | Low priority (edge cases, rarely used) |

### Completed Sprints
- **Sprint 1 (P0)**: ✅ Store/Memory — BaseStore, InMemoryStore, Item/SearchItem, MatchCondition
- **Sprint 2 (P1-core)**: ✅ Interrupts (NodeInterrupt, HumanInterrupt/Response), graph validation, SSE streaming, thread history
- **Sprint 3 (P1-agents)**: ✅ PARTIAL — ReAct with prompt done, ToolNode+interceptors done. Missing: InjectedState, model selection

### Next Sprint
- **Sprint 4 (P1-storage)**: Real LanceSessionStorage (Lance I/O, not DashMap mock), PostgreSQL BYTEA migration, TieredSessionStorage, LanceStore for BaseStore
- **Sprint 5 (P2)**: Functional API macros, Mermaid export, advanced channels, `Runtime`
