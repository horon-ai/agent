//! Retrieval helpers that rank or score a set of memories.
//!
//! These are the ranking paths: every one reads the scopes it was asked for and
//! then ranks in memory, so nothing here decides how a memory is stored. Dense
//! scoring uses vectors the caller supplies in the same call, because the wiki
//! stores no embeddings.
//!
//! No production path calls the dense or hybrid functions today: recall is Jev's
//! and local search is the substring match in [`crate::memory::MemoryManager::search_scoped`].
//! They remain because a memory that was embedded before the wiki existed can
//! still be scored when a caller hands over a query vector, and an embedding is
//! derivable from the content at any later time.

use super::*;

/// Find similar memories to `text` through the active embedding backend.
pub(crate) fn find_similar(
    manager: &MemoryManager,
    text: &str,
    threshold: f32,
    limit: usize,
) -> Result<Vec<(MemoryEntry, f32)>> {
    find_similar_scoped(manager, text, threshold, limit, MemoryScope::All)
}

pub(crate) fn find_similar_scoped(
    manager: &MemoryManager,
    text: &str,
    threshold: f32,
    limit: usize,
    scope: MemoryScope,
) -> Result<Vec<(MemoryEntry, f32)>> {
    let query_embedding = match crate::embedding_backend::embed_query_active(text) {
        Ok((emb, _model)) => emb,
        Err(error) => {
            crate::logging::info(&format!("Embedding failed, falling back to keyword search: {error}"));
            return Ok(Vec::new());
        }
    };
    find_similar_with_embedding_scoped(manager, &query_embedding, threshold, limit, scope)
}

pub(crate) fn find_similar_with_embedding(
    manager: &MemoryManager,
    query_embedding: &[f32],
    threshold: f32,
    limit: usize,
) -> Result<Vec<(MemoryEntry, f32)>> {
    let entries = collect_all_memories_with_embeddings(manager)?;
    score_and_filter(entries, query_embedding, "", threshold, limit)
}

pub(crate) fn find_similar_with_embedding_scoped(
    manager: &MemoryManager,
    query_embedding: &[f32],
    threshold: f32,
    limit: usize,
    scope: MemoryScope,
) -> Result<Vec<(MemoryEntry, f32)>> {
    let entries = collect_memories_with_embeddings_scoped(manager, scope)?;
    score_and_filter(entries, query_embedding, "", threshold, limit)
}

/// Hybrid retrieval: fuse dense (embedding cosine) and sparse (BM25 over memory
/// search text) rankings with Reciprocal Rank Fusion.
///
/// Unlike [`find_similar_with_embedding`], it does NOT apply a hard cosine floor
/// (which benchmarking showed zeroes out recall): instead it pulls a generous
/// candidate pool from each retriever and lets RRF + the downstream
/// sidecar/rerank decide. Lexical signal is essential for the
/// identifier/path/term-heavy memories agents store.
pub(crate) fn find_similar_hybrid(
    manager: &MemoryManager,
    query_text: &str,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<(MemoryEntry, f32)>> {
    find_similar_hybrid_scoped(manager, query_text, query_embedding, limit, MemoryScope::All)
}

pub(crate) fn find_similar_hybrid_scoped(
    manager: &MemoryManager,
    query_text: &str,
    query_embedding: &[f32],
    limit: usize,
    scope: MemoryScope,
) -> Result<Vec<(MemoryEntry, f32)>> {
    let entries = collect_memories_with_embeddings_scoped(manager, scope)?;
    Ok(hybrid_fuse(entries, query_text, query_embedding, limit))
}

/// Pull pool, rank by dense and BM25 separately, fuse with RRF.
pub(crate) fn hybrid_fuse(
    entries: Vec<MemoryEntry>,
    query_text: &str,
    query_embedding: &[f32],
    limit: usize,
) -> Vec<(MemoryEntry, f32)> {
    let entries: Vec<MemoryEntry> = entries.into_iter().filter(|e| e.embedding.is_some()).collect();
    if entries.is_empty() {
        return Vec::new();
    }

    // Generous per-retriever pool so fusion has signal to work with.
    let pool = (limit * 5).max(HYBRID_POOL_MIN);

    // Dense ranking (no hard threshold; just take the top by cosine).
    // Vector-space gate: only entries embedded by the ACTIVE backend share a
    // comparable space, so dense scores are computed over those only. Other
    // entries (different model, e.g. not-yet-re-embedded local memories when
    // OpenAI is active) still participate via the BM25 lexical half below, so
    // they remain reachable rather than disappearing on a backend switch.
    let active_model = crate::embedding_backend::active_model_id();
    let dense_eligible: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.effective_embedding_model() == active_model)
        .map(|(i, _)| i)
        .collect();
    let emb_refs: Vec<&[f32]> = dense_eligible
        .iter()
        .filter_map(|&i| entries[i].embedding.as_deref())
        .collect();
    let dense_scores = crate::embedding::batch_cosine_similarity(query_embedding, &emb_refs);
    let mut dense: Vec<(usize, f32)> = dense_eligible.iter().copied().zip(dense_scores).collect();
    dense.sort_by(|a, b| b.1.total_cmp(&a.1));
    dense.truncate(pool);

    // Sparse (BM25) ranking over memory search text.
    let sparse = super::bm25_rank(&entries, query_text, pool);

    // RRF fusion.
    const RRF_K: f32 = 60.0;
    let mut fused: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    for (rank, (idx, _)) in dense.iter().enumerate() {
        *fused.entry(*idx).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
    }
    for (rank, (idx, _)) in sparse.iter().enumerate() {
        *fused.entry(*idx).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
    }

    let mut entries: Vec<Option<MemoryEntry>> = entries.into_iter().map(Some).collect();
    top_k_by_score(
        fused
            .into_iter()
            .filter_map(|(idx, score)| entries[idx].take().map(|e| (e, score))),
        limit,
    )
}

