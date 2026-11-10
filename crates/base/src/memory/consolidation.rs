//! Cross-session consolidation of extracted memories.
//!
//! Extraction runs once per session and only appends, so the same subject ends
//! up stored several times with drifting wording. This module is the second
//! phase omp's local memory backend runs: it merges entries that describe the
//! same subject into one active entry, marks the rest superseded (kept on disk,
//! never deleted) and writes one bounded summary per scope that the next turn
//! injects as memory guidance.
//!
//! The pass runs after extraction, inside the extraction task, and only once
//! `[agents] memory_consolidation_min_new` memories have been stored since the
//! previous pass. A pass loads the graph, merges, and rewrites it, so it is
//! batched rather than run per session.

use super::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Version of the per-scope consolidation state file. An older file is treated
/// as absent, so a bump only costs one extra pass.
const STATE_VERSION: u32 = 1;

/// Header of the generated summary, matching the `# Memory` marker the
/// relevance payload uses so both read as memory guidance.
const SUMMARY_HEADER: &str = "# Memory Summary";

/// Entries considered for one summary before the byte cap applies. A larger
/// store still consolidates; the summary lists the highest-scoring entries.
const MAX_SUMMARY_ENTRIES: usize = 256;

/// Terms that appear in most memories and say nothing about the subject.
const SUBJECT_STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "were", "not", "but", "you", "your", "use",
    "uses", "used", "using", "has", "have", "had", "can", "will", "would", "should", "when", "then", "than", "there",
    "their", "its", "our", "out", "into", "also", "all", "any", "may", "must", "does", "did", "how", "what", "which",
    "who", "why", "per", "via", "one", "two", "still", "only", "just", "more", "most", "some", "such", "same", "other",
    "each", "every",
];

/// Two memories describe the same subject when they share at least this many
/// content terms...
const MIN_SHARED_SUBJECT_TERMS: usize = 2;

/// ...and this share of their combined term set (Jaccard overlap). The pass
/// prefers merging over keeping near-duplicates: a superseded entry is retained
/// and its id is logged, so a wrong merge costs a ranking slot, not data.
const SUBJECT_OVERLAP_RATIO: f32 = 0.6;

/// Which store a pass runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Project,
    Global,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

/// Limits for one consolidation pass, from the `[agents]` memory keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsolidationLimits {
    /// Memories stored since the last pass required before it runs.
    /// `[agents] memory_consolidation_min_new`, horon-chosen default `20`.
    pub min_new: usize,
    /// Byte cap on the summary this module writes and injects.
    /// `[agents] memory_consolidation_summary_bytes`, horon-chosen default `4096`.
    pub summary_bytes: usize,
}

impl ConsolidationLimits {
    /// Read the limits from configuration.
    pub fn from_config() -> Self {
        let agents = &crate::config::config().agents;
        Self {
            min_new: agents.memory_consolidation_min_new,
            summary_bytes: agents.memory_consolidation_summary_bytes,
        }
    }
}

/// A memory a merge marked superseded. The record itself stays on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedMemory {
    pub id: String,
    /// Session or tool that stored it, when the entry carries one.
    pub source: Option<String>,
}

/// One subject cluster that merged: the entry that stayed active and the
/// entries it replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRecord {
    /// `"project"` or `"global"`.
    pub scope: &'static str,
    pub survivor_id: String,
    pub superseded: Vec<MergedMemory>,
}

/// Result of one consolidation attempt.
#[derive(Debug, Clone, Default)]
pub struct ConsolidationReport {
    /// Merges performed, project scope first.
    pub merges: Vec<MergeRecord>,
    /// The composed, byte-capped summary the next turn injects.
    pub summary: Option<String>,
    /// Whether the project pass ran (its threshold was reached).
    pub project_ran: bool,
    /// Whether the global pass ran (its threshold was reached).
    pub global_ran: bool,
}

/// State kept next to each scope's memory file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConsolidationState {
    version: u32,
    /// When the last pass completed. Absent means it never ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_pass: Option<DateTime<Utc>>,
    /// Summary produced by the last pass for this scope.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    summary: String,
    /// Active entries that summary covers, so the injection counts the memories
    /// the digest actually speaks for.
    #[serde(default)]
    summary_entries: usize,
}

impl Default for ConsolidationState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            last_pass: None,
            summary: String::new(),
            summary_entries: 0,
        }
    }
}

