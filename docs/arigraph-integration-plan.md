# Integration Plan: AriGraph → rs-graph-llm

> **Goal**: Migrate AriGraph's dual-memory (semantic KG + episodic) architecture into
> graph-flow as a reusable module, leveraging existing graph-flow primitives and the
> broader Ada ecosystem (ndarray HPC, lance-graph, ladybug-rs).

---

## 0. Why This Matters

AriGraph proves that:
1. Structured KG memory dramatically outperforms flat RAG (1.0 vs 0.36 on Cooking)
2. Dual memory (semantic + episodic) outperforms either alone
3. Separate planning/action LLM calls >> combined calls
4. Importance-weighted retrieval depth matters
5. Conservative update (prefer keeping facts) prevents knowledge loss

graph-flow already has: Task trait, Context, Session, FlowRunner, FanOut, SubgraphTask,
thinking pipeline, MCP tools, ReAct agent, BaseStore with vector search. We are NOT
starting from zero — we're adding a knowledge graph memory layer on top.

---

## 1. Architecture Overview

### New Crate: `graph-flow-memory`

```
graph-flow-memory/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── triplet.rs              # Triplet data structure
    ├── triplet_graph.rs        # Semantic memory (TripletGraph)
    ├── episodic_store.rs       # Episodic memory
    ├── hypergraph.rs           # Optional: thesis-based memory
    ├── retrieval.rs            # Embedding-guided BFS
    ├── update_cycle.rs         # The core Extract→Refine→Insert→Retrieve loop
    ├── entity_scorer.rs        # LLM-based entity extraction + scoring
    ├── exploration.rs          # Unexplored-exit tracking
    └── spatial.rs              # Spatial subgraph + pathfinding
```

**Dependencies**: graph-flow (Context, Task trait), rig-core (LLM), lance (optional vector store), ndarray (embeddings)

### Integration Point

```
graph-flow-memory provides:
  - KnowledgeGraphMemory: impl BaseStore  (drop-in for existing store interface)
  - AriGraphUpdateTask: impl Task         (wired into graph-flow pipelines)
  - SemanticRetrievalTask: impl Task
  - EpisodicRetrievalTask: impl Task
  - PlanningTask: impl Task               (separated planner)
  - ActionSelectionTask: impl Task         (separated action selector)
  - ExplorationDeciderTask: impl Task
```

---

## 2. Phase 1: Core Data Structures

### 2.1 Triplet

```rust
/// A knowledge graph triplet: (subject, relation, object).
///
/// Direct port of AriGraph's internal representation.
/// AriGraph: [subject, object, {"label": relation}]
/// Ours:     struct with typed fields
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Triplet {
    pub subject: String,
    pub relation: String,
    pub object: String,
}

impl Triplet {
    /// Parse from "subject, relation, object" string format.
    pub fn parse(s: &str) -> Option<Self> { ... }

    /// Render as "subject, relation, object" string.
    pub fn to_string_form(&self) -> String { ... }

    /// Normalize: lowercase, trim quotes/whitespace, "I" → "inventory", "P" → "player"
    pub fn normalize(&mut self) { ... }
}
```

**Why not SPO from ladybug-rs?** SPO is 16K-bit fingerprint-space. Triplet is string-space.
We keep both: Triplet for LLM-readable memory, SPO for fingerprint-space operations.
A bridge function `Triplet::to_spo()` can project into fingerprint space when needed.

### 2.2 TripletGraph

```rust
/// Semantic memory: a deduplicated set of triplets with entity tracking.
///
/// Maps to AriGraph's TripletGraph + ContrieverGraph.
pub struct TripletGraph {
    triplets: Vec<Triplet>,
    entities: HashSet<String>,
    triplet_embeddings: HashMap<String, Vec<f32>>,  // triplet_str → embedding
    entity_embeddings: HashMap<String, Vec<f32>>,    // entity → embedding
    embed_fn: Arc<dyn EmbedFn>,                      // pluggable embedder
}

#[async_trait]
pub trait EmbedFn: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn similarity(&self, query: &[f32], candidates: &[Vec<f32>]) -> Result<Vec<f32>>;
}
```

**Operations** (matching AriGraph exactly):

