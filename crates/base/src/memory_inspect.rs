//! In-place memory updates and `memory://` record reads.
//!
//! The `memory` tool can rewrite one stored record by id without losing what
//! made it that record: the id, creation time, reinforcement history and graph
//! links all stay. The `read` tool uses the same lookups to render one full
//! record, or the newest-first listing, for `memory://` references.
//!
//! Both paths are scope-bound the same way the memory tool is. A record is
//! reachable only from the project store of the caller's working directory or
//! from the user-level global store, so a memory written in another project is
//! not found rather than merely hidden.

use crate::memory::{MemoryCategory, MemoryEntry, MemoryManager, MemoryScope};
use crate::memory_graph::{EdgeKind, MemoryGraph};
use anyhow::Result;
use chrono::Utc;
use std::collections::HashSet;

/// The fields an in-place update may replace. `None` means "leave as is".
#[derive(Debug, Default, Clone)]
pub struct MemoryUpdate {
    pub content: Option<String>,
    pub category: Option<MemoryCategory>,
    pub tags: Option<Vec<String>>,
}

impl MemoryUpdate {
    /// Whether the update names no field at all. Such a call is a mistake, not
    /// a no-op write, so the tool refuses it.
    pub fn is_empty(&self) -> bool {
        self.content.is_none() && self.category.is_none() && self.tags.is_none()
    }
}

/// A stored record together with the store it lives in and its outgoing links.
#[derive(Debug, Clone)]
pub struct FoundMemory {
    pub entry: MemoryEntry,
    pub scope: MemoryScope,
    /// Memory-to-memory edges from this record (tag and cluster edges excluded).
    pub links: Vec<String>,
}

/// Parse a category name. `MemoryCategory` is total (an unknown name becomes a
/// custom category), so this cannot fail.
pub fn parse_category(raw: &str) -> MemoryCategory {
    // `FromStr for MemoryCategory` accepts every name, so the fallback only
    // exists because the trait's signature allows an error.
    raw.parse::<MemoryCategory>()
        .unwrap_or_else(|_| MemoryCategory::Custom(raw.to_string()))
}

/// Look up one record inside the allowed scopes, project first.
pub fn find_memory(manager: &MemoryManager, id: &str, scope: MemoryScope) -> Result<Option<FoundMemory>> {
    if id.trim().is_empty() {
        return Ok(None);
    }
    if scope.includes_project() {
        let graph = manager.load_project_graph()?;
        if let Some(entry) = graph.get_memory(id) {
            return Ok(Some(found(entry, MemoryScope::Project, &graph)));
        }
    }
    if scope.includes_global() {
        let graph = manager.load_global_graph()?;
        if let Some(entry) = graph.get_memory(id) {
            return Ok(Some(found(entry, MemoryScope::Global, &graph)));
        }
    }
    Ok(None)
}

/// Apply `update` to the stored record with `id`, keeping its identity.
///
/// The record's id, `created_at`, access count, reinforcement breadcrumbs and
/// graph edges are untouched; only the named fields, the search text, the
/// embedding (when content changed) and `updated_at` change. Content is secret
/// redacted before it is stored, exactly as persisted history is.
pub fn update_memory(manager: &MemoryManager, id: &str, update: MemoryUpdate) -> Result<Option<MemoryEntry>> {
    anyhow::ensure!(
        !update.is_empty(),
        "update requires at least one of content, category, or tags"
    );
    if let Some(content) = update.content.as_deref() {
        anyhow::ensure!(!content.trim().is_empty(), "content must not be empty");
    }

    let mut project = manager.load_project_graph()?;
    if let Some(entry) = apply_update(&mut project, id, &update) {
        manager.save_project_graph(&project)?;
        return Ok(Some(entry));
    }

    let mut global = manager.load_global_graph()?;
    if let Some(entry) = apply_update(&mut global, id, &update) {
        manager.save_global_graph(&global)?;
        return Ok(Some(entry));
    }

    Ok(None)
}

/// Every record in `scope`, newest first with a deterministic tie-break.
pub fn list_memories(manager: &MemoryManager, scope: MemoryScope) -> Result<Vec<FoundMemory>> {
    let mut entries = Vec::new();
    if scope.includes_project() {
        let graph = manager.load_project_graph()?;
        entries.extend(
            graph
                .all_memories()
                .map(|entry| found(entry, MemoryScope::Project, &graph)),
        );
    }
    if scope.includes_global() {
        let graph = manager.load_global_graph()?;
        entries.extend(
            graph
                .all_memories()
                .map(|entry| found(entry, MemoryScope::Global, &graph)),
        );
    }

    // HashMap iteration order is arbitrary, so order ties explicitly: two
    // records stored in the same millisecond must still list the same way every
    // time.
    entries.sort_by(|a, b| {
        b.entry
            .updated_at
            .cmp(&a.entry.updated_at)
            .then_with(|| b.entry.created_at.cmp(&a.entry.created_at))
            .then_with(|| b.entry.id.cmp(&a.entry.id))
    });
    Ok(entries)
}