/// What a merge decision needs from one memory, captured once so the pass does
/// not re-read the graph entry per comparison.
struct SubjectKey {
    id: String,
    source: Option<String>,
    terms: BTreeSet<String>,
    tags: BTreeSet<String>,
    category: MemoryCategory,
    created_at: DateTime<Utc>,
    strength: u32,
}

/// Consolidate one scope's memories with explicit limits.
///
/// Runs the project pass and then the global pass, each gated by its own
/// threshold, and returns the composed summary the next turn injects.
fn consolidate_with_limits(manager: &MemoryManager, limits: ConsolidationLimits) -> Result<ConsolidationReport> {
    let mut report = ConsolidationReport::default();
    let project = run_scope(manager, Scope::Project, limits, &mut report)?;
    let global = run_scope(manager, Scope::Global, limits, &mut report)?;
    report.summary = compose_summaries(&[project, global], limits.summary_bytes).map(|(text, _)| text);
    Ok(report)
}

impl MemoryManager {
    /// Run the pass with the configured limits after an extraction completes.
    ///
    /// Never fatal: extraction has already succeeded, and a failed pass leaves
    /// the stores untouched.
    pub fn run_consolidation(&self) {
        let limits = ConsolidationLimits::from_config();
        match consolidate_with_limits(self, limits) {
            Ok(report) => {
                for merge in &report.merges {
                    log_merge(merge);
                }
            }
            Err(err) => crate::logging::info(&format!("Memory consolidation skipped: {err}")),
        }
    }

    /// The bounded cross-session summary for this turn's memory guidance, with
    /// the number of active memories it speaks for.
    ///
    /// `None` until a pass has written a summary, and absent for a project
    /// without a working directory.
    pub fn consolidation_summary(&self) -> Option<(String, usize)> {
        let limits = ConsolidationLimits::from_config();
        match summary_with_limits(self, limits.summary_bytes) {
            Ok(summary) => summary,
            Err(err) => {
                crate::logging::info(&format!("Memory consolidation summary unavailable: {err}"));
                None
            }
        }
    }
}

fn summary_with_limits(manager: &MemoryManager, max_bytes: usize) -> Result<Option<(String, usize)>> {
    let mut sections: Vec<ScopeSummary> = Vec::new();
    if let Some(path) = manager.project_memory_path()? {
        sections.push(scope_summary(load_state(&state_path_for(&path))));
    }
    sections.push(scope_summary(load_state(&state_path_for(
        &manager.global_memory_path()?,
    ))));
    Ok(compose_summaries(&sections, max_bytes))
}

fn scope_summary(state: ConsolidationState) -> ScopeSummary {
    ScopeSummary {
        text: state.summary,
        entries: state.summary_entries,
    }
}

/// One scope's stored summary and the number of memories it covers.
#[derive(Debug, Clone, Default)]
struct ScopeSummary {
    text: String,
    entries: usize,
}

fn run_scope(
    manager: &MemoryManager,
    scope: Scope,
    limits: ConsolidationLimits,
    report: &mut ConsolidationReport,
) -> Result<ScopeSummary> {
    let Some(memory_path) = memory_path_for(manager, scope)? else {
        // The project store needs a working directory; without one there is
        // nothing to consolidate and nothing to inject.
        return Ok(ScopeSummary::default());
    };
    let state_path = state_path_for(&memory_path);
    let mut state = load_state(&state_path);
    let mut graph = match scope {
        Scope::Project => manager.load_project_graph()?,
        Scope::Global => manager.load_global_graph()?,
    };

    if new_memory_count(&graph, &state) >= limits.min_new {
        let merges = merge_subjects(&mut graph, scope.label());
        save_graph(manager, scope, &graph)?;
        report.merges.extend(merges);
        state.last_pass = Some(Utc::now());
        let (summary, entries) = build_summary(&graph, limits.summary_bytes);
        state.summary = summary;
        state.summary_entries = entries;
        storage::write_json(&state_path, &state)?;
        match scope {
            Scope::Project => report.project_ran = true,
            Scope::Global => report.global_ran = true,
        }
    }

    Ok(scope_summary(state))
}

fn memory_path_for(manager: &MemoryManager, scope: Scope) -> Result<Option<PathBuf>> {
    match scope {
        Scope::Project => manager.project_memory_path(),
        Scope::Global => manager.global_memory_path().map(Some),
    }
}