| Method | AriGraph Equivalent | Notes |
|--------|-------------------|-------|
| `add_triplets(&mut self, triplets)` | `add_triplets()` | Dedup + embed on insert |
| `delete_triplets(&mut self, triplets, locations)` | `delete_triplets()` | Skip navigation triplets |
| `get_associated(&self, entities, depth)` | `get_associated_triplets()` | BFS through triplets |
| `exclude(&self, triplets) -> Vec<Triplet>` | `exclude()` | Filter already-known |
| `spatial_subgraph(&self, locations) -> SpatialGraph` | `compute_spatial_graph()` | Location-only graph |
| `find_path(&self, from, to, locations)` | `find_path()` | BFS shortest path |

### 2.3 EpisodicStore

```rust
/// Episodic memory: observations linked to their extracted triplets and embeddings.
///
/// Maps to AriGraph's obs_episodic dict.
pub struct EpisodicStore {
    episodes: IndexMap<String, Episode>,  // observation_text → Episode
    embed_fn: Arc<dyn EmbedFn>,
}

pub struct Episode {
    pub observation: String,
    pub triplets: Vec<String>,     // string-form triplets extracted from this observation
    pub embedding: Vec<f32>,       // 768-dim (or whatever embedder produces)
    pub timestamp: u64,
}
```

