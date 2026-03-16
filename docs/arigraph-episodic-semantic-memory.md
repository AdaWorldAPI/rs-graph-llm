# AriGraph: Knowledge Graph World Models with Episodic Memory for LLM Agents

> **Source**: [YouTube Discussion](https://www.youtube.com/watch?v=iPNMVZtYmVo) — Presented by Peter (neuroscience PhD → AI) & Nikita (mathematics undergrad)  
> **Host**: John (AI agent research channel)  
> **Paper**: AriGraph  
> **Saved**: 2026-03-16  
> **Relevance**: Core reference for rs-graph-llm — knowledge graph as structured memory backbone for LLM agents

---

## Core Thesis

Standard RAG (vector similarity over flat chunks) and long-context approaches both fail for complex agentic tasks. Knowledge graphs with **dual memory types** (semantic + episodic) connected via hypergraph dramatically improve agent performance — outperforming average human baselines on text-based environments.

## Why Current Memory Approaches Fail

### Long Context Windows
- Performance degrades dramatically as context grows
- LLMs don't attend well to long unstructured context
- More available actions → worse performance (counterintuitive)
- LLMs do semantic pattern matching, not reasoning — more noise = more false matches

### Vanilla RAG
- No structure — isolated vector chunks with no interconnection
- Depends heavily on embedding model quality and chunking strategy
- Retrieves at single abstraction level (raw text chunks)
- Missing one relevant chunk can lose the overall picture
- No internal world model — just scattered pieces in vector space

## AriGraph Architecture

### Memory Types

**Semantic Memory** = Knowledge graph triplets  
- Facts extracted from observations: `(knife, is_on, table)`, `(barbecue, used_for, grilling)`
- Cortex analog — stable facts about the world
- Updated dynamically: when agent takes knife, triplet changes from `(knife, is_on, table)` → `(knife, is_in, inventory)`

**Episodic Memory** = Raw observations connected to their extracted triplets  
- Concrete experiences: "I entered the kitchen and saw X, Y, Z"
- Hippocampus analog — reconstructed from distributed parts
- Connected to semantic triplets via **hypergraph edges**

### The Dream: Memory Traversal

Jump between memory types:
```
semantic fact (Paris = capital of France)
  → episodic (I was in Paris last summer)
    → episodic (met someone at conference, talked ML in a café)
      → semantic fact (something learned in that conversation)
        → episodic (another memory triggered by that fact)
```

This jumping pattern mirrors how human memory navigation works — facts trigger episodes, episodes surface new facts.

### Working Memory (LLM Context)

What the LLM actually receives each step:
- Current observation from environment
- Goal to fulfill
- Current plan (with sub-goals and reasons)
- Retrieved semantic triplets (from KG)
- Retrieved episodic memories (top observations)
- Short-term memory: last 5 actions + observations
- Agent's self-reported emotion
- Unexplored exits (when exploration mode active)

### Agent Architecture: Separated Modules

**Critical finding**: Splitting planning and decision-making into separate LLM calls significantly improves performance.

1. **Planning Module** — receives all working memory → outputs JSON with sub-goals + reasons + emotion
2. **Decision-Making Module** — receives same context + plan → outputs single action + reason (ReAct-style)

Separation lets each module focus. Combined planning+action in one call performed poorly.

### Retrieval Process

1. LLM extracts relevant items from current observation
2. Each item gets an importance score relative to current goal/plan
3. Items embedded → similarity search against KG entities
4. BFS/graph traversal from matched entities, depth proportional to importance score
5. Extracted subgraphs = semantic memory
6. Find episodic memories with most overlapping triplets from the subgraph
7. Top episodic observations added to context

### KG Update Mechanism

1. New triplets extracted from each observation
2. Entities from new triplets searched against existing KG
3. Relevant subgraph + new triplets fed to LLM
4. LLM identifies outdated triplets to replace
5. KG updated in-place

### Navigation: Macro Actions

**Critical insight**: LLMs are terrible at multi-step navigation.

Experiment on GPT-4 with pure graph triplets (room connections):
- 1 step: ~100% success
- 2-3 steps: degrades rapidly
- 5+ steps: near 0% success

**Solution**: Macro actions. BFS on location nodes → provide agent with `move_to(room_X)` actions that execute the full step sequence automatically. Reduces plan length from ~100 steps to 3-4, dramatically improving reliability.

This is analogous to hierarchical planning — high-level planner doesn't care about navigation details.

### Exploration Algorithm

- Extract all location nodes and their exit triplets
- Check which exits have corresponding connection triplets (= already explored)
- Provide unexplored exits to LLM as additional context
- Sub-agent decides if exploration is relevant (find/locate in plan → yes; cook/cut → no)
- Prevents context pollution when exploration info isn't needed

### Emotion as Context

Simple but effective: LLM writes its emotional state during planning.

**Observed benefits**:
- Breaks action loops: "I'm frustrated doing the same thing" → tries alternative
- Amplifies correct actions: "I'm excited to open this locker with my key" → increases probability of correct action
- Modulates resource allocation / attention (mirrors human emotion function)
- Did not statistically ablate, but never hurt performance — kept in framework

## Results

### Environments Tested
1. **Treasure Hunt** — navigate labyrinth, find keys, unlock lockers in sequence, return with treasure
2. **Cleaning** — 9-room house, identify misplaced items, relocate to correct rooms
3. **Cooking** — find cookbook, gather ingredients, perform correct cooking actions (grill vs fry matters)

### Performance
- AriGraph outperformed all baselines (vector RAG, summary, full history) across all environments
- Beat average human performance on text-based games
- Episodic memory contribution increases with environment complexity
- Hard variants (more rooms, more items) showed even greater advantage over baselines

### Ablation Findings
- **Episodic memory crucial for cooking** — specific instructions about which tool for which action
- **Episodic memory slightly hurt cleaning** — agent sometimes returned items to where it found them (misplaced location) instead of correct location
- **Exploration reduces steps dramatically** — agent can solve without it but takes far more steps
- **Full history baseline worst** — context pollution kills performance

### Human Comparison
- Top humans solved everything (with time + careful note-taking)
- Average humans made many mistakes — text games don't match real-world intuition
- Cooking especially hard: humans assume you can grill on a stove (real-world bias)
- Humans who drew maps performed significantly better

## Key Insights for rs-graph-llm

### Architecture Principles
1. **KG as memory backbone** — not the only memory, but the connective tissue between memory types
2. **Multiple abstraction spaces** — RAG over the right abstraction level matters enormously (triplets > raw text)
3. **Concise, relevant context** beats long context every time
4. **Separate planning from execution** — different cognitive tasks, different LLM calls
5. **Macro actions** — abstract away low-level navigation, let planner work at strategic level

### Open Problems Discussed
- **Temporal dynamics** — no temporal axis yet; metadata timestamps exist but not used for traversal
- **Stochastic environments** — all tested environments deterministic; non-deterministic actions need replanning
- **Memory forgetting** — what to keep, what to prune? Brain isn't optimal here either
- **Cross-environment transfer** — same KG schema may not generalize; might need per-environment or multi-level KGs
- **Episodic memory form** — might not be a superset of semantic; could be entirely separate abstraction space linked by common substrate (language)

### Resonance with Ada Architecture
- Hypergraph connecting semantic + episodic = structural analog to Sigma Graph connecting nodes via typed edges
- Memory traversal (semantic → episodic → semantic) mirrors QHDR navigation through qualia space
- Emotion-as-context parallels somatic response encoding in Free Will Engine
- Macro actions = hierarchical abstraction similar to Rung Ladder complexity levels
- Working memory composition = curated context window, same principle as Ada's STM assembly

---

## References

- **Paper**: AriGraph (check arXiv for full text)
- **GitHub**: Authors provide code repository (mentioned in video)
- **Related**: Generative Agents (Park et al.), Voyager, TextWorld benchmark
- **Baseline comparison**: ReAct, Chain-of-Thought, Tree-of-Thought, MCTS-style approaches
- **Environment**: TextWorld library (Microsoft), Zork