fn save_graph(manager: &MemoryManager, scope: Scope, graph: &MemoryGraph) -> Result<()> {
    match scope {
        Scope::Project => manager.save_project_graph(graph),
        Scope::Global => manager.save_global_graph(graph),
    }
}

/// The state file sits next to the memory file it describes, so the test
/// storage and the real storage stay separate without a second path rule.
fn state_path_for(memory_path: &Path) -> PathBuf {
    memory_path.with_extension("consolidation.json")
}

fn load_state(path: &Path) -> ConsolidationState {
    if !path.exists() {
        return ConsolidationState::default();
    }
    match storage::read_json::<ConsolidationState>(path) {
        Ok(state) if state.version == STATE_VERSION => state,
        Ok(state) => {
            crate::logging::info(&format!(
                "Ignoring memory consolidation state {} written by version {}",
                path.display(),
                state.version
            ));
            ConsolidationState::default()
        }
        Err(err) => {
            crate::logging::info(&format!(
                "Ignoring unreadable memory consolidation state {}: {err}",
                path.display()
            ));
            ConsolidationState::default()
        }
    }
}

fn new_memory_count(graph: &MemoryGraph, state: &ConsolidationState) -> usize {
    graph
        .all_memories()
        .filter(|entry| state.last_pass.is_none_or(|last| entry.created_at > last))
        .count()
}

/// Merge same-subject active entries in one graph.
///
/// The survivor keeps its id and content; every other member of its cluster is
/// marked superseded through the graph's own `supersede`, which also records a
/// `Supersedes` edge.
fn merge_subjects(graph: &mut MemoryGraph, scope: &'static str) -> Vec<MergeRecord> {
    let mut candidates: Vec<SubjectKey> = graph.active_memories().map(subject_key).collect();
    candidates.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut clusters: Vec<Vec<&SubjectKey>> = Vec::new();
    for candidate in &candidates {
        match clusters
            .iter_mut()
            .find(|cluster| cluster.first().is_some_and(|first| same_subject(first, candidate)))
        {
            Some(cluster) => cluster.push(candidate),
            None => clusters.push(vec![candidate]),
        }
    }

    let mut merges = Vec::new();
    for cluster in clusters.iter().filter(|cluster| cluster.len() > 1) {
        let Some(survivor) = cluster
            .iter()
            .copied()
            .max_by(|left, right| compare_authority(left, right))
        else {
            continue;
        };
        let superseded: Vec<MergedMemory> = cluster
            .iter()
            .filter(|candidate| candidate.id != survivor.id)
            .map(|candidate| MergedMemory {
                id: candidate.id.clone(),
                source: candidate.source.clone(),
            })
            .collect();
        if superseded.is_empty() {
            continue;
        }
        for merged in &superseded {
            graph.supersede(&survivor.id, &merged.id);
        }
        merges.push(MergeRecord {
            scope,
            survivor_id: survivor.id.clone(),
            superseded,
        });
    }
    merges
}

fn subject_key(entry: &MemoryEntry) -> SubjectKey {
    SubjectKey {
        id: entry.id.clone(),
        source: entry.source.clone(),
        terms: subject_terms(entry),
        tags: entry.tags.iter().map(|tag| tag.to_lowercase()).collect(),
        category: entry.category.clone(),
        created_at: entry.created_at,
        strength: entry.strength,
    }
}

fn subject_terms(entry: &MemoryEntry) -> BTreeSet<String> {
    entry
        .searchable_text()
        .split_whitespace()
        .filter(|word| word.len() >= 3 && !SUBJECT_STOP_WORDS.contains(word))
        .map(str::to_string)
        .collect()
}

fn same_subject(left: &SubjectKey, right: &SubjectKey) -> bool {
    if !left.tags.is_empty() && !right.tags.is_empty() && left.tags.iter().any(|tag| right.tags.contains(tag)) {
        return true;
    }
    let shared = left.terms.intersection(&right.terms).count();
    if shared < MIN_SHARED_SUBJECT_TERMS {
        return false;
    }
    let union = left.terms.len() + right.terms.len() - shared;
    union > 0 && shared as f32 / union as f32 >= SUBJECT_OVERLAP_RATIO
}

/// A correction outranks every other category on its subject, because it
/// records what the user said is true. Within a rank the newest wins, then the
/// most reinforced, then the id, so the survivor never depends on hash order.
fn compare_authority(left: &SubjectKey, right: &SubjectKey) -> Ordering {
    correction_rank(&left.category)
        .cmp(&correction_rank(&right.category))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.strength.cmp(&right.strength))
        .then_with(|| left.id.cmp(&right.id))
}