/// The full-record rendering behind `read memory://<id>`.
pub fn render_record(found: &FoundMemory) -> String {
    let tags = if found.entry.tags.is_empty() {
        "(none)".to_string()
    } else {
        found.entry.tags.join(", ")
    };
    let links = if found.links.is_empty() {
        "(none)".to_string()
    } else {
        found.links.join(", ")
    };
    let mut out = String::new();
    push_line(&mut out, &format!("id: {}", found.entry.id));
    push_line(&mut out, &format!("category: {}", found.entry.category));
    push_line(&mut out, &format!("scope: {}", scope_label(found.scope)));
    push_line(&mut out, &format!("tags: {tags}"));
    push_line(&mut out, &format!("created: {}", found.entry.created_at.to_rfc3339()));
    push_line(&mut out, &format!("updated: {}", found.entry.updated_at.to_rfc3339()));
    push_line(&mut out, &format!("links: {links}"));
    push_line(&mut out, "content:");
    out.push_str(&found.entry.content);
    out.push('\n');
    out
}

/// The one-line-per-record rendering behind `read memory://`.
pub fn render_listing(entries: &[FoundMemory]) -> String {
    if entries.is_empty() {
        return "No memories stored.\n".to_string();
    }
    let mut out = String::new();
    push_line(&mut out, &format!("Memories ({}):", entries.len()));
    for found in entries {
        push_line(
            &mut out,
            &format!(
                "- {} [{}/{}] {}",
                found.entry.id,
                scope_label(found.scope),
                found.entry.category,
                preview(&found.entry.content)
            ),
        );
    }
    out
}

/// Append one rendered line. Writing into a `String` cannot fail, so the line
/// goes through `format!` rather than a `writeln!` whose result is discarded.
fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// One line, cut at 80 characters with an ellipsis when longer.
fn preview(content: &str) -> String {
    const PREVIEW_CHARS: usize = 80;
    let first_line = content.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= PREVIEW_CHARS {
        return first_line.to_string();
    }
    let head: String = first_line.chars().take(PREVIEW_CHARS).collect();
    format!("{head}…")
}

fn scope_label(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Project => "project",
        MemoryScope::Global => "global",
        MemoryScope::All => "all",
    }
}

fn found(entry: &MemoryEntry, scope: MemoryScope, graph: &MemoryGraph) -> FoundMemory {
    FoundMemory {
        entry: entry.clone(),
        scope,
        links: relation_targets(graph, &entry.id),
    }
}

/// Memory-to-memory edge targets. Tag and cluster edges are metadata about the
/// record, not links to other records.
fn relation_targets(graph: &MemoryGraph, id: &str) -> Vec<String> {
    let Some(edges) = graph.edges.get(id) else {
        return Vec::new();
    };
    edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::RelatesTo { .. } | EdgeKind::Supersedes | EdgeKind::Contradicts | EdgeKind::DerivedFrom
            )
        })
        .map(|edge| edge.target.clone())
        .collect()
}

/// Rewrite one record in `graph` and return the stored snapshot.
fn apply_update(graph: &mut MemoryGraph, id: &str, update: &MemoryUpdate) -> Option<MemoryEntry> {
    let previous = graph.get_memory(id)?.clone();
    let mut entry = previous.clone();

    if let Some(content) = update.content.as_deref() {
        entry.content = crate::message::redact_secrets(content);
    }
    if let Some(category) = &update.category {
        entry.category = category.clone();
    }
    if let Some(tags) = &update.tags {
        entry.tags = normalized_tags(tags);
    }

    // A changed body invalidates the stored vector; the next recall re-embeds
    // from the new text. Untouched content keeps its embedding.
    if entry.content != previous.content {
        entry.set_embedding(None, None);
    }
    entry.refresh_search_text();
    entry.updated_at = Utc::now();

    let old_tags: HashSet<&String> = previous.tags.iter().collect();
    let new_tags: HashSet<&String> = entry.tags.iter().collect();
    for tag in old_tags.difference(&new_tags) {
        graph.untag_memory(id, tag.as_str());
    }
    for tag in new_tags.difference(&old_tags) {
        graph.tag_memory(id, tag.as_str());
    }

    *graph.get_memory_mut(id)? = entry.clone();
    Some(entry)
}

/// Trim, drop empty tags and drop repeats while keeping the given order.
fn normalized_tags(tags: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || normalized.iter().any(|existing| existing == tag) {
            continue;
        }
        normalized.push(tag.to_string());
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_update_fields_normalize_tags_and_report_emptiness() {
        assert!(MemoryUpdate::default().is_empty());
        assert!(
            !MemoryUpdate {
                tags: Some(Vec::new()),
                ..MemoryUpdate::default()
            }
            .is_empty()
        );

        assert_eq!(
            normalized_tags(&[
                " alpha ".to_string(),
                String::new(),
                "alpha".to_string(),
                "beta".to_string()
            ]),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn memory_update_preview_is_one_bounded_line() {
        assert_eq!(preview("first line\nsecond line"), "first line");
        let long = "x".repeat(200);
        let rendered = preview(&long);
        assert_eq!(rendered.chars().count(), 81);
        assert!(rendered.ends_with('…'));
    }
}
