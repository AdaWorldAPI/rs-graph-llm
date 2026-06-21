//! # episodic-arc-task — graph-flow Tasks for the AriGraph/OSINT episodic arc
//!
//! The **training-wheels** half of the Cognitive Compilation loop, pointed at
//! OSINT/online-research. During a rig-driven research run these Tasks:
//!
//! - **record** each observation as an episodic vertex + content-address its
//!   source text (so identical sources dedup — many episodes → one source row);
//! - **retrieve** relevant past episodes for the current query;
//! - **gate** the run fail-closed: every claim must carry a valid
//!   content-addressed span ("no source span → no claim") AND the run must
//!   declare a public-interest reason (the §14 OSINT guardrail).
//!
//! ## The rig seam
//!
//! These Tasks drive rig's store adapters — `rig-lancedb` (episodic
//! similarity/retrieval) + `rig-surrealdb` (kv-lance semantic SPO graph + the
//! versioned commit *arc*). What rig persists IS the AriGraph tenant SoA,
//! transparently the surrealdb kv-lance view. AS-OF replay of that arc (the
//! cognitive-compiler → template-equivalence step) rests on surrealdb #50's
//! transparent versioning.
//!
//! Self-contained + cherry-pickable (sibling of `template-task`). Content
//! addressing uses a local `fnv1a` mirroring `lance_graph_contract::hash::fnv1a`;
//! migrate to `lance_graph_contract::content_store::{ContentId, SourceSpan}` once
//! that contract lands.

use async_trait::async_trait;
use graph_flow::{Context, NextAction, Result, Task, TaskResult};

/// Raw observation text to record.
pub const KEY_OBSERVATION: &str = "observation";
/// Source document text the observation was drawn from (content-addressed).
pub const KEY_SOURCE_TEXT: &str = "source_text";
/// Content address of the source (written by [`RecordEpisodeTask`]).
pub const KEY_CONTENT_ID: &str = "content_id";
/// Set true once an episode is recorded.
pub const KEY_EPISODE_RECORDED: &str = "episode_recorded";
/// Query for episodic retrieval.
pub const KEY_QUERY: &str = "query";
/// Episodic recall result (written by [`RetrieveEpisodicTask`]).
pub const KEY_EPISODIC_RECALL: &str = "episodic_recall";
/// Claims to gate, one per line: `claim\tcontent_id\tstart\tend`.
pub const KEY_CLAIMS: &str = "claims";
/// The §14 OSINT public-interest justification (required for the gate to pass).
pub const KEY_PUBLIC_INTEREST: &str = "public_interest_reason";
/// Citation-gate outcome (written by [`CiteGateTask`]).
pub const KEY_CITE_STATUS: &str = "cite_status";