fn correction_rank(category: &MemoryCategory) -> u8 {
    match category {
        MemoryCategory::Correction => 1,
        _ => 0,
    }
}

/// One bounded summary of the active memories in a graph, with the number of
/// memories it lists.
///
/// Empty when the graph has nothing to summarize or the cap is zero.
fn build_summary(graph: &MemoryGraph, max_bytes: usize) -> (String, usize) {
    if max_bytes == 0 {
        return (String::new(), 0);
    }
    let mut entries: Vec<&MemoryEntry> = graph.active_memories().collect();
    if entries.is_empty() {
        return (String::new(), 0);
    }
    entries.sort_by(|left, right| {
        let left_score = crate::memory_types::memory_score(left) as f32;
        let right_score = crate::memory_types::memory_score(right) as f32;
        right_score.total_cmp(&left_score).then_with(|| left.id.cmp(&right.id))
    });
    entries.truncate(MAX_SUMMARY_ENTRIES);
    let listed = entries.len();
    let owned: Vec<MemoryEntry> = entries.into_iter().cloned().collect();
    let Some(body) = format_entries_for_prompt(&owned, MAX_SUMMARY_ENTRIES) else {
        return (String::new(), 0);
    };
    (cap_summary(&format!("{SUMMARY_HEADER}\n\n{body}"), max_bytes), listed)
}

/// Compose per-scope summaries into one capped injection, with the number of
/// memories the included sections speak for.
fn compose_summaries(sections: &[ScopeSummary], max_bytes: usize) -> Option<(String, usize)> {
    let mut text = String::new();
    let mut entries = 0;
    for section in sections {
        let section_text = section.text.trim();
        if section_text.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(section_text);
        entries += section.entries;
    }
    let capped = cap_summary(&text, max_bytes);
    if capped.trim().is_empty() {
        None
    } else {
        Some((capped, entries))
    }
}

fn cap_summary(text: &str, max_bytes: usize) -> String {
    crate::util::truncate_str(text, max_bytes).to_string()
}

