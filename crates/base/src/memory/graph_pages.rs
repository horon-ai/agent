//! Between the graph the manager speaks and the pages the wiki stores.
//!
//! A [`MemoryGraph`] is a map of entries plus tag and link edges used for
//! cascade retrieval. A [`MemoryPage`] is one markdown file. This module is the
//! one conversion in each direction, so both sides stay in step:
//!
//! * a page becomes an entry through the shared [`MemoryPage::to_entry`], and
//!   the graph is assembled with [`MemoryGraph::add_memory`] so tag nodes, tag
//!   counts, supersede edges and reverse edges are exactly what the graph code
//!   built before the wiki existed;
//! * an entry becomes a page through [`MemoryPage::from_entry`] plus the
//!   memory-to-memory edges of the graph it is being stored with, because links
//!   live on the source page.
//!
//! The one lossy corner is deliberate: the wiki stores no embeddings, which are
//! derived per query, and it stores a link weight inside `0..=1`, so a weight
//! outside that range is clamped and logged rather than refused.

use crate::memory_graph::MemoryGraph;
use crate::memory_types::MemoryEntry;
use horon_agent_memory_types::page::{MemoryPage, PageLink, PageLinkKind};

/// One scope as the store reads it: the graph the manager speaks, and the pages
/// it came from so a write can tell what actually changed.
#[derive(Debug)]
pub(crate) struct GraphSnapshot {
    pub(crate) graph: MemoryGraph,
    pub(crate) pages: std::collections::HashMap<String, MemoryPage>,
}

/// Build the graph for one scope's pages.
pub(crate) fn snapshot_from_pages(pages: Vec<MemoryPage>) -> GraphSnapshot {
    let mut graph = MemoryGraph::new();
    let mut by_id = std::collections::HashMap::with_capacity(pages.len());
    for page in &pages {
        graph.add_memory(page.to_entry());
    }
    // Links are edges on the source page, so they are added after every entry
    // exists: `add_edge` does not require the destination to be present, but a
    // page may link to a page stored later in the scan.
    for page in &pages {
        for link in &page.links {
            graph.add_edge(page.id.as_str(), link.dst.as_str(), link.kind.to_edge_kind(link.weight));
        }
    }
    for page in pages {
        by_id.insert(page.id.as_str().to_string(), page);
    }
    GraphSnapshot { graph, pages: by_id }
}

/// The page an entry is stored as, carrying the graph's memory-to-memory edges.
///
/// `project` is the scope's project key (`None` for the user-level store).
pub(crate) fn page_for_entry(
    entry: &MemoryEntry,
    project: Option<&str>,
    graph: &MemoryGraph,
) -> anyhow::Result<MemoryPage> {
    let mut page = MemoryPage::from_entry(entry, project.map(str::to_string))?;
    if page.active && page.superseded_by.is_some() {
        // The graph sets both fields together; a record naming its replacement
        // is not current, so the supersede is kept and only the flag is fixed.
        crate::logging::debug(&format!(
            "memory {} names a superseding record while active; storing it as superseded",
            entry.id
        ));
        page.active = false;
    }
    if !page.confidence.is_finite() {
        page.confidence = 1.0;
    }
    page.confidence = page.confidence.clamp(0.0, 1.0);
    let mut links = Vec::new();
    for edge in graph.get_edges(&entry.id) {
        let Some((kind, weight)) = PageLinkKind::from_edge_kind(&edge.kind) else {
            continue;
        };
        let weight = match kind {
            PageLinkKind::RelatesTo => Some(clamped_weight(weight, &entry.id, &edge.target)),
            _ => None,
        };
        let dst = match horon_agent_memory_types::PageId::parse(&edge.target) {
            Ok(dst) => dst,
            Err(error) => {
                // A link to a tag or cluster node never reaches a page, and a
                // memory id the wiki cannot store is reported instead of
                // silently dropped.
                return Err(anyhow::anyhow!(
                    "memory {} links to `{}`, which is not a storable page id: {error}",
                    entry.id,
                    edge.target
                ));
            }
        };
        links.push(PageLink { kind, dst, weight });
    }
    // A page may not repeat a link, and the graph can hold the same pair twice
    // when an earlier store appended edges. Keep the first, which is the oldest.
    let mut unique_len = 0usize;
    for idx in 0..links.len() {
        let is_dup = links[..unique_len]
            .iter()
            .any(|prev| prev.kind == links[idx].kind && prev.dst == links[idx].dst);
        if !is_dup {
            if unique_len != idx {
                links.swap(unique_len, idx);
            }
            unique_len += 1;
        }
    }
    links.truncate(unique_len);
    page.links = links;
    Ok(page)
}

/// A `relates_to` weight inside the page's `0..=1` range.
fn clamped_weight(weight: Option<f32>, from: &str, to: &str) -> f32 {
    let weight = weight.unwrap_or(1.0);
    if !weight.is_finite() {
        crate::logging::debug(&format!(
            "memory {from} -> {to} has a non-finite link weight; storing 1.0"
        ));
        return 1.0;
    }
    if !(0.0..=1.0).contains(&weight) {
        crate::logging::debug(&format!(
            "memory {from} -> {to} has link weight {weight} outside 0..=1; clamping"
        ));
        return weight.clamp(0.0, 1.0);
    }
    weight
}