**Retrieval** (matching AriGraph's dual scoring):

```rust
impl EpisodicStore {
    /// Score all episodes against a query, combining embedding similarity + structural overlap.
    ///
    /// AriGraph formula:
    ///   score = normalized_embedding_sim + normalized_structural_overlap
    ///   structural = (matching_triplets / total_triplets) * ln(total_triplets)
    pub async fn retrieve(
        &self,
        plan_embedding: &[f32],
        current_subgraph: &[String],
        top_k: usize,
        exclude: &[String],
    ) -> Vec<&Episode> { ... }
}
```

---

## 3. Phase 2: Retrieval Engine

### 3.1 Embedding-Guided BFS

Direct port of AriGraph's `graph_retr_search`:

```rust
/// BFS through triplet strings using embedding similarity at each hop.
///
/// NOT traditional graph BFS — at each step:
/// 1. Embed query entity
/// 2. Find top-k most similar triplet strings
/// 3. Extract entities from matched triplets
/// 4. Enqueue new entities (depth-limited)
pub async fn graph_retrieval_search(
    start_query: &str,
    triplet_graph: &TripletGraph,
    max_depth: usize,     // AriGraph: importance score (1-3)
    top_k: usize,         // AriGraph default: 6
    threshold: f32,        // AriGraph default: 0.75
) -> Vec<Triplet> { ... }
```

### 3.2 Embedding Backend Options

```rust
/// Pluggable embedding backends.
pub enum EmbedBackend {
    /// Local: fastembed (AllMiniLM, 384-dim) — no API calls
    FastEmbed(fastembed::TextEmbedding),

    /// Local: ndarray HPC projection (SimHash) — zero external deps
    NdarraySimHash { dim: usize },

    /// Remote: Jina API (768-dim) — matches ladybug-rs spo_jina feature
    JinaApi { api_key: String },

    /// Fingerprint: project to 16K-bit via ndarray, use Hamming distance
    Fingerprint,
}
```

**Recommendation**: Start with `FastEmbed` (AllMiniLM) for parity with AriGraph's mcontriever.
Later, add `Fingerprint` backend for ladybug-rs integration.

---

## 4. Phase 3: Update Cycle as graph-flow Task

### 4.1 The Core Update Task

```rust
/// AriGraph's update cycle as a graph-flow Task.
///
/// Wired into a workflow graph between observation input and action selection.
pub struct KnowledgeGraphUpdateTask {
    triplet_graph: Arc<RwLock<TripletGraph>>,
    episodic_store: Arc<RwLock<EpisodicStore>>,
    extraction_prompt: String,
    refinement_prompt: String,
}

#[async_trait]
impl Task for KnowledgeGraphUpdateTask {
    fn id(&self) -> &str { "kg_update" }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        // Read inputs from context
        let observation: String = context.get("observation").await?;
        let plan: String = context.get("current_plan").await.unwrap_or_default();
        let prev_subgraph: Vec<String> = context.get("prev_subgraph").await.unwrap_or_default();
        let locations: Vec<String> = context.get("locations").await.unwrap_or_default();
        let items: HashMap<String, usize> = context.get("scored_entities").await.unwrap_or_default();

        // Step 1: Extract triplets via LLM
        let new_triplets = self.extract_triplets(&observation, &prev_subgraph).await?;

        // Step 2: Exclude already-known
        let novel_triplets = self.triplet_graph.read().await.exclude(&new_triplets);

        // Step 3: Refine (identify outdated)
        let outdated = self.identify_outdated(&novel_triplets, &new_triplets).await?;
        self.triplet_graph.write().await.delete_triplets(&outdated, &locations);

        // Step 4: Insert new triplets
        self.triplet_graph.write().await.add_triplets(&new_triplets).await?;

        // Step 5: Semantic retrieval (importance-weighted depth)
        let mut subgraph = Vec::new();
        for (entity, importance) in &items {
            let results = graph_retrieval_search(
                entity,
                &self.triplet_graph.read().await,
                *importance,
                6,
                0.75,
            ).await;
            subgraph.extend(results);
        }

        // Step 6: Episodic retrieval
        let plan_emb = self.triplet_graph.read().await.embed_fn.embed(&[plan.clone()]).await?;
        let top_episodic = self.episodic_store.read().await.retrieve(
            &plan_emb[0],
            &subgraph.iter().map(|t| t.to_string_form()).collect::<Vec<_>>(),
            2,  // top_k
            &context.get::<Vec<String>>("recent_observations").await.unwrap_or_default(),
        ).await;

        // Step 7: Store episode
        self.episodic_store.write().await.store(&observation, &new_triplets).await?;

        // Write outputs to context
        context.set("subgraph", subgraph).await;
        context.set("top_episodic", top_episodic).await;
        context.set("prev_subgraph", subgraph.iter().map(|t| t.to_string_form()).collect::<Vec<String>>()).await;

        Ok(TaskResult::new(
            Some(format!("KG updated: +{} triplets, {} in subgraph, {} episodic",
                new_triplets.len(), subgraph.len(), top_episodic.len())),
            NextAction::ContinueAndExecute,
        ))
    }
}
```

### 4.2 Complete Agent Pipeline as Graph

```rust
/// Build the full AriGraph-style agent pipeline as a graph-flow Graph.
///
/// Matches pipeline_arigraph.py exactly:
///   EntityScorer → KGUpdate → ExplorationDecider → Planner → ActionSelector
pub fn build_arigraph_pipeline(
    triplet_graph: Arc<RwLock<TripletGraph>>,
    episodic_store: Arc<RwLock<EpisodicStore>>,
) -> Graph {
    let entity_scorer = Arc::new(EntityScorerTask::new());
    let kg_update = Arc::new(KnowledgeGraphUpdateTask::new(
        triplet_graph.clone(), episodic_store.clone(),
    ));
    let exploration = Arc::new(ExplorationDeciderTask::new());
    let planner = Arc::new(PlanningTask::new());
    let action = Arc::new(ActionSelectionTask::new());

    GraphBuilder::new("arigraph_pipeline")
        .add_task(entity_scorer.clone())
        .add_task(kg_update.clone())
        .add_task(exploration.clone())
        .add_task(planner.clone())
        .add_task(action.clone())
        // Linear chain (matching AriGraph's sequential pipeline)
        .add_edge("entity_scorer", "kg_update")
        .add_edge("kg_update", "exploration_decider")
        .add_edge("exploration_decider", "planner")
        .add_edge("planner", "action_selector")
        .build()
}
```

---

## 5. Phase 4: Individual Task Implementations

### 5.1 Entity Scorer

```rust
/// Extract entities from observation + plan, assign importance scores 1-3.
///
/// Maps to AriGraph's GPTagent.item_processing_scores().
/// Output written to context as HashMap<String, usize>.
pub struct EntityScorerTask { ... }
```

**LLM prompt** (ported from AriGraph):
```
Extract entities from observation that can query the memory module.
Assign relevance score 1-3 reflecting importance for current plan.
Do not extract directional words (west, east, north exit).
Output: {"entity_1": score1, "entity_2": score2, ...}
```

### 5.2 Exploration Decider

```rust
/// Decide whether to explore based on plan content.
///
/// Maps to AriGraph's agent_if_expl.
/// Reads plan from context, writes "should_explore" bool.
pub struct ExplorationDeciderTask { ... }
```

Simple LLM call: "Do these sub-goals require exploration/finding/locating? True or False."

If True, compute unexplored exits from TripletGraph and write to context.

### 5.3 Planning Task

```rust
/// Generate/update plan with sub-goals, reasons, and emotion.
///
/// Maps to AriGraph's agent_plan (separate LLM call).
/// Reads: observation, history, subgraph, episodic, previous plan, unexplored exits.
/// Writes: current_plan (JSON with main_goal, plan_steps, emotion).
pub struct PlanningTask { ... }
```

**Critical**: This is a SEPARATE task from action selection. AriGraph proved that
combining them degrades performance.

### 5.4 Action Selection Task

```rust
/// Select a single action from valid actions based on plan + memory.
///
/// Maps to AriGraph's agent_action (separate LLM call).
/// Reads: observation, history, subgraph, episodic, plan, valid_actions, unexplored exits.
/// Writes: selected_action.
pub struct ActionSelectionTask { ... }
```

---

## 6. Phase 5: Ecosystem Integration

### 6.1 lance-graph Bridge

```rust
// Store triplet graph in Lance for persistence + time-travel
impl TripletGraph {
    /// Serialize to Lance RecordBatch for persistent storage.
    pub fn to_lance_batch(&self) -> RecordBatch { ... }

    /// Load from Lance dataset.
    pub fn from_lance(dataset: &Dataset) -> Result<Self> { ... }
}
```

Uses lance-graph's existing Arrow bridge for zero-copy transfer.

### 6.2 ndarray HPC Bridge

```rust
// Use ndarray's BLAS-accelerated operations for embedding similarity
use ndarray::hpc::BlasLevel1;

impl TripletGraph {
    /// Batch cosine similarity using ndarray BLAS dot product.
    pub fn batch_similarity(query: &Array1<f32>, candidates: &Array2<f32>) -> Array1<f32> {
        // Uses ndarray's BlasLevel1::dot for SIMD-accelerated computation
        candidates.dot(&query) / (candidates.row_norms() * query.norm())
    }
}
```

### 6.3 ladybug-rs Fingerprint Bridge

```rust
// Project triplets into fingerprint space for ladybug-rs integration
impl Triplet {
    /// Project to 16K-bit fingerprint via SPO encoding.
    ///
    /// Uses ndarray's spo_bundle module for cyclic permutation bundling.
    pub fn to_fingerprint(&self) -> Fingerprint<2048> {
        // subject ⊕ rotate(predicate, 1) ⊕ rotate(object, 2)
        spo_bundle::encode(
            &Fingerprint::from_content(&self.subject),
            &Fingerprint::from_content(&self.relation),
            &Fingerprint::from_content(&self.object),
        )
    }
}
```

### 6.4 Hypergraph Extension (Optional, Phase 6+)

```rust
/// Thesis-based memory for richer fact representation.
///
/// Maps to AriGraph's Hypergraph class.
/// 3-level hierarchy: Event → Thesis → Entity.
pub struct HypergraphMemory {
    events: HashMap<u64, Event>,
    thesises: HashMap<u64, Thesis>,
    entities: HashMap<u64, Entity>,
    embed_fn: Arc<dyn EmbedFn>,
}
```

This is lower priority — TripletGraph covers the core value. Hypergraph adds
context-preserving facts ("North exit from kitchen is blocked by door") that
triplets can't represent well.

---

## 7. Testing Strategy

### 7.1 Unit Tests

| Test | Validates |
|------|-----------|
| `test_triplet_parse` | "subject, relation, object" parsing |
| `test_triplet_normalize` | "I" → "inventory", lowercase, trim |
| `test_triplet_graph_add_dedup` | Deduplication on insert |
| `test_triplet_graph_delete_skip_nav` | Navigation triplets protected |
| `test_associated_bfs` | BFS retrieval at various depths |
| `test_spatial_subgraph` | Location-only extraction |
| `test_pathfinding` | BFS shortest path |
| `test_episodic_dual_scoring` | Embedding sim + structural overlap |
| `test_graph_retrieval_search` | Embedding-guided BFS |
| `test_exclude_known` | Filter already-known triplets |

### 7.2 Integration Tests

| Test | Validates |
|------|-----------|
| `test_update_cycle_end_to_end` | Full Extract→Refine→Insert→Retrieve pipeline |
| `test_pipeline_graph_execution` | All 5 tasks in sequence via graph-flow |
| `test_episodic_retrieval_quality` | Top-k episodic matches expected observations |
| `test_navigation_macro_actions` | find_path produces correct step sequences |
| `test_lance_persistence` | TripletGraph survives save/load cycle |

### 7.3 Benchmark (vs AriGraph Python)

Create a deterministic test scenario with known observations and verify:
- Same triplets extracted (modulo LLM variance)
- Same outdated triplets identified
- Same subgraph retrieved
- Same episodic memories ranked

---

## 8. Implementation Order

### Sprint 1: Core Data Structures (Week 1)
1. `triplet.rs` — Triplet struct, parse, normalize, to_string_form
2. `triplet_graph.rs` — TripletGraph with add/delete/exclude/associated/spatial/pathfind
3. Unit tests for all of the above

### Sprint 2: Embedding + Retrieval (Week 2)
4. `EmbedFn` trait + FastEmbed backend
5. Embedding-on-insert for TripletGraph (ContrieverGraph equivalent)
6. `retrieval.rs` — graph_retrieval_search (embedding-guided BFS)
7. `episodic_store.rs` — EpisodicStore with dual scoring
8. Unit tests for retrieval

### Sprint 3: graph-flow Tasks (Week 3)
9. `entity_scorer.rs` — EntityScorerTask (LLM-based)
10. `update_cycle.rs` — KnowledgeGraphUpdateTask
11. `exploration.rs` — ExplorationDeciderTask + unexplored exit computation
12. Pipeline builder: `build_arigraph_pipeline()`
13. Integration tests

### Sprint 4: Agent Tasks + Example (Week 4)
14. PlanningTask (separate planner, JSON output)
15. ActionSelectionTask (separate selector)
16. Example service: `arigraph-agent-service/` (like insurance-claims-service)
17. End-to-end test with mock environment

### Sprint 5: Ecosystem Bridges (Week 5+)
18. ndarray BLAS-accelerated similarity
19. lance-graph persistence
20. ladybug-rs fingerprint projection
21. Hypergraph extension

---

## 9. Risk Assessment

| Risk | Mitigation |
|------|-----------|
| Embedding model mismatch (mcontriever vs fastembed) | Pluggable EmbedFn trait; can swap backends |
| LLM extraction quality varies by model | Port exact AriGraph prompts; test with same models |
| Performance at scale (AriGraph is O(n) list scans) | Use DashMap + BLAS batch similarity from day 1 |
| Complexity creep in update cycle | Keep Phase 1-3 minimal; Hypergraph is optional Phase 6 |
| Testing without TextWorld (Linux-only, heavy deps) | Create mock environment for deterministic tests |

---

## 10. What We Are NOT Doing

1. **NOT re-implementing TextWorld** — we provide the memory layer, not the game env
2. **NOT replacing BaseStore** — KnowledgeGraphMemory implements BaseStore for compatibility
3. **NOT duplicating ladybug-rs SPO** — bridge function projects triplets to fingerprints
4. **NOT building a monolith** — separate crate (graph-flow-memory) with feature gates
5. **NOT gold-plating** — Hypergraph, advanced NARS truth values, etc. are later phases

---

## 11. Success Criteria

1. `TripletGraph` passes all unit tests from AriGraph's `pipeline_graph_evaluation.py`
2. Full pipeline executes as graph-flow Graph with correct task ordering
3. Retrieval quality matches AriGraph within 5% on deterministic test scenarios
4. Example service demonstrates end-to-end agent loop
5. Lance persistence survives restart without data loss
6. BLAS-accelerated similarity is >10x faster than naive Python for >1000 triplets