/// Canonical content address — `fnv1a`-64 of the bytes.
///
/// Mirrors `lance_graph_contract::hash::fnv1a`: stable across versions/platforms
/// (unlike `DefaultHasher`, which must never key a content address). `0` is the
/// reserved empty/sentinel (no content).
#[must_use]
pub fn content_id(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Record one observation as an episodic vertex and content-address its source.
///
/// Wiring target: `rig-lancedb` (episode embedding) + `rig-surrealdb` (SPO +
/// witness provenance, one versioned commit). Until those are wired this records
/// the content address + a recorded flag so the surrounding graph is exercisable.
pub struct RecordEpisodeTask;

#[async_trait]
impl Task for RecordEpisodeTask {
    fn id(&self) -> &str {
        "record_episode"
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        let Some(observation) = context.get::<String>(KEY_OBSERVATION).await else {
            return Ok(TaskResult::new(
                Some("no observation to record".to_string()),
                NextAction::WaitForInput,
            ));
        };
        // Content-address the source so identical sources dedup (many episodes →
        // one source row). Empty source ⇒ sentinel 0.
        let source: String = context.get(KEY_SOURCE_TEXT).await.unwrap_or_default();
        let cid = if source.is_empty() { 0 } else { content_id(source.as_bytes()) };
        context.set(KEY_CONTENT_ID, cid).await;
        context.set(KEY_EPISODE_RECORDED, true).await;
        Ok(TaskResult::new(
            Some(format!("recorded episode ({} chars), source_content_id={cid:#018x}", observation.len())),
            NextAction::Continue,
        ))
    }
}

/// Retrieve relevant past episodes for the current query (rig-lancedb similarity).
pub struct RetrieveEpisodicTask;

#[async_trait]
impl Task for RetrieveEpisodicTask {
    fn id(&self) -> &str {
        "retrieve_episodic"
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        let query: String = context.get(KEY_QUERY).await.unwrap_or_default();
        if query.is_empty() {
            return Ok(TaskResult::new(
                Some("no query for episodic retrieval".to_string()),
                NextAction::WaitForInput,
            ));
        }
        // Placeholder for rig-lancedb nearest-episode recall.
        context.set(KEY_EPISODIC_RECALL, format!("recall for: {query}")).await;
        Ok(TaskResult::new(Some("episodic recall ready".to_string()), NextAction::Continue))
    }
}

/// Fail-closed OSINT citation gate.
///
/// Two conditions, both required to pass (else escalate via `WaitForInput`):
/// 1. §14 OSINT guardrail — the run declares a non-empty public-interest reason.
/// 2. "No source span → no claim" — every claim carries a valid content-addressed
///    span (non-sentinel content id AND non-empty `[start, end)`), mirroring
///    `SourceSpan::is_cited` and the merged `template-equivalence` provenance gate.
pub struct CiteGateTask;

#[async_trait]
impl Task for CiteGateTask {
    fn id(&self) -> &str {
        "cite_gate"
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        let reason: String = context.get(KEY_PUBLIC_INTEREST).await.unwrap_or_default();
        if reason.trim().is_empty() {
            context.set(KEY_CITE_STATUS, "blocked_no_public_interest").await;
            return Ok(TaskResult::new(
                Some("OSINT gate: missing public_interest_reason".to_string()),
                NextAction::WaitForInput,
            ));
        }

        let claims: String = context.get(KEY_CLAIMS).await.unwrap_or_default();
        for line in claims.lines().filter(|l| !l.trim().is_empty()) {
            let mut fields = line.split('\t');
            let claim = fields.next().unwrap_or("").trim();
            let cid = fields.next().and_then(parse_u64).unwrap_or(0);
            let start = fields.next().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
            let end = fields.next().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
            // is_cited: non-sentinel content + non-empty span.
            if cid == 0 || end <= start {
                context.set(KEY_CITE_STATUS, "blocked_uncited_claim").await;
                return Ok(TaskResult::new(
                    Some(format!("OSINT gate: uncited claim: {claim}")),
                    NextAction::WaitForInput,
                ));
            }
        }
        context.set(KEY_CITE_STATUS, "passed").await;
        Ok(TaskResult::new(Some("OSINT gate: all claims cited".to_string()), NextAction::Continue))
    }
}

fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    s.strip_prefix("0x").map_or_else(|| s.parse().ok(), |hex| u64::from_str_radix(hex, 16).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_address_is_stable_and_dedups() {
        let a = content_id(b"shared OSINT source");
        let b = content_id(b"shared OSINT source");
        assert_eq!(a, b);
        assert_ne!(a, content_id(b"other source"));
    }

    #[tokio::test]
    async fn record_continues_and_sets_content_id() {
        let ctx = Context::new();
        ctx.set(KEY_OBSERVATION, "Alice met Bob".to_string()).await;
        ctx.set(KEY_SOURCE_TEXT, "Alice met Bob in Paris.".to_string()).await;
        let r = RecordEpisodeTask.run(ctx.clone()).await.unwrap();
        assert!(matches!(r.next_action, NextAction::Continue));
        let cid: Option<u64> = ctx.get(KEY_CONTENT_ID).await;
        assert_eq!(cid, Some(content_id(b"Alice met Bob in Paris.")));
        assert_eq!(ctx.get::<bool>(KEY_EPISODE_RECORDED).await, Some(true));
    }

    #[tokio::test]
    async fn record_waits_without_observation() {
        let ctx = Context::new();
        let r = RecordEpisodeTask.run(ctx).await.unwrap();
        assert!(matches!(r.next_action, NextAction::WaitForInput));
    }

    #[tokio::test]
    async fn retrieve_continues_with_query() {
        let ctx = Context::new();
        ctx.set(KEY_QUERY, "who met whom".to_string()).await;
        let r = RetrieveEpisodicTask.run(ctx).await.unwrap();
        assert!(matches!(r.next_action, NextAction::Continue));
    }

    #[tokio::test]
    async fn cite_gate_passes_when_cited_and_justified() {
        let ctx = Context::new();
        ctx.set(KEY_PUBLIC_INTEREST, "rank a public official's claim".to_string()).await;
        ctx.set(KEY_CLAIMS, "Bob was in Paris\t12345\t0\t13".to_string()).await;
        let r = CiteGateTask.run(ctx.clone()).await.unwrap();
        assert!(matches!(r.next_action, NextAction::Continue));
        assert_eq!(ctx.get::<String>(KEY_CITE_STATUS).await.as_deref(), Some("passed"));
    }

    #[tokio::test]
    async fn cite_gate_blocks_without_public_interest() {
        let ctx = Context::new();
        ctx.set(KEY_CLAIMS, "Bob was in Paris\t12345\t0\t13".to_string()).await;
        let r = CiteGateTask.run(ctx.clone()).await.unwrap();
        assert!(matches!(r.next_action, NextAction::WaitForInput));
        assert_eq!(ctx.get::<String>(KEY_CITE_STATUS).await.as_deref(), Some("blocked_no_public_interest"));
    }

    #[tokio::test]
    async fn cite_gate_blocks_uncited_claim() {
        let ctx = Context::new();
        ctx.set(KEY_PUBLIC_INTEREST, "public interest".to_string()).await;
        // sentinel content id 0 ⇒ uncited
        ctx.set(KEY_CLAIMS, "unsourced rumor\t0\t0\t0".to_string()).await;
        let r = CiteGateTask.run(ctx.clone()).await.unwrap();
        assert!(matches!(r.next_action, NextAction::WaitForInput));
        assert_eq!(ctx.get::<String>(KEY_CITE_STATUS).await.as_deref(), Some("blocked_uncited_claim"));
    }
}