/// Every memory of `scope`, with the id as nothing but a set membership test.
#[cfg(test)]
pub(crate) fn collect_memories_scoped(manager: &MemoryManager, scope: MemoryScope) -> Result<Vec<MemoryEntry>> {
    let project = if scope.includes_project() {
        manager.load_project_graph_arc().ok()
    } else {
        None
    };
    let global = if scope.includes_global() {
        manager.load_global_graph_arc().ok()
    } else {
        None
    };
    let total = project.as_ref().map_or(0, |g| g.memories.len()) + global.as_ref().map_or(0, |g| g.memories.len());
    let mut entries = Vec::with_capacity(total);
    if let Some(graph) = project {
        entries.extend(graph.all_memories().cloned());
    }
    if let Some(graph) = global {
        entries.extend(graph.all_memories().cloned());
    }
    Ok(entries)
}

/// Every memory of `scope` that carries a vector.
pub(crate) fn collect_all_memories_with_embeddings(manager: &MemoryManager) -> Result<Vec<MemoryEntry>> {
    collect_memories_with_embeddings_scoped(manager, MemoryScope::All)
}

pub(crate) fn collect_memories_with_embeddings_scoped(
    manager: &MemoryManager,
    scope: MemoryScope,
) -> Result<Vec<MemoryEntry>> {
    let mut entries: Vec<MemoryEntry> = Vec::new();
    if scope.includes_project()
        && let Ok(project) = manager.load_project_graph_arc()
    {
        entries.extend(project.active_memories().filter(|m| m.embedding.is_some()).cloned());
    }
    if scope.includes_global()
        && let Ok(global) = manager.load_global_graph_arc()
    {
        entries.extend(global.active_memories().filter(|m| m.embedding.is_some()).cloned());
    }
    Ok(entries)
}

pub(crate) fn score_and_filter(
    entries: Vec<MemoryEntry>,
    query_embedding: &[f32],
    query_text: &str,
    threshold: f32,
    limit: usize,
) -> Result<Vec<(MemoryEntry, f32)>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let mut filtered_entries = Vec::with_capacity(entries.len());
    let mut skipped_missing_embeddings = 0usize;
    // Vector-space gate: only compare embeddings produced by the ACTIVE backend
    // (same model id). When the active backend differs from an entry's stored
    // model (e.g. user switched to OpenAI but this memory was embedded with
    // local MiniLM, not yet re-embedded), the cosine would be meaningless, so it
    // is excluded from dense scoring. Such memories remain reachable via the
    // lexical/BM25 path in hybrid retrieval.
    let active_model = crate::embedding_backend::active_model_id();
    let mut skipped_model_mismatch = 0usize;
    for entry in entries {
        if entry.embedding.is_none() {
            skipped_missing_embeddings += 1;
        } else if entry.effective_embedding_model() != active_model {
            skipped_model_mismatch += 1;
        } else {
            filtered_entries.push(entry);
        }
    }
    if skipped_missing_embeddings > 0 {
        crate::logging::warn(&format!(
            "Skipped {} retrieval candidate(s) without embeddings during similarity scoring",
            skipped_missing_embeddings
        ));
    }
    if skipped_model_mismatch > 0 {
        crate::logging::info(&format!(
            "Skipped {} retrieval candidate(s) embedded with a different model than the active backend ({})",
            skipped_model_mismatch, active_model
        ));
    }
    if filtered_entries.is_empty() {
        return Ok(Vec::new());
    }
    let emb_refs: Vec<&[f32]> = filtered_entries
        .iter()
        .filter_map(|entry| entry.embedding.as_deref())
        .collect();
    let scores = crate::embedding::batch_cosine_similarity(query_embedding, &emb_refs);
    let skill_query_terms = collect_skill_query_terms(query_text);

    let scored = top_k_by_score(
        filtered_entries
            .into_iter()
            .zip(scores)
            .map(|(entry, sim)| {
                let adjusted = sim + skill_retrieval_bonus(&entry, &skill_query_terms);
                (entry, adjusted)
            })
            .filter(|(_, sim)| *sim >= threshold),
        limit,
    );

    Ok(apply_gap_filter(scored))
}

/// Drop trailing low-relevance results by detecting natural gaps in the score
/// distribution. If the top hit is 0.85 and the next cluster is 0.40-0.42, the
/// 0.15+ gap tells us those lower results are noise.
///
/// Algorithm: walk the sorted scores and cut when the drop from one score to the
/// next exceeds `GAP_FACTOR` of the range (top - floor_threshold).
fn apply_gap_filter(scored: Vec<(MemoryEntry, f32)>) -> Vec<(MemoryEntry, f32)> {
    if scored.len() <= 1 {
        return scored;
    }

    const GAP_FACTOR: f32 = 0.25;
    const MIN_KEEP: usize = 1;

    let top_score = scored[0].1;
    let range = (top_score - EMBEDDING_SIMILARITY_THRESHOLD).max(0.01);
    let max_gap = range * GAP_FACTOR;

    let mut keep = scored.len();
    for i in 1..scored.len() {
        let drop = scored[i - 1].1 - scored[i].1;
        if drop > max_gap && i >= MIN_KEEP {
            keep = i;
            break;
        }
    }

    scored.into_iter().take(keep).collect()
}

/// Minimum per-retriever candidate pool size for hybrid fusion.
const HYBRID_POOL_MIN: usize = 50;
