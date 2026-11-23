//! Memory system for cross-session learning
//!
//! Provides persistent memory that survives across sessions, organized by:
//! - Project (per working directory)
//! - Global (user-level preferences)
//!
//! # Storage
//!
//! Memory is stored in the per-user markdown wiki described in
//! [`crate::memory::store`]: one markdown page per memory and a SQLite index
//! that is derived from those pages. A [`MemoryManager`] reads and writes a
//! [`MemoryGraph`] per scope, and [`backend`] turns that graph into pages.
//!
//! The pre-wiki JSON files (`memory/global.json`, `memory/projects/<hash>.json`)
//! are a migration source only: they are read once and never written again
//! ([`migrate`]). `memory migrate` imports every one of them on demand.
//!
//! Embeddings are not stored: they are derived per query, so the dense retrieval
//! helpers below only see vectors a caller supplies in the same call.
//!
//! Jev provides typed relevance decisions. Optional text-generating extraction
//! is independent of recall and is never required to read existing memories.

use crate::memory_graph::MemoryGraph;
use crate::memory_types::{
    InjectedMemoryItem, MemoryActivity, MemoryEvent, MemoryEventKind, MemoryState, StepResult, StepStatus,
    ranking::{top_k_by_ord, top_k_by_score},
};
use crate::sidecar::Sidecar;
use crate::storage;
use anyhow::Result;
use horon_agent_time::Timestamp;
use std::path::PathBuf;
use std::time::Instant;

mod activity;
mod backend;
mod cache;
mod consolidation;
mod graph_pages;
mod migrate;
mod pending;
mod project_key;
#[path = "memory_prompt.rs"]
mod prompt_support;
mod retrieval;
pub mod store;

pub use crate::memory_types::{
    MemoryCategory, MemoryEntry, MemoryScope, MemoryStore, Reinforcement, TrustLevel, format_relevant_display_prompt,
    format_relevant_prompt,
};
use crate::memory_types::{
    collect_skill_query_terms, format_entries_for_prompt, memory_matches_search, normalize_search_text,
    skill_retrieval_bonus,
};
pub use activity::{
    activity_snapshot, add_event, apply_remote_activity_snapshot, check_staleness, clear_activity, get_activity,
    pipeline_start, pipeline_update, record_injected_prompt, set_state,
};
#[cfg(test)]
use pending::backdate_injected_memory_for_test;
pub use pending::{
    PendingMemory, clear_all_injected_memories, clear_all_pending_memory, clear_injected_memories,
    clear_pending_memory, has_pending_memory, is_memory_injected, mark_memories_injected, mark_memories_known,
    sync_injected_memories,
};
pub(crate) use prompt_support::format_context_for_extraction;
pub use prompt_support::{focus_query_text, format_context_for_relevance, format_focused_query_for_relevance};

use backend::{Store, StoreScope};

/// Category the flat note files migrated into, before the graph format existed.
const PRE_WIKI_NOTE_CATEGORY: &str = "note";
/// Source recorded for an entry that came from those note files.
const PRE_WIKI_NOTES_SOURCE: &str = "legacy_remember_migration";
const MEMORY_RELEVANCE_MAX_CANDIDATES: usize = 30;
const MEMORY_RELEVANCE_MAX_RESULTS: usize = 10;

/// Producer of synthetic [`MemoryEntry`] values contributed by a higher layer.
///
/// Used to invert the former `memory -> skill` dependency: the `skill` layer
/// (which already depends on `MemoryEntry`) registers a provider that turns the
/// shared skill registry into synthetic memory entries, instead of `memory`
/// reaching up into `skill::SkillRegistry`.
type SyntheticEntryProvider = fn() -> Vec<MemoryEntry>;

static SYNTHETIC_ENTRY_PROVIDERS: std::sync::LazyLock<std::sync::RwLock<Vec<SyntheticEntryProvider>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));

