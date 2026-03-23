# SCOPE F: MCP Server — Claude Code ↔ Notebook

## You touch: rs-graph-llm/crates/notebook
## You do NOT touch: other crates' internals (call their APIs)

## Goal
The notebook binary exposes an MCP server. Claude Code connects to it,
executes cells, reads results, inspects state, and drives the notebook
as an agent. No REST API layer needed — MCP IS the API.

## MCP Transport
SSE on the existing axum server. Endpoint: `/mcp/sse`

Claude Code connects with:
```
mcp_servers: [{ type: "url", url: "http://localhost:2718/mcp/sse", name: "notebook" }]
```

## Tools to Expose

### cell_execute
Execute a cell. Returns result immediately (sync) or streams (async).
```json
{
  "name": "cell_execute",
  "input": {
    "code": "MATCH (a)-[:CAUSES]->(b) RETURN a, b",
    "lang": "cypher",
    "cell_id": "optional — auto-generates if omitted"
  },
  "output": {
    "cell_id": "c_001",
    "status": "ok|error",
    "output": "Arrow JSON or text",
    "timing_ms": 12,
    "downstream_rerun": ["c_003", "c_007"]
  }
}
```

Supported `lang`: cypher, gremlin, sparql, nars, rust, r, python, markdown

### cell_get
Read a cell's current state.
```json
{
  "name": "cell_get",
  "input": { "cell_id": "c_001" },
  "output": {
    "cell_id": "c_001",
    "code": "MATCH ...",
    "lang": "cypher",
    "status": "ok|error|stale|pending",
    "output": "...",
    "refs": ["graph_data"],
    "defs": ["query_result"],
    "last_run_ms": 12
  }
}
```

### cells_list
All cells, ordered by position. Includes DAG state.
```json
{
  "name": "cells_list",
  "input": {},
  "output": {
    "cells": [
      { "cell_id": "c_001", "lang": "cypher", "status": "ok", "defs": ["result"] },
      { "cell_id": "c_002", "lang": "rust", "status": "stale", "refs": ["result"] }
    ]
  }
}
```

### cell_create
Add a cell at a position. Does not execute.
```json
{
  "name": "cell_create",
  "input": {
    "code": "println!(\"{}\", result.len())",
    "lang": "rust",
    "after": "c_001"
  },
  "output": { "cell_id": "c_002" }
}
```

### cell_update
Modify cell code. Triggers reactive re-execution of downstream cells.
```json
{
  "name": "cell_update",
  "input": {
    "cell_id": "c_001",
    "code": "MATCH (a)-[:CAUSES*1..3]->(b) RETURN a, b"
  },
  "output": {
    "cell_id": "c_001",
    "status": "ok",
    "downstream_rerun": ["c_002", "c_003"]
  }
}
```

### cell_delete
Remove a cell. Marks downstream cells as stale.
```json
{
  "name": "cell_delete",
  "input": { "cell_id": "c_001" },
  "output": { "stale_cells": ["c_002", "c_003"] }
}
```

### dag_get
The dependency graph as nodes and edges. Claude Code uses this to
understand what depends on what.
```json
{
  "name": "dag_get",
  "input": {},
  "output": {
    "nodes": [
      { "cell_id": "c_001", "defs": ["result"], "status": "ok" },
      { "cell_id": "c_002", "refs": ["result"], "status": "ok" }
    ],
    "edges": [
      { "from": "c_001", "to": "c_002", "via": "result" }
    ]
  }
}
```

### notebook_save / notebook_load
Serialize/deserialize the full notebook state.
```json
{
  "name": "notebook_save",
  "input": { "path": "/home/claude/runbook.nb" },
  "output": { "saved": true, "cells": 7 }
}
```

### notebook_export
Render the notebook to a document via the publish crate.
```json
{
  "name": "notebook_export",
  "input": { "format": "html|pdf", "path": "/home/claude/report.html" },
  "output": { "exported": true, "path": "/home/claude/report.html" }
}
```

## Implementation

Use `rmcp` crate (Rust MCP SDK) or hand-roll the SSE transport.
The MCP tool handler dispatches to the existing crate APIs:

```rust
match tool_name {
    "cell_execute" => runtime.execute(cell_id, code, lang).await,
    "cell_get"     => runtime.get_cell(cell_id),
    "cells_list"   => runtime.list_cells(),
    "cell_create"  => runtime.create_cell(code, lang, after),
    "cell_update"  => runtime.update_cell(cell_id, code).await,
    "cell_delete"  => runtime.delete_cell(cell_id),
    "dag_get"      => runtime.dag(),
    "notebook_save"  => runtime.save(path),
    "notebook_load"  => runtime.load(path),
    "notebook_export" => publish.render(format, path).await,
}
```

## Constraints
- MCP is the ONLY API. No separate REST endpoints (except /health for probes).
- SSE transport, not stdio (the binary is a server, not a subprocess).
- Arrow-serialized results for DataFrames (not JSON rows).
- Reactive: cell_update and cell_execute trigger downstream re-execution
  and report which cells were re-run in the response.
