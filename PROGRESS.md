# rs-graph-llm — Progress Tracker

> Tracks progress against the master integration plan plateaus.
> See /home/user/INTEGRATION_PLAN.md for full context.

## Plateau 0: Everything Compiles

- [x] graph-flow core compiles
- [ ] ort-sys SSL certificate error blocks full build (environment issue)

## Plateau 1: Schema Migration

- [ ] 1A.1: Create graph-flow-memory crate
- [ ] 1A.2: Port Triplet struct from AriGraph
- [ ] 1A.3: Port TripletGraph
- [ ] 1A.4: Port EpisodicStore
- [ ] 1A.5: Add Triplet::to_spo() bridge (uses ndarray Fingerprint)
- [ ] 1A.6: Wire KnowledgeGraphMemory: impl BaseStore
- [ ] 1B.1: Audit crewai-rust drivers for reusable traits
- [ ] 1B.2: Document TypedSlot protocol for graph-flow Tasks
- [ ] 1B.3: Define AgentTask wrapper
- [ ] 1B.4: Decide integration depth (A/B/C) — DECISION POINT
- [ ] 1C.1: Update CLAUDE.md with ndarray/lance-graph deps — DONE (2026-03-22)
- [ ] 1C.2: Update arigraph plan with ndarray reference — DONE (2026-03-22)
- [ ] 1C.3: Update CRATE_STRUCTURE.md — DONE (2026-03-22)

## Plateau 3: Full Stack Integration

- [ ] 3B.1: Wire lance-graph as graph backend for graph-flow-memory
- [ ] 3B.2: Implement SemiringReasonTask (thinking.rs Layer 5)
- [ ] 3B.3: Wire ndarray cascade search into SemanticRetrievalTask
- [ ] 3B.4: End-to-end AriGraph cycle test

---
*Last updated: 2026-03-22*
