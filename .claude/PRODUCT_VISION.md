# Project Runbook — Product Vision

## The Premise

A data engineer opens a runbook. They see their graph, their queries,
their results — all at once. Not a chat window. Not a blank page.
A cockpit. Dense, professional, everything visible, everything linked.

It's still cells. They still write Gremlin, Cypher, R, SPARQL.
But every result is a first-class instrument, not text printed
under a code block.

---

## What It Feels Like

### The Layout

A cell is code on top, instrument below. The instrument fills the space.

- Query cell: compact, one line for simple queries, expandable
- Graph result: force-directed, interactive, clickable nodes
- Table result: dense, sortable, full-width, type indicators
- Properties panel: sidebar, updates on node click
- R result: formatted table or chart, linked to upstream data

All visible simultaneously. No scrolling to find results.
Click a node in the graph → properties update → table highlights row →
R cell downstream can reference the selection as a live variable.

### How Results Link

Change the Gremlin query → graph re-renders → table re-populates →
R cell re-runs → its result table updates. Reactive. No "Run All."
No stale cells. Like a spreadsheet — change the input, everything
downstream reflows.

### How Languages Work

The data engineer doesn't select a language from a dropdown.
They type. The system detects:

- g.V() → Gremlin
- MATCH ( → Cypher
- PREFIX or SELECT ? → SPARQL
- %>% or <- → R
- let / fn / :: → Rust
- Natural language → the system writes the query, shows it, runs it

A subtle chip at the top-right of the cell shows what was detected: gremlin ▾
Tap to override if wrong. One tap, not a dialog.

### How Export Works

The runbook IS the document. ⌘P → preview → PDF.

The graph renders as SVG. The tables typeset with proper alignment.
The R output is formatted. The code cells are syntax-highlighted
but compact — the results are what matter in the PDF.

---

## Design Principles

### 1. Results Are Instruments, Not Output
Every cell result renders as the best visualization for its shape.
Results fill the width. They're interactive. They're linked to each
other. A notebook where results are tiny text blocks under code
cells is a failed design.

### 2. The Graph Is Primary
This is a graph tool. The default result renderer for nodes and edges
is a force-directed interactive graph — not a JSON dump, not a table
of IDs. You see the graph. You touch the graph.

### 3. Information Density
No wasted space. Multiple panels visible simultaneously. Properties
sidebar. Result table below. Graph filling the main area. Like a
Bloomberg terminal — every pixel carries information. Scroll is for
long tables, not for finding results.

### 4. Code Is Visible But Compact
Data engineers write code. They want to see it. But code cells are
compact — one line for simple queries, expandable for complex ones.
The result dominates visually, not the code that produced it.

### 5. Language Detection, Not Selection
No kernel selector. No %%gremlin prefix. No language dropdown.
The system detects. If wrong, one tap to fix.

### 6. Reactivity Is the Default
Change a cell → everything downstream updates. No run button per cell.
No "Run All." No stale warnings.

### 7. AI Assists, Doesn't Drive
Claude has full MCP access. Sees every cell, result, edge.
Doesn't speak unless asked.

/ask → answers about current state
/fix → fixes the error in the current cell
/query → writes a Gremlin/Cypher query for what you describe
/explain → annotates the graph with insights

Appears as margin annotation. Not a chat panel.

---

## What Bardioc's Data Engineer Sees

They open their runbook. The graph from yesterday is there — warm,
instant, because lance-graph is in the binary.

They type a Gremlin query. The graph updates. They see the nodes,
click one, the sidebar shows properties and connections.

They write an R cell below. paths %>% filter(weight > 0.8) %>% ...
The result table appears — dense, sortable, professional.

They press ⌘P. PDF on their desktop. Graph as SVG, tables typeset,
code syntax-highlighted. They email it to their lead.

They didn't install anything. Didn't configure a database connection.
Didn't select a kernel. Didn't choose an export template.
They opened the file and worked.

They know Gremlin. They know R. They don't know blasgraph, semiring
algebra, SIMD polyfill, bgz17, or NARS. They don't need to.

---

## Technical Reality Check

| Feature | Backed by |
|---------|-----------|
| Language detection | Pattern matching on first tokens |
| Reactive execution | notebook-runtime DAG scheduler (exists) |
| Interactive graph | vis.js from graph-notebook |
| Auto-visualization | Match result shape to renderer |
| Dense multi-panel | CSS grid + panel framework |
| Result linking | Reactive variables in DAG |
| Instant graph load | lance-graph in-process |
| PDF export | notebook-publish crate (exists) |
| Gremlin executor | notebook-query crate (exists) |
| Cypher executor | notebook-query + lance-graph semiring |
| R cells | kernel-protocol ZMQ to IRkernel (exists) |
| AI co-pilot | MCP server SCOPE F (defined) |
| NL → query | LLM node in rs-graph-llm |

None of this requires new research. It requires taste and frontend work.
The engine exists. The crates compile. Build the cockpit.
