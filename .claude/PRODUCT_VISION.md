# Project Runbook — Product Vision

## The Premise

You open it. There's a blank page and a blinking cursor.
You type. It knows what you mean.

No kernel selector. No connection dialog. No language dropdown.
No "configure your database endpoint." No "install this extension."
No "which output format would you like?"

You type a Cypher query, it runs. You type R, it runs R.
You type a question in German, it answers.
You type nothing and just look at the graph — and you can touch it,
drag it, ask it things by clicking.

The tool disappears. The data breathes.

---

## What It Feels Like

### First Open

A dark canvas. Warm typography. One line:

> _What would you like to explore?_

You type: `show me everything connected to Ada`

The system figures out: that's a graph query. It finds your graph
(the one you opened, or the one you always use — it remembers).
Nodes appear. Edges draw themselves. Labels fade in.
The layout settles like leaves on water.

No loading spinner. No "connecting to database." It was already connected
the moment you opened the file. The graph was already warm in memory.

### Second Cell

You click below the graph. The cursor appears.
You type: `what's the strongest causal path?`

It knows you're asking about the graph above. It runs NARS truth values
across the edges, highlights the path in gold, fades everything else.
A small annotation appears: _truth ⟨0.87, 0.93⟩ across 4 hops_.

You didn't write a query. You asked a question. The system chose
Cypher + NARS + visual highlighting. You just see the answer.

### Third Cell

You want numbers. You type:

```
edge_weights %>% 
  group_by(relation_type) %>% 
  summarize(mean_truth = mean(truth_value))
```

It's R. The system knows. No `%%r` prefix needed. The R kernel was
already warm (it started when you opened the notebook — like Spotlight
indexing, invisible). The result is a table. Clean. Sortable.
Bardioc's analyst recognizes this. It's their language.

### The Export

You press `⌘P`. Not a dialog — a preview. The notebook is already
a document. The graph is a vector image. The table is typeset.
The title is the filename. Hit Enter. PDF on your desktop.

You didn't choose a template. The system used the one that fits —
if there's a graph, it's the graph report template. If it's all code,
it's the technical document. If there's prose, it's the narrative.
You can override. But you won't need to.

---

## Design Principles

### 1. The Graph Is Primary
This is not a code notebook with graph support bolted on.
This is a graph exploration tool that happens to support code.
The default view is the graph. Code is what you type when
pointing and clicking isn't enough.

### 2. Language Vanishes
The user never selects a language. The system detects:
- Starts with `MATCH` / `CREATE` → Cypher
- Starts with `g.V()` → Gremlin  
- Starts with `SELECT` / `PREFIX` → SPARQL
- Has `%>%` or `<-` → R
- Has `let` / `fn` / `use` → Rust
- Has `import` / `def` → Python
- Everything else → natural language → the system writes the query

No `%%cypher` prefix. No kernel selector dropdown. The prefix is
for machines. Humans just write.

If the detection is wrong — unlikely but possible — a subtle
chip appears: `cypher ▾` and you tap to change it. One tap.
Not a modal dialog. Not a settings page.

### 3. Connection Is Invisible
The notebook file knows its graph. Open the file, the graph is there.
Like opening a Pages document — you don't configure which font server
to connect to. The fonts are embedded. The graph connection is embedded.

Local graphs (lance-graph) are instant — they're in the binary.
Remote graphs (Neo4j, FalkorDB) reconnect silently on open.
If the remote is down, you see the last-known state with a gentle
amber dot. Not an error dialog. A dot.

### 4. Results Render Themselves
The system looks at the result and chooses the best rendering:
- Nodes and edges → interactive graph (vis.js, force-directed)
- Rows and columns → sortable table
- Single number → large, centered, typeset
- Time series → line chart
- Distribution → histogram
- Text → prose, formatted
- Error → inline, red underline on the problematic token, not a stacktrace

You can override. Click the tiny `⊞` to switch between renderings.
But the default is right 90% of the time.

### 5. Reactivity Is Silent
Change a cell, downstream cells re-run. No "Run All" button.
No stale warnings. No "cell 7 depends on cell 3 which hasn't been run."
It just works. Like a spreadsheet — change A1, B1 updates.

The dependency graph exists but you never see it unless you ask.
Triple-click the margin → the DAG appears as a ghost overlay.
Click away, it's gone.

### 6. The Notebook Is Already a Document
There is no separate "export" step. The notebook IS the document.
What you see is what prints. The graph renders as SVG in the PDF.
The table typesets with proper alignment. The code is syntax-highlighted.

`⌘P` → preview → Enter → PDF. Three keystrokes.

### 7. AI Is the Co-pilot, Not the Driver
Claude is there. Always. But doesn't speak unless spoken to.

Click a node → Claude sees what you clicked.
Type `/ask` → Claude answers about the current graph state.
Type `/fix` → Claude fixes the error in the current cell.
Type `/write` → Claude drafts a prose section about the results.

Claude has full MCP access to the notebook state. It knows every cell,
every result, every edge. But it waits.

When it does speak, it speaks in the margin — a subtle annotation,
not a popup. Not a chat window. An annotation. Like a professor's
pencil mark on your paper.

---

## What Bardioc Sees

Their analyst opens the file. They see their graph.
They type R. It runs R. They type Gremlin. It runs Gremlin.
They press ⌘P. They get a PDF.

They don't know about blasgraph. They don't know about semiring algebra.
They don't know about SIMD polyfill or bgz17 palette compression.
They don't know there's a Cypher→semiring planner running under the hood.

They know: I typed a query, I got my graph, I made my report.
It was fast. It was beautiful. It just worked.

That's the car.

---

## The Name

Not "notebook." Not "workbench." Not "studio."

Something that sounds like what it does — you look at a graph
and understand it. You explore connections. You find paths.

_Runbook_ is internal. For users it needs to feel like a place,
not a tool. Like how "Safari" isn't "Web Browser" and "Pages" 
isn't "Word Processor."

Naming is deferred. Ship it first. The name will come from
watching someone use it for the first time and asking them
what they'd call it.

---

## Technical Reality Check

Everything described above is buildable with what exists today:

| Feature | Backed by |
|---------|-----------|
| Language detection | ~50 lines of pattern matching on first tokens |
| Reactive re-execution | notebook-runtime DAG (exists, compiles) |
| Graph rendering | vis.js (exists in graph-notebook, extract) |
| Auto-visualization | Match result shape to renderer (table/graph/chart) |
| Instant open | lance-graph in-process, no connection overhead |
| PDF export | notebook-publish crate (exists, compiles) |
| AI co-pilot | MCP server (SCOPE F, defined) |
| R integration | kernel-protocol ZMQ (exists, compiles) |
| Natural language → query | LLM node in rs-graph-llm graph |

None of this requires new research. It requires taste.
