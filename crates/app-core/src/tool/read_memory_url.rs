//! `memory://` reads: one stored memory record, or the newest-first listing.
//!
//! `read memory://<id>` renders the whole record (content, category, scope,
//! tags, links, created and updated times) because the copy injected into the
//! prompt is clipped. `read memory://` lists ids with a one-line preview each.
//! Output is paged exactly like a file read, so the same `:N-M` selector and
//! `offset`/`limit` bounds apply.
//!
//! The scheme is read-only and never resolves to a filesystem path. Records are
//! loaded through the memory store of this call's working directory plus the
//! user-level global store, so a memory belonging to another project is not
//! found here.

use super::{ReadInput, page_text};
use crate::memory::{MemoryManager, MemoryScope};
use crate::memory_inspect::{self, FoundMemory};
use crate::tool::{ToolContext, ToolOutput};
use anyhow::{Result, anyhow};

pub(super) const SCHEME: &str = "memory://";

/// Whether `path` names the stored-memory reader (`memory://...`).
pub(super) fn is_memory_url(path: &str) -> bool {
    path.get(..SCHEME.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SCHEME))
}

pub(super) fn read_memory_url(params: &ReadInput, ctx: &ToolContext) -> Result<ToolOutput> {
    let (uri, selector) = crate::tool::read_selector::split_uri_and_selector(&params.file_path)?;
    let reference = uri
        .get(..SCHEME.len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(SCHEME))
        .map(|_| &uri[SCHEME.len()..])
        .ok_or_else(|| anyhow!("not a memory:// reference: {uri}"))?
        .trim()
        .trim_end_matches('/');

    let manager = manager_for(ctx);
    let rendered = if reference.is_empty() {
        memory_inspect::render_listing(&memory_inspect::list_memories(&manager, MemoryScope::All)?)
    } else {
        let found: FoundMemory = memory_inspect::find_memory(&manager, reference, MemoryScope::All)?
            .ok_or_else(|| anyhow!("memory:// record not found: {reference}"))?;
        memory_inspect::render_record(&found)
    };

    let page = page_text(params, &selector, &rendered)?;
    Ok(ToolOutput::new(page).with_title(params.file_path.clone()))
}

/// The memory store this call may see: the working directory's project store
/// plus the user-level global store.
fn manager_for(ctx: &ToolContext) -> MemoryManager {
    match ctx.working_dir.as_deref() {
        Some(dir) if !dir.as_os_str().is_empty() => MemoryManager::new().with_project_dir(dir),
        _ => MemoryManager::new(),
    }
}

#[cfg(test)]
#[path = "read_memory_url_tests.rs"]
mod tests;
