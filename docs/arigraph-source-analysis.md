# AriGraph: Source-Level Architecture Analysis

> **Repository**: [AriGraph](https://github.com/AdaWorldAPI/AriGraph) (Python, ~2,500 LOC)
> **Paper**: [arXiv:2407.04363](https://arxiv.org/abs/2407.04363) (July 2024)
> **Authors**: Petr Anokhin, Nikita Semenov, Artyom Sorokin, Dmitry Evseev, Mikhail Burtsev, Evgeny Burnaev
> **Purpose**: External memory architecture for LLM agents using a knowledge graph with dual (semantic + episodic) memory

---

## 1. Problem Statement

Standard approaches to LLM agent memory all fail at complex multi-step tasks:

| Approach | Failure Mode |
|----------|-------------|
| **Long context** | Performance degrades as context grows; more actions = worse decisions |
| **Vanilla RAG** | Flat vector chunks with no interconnection; miss one chunk = lose the picture |
| **Summary** | Lossy compression; critical details lost |
| **Full history** | Context pollution; noise overwhelms signal |

AriGraph's answer: a **structured knowledge graph** with **two memory types** (semantic triplets + episodic observations) connected via embedding similarity and graph traversal.

---

## 2. Data Structures (Source-Verified)

### 2.1 Triplet (Core Atom)

From `graphs/parent_graph.py`:

```python
# Internal representation: [subject, object, {"label": relation}]
triplet = ["knife", "table", {"label": "is_on"}]

# String form: "knife, is_on, table"
def str(self, triplet):
    return triplet[0] + ", " + triplet[2]["label"] + ", " + triplet[1]
```

**Key properties**:
- Subject and object are **atomic entities** (single noun phrases)
- Relation is a **short label** (max 7 words per extraction prompt)
- Stored as Python list with dict for relation metadata
- Equality is structural (list comparison)

### 2.2 TripletGraph (Base Class)

From `graphs/parent_graph.py` (210 lines):

```python
class TripletGraph:
    def __init__(self, model, system_prompt, api_key):
        self.triplets = []           # List[triplet]
        self.items = []              # implicit via add_triplets
        self.model = model           # OpenAI model name
        self.client = OpenAI(...)    # Direct OpenAI API
        self.total_amount = 0        # Cost tracking ($)
```

**Core operations**:

| Method | Purpose | Complexity |
|--------|---------|-----------|
| `add_triplets(triplets)` | Deduplicated insert, auto-extract items | O(n*m) |
| `delete_triplets(triplets, locations)` | Remove non-navigation triplets | O(n*m) |
| `get_associated_triplets(items, steps=2)` | BFS through triplet graph | O(steps * T * I) |
| `exclude(triplets)` | Filter already-known triplets | O(n*m) |
| `compute_spatial_graph(locations)` | Extract location-only subgraph | O(T) |
| `find_path(a, b, locations)` | BFS shortest path on spatial graph | O(V+E) |
| `generate(prompt, jsn, t)` | OpenAI API call with cost tracking | API call |

**Navigation / Macro Actions** (`find_path`):
- Builds a spatial-only subgraph from triplets where both subject and object are locations
- BFS from A to B, tracking parent pointers
- Returns list of cardinal directions (north/south/east/west)
- Critical insight: LLMs can't navigate >2 steps; macro actions reduce 100-step plans to 3-4

### 2.3 ContrieverGraph (Main Implementation)

From `graphs/contriever_graph.py` (220 lines):

```python
class ContrieverGraph(TripletGraph):
    def __init__(self, model, system_prompt, api_key, device="cpu"):
        super().__init__(model, system_prompt, api_key)
        self.retriever = Retriever(device)       # Facebook mcontriever (768-dim)
        self.triplets_emb = {}                    # str(triplet) -> embedding
        self.items_emb = {}                       # entity_name -> embedding
        self.obs_episodic = {}                    # observation_text -> [triplet_strs, embedding]
        self.obs_episodic_list = []               # history snapshots
        self.top_episodic_dict_list = []          # episodic ranking history
```

**Embedding model**: Facebook's `mcontriever` (BERT-based, 768 dimensions)
- Every triplet string gets embedded on insertion
- Every entity gets embedded on first appearance
- Every observation gets embedded after processing

**`add_triplets` override**:
```python
def add_triplets(self, triplets):
    for triplet in triplets:
        triplet = clear_triplet(triplet)
        if triplet not in self.triplets:
            self.triplets.append(triplet)
            self.triplets_emb[self.str(triplet)] = self.get_embedding_local(self.str(triplet))
            if triplet[0] not in self.items_emb:
                self.items_emb[triplet[0]] = self.get_embedding_local(triplet[0])
            if triplet[1] not in self.items_emb:
                self.items_emb[triplet[1]] = self.get_embedding_local(triplet[1])
```

### 2.4 Hypergraph (Advanced Structure)

From `graphs/hypergraph.py` (234 lines):

Three node types forming a **3-level hierarchy**:

```
Event (observation text)
  └── Hyperedge / "Thesis" (a claim extracted from the event)
        └── Entity (atomic noun extracted from the thesis)
```

```python
class Event:
    name: str            # observation text
    embedding: tensor    # 768-dim
    children: list       # thesis IDs

class Hyperedge:
    name: str            # thesis text (e.g., "kitchen contains apple")
    parents: list        # event IDs that produced this thesis
    embedding: tensor    # 768-dim
    children: list       # entity IDs

class Entity:
    name: str            # atomic noun
    parents: list        # thesis IDs that reference this entity
    embedding: tensor    # 768-dim
```

**Key difference from TripletGraph**: Thesises are **natural language sentences** (not subject-relation-object tuples). This allows richer, context-preserving facts like "North exit from kitchen is blocked by door" instead of splitting into two decontextualized triplets.

**BFS traversal** works bottom-up: entity → parent thesises → sibling entities → their parent thesises...

**Episodic scoring** combines two signals:
1. **Embedding similarity**: cosine(plan_embedding, event_embedding)
2. **Structural overlap**: (matching_thesises / total_thesises) * log(total_thesises)

Both normalized to [0,1] and summed for final ranking.

---

## 3. The Update Cycle (Core Algorithm)

From `ContrieverGraph.update()` — this is the heart of AriGraph:

```
INPUT:  observation, plan, previous_subgraph, locations, items_with_scores
OUTPUT: associated_subgraph, top_episodic_observations

STEP 1: EXTRACT
  - LLM extracts triplets from current observation
  - Prompt: prompt_extraction_current (structured instructions, 7-word max relation)
  - Temperature: 0.001 (near-deterministic)
  - Example triplets from previous subgraph provided as few-shot context

STEP 2: EXCLUDE
  - Filter out triplets already in the graph
  - Only new facts proceed to refinement

STEP 3: REFINE (Outdated Detection)
  - Retrieve 1-hop associated triplets for entities in new triplets
  - Filter out navigation triplets (west/east/south/north)
  - LLM identifies which existing triplets are replaced by new ones
  - Prompt: prompt_refining_items (conservative — only replace semantic duplicates)
  - Delete identified outdated triplets (skip if both ends are locations)

STEP 4: ADD NAVIGATION
  - If action was a cardinal direction (not "go to"):
    - Add bidirectional spatial triplets between current and previous location
    - e.g., [kitchen, bedroom, {"label": "is north of"}]

STEP 5: INSERT
  - Add all new triplets to graph (with embeddings)

STEP 6: SEMANTIC RETRIEVAL
  - For each item in items_with_scores (entity: importance_score):
    - graph_retr_search(query=entity, max_depth=importance_score, topk=6, threshold=0.75)
    - BFS through triplet graph using embedding similarity
  - Exclude triplets already in new_triplets

STEP 7: EPISODIC RETRIEVAL
  - Embed the current plan
  - For each past observation in obs_episodic:
    - Score = normalized_embedding_similarity + normalized_structural_overlap
    - structural_overlap = (matching_triplets / total_triplets) * log(total_triplets)
  - Return top_k episodic observations (excluding recent history)

STEP 8: STORE EPISODIC
  - Store current observation with its triplets and embedding in obs_episodic
```

### 3.1 Graph Retrieval Search (BFS + Embedding)

From `utils/retriever_search_drafts.py`:

```python
def graph_retr_search(start_query, triplets, retriever, max_depth=2, topk=3,
                      post_retrieve_threshold=0.7):
    queue = deque([start_query])
    d = {start_query: 0}
    result = []

    while queue:
        q = queue.popleft()
        if d[q] >= max_depth: continue

        res = retriever.search(triplets, q, topk=topk, return_scores=True)
        for s, score in zip(res['strings'], res['scores']):
            if score < post_retrieve_threshold: continue
            v1, e, v2 = edge(s)          # parse "subj, rel, obj"
            for v in [v1, v2]:
                if v not in d:
                    queue.append(v)
                    d[v] = d[q] + 1
            if s not in result:
                result.append(s)

    return result
```

**Key insight**: This is NOT graph BFS — it's **embedding-guided BFS over triplet strings**. Each hop:
1. Embed the query entity
2. Find top-k most similar triplet strings
3. Extract entities from matched triplets
4. Enqueue new entities as next-hop queries
5. Depth-limited by importance score from the entity scorer

---

## 4. Agent Architecture

### 4.1 Four Separated Agents

From `pipeline_arigraph.py`:

```python
agent         = GPTagent(model="gpt-4o",             system_prompt=default_system_prompt)
agent_plan    = GPTagent(model="gpt-4-0125-preview",  system_prompt=system_plan_agent)
agent_action  = GPTagent(model="gpt-4-0125-preview",  system_prompt=system_action_agent_sub_expl)
agent_if_expl = GPTagent(model="gpt-4o",              system_prompt=if_exp_prompt)
```

| Agent | Role | Output Format |
|-------|------|---------------|
| **Entity scorer** | Extract entities + importance scores from observation | `{"entity": score}` dict |
| **Planner** | Create/update plan with sub-goals, reasons, emotion | JSON with plan_steps array |
| **Action selector** | Choose single action from valid actions | JSON with action_to_take |
| **Exploration decider** | Should we explore? (True/False) | Boolean string |

**Critical design decision**: Planning and action selection are **separate LLM calls**. Combined calls performed significantly worse.

### 4.2 Working Memory Composition

What each agent actually receives (from `choose_action` and `planning` functions):

```
1. Main goal: {main_goal}
2. History of N last observations and actions: {hist_obs}
3. Current observation: {observation}
4. Information from memory module: {subgraph}           ← semantic memory
5. Top K relevant episodic memories: {top_episodic}     ← episodic memory
6. Current plan: {plan0}
7. [Optional] Unexplored exits: {all_unexpl_exits}      ← if exploring
```

Plus for action selection:
```
Possible actions in current situation: {valid_actions}
```

### 4.3 Plan Structure

```json
{
  "main_goal": "Find the treasure",
  "plan_steps": [
    {"sub_goal_1": "Find the key", "reason": "Need key to open locker"},
    {"sub_goal_2": "Navigate to treasure room", "reason": "Treasure is there"}
  ],
  "your_emotion": {
    "your_current_emotion": "excited",
    "reason_behind_emotion": "Found the key!"
  }
}
```

**Emotion tracking**: LLM writes its emotional state during planning. Observed to break action loops ("frustrated → try alternative") and amplify correct actions ("excited → higher confidence"). Never hurt performance.

### 4.4 Exploration Sub-System

From `get_unexpl_exits`:

```python
for loc in locations:
    loc_gr = graph.get_associated_triplets([loc], steps=1)
    unexplored_exits = find_unexplored_exits(loc, loc_gr)
```

`find_unexplored_exits` logic:
1. Find all exit triplets for a location (e.g., "kitchen, has exit, east")
2. Find all connection triplets that prove an exit was explored (e.g., "bedroom, is east of, kitchen")
3. Unexplored = exits − explored_directions

Only activated when exploration sub-agent says True (based on plan content — "find" or "locate" → yes; "cook" or "cut" → no).

---

## 5. Triplet Extraction Prompts (Exact)

### 5.1 Extraction Prompt

Key constraints from `prompt_extraction_current`:
- Break complex triplets into simple ones: "John, position, engineer in Google" → "John, position, engineer" + "John, work at, Google"
- Max 7 words per triplet
- Subject and object must be atomic; relation can be longer
- Extract hypotheses as such: "could be winner" not "will be winner"
- "item is in inventory" for taken items
- Don't miss connections across observation parts
- Don't include agent location triplets ("you, are in, location")
- Previous subgraph provided as few-shot examples

### 5.2 Refinement Prompt

Key constraints from `prompt_refining_items`:
- Only replace if **semantic duplication** between existing and new triplet
- Different properties of same entity → DO NOT replace
- "brush, used for, painting" vs "brush, is in, art class" → NO replacement
- Conservative: "It is better to leave a triplet than to replace one that has important information"
- Output format: `[[old_triplet -> new_triplet], ...]`

### 5.3 Thesis Extraction (Hypergraph)

From `prompt_extraction_thesises`:
- Natural language sentences instead of triplets
- Each thesis comes with entity list: `"thesis text; ['entity1', 'entity2']"`
- Thesises must be "comprehension and consistent" — prefer one long thesis over two decontextualized ones
- "North exit from kitchen is blocked by door" > "kitchen has door" + "north exit is blocked"

---

## 6. Results & Performance

### Text Games (Normalized Scores)

| Method | Hunt | Clean | Cook | Hunt Hard | Cook Hard |
|--------|------|-------|------|-----------|-----------|
| **AriGraph** | **1.0** | **0.79** | **1.0** | **1.0** | **1.0** |
| Human Top-3 | 1.0 | 0.85 | 1.0 | - | - |
| Human All | 0.96 | 0.59 | 0.32 | - | - |
| Full History | 0.49 | 0.05 | 0.18 | - | - |
| Summary | 0.33 | 0.39 | 0.52 | 0.17 | 0.21 |
| RAG | 0.33 | 0.35 | 0.36 | 0.17 | 0.17 |

### QA Benchmarks (MuSiQue / HotpotQA)

| Method | MuSiQue EM | HotpotQA F1 |
|--------|-----------|------------|
| AriGraph (GPT-4) | 37.0 | 69.9 |
| GraphReader (GPT-4) | 38.0 | 70.0 |
| GraphRAG (GPT-4o-mini) | 40.0 | 63.3 |
| HOLMES (GPT-4) | 48.0 | 78.0 |

### Key Ablation Findings

1. **Episodic memory crucial for cooking** — specific instructions about which tool for which action
2. **Episodic memory slightly hurt cleaning** — agent returned items to found-location (wrong) instead of correct location
3. **Exploration reduces steps dramatically** — solvable without, but far more steps
4. **Full history is worst** — context pollution kills performance
5. **Separate plan/action calls >> combined** — each module can focus

---

## 7. Architectural Principles (Extracted)

1. **KG as connective tissue** — not the only memory, but the glue between memory types
2. **Multiple abstraction levels** — triplets > raw text for retrieval
3. **Concise, curated context** beats long context every time
4. **Separate cognitive functions** — different tasks, different LLM calls
5. **Macro actions** — abstract navigation, let planner work strategically
6. **Conservative update** — prefer keeping existing knowledge over replacing it
7. **Embedding-guided graph traversal** — combines vector similarity with graph structure
8. **Importance-weighted depth** — higher-scored entities get deeper traversal
9. **Dual scoring for episodic** — embedding similarity + structural overlap
10. **Emotion as loop-breaker** — simple but effective anti-repetition mechanism

---

## 8. Open Problems (From Paper Discussion)

| Problem | Status | Notes |
|---------|--------|-------|
| Temporal dynamics | Unsolved | Timestamps exist but not used for traversal |
| Stochastic environments | Unsolved | All tests deterministic; non-deterministic needs replanning |
| Memory forgetting | Unsolved | What to prune? Brain isn't optimal here either |
| Cross-environment transfer | Unsolved | Same schema may not generalize |
| Episodic memory form | Open question | May not be superset of semantic; could be separate space |
| Scalability | Untested | Python lists; O(n) scans; no indexing |
| Multi-agent | Not addressed | Single agent only |

---

## 9. File Reference

| File | Lines | Purpose |
|------|-------|---------|
| `graphs/parent_graph.py` | 210 | TripletGraph base: triplet storage, BFS, pathfinding |
| `graphs/contriever_graph.py` | 220 | ContrieverGraph: embeddings + episodic memory + update cycle |
| `graphs/hypergraph.py` | 234 | Hypergraph: 3-level hierarchy (Event→Thesis→Entity) |
| `agents/parent_agent.py` | 90 | GPTagent: OpenAI API wrapper with cost tracking |
| `agents/llama_agent.py` | 46 | LLaMA local inference wrapper |
| `pipeline_arigraph.py` | 233 | Main execution pipeline (4 agents, step loop) |
| `utils/utils.py` | 420 | Core utilities (triplet parsing, navigation, episodic scoring) |
| `utils/retriever_search_drafts.py` | 143 | Embedding-guided BFS graph search |
| `utils/contriever.py` | ~100 | Retriever class (Facebook mcontriever wrapper) |
| `utils/textworld_adapter.py` | 123 | TextWorld environment wrapper |
| `prompts/prompts.py` | 133 | Extraction and refinement prompts |
| `prompts/system_prompts.py` | 70 | Agent system prompts (planner, action, exploration) |
