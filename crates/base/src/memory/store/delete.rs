//! Deleting a page: the file first, then every index row.
//!
//! Order is the contract. The markdown file is removed before any row, because
//! a full reindex can only see files: if the process dies after the unlink and
//! before the row delete, the index still holds a ghost row that the next
//! [`super::MemoryWiki::open`] prunes (no file, so no page), and a reindex
//! cannot resurrect it. The other order would let a crash leave a file that the
//! next rebuild happily indexes again, which is the failure the deletion
//! requirement forbids.
//!
//! Deletion also sweeps duplicate copies of the same page id. The index can only
//! hold one page per id, so a hand-copied file would become the survivor after
//! the indexed copy is deleted; removing every file that claims the id is what
//! keeps "deleted" true across a rebuild.

use super::document::{self, MAX_BODY_BYTES};
use super::reindex;
use super::schema::Index;
use anyhow::{Context, Result, anyhow};
use std::path::{Component, Path, PathBuf};

/// What one delete did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteOutcome {
    /// Whether the indexed markdown file was removed.
    pub file_removed: bool,
    /// Whether the index held rows for the page.
    pub rows_removed: bool,
    /// Extra copies of the same page id that were removed.
    pub duplicate_files_removed: usize,
}

/// Delete one page by id.
pub fn delete_page(index: &mut Index, wiki_root: &Path, id: &str) -> Result<DeleteOutcome> {
    let mut outcome = DeleteOutcome::default();
    let recorded_path = match index.page_row(id)? {
        Some(row) => Some(row.path),
        None => index.recorded_path_for_id(id)?,
    };
    if let Some(relative) = recorded_path {
        let path = safe_join(wiki_root, &relative)?;
        match std::fs::remove_file(&path) {
            Ok(()) => outcome.file_removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow!(error).context(format!("cannot remove the page file {}", path.display())));
            }
        }
    }
    outcome.duplicate_files_removed = sweep_duplicates(wiki_root, id)?;
    outcome.rows_removed = index.remove_page(id)?;
    if outcome.rows_removed || outcome.file_removed || outcome.duplicate_files_removed > 0 {
        index.purge_deleted()?;
    }
    Ok(outcome)
}

/// Remove every remaining page file that claims `id`, returning how many.
fn sweep_duplicates(wiki_root: &Path, id: &str) -> Result<usize> {
    let files = reindex::scan(wiki_root)?;
    let mut removed = 0;
    for file in files {
        let bytes = match std::fs::read(&file.absolute) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if bytes.len() > MAX_BODY_BYTES + 64 * 1024 {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let Some(stem) = file.absolute.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let page_id_matches = match document::decode(&text, stem, horon_agent_time::Timestamp::UNIX_EPOCH) {
            Ok(page) => page.id.as_str() == id,
            Err(_) => false,
        };
        if page_id_matches {
            std::fs::remove_file(&file.absolute)
                .with_context(|| format!("cannot remove the duplicate page file {}", file.relative))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Join a recorded relative path onto the wiki root, refusing anything that is
/// not a plain path below it.
///
/// The index stores paths this module wrote, but a restored or hand-edited
/// database is untrusted input, so `..`, an absolute path and a Windows prefix
/// are refused here instead of being resolved.
pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let candidate = Path::new(relative);
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("page path `{relative}` escapes the wiki root"));
            }
        }
    }
    Ok(root.join(candidate))
}