/// Register a provider of synthetic memory entries (e.g. skills).
///
/// Inverts `memory -> skill`: higher layers register their synthetic-entry
/// source here at startup so `memory` stays free of upward references.
pub fn register_synthetic_entry_provider(provider: SyntheticEntryProvider) {
    SYNTHETIC_ENTRY_PROVIDERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(provider);
}

#[cfg(test)]
fn collect_synthetic_entries() -> Vec<MemoryEntry> {
    let providers = SYNTHETIC_ENTRY_PROVIDERS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut entries = Vec::new();
    for provider in providers.iter() {
        entries.extend(provider());
    }
    entries
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct PreWikiNotesFile {
    #[serde(default)]
    entries: Vec<PreWikiNoteEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PreWikiNoteEntry {
    id: String,
    content: String,
    created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

/// Optional text-generating extraction is independent of Jev recall.
pub fn memory_sidecar_enabled() -> bool {
    crate::config::config().agents.memory_sidecar_enabled
}

/// Availability of the optional extraction sidecar, never used to gate recall.
pub fn memory_llm_judge_available() -> bool {
    memory_sidecar_enabled() && crate::sidecar::Sidecar::llm_backend_available()
}

/// Recall requires a Jev credential route. Subscription entitlement is checked
/// by the gateway, not inferred from a cached client tier.
pub fn memory_runtime_active() -> bool {
    crate::jev::JevClient::available()
}

/// The pre-wiki JSON file a project's memories were stored in before the wiki.
///
/// Nothing writes this file any more — the wiki is the store — but `cloud move`
/// transfers it between hosts and the migration reads it, so the path stays as
/// the one name for that file. It is derived from the caller's `PathBuf` with
/// `DefaultHasher`, exactly as before; the wiki's own project key lives in
/// [`project_key`].
pub fn project_memory_file(project_dir: &std::path::Path) -> Result<PathBuf> {
    let project_hash = project_key::pre_wiki_project_hash(project_dir);
    let memory_dir = crate::storage::horon_agent_dir()?.join("memory").join("projects");
    Ok(memory_dir.join(format!("{}.json", project_hash)))
}

#[derive(Debug, Clone)]
pub struct MemoryManager {
    project_dir: Option<PathBuf>,
    /// When true, use isolated test storage instead of real memory
    test_mode: bool,
    include_skills: bool,
}

impl MemoryManager {
    pub fn new() -> Self {
        Self {
            project_dir: None,
            test_mode: false,
            include_skills: true,
        }
    }

    pub fn with_project_dir(mut self, project_dir: impl Into<PathBuf>) -> Self {
        self.project_dir = Some(project_dir.into());
        self
    }

    pub fn with_skills(mut self, include_skills: bool) -> Self {
        self.include_skills = include_skills;
        self
    }

    /// Create a memory manager in test mode (isolated storage)
    pub fn new_test() -> Self {
        Self {
            project_dir: None,
            test_mode: true,
            include_skills: true,
        }
    }

    /// Check if running in test mode
    pub fn is_test_mode(&self) -> bool {
        self.test_mode
    }

    /// Set test mode (for debug sessions)
    pub fn set_test_mode(&mut self, test_mode: bool) {
        self.test_mode = test_mode;
    }

    /// Clear all test memories (only works in test mode)
    pub fn clear_test_storage(&self) -> Result<()> {
        if !self.test_mode {
            anyhow::bail!("clear_test_storage only allowed in test mode");
        }

        let test_dir = crate::storage::horon_agent_dir()?.join("memory").join("test");
        if test_dir.exists() {
            std::fs::remove_dir_all(&test_dir)?;
            // The store handle and every cached graph for that directory are
            // still open in this process; drop them or the next read answers
            // from the deleted pages.
            backend::forget_all();
            crate::logging::info("Cleared test memory storage");
        }
        Ok(())
    }

    /// The store this manager addresses.
    pub(crate) fn store(&self) -> Result<Store> {
        Store::open(self.test_mode, self.project_dir.as_deref())
    }

    /// Store without embedding inference. Exact duplicates reinforce an existing
    /// entry only within the requested scope, never mutate a different project.
    pub fn remember_project(&self, entry: MemoryEntry) -> Result<String> {
        let store = self.store()?;
        store.project_key()?;
        store.update(StoreScope::Project, |graph| Ok(Self::remember_in_graph(graph, entry)))
    }

    pub fn remember_global(&self, entry: MemoryEntry) -> Result<String> {
        self.store()?
            .update(StoreScope::Global, |graph| Ok(Self::remember_in_graph(graph, entry)))
    }

    fn remember_in_graph(graph: &mut MemoryGraph, entry: MemoryEntry) -> String {
        let normalized = entry.content.trim();
        let duplicate = graph
            .active_memories()
            .into_iter()
            .find(|existing| existing.category == entry.category && existing.content.trim() == normalized)
            .map(|existing| existing.id.clone());
        if let Some(id) = duplicate {
            if let Some(existing) = graph.get_memory_mut(&id) {
                existing.reinforce(entry.source.as_deref().unwrap_or("dedup"), 0);
            }
            return id;
        }
        graph.add_memory(entry)
    }

    /// Insert or update a memory with a stable ID in the project graph.
    /// Preserves existing inbound/outbound graph relationships while refreshing
    /// content and tags.
    pub fn upsert_project_memory(&self, entry: MemoryEntry) -> Result<String> {
        let store = self.store()?;
        store.project_key()?;
        store.update(StoreScope::Project, |graph| {
            Ok(Self::upsert_memory_in_graph(graph, entry))
        })
    }

    /// Insert or update a memory with a stable ID in the global graph.
    /// Preserves existing inbound/outbound graph relationships while refreshing
    /// content and tags.
    pub fn upsert_global_memory(&self, entry: MemoryEntry) -> Result<String> {
        self.store()?.update(StoreScope::Global, |graph| {
            Ok(Self::upsert_memory_in_graph(graph, entry))
        })
    }

    fn upsert_memory_in_graph(graph: &mut MemoryGraph, entry: MemoryEntry) -> String {
        let id = entry.id.clone();

        let Some(existing_snapshot) = graph.get_memory(&id).cloned() else {
            return graph.add_memory(entry);
        };

        let old_tags: std::collections::HashSet<String> = existing_snapshot.tags.iter().cloned().collect();
        let new_tags: std::collections::HashSet<String> = entry.tags.iter().cloned().collect();

        for tag in old_tags.difference(&new_tags) {
            graph.untag_memory(&id, tag);
        }
        for tag in new_tags.difference(&old_tags) {
            graph.tag_memory(&id, tag);
        }

        if let Some(existing) = graph.get_memory_mut(&id) {
            let content_changed = existing.content != entry.content;
            existing.category = entry.category;
            existing.content = entry.content;
            existing.tags = entry.tags;
            existing.updated_at = entry.updated_at;
            existing.source = entry.source;
            existing.trust = entry.trust;
            existing.active = entry.active;
            existing.superseded_by = entry.superseded_by;
            existing.confidence = entry.confidence;
            if content_changed {
                existing.set_embedding(None, None);
            }
        }

        id
    }

    fn collect_memories_scoped(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>> {
        retrieval::collect_memories_scoped(self, scope)
    }

    #[cfg(test)]
    fn synthetic_skill_entries(&self) -> Vec<MemoryEntry> {
        if !self.include_skills {
            return Vec::new();
        }

        collect_synthetic_entries()
    }

    #[cfg(test)]
    fn collect_retrieval_candidates_scoped(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>> {
        let mut entries = self.collect_memories_scoped(scope)?;
        if scope.includes_global() {
            entries.extend(self.synthetic_skill_entries());
        }
        Ok(entries)
    }

    /// Find memories similar to `text` through the active embedding backend.
    pub fn find_similar(&self, text: &str, threshold: f32, limit: usize) -> Result<Vec<(MemoryEntry, f32)>> {
        retrieval::find_similar(self, text, threshold, limit)
    }

    pub fn find_similar_scoped(
        &self,
        text: &str,
        threshold: f32,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        retrieval::find_similar_scoped(self, text, threshold, limit, scope)
    }

    /// Find memories similar to the given embedding
    pub fn find_similar_with_embedding(
        &self,
        query_embedding: &[f32],
        threshold: f32,
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        retrieval::find_similar_with_embedding(self, query_embedding, threshold, limit)
    }

    pub fn find_similar_with_embedding_scoped(
        &self,
        query_embedding: &[f32],
        threshold: f32,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        retrieval::find_similar_with_embedding_scoped(self, query_embedding, threshold, limit, scope)
    }

    /// Hybrid dense plus lexical retrieval fused with reciprocal rank fusion.
    pub fn find_similar_hybrid(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        retrieval::find_similar_hybrid(self, query_text, query_embedding, limit)
    }

    pub fn find_similar_hybrid_scoped(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        retrieval::find_similar_hybrid_scoped(self, query_text, query_embedding, limit, scope)
    }

    /// Score and filter candidates against one query vector.
    #[cfg(test)]
    fn score_and_filter(
        entries: Vec<MemoryEntry>,
        query_embedding: &[f32],
        query_text: &str,
        threshold: f32,
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        retrieval::score_and_filter(entries, query_embedding, query_text, threshold, limit)
    }

    /// Generate the missing embeddings of every stored memory.
    ///
    /// The wiki does not keep vectors, so nothing is written here: the counts
    /// report that the store has no embedding to backfill, which is the
    /// documented behavior change of the wiki backend. A caller that needs a
    /// vector embeds the content when it needs one.
    pub fn backfill_embeddings(&self) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    /// Fuse a dense ranking and a lexical ranking with reciprocal rank fusion.
    #[cfg(test)]
    fn hybrid_fuse(
        entries: Vec<MemoryEntry>,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Vec<(MemoryEntry, f32)> {
        retrieval::hybrid_fuse(entries, query_text, query_embedding, limit)
    }

    pub fn get_prompt_memories(&self, limit: usize) -> Option<String> {
        self.get_prompt_memories_scoped(limit, MemoryScope::All)
    }

    pub fn get_prompt_memories_scoped(&self, limit: usize, scope: MemoryScope) -> Option<String> {
        let all_entries: Vec<_> = top_k_by_ord(
            self.collect_memories_scoped(scope).ok()?.into_iter().map(|entry| {
                let updated_at = entry.updated_at.timestamp_millis();
                (entry, updated_at)
            }),
            limit,
        )
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();

        if all_entries.is_empty() {
            return None;
        }

        format_entries_for_prompt(&all_entries, limit)
    }

    pub async fn relevant_prompt_for_messages(&self, messages: &[crate::message::Message]) -> Result<Option<String>> {
        let context = format_context_for_relevance(messages);
        if context.is_empty() {
            return Ok(None);
        }
        self.relevant_prompt_for_context(&context, MEMORY_RELEVANCE_MAX_CANDIDATES, MEMORY_RELEVANCE_MAX_RESULTS)
            .await
    }

    pub async fn relevant_prompt_for_context(
        &self,
        context: &str,
        max_candidates: usize,
        limit: usize,
    ) -> Result<Option<String>> {
        let relevant = self.get_relevant_for_context(context, max_candidates).await?;
        if relevant.is_empty() {
            return Ok(None);
        }
        Ok(format_relevant_prompt(&relevant, limit))
    }

    pub fn search(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        self.search_scoped(query, MemoryScope::All)
    }

    /// Every memory of `scope` whose search text contains the query.
    ///
    /// This is the substring match the `memory` tool has always used: the query
    /// and the stored text are normalized the same way (lowercase, whitespace and
    /// separators folded), so `side-panel` and `BUILD.CACHE` still match. It
    /// deliberately does not go through the store index, whose ranking adds
    /// streams (links, recency, kind authority) that would change which memories
    /// this action returns.
    pub fn search_scoped(&self, query: &str, scope: MemoryScope) -> Result<Vec<MemoryEntry>> {
        let query_lower = normalize_search_text(query);
        if query_lower.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();

        for memory in self.collect_memories_scoped(scope)? {
            if memory_matches_search(&memory, &query_lower) {
                results.push(memory);
            }
        }

        Ok(results)
    }

    pub fn list_all(&self) -> Result<Vec<MemoryEntry>> {
        self.list_all_scoped(MemoryScope::All)
    }

    /// Every memory of `scope`, newest first with the id as the final tie-break.
    pub fn list_all_scoped(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>> {
        let mut all = self.collect_memories_scoped(scope)?;
        all.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(all)
    }

    /// Forget one memory, project scope first.
    ///
    /// The scope that holds the id is read before either is written, so a
    /// forget in the global store does not rewrite the project store first.
    /// A manager with no working directory has no project scope to read and
    /// goes straight to the global store, the same refusal-free read the rest
    /// of the manager uses.
    pub fn forget(&self, id: &str) -> Result<bool> {
        let store = self.store()?;
        let in_project = store
            .read(StoreScope::Project)
            .is_ok_and(|graph| graph.memories.contains_key(id));
        if in_project {
            return store.update(StoreScope::Project, |graph| Ok(graph.remove_memory(id).is_some()));
        }
        store.update(StoreScope::Global, |graph| Ok(graph.remove_memory(id).is_some()))
    }

    // === Sidecar Integration ===

    /// Extract memories from a session transcript using the Haiku sidecar
    pub async fn extract_from_transcript(&self, transcript: &str, session_id: &str) -> Result<Vec<String>> {
        if !memory_llm_judge_available() {
            crate::logging::info("Memory transcript extraction skipped: LLM judge unavailable");
            return Ok(Vec::new());
        }

        let sidecar = crate::session::Session::load_startup_stub(session_id)
            .map(|session| Sidecar::for_session(&session))
            .unwrap_or_else(|_| Sidecar::new());
        let extracted = sidecar.extract_memories(transcript).await?;

        let mut ids = Vec::new();
        for memory in extracted {
            let category: MemoryCategory = memory.category.parse().unwrap_or(MemoryCategory::Fact);
            let trust = match memory.trust.as_str() {
                "high" => TrustLevel::High,
                "medium" => TrustLevel::Medium,
                _ => TrustLevel::Low,
            };

            let entry = MemoryEntry::new(category, memory.content)
                .with_source(session_id)
                .with_trust(trust);

            // Store in project scope by default
            let id = self.remember_project(entry)?;
            ids.push(id);
        }

        Ok(ids)
    }

    /// Recall directly through Jev. The former `max_candidates` argument now
    /// limits output, not the input pool: old memories must remain discoverable.
    pub async fn get_relevant_for_context(&self, context: &str, max_candidates: usize) -> Result<Vec<MemoryEntry>> {
        Ok(
            crate::memory_jev::recall(self, context, max_candidates, MemoryScope::All)
                .await?
                .into_iter()
                .map(|(entry, _)| entry)
                .collect(),
        )
    }

    /// Local keyword lookup, available without a remote decision provider.
    pub fn get_relevant_keywords(&self, keywords: &[&str], limit: usize) -> Result<Vec<MemoryEntry>> {
        let normalized_keywords: Vec<String> = keywords
            .iter()
            .map(|keyword| normalize_search_text(keyword))
            .filter(|keyword| !keyword.is_empty())
            .collect();
        if normalized_keywords.is_empty() {
            return Ok(Vec::new());
        }

        let matches: Vec<_> = top_k_by_ord(
            self.collect_memories_scoped(MemoryScope::All)?
                .into_iter()
                .filter(|entry| {
                    let content_lower = normalize_search_text(&entry.content);
                    normalized_keywords.iter().any(|kw| content_lower.contains(kw))
                })
                .map(|entry| {
                    let updated_at = entry.updated_at.timestamp_millis();
                    (entry, updated_at)
                }),
            limit,
        )
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();

        Ok(matches)
    }

    /// Load the project graph.
    pub fn load_project_graph(&self) -> Result<MemoryGraph> {
        self.store()?.read(StoreScope::Project)
    }

    /// Load the global graph.
    pub fn load_global_graph(&self) -> Result<MemoryGraph> {
        self.store()?.read(StoreScope::Global)
    }

    /// Replace the project graph with `graph`.
    ///
    /// A whole-scope write: pages the graph no longer names are deleted and
    /// pages that changed are rewritten. Used by consolidation, which merges
    /// records in place, and by callers that already hold the graph they want.
    pub fn save_project_graph(&self, graph: &MemoryGraph) -> Result<()> {
        let store = self.store()?;
        store.project_key()?;
        store.update(StoreScope::Project, |target| {
            *target = graph.clone();
            Ok(())
        })
    }

    /// Replace the global graph with `graph`.
    pub fn save_global_graph(&self, graph: &MemoryGraph) -> Result<()> {
        self.store()?.update(StoreScope::Global, |target| {
            *target = graph.clone();
            Ok(())
        })
    }

    /// Add one tag to a memory.
    pub fn tag_memory(&self, memory_id: &str, tag: &str) -> Result<()> {
        self.tag_memories(memory_id, std::slice::from_ref(&tag.to_string()))
    }

    /// Add every tag in `tags` to one memory in a single write.
    ///
    /// The `memory` tool's `tag` action used to write the whole graph once per
    /// tag; one write keeps the store consistent if a later tag fails, and it is
    /// the only difference callers can observe.
    pub fn tag_memories(&self, memory_id: &str, tags: &[String]) -> Result<()> {
        let store = self.store()?;
        let tagged = store.update(StoreScope::Project, |graph| {
            Ok(match graph.get_memory(memory_id) {
                Some(_) => {
                    for tag in tags {
                        graph.tag_memory(memory_id, tag);
                    }
                    true
                }
                None => false,
            })
        })?;
        if tagged {
            return Ok(());
        }
        let tagged = store.update(StoreScope::Global, |graph| {
            Ok(match graph.get_memory(memory_id) {
                Some(_) => {
                    for tag in tags {
                        graph.tag_memory(memory_id, tag);
                    }
                    true
                }
                None => false,
            })
        })?;
        if tagged {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Memory not found: {}", memory_id))
        }
    }

    /// Link two memories with a RelatesTo edge
    pub fn link_memories(&self, from_id: &str, to_id: &str, weight: f32) -> Result<()> {
        let store = self.store()?;
        let link = |graph: &mut MemoryGraph| -> Result<bool> {
            if graph.memories.contains_key(from_id) && graph.memories.contains_key(to_id) {
                graph.link_memories(from_id, to_id, weight);
                return Ok(true);
            }
            Ok(false)
        };
        if store.update(StoreScope::Project, link)? || store.update(StoreScope::Global, link)? {
            return Ok(());
        }

        // Cross-store links not supported for now
        Err(anyhow::anyhow!(
            "Both memories must be in the same store (project or global)"
        ))
    }

    /// Get memories related to a given memory via graph traversal
    pub fn get_related(&self, memory_id: &str, depth: usize) -> Result<Vec<MemoryEntry>> {
        // Find which store contains the memory
        let mut graph = self.load_project_graph()?;
        if !graph.memories.contains_key(memory_id) {
            graph = self.load_global_graph()?;
            if !graph.memories.contains_key(memory_id) {
                return Err(anyhow::anyhow!("Memory not found: {}", memory_id));
            }
        }

        // Use cascade retrieval to find related memories
        let results = graph.cascade_retrieve(&[memory_id.to_string()], &[1.0], depth, 20);

        // Collect memory entries (excluding the seed)
        let entries: Vec<MemoryEntry> = results
            .into_iter()
            .filter(|(id, _)| id != memory_id)
            .filter_map(|(id, _)| graph.get_memory(&id).cloned())
            .collect();

        Ok(entries)
    }

    /// Find similar memories with cascade retrieval through the graph
    ///
    /// This extends the basic embedding search by also traversing through
    /// tags to find related memories that might not have direct embedding similarity.
    pub fn find_similar_with_cascade(
        &self,
        text: &str,
        threshold: f32,
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        self.find_similar_with_cascade_scoped(text, threshold, limit, MemoryScope::All)
    }

    pub fn find_similar_with_cascade_scoped(
        &self,
        text: &str,
        threshold: f32,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<(MemoryEntry, f32)>> {
        // First, do basic embedding search
        let embedding_hits = self.find_similar_scoped(text, threshold, limit, scope)?;

        if embedding_hits.is_empty() {
            return Ok(Vec::new());
        }

        // Get seed IDs and scores
        let seed_ids: Vec<String> = embedding_hits.iter().map(|(e, _)| e.id.clone()).collect();
        let seed_scores: Vec<f32> = embedding_hits.iter().map(|(_, s)| *s).collect();

        // Load graphs and perform cascade retrieval
        let mut project_graph = if scope.includes_project() {
            Some(self.load_project_graph()?)
        } else {
            None
        };
        let mut global_graph = if scope.includes_global() {
            Some(self.load_global_graph()?)
        } else {
            None
        };

        // Cascade through project graph
        let project_cascade = project_graph
            .as_mut()
            .map(|graph| graph.cascade_retrieve(&seed_ids, &seed_scores, 2, limit * 2))
            .unwrap_or_default();

        // Cascade through global graph
        let global_cascade = global_graph
            .as_mut()
            .map(|graph| graph.cascade_retrieve(&seed_ids, &seed_scores, 2, limit * 2))
            .unwrap_or_default();

        // Merge results, keeping highest score for each memory
        let mut merged: std::collections::HashMap<String, f32> = std::collections::HashMap::new();

        for (id, score) in embedding_hits.iter() {
            merged.insert(id.id.clone(), *score);
        }
        for (id, score) in project_cascade {
            let existing = merged.get(&id).copied().unwrap_or(0.0);
            if score > existing {
                merged.insert(id, score);
            }
        }
        for (id, score) in global_cascade {
            let existing = merged.get(&id).copied().unwrap_or(0.0);
            if score > existing {
                merged.insert(id, score);
            }
        }

        // Look up entries and keep only the top-scoring results
        let results: Vec<(MemoryEntry, f32)> = top_k_by_score(
            merged.into_iter().filter_map(|(id, score)| {
                project_graph
                    .as_ref()
                    .and_then(|graph| graph.get_memory(&id))
                    .or_else(|| global_graph.as_ref().and_then(|graph| graph.get_memory(&id)))
                    .cloned()
                    .map(|entry| (entry, score))
            }),
            limit,
        );

        Ok(results)
    }

    /// Get graph statistics for display
    pub fn graph_stats(&self) -> Result<(usize, usize, usize, usize)> {
        let project = self.load_project_graph()?;
        let global = self.load_global_graph()?;

        let memories = project.memories.len() + global.memories.len();
        let tags = project.tags.len() + global.tags.len();
        let edges = project.edge_count() + global.edge_count();
        let clusters = project.clusters.len() + global.clusters.len();

        Ok((memories, tags, edges, clusters))
    }
}

/// Embedding similarity threshold (0.0 - 1.0)
/// Lower = more candidates, higher = fewer but more relevant
pub const EMBEDDING_SIMILARITY_THRESHOLD: f32 = 0.5;

/// Maximum embedding hits to verify with sidecar
pub const EMBEDDING_MAX_HITS: usize = 10;

#[path = "memory/ranking.rs"]
mod ranking;
use ranking::bm25_rank;
impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

/// The `memory migrate` report, re-exported for the CLI.
pub use migrate::MigrateReport;

/// Import every pre-wiki JSON memory file under this home into the wiki.
///
/// `dry_run` reports what would be imported and writes nothing.
pub fn migrate_pre_wiki_memories(dry_run: bool) -> Result<MigrateReport> {
    let store = Store::open(false, None)?;
    migrate::migrate_all(&store, dry_run)
}

#[cfg(test)]
#[path = "tests_support.rs"]
pub(crate) mod memory_test_support;

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