fn log_merge(merge: &MergeRecord) {
    let superseded: Vec<String> = merge
        .superseded
        .iter()
        .map(|merged| match &merged.source {
            Some(source) => format!("{} ({source})", merged.id),
            None => merged.id.clone(),
        })
        .collect();
    crate::logging::info(&format!(
        "Memory consolidation ({}) merged subject into {}: superseded {}",
        merge.scope,
        merge.survivor_id,
        superseded.join(", ")
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn with_project_home<F, T>(f: F) -> T
    where
        F: FnOnce(&MemoryManager) -> T,
    {
        let _guard = crate::storage::lock_test_env();
        let previous_home = std::env::var_os("HORON_HOME");
        let dir = tempfile::TempDir::new().expect("tempdir");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::config::Config::invalidate_cache();
        let manager = MemoryManager::new().with_project_dir(dir.path().join("project"));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&manager)));

        match previous_home {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn remember(manager: &MemoryManager, category: MemoryCategory, content: &str, hours_ago: u64) -> String {
        let created = Utc::now() - Duration::from_secs(hours_ago * 3_600);
        manager
            .remember_project(MemoryEntry::new(category, content).with_timestamps(created, created))
            .expect("remember project memory")
    }

    #[test]
    fn memory_consolidation_merges_two_entries_with_the_same_subject() {
        with_project_home(|manager| {
            let older = remember(manager, MemoryCategory::Fact, "Release builds run sccache", 2);
            let newer = remember(
                manager,
                MemoryCategory::Fact,
                "Release builds run sccache for the workspace",
                1,
            );
            let unrelated = remember(
                manager,
                MemoryCategory::Preference,
                "Prefer tabs over spaces in Rust",
                1,
            );
            assert_ne!(older, newer);

            let report = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 2,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");

            assert_eq!(report.merges.len(), 1, "one subject merged: {report:?}");
            let merge = &report.merges[0];
            assert_eq!(merge.scope, "project");
            assert_eq!(merge.survivor_id, newer, "the newest entry survives");
            assert_eq!(merge.superseded.len(), 1);
            assert_eq!(merge.superseded[0].id, older);

            let graph = manager.load_project_graph().expect("load project graph");
            let superseded = graph.get_memory(&older).expect("superseded entry is kept");
            assert!(!superseded.active);
            assert_eq!(superseded.superseded_by.as_deref(), Some(newer.as_str()));
            assert!(graph.get_memory(&newer).expect("survivor").active);
            assert!(graph.get_memory(&unrelated).expect("unrelated entry").active);
        });
    }

    #[test]
    fn memory_consolidation_correction_supersedes_older_fact() {
        with_project_home(|manager| {
            let fact = remember(manager, MemoryCategory::Fact, "Release builds use sccache", 3);
            let correction = remember(
                manager,
                MemoryCategory::Correction,
                "Release builds no longer use sccache",
                1,
            );

            let report = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 1,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");

            assert_eq!(
                report.merges.len(),
                1,
                "correction and fact share a subject: {report:?}"
            );
            assert_eq!(report.merges[0].survivor_id, correction);
            let graph = manager.load_project_graph().expect("load project graph");
            let fact_entry = graph.get_memory(&fact).expect("fact is kept");
            assert!(!fact_entry.active);
            assert_eq!(fact_entry.superseded_by.as_deref(), Some(correction.as_str()));
            assert!(graph.get_memory(&correction).expect("correction").active);
        });
    }

    #[test]
    fn memory_consolidation_correction_outranks_a_later_fact() {
        with_project_home(|manager| {
            let correction = remember(
                manager,
                MemoryCategory::Correction,
                "Release builds no longer use sccache",
                3,
            );
            let fact = remember(manager, MemoryCategory::Fact, "Release builds use sccache", 1);

            let report = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 1,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");

            assert_eq!(report.merges.len(), 1, "one subject merged: {report:?}");
            assert_eq!(
                report.merges[0].survivor_id, correction,
                "a correction is the durable statement even when a fact was stored later"
            );
            let graph = manager.load_project_graph().expect("load project graph");
            assert!(!graph.get_memory(&fact).expect("fact is kept").active);
        });
    }

    #[test]
    fn memory_consolidation_summary_never_exceeds_the_byte_cap() {
        with_project_home(|manager| {
            for (name, port) in [
                ("alpha", 8101),
                ("beta", 8102),
                ("gamma", 8103),
                ("delta", 8104),
                ("epsilon", 8105),
                ("zeta", 8106),
            ] {
                remember(
                    manager,
                    MemoryCategory::Fact,
                    &format!("Subsystem {name} owns datastore {name} on port {port}"),
                    1,
                );
            }

            let limits = ConsolidationLimits {
                min_new: 6,
                summary_bytes: 240,
            };
            let report = consolidate_with_limits(manager, limits).expect("consolidate");
            assert!(report.project_ran, "six new memories reach the threshold");
            let summary = report.summary.expect("summary");
            assert!(summary.len() <= 240, "summary was {} bytes", summary.len());
            assert!(summary.starts_with(SUMMARY_HEADER));
            assert!(
                summary.len() > 200,
                "the cap truncates a longer summary instead of dropping it: {} bytes",
                summary.len()
            );

            let (injected, entries) = summary_with_limits(manager, limits.summary_bytes)
                .expect("read stored summary")
                .expect("stored summary");
            assert_eq!(injected, summary);
            assert_eq!(entries, 6, "the digest says how many active memories it speaks for");
        });
    }

    #[test]
    fn memory_consolidation_skips_below_the_min_new_threshold() {
        with_project_home(|manager| {
            let older = remember(manager, MemoryCategory::Fact, "Release builds run sccache", 2);
            let newer = remember(
                manager,
                MemoryCategory::Fact,
                "Release builds run sccache for the workspace",
                1,
            );

            let skipped = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 3,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");
            assert!(!skipped.project_ran);
            assert!(skipped.merges.is_empty());
            assert!(skipped.summary.is_none());
            let graph = manager.load_project_graph().expect("load project graph");
            assert!(graph.get_memory(&older).expect("older").active);
            assert!(graph.get_memory(&newer).expect("newer").active);

            let ran = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 2,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");
            assert!(ran.project_ran, "a lower threshold lets the same store run");
            assert_eq!(ran.merges.len(), 1);
        });
    }

    #[test]
    fn memory_consolidation_counts_only_memories_stored_since_the_last_pass() {
        with_project_home(|manager| {
            remember(manager, MemoryCategory::Fact, "Release builds run sccache", 3);
            remember(
                manager,
                MemoryCategory::Fact,
                "Release builds run sccache for the workspace",
                2,
            );

            let first = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 2,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");
            assert!(first.project_ran);

            remember(
                manager,
                MemoryCategory::Preference,
                "Prefer cargo nextest over cargo test",
                0,
            );
            let second = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 2,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");
            assert!(!second.project_ran, "one new memory is below the threshold of two");

            let third = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 1,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");
            assert!(third.project_ran, "the pass marker moved to the first pass");
        });
    }

    #[test]
    fn memory_consolidation_runs_on_the_global_scope_too() {
        with_project_home(|manager| {
            let older = manager
                .remember_global(MemoryEntry::new(
                    MemoryCategory::Preference,
                    "Prefer dark mode in every terminal",
                ))
                .expect("remember global memory");
            let newer = manager
                .remember_global(MemoryEntry::new(
                    MemoryCategory::Preference,
                    "Prefer dark mode in every terminal session",
                ))
                .expect("remember global memory");

            let report = consolidate_with_limits(
                manager,
                ConsolidationLimits {
                    min_new: 2,
                    summary_bytes: 4096,
                },
            )
            .expect("consolidate");

            assert!(report.global_ran, "the global store reaches its own threshold");
            assert!(!report.project_ran, "the empty project store has nothing to run on");
            let graph = manager.load_global_graph().expect("load global graph");
            assert!(
                graph
                    .get_memory(&older)
                    .expect("superseded global entry")
                    .superseded_by
                    .is_some(),
                "a global duplicate is superseded like a project one"
            );
            assert!(graph.get_memory(&newer).expect("global survivor").active);
        });
    }

    #[test]
    fn memory_consolidation_defaults_are_horon_chosen() {
        let agents = crate::config::Config::default().agents;
        assert_eq!(agents.memory_consolidation_min_new, 20);
        assert_eq!(agents.memory_consolidation_summary_bytes, 4096);
    }

    #[test]
    fn memory_consolidation_limits_load_from_a_real_config_file() {
        let _guard = crate::storage::lock_test_env();
        let previous_home = std::env::var_os("HORON_HOME");
        let dir = tempfile::TempDir::new().expect("tempdir");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::config::Config::invalidate_cache();

        let path = crate::config::Config::path().expect("config path");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("create config parent");
        std::fs::write(
            &path,
            "[agents]\nmemory_consolidation_min_new = 7\nmemory_consolidation_summary_bytes = 512\n",
        )
        .expect("write config");

        let limits = ConsolidationLimits::from_config();
        assert_eq!(
            limits,
            ConsolidationLimits {
                min_new: 7,
                summary_bytes: 512
            }
        );

        match previous_home {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }

    #[test]
    fn memory_consolidation_keys_drive_the_pass_from_config() {
        with_project_home(|manager| {
            let path = crate::config::Config::path().expect("config path");
            std::fs::create_dir_all(path.parent().expect("config parent")).expect("create config parent");
            std::fs::write(
                &path,
                "[agents]\nmemory_consolidation_min_new = 1\nmemory_consolidation_summary_bytes = 96\n",
            )
            .expect("write config");
            crate::config::Config::invalidate_cache();

            let older = remember(manager, MemoryCategory::Fact, "Release builds run sccache", 2);
            let newer = remember(
                manager,
                MemoryCategory::Fact,
                "Release builds run sccache for the workspace",
                1,
            );

            manager.run_consolidation();

            let graph = manager.load_project_graph().expect("load project graph");
            assert!(
                !graph.get_memory(&older).expect("older").active,
                "a threshold of one runs a pass the default of twenty would skip"
            );
            assert!(graph.get_memory(&newer).expect("newer").active);

            let (summary, entries) = manager.consolidation_summary().expect("the pass stores a summary");
            assert!(
                summary.len() <= 96,
                "the configured byte cap applies to the injected summary: {} bytes",
                summary.len()
            );
            assert_eq!(entries, 1, "the merged subject leaves one active memory");
        });
    }

    #[test]
    fn default_config_template_documents_the_consolidation_keys() {
        let template = crate::config::Config::default_config_file_contents();
        assert!(template.contains("# memory_consolidation_min_new = 20"), "{template}");
        assert!(
            template.contains("# memory_consolidation_summary_bytes = 4096"),
            "{template}"
        );
        assert!(
            template.contains("Horon default"),
            "the template must say the defaults are horon-chosen"
        );
    }
}
