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
use super::frontmatter::{MAX_KEYS, MAX_LINE_BYTES, parse_id_field_value};
use super::reindex::{MAX_DEPTH, MAX_PAGE_FILE_BYTES};
use super::schema::Index;
use anyhow::{Context, Result, anyhow};
use horon_agent_memory_types::page::PageId;
use std::io::{BufRead, Read, Seek, SeekFrom};
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
    if !wiki_root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    let mut line = String::new();
    let mut directories = vec![(wiki_root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .with_context(|| format!("cannot read the wiki directory {}", directory.display()))?;
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth + 1 < MAX_DEPTH {
                    directories.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if file_name.starts_with('.') {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if !header_might_claim_id(&path, stem, id, &mut line) {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.len() > MAX_PAGE_FILE_BYTES {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if bytes.len() > MAX_BODY_BYTES + 64 * 1024 {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            let page_id_matches = match document::decode(&text, stem, horon_agent_time::Timestamp::UNIX_EPOCH) {
                Ok(page) => page.id.as_str() == id,
                Err(_) => false,
            };
            if page_id_matches {
                std::fs::remove_file(&path)
                    .with_context(|| format!("cannot remove the duplicate page file {}", path.display()))?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Check only the frontmatter header (or stem fallback) before reading/decoding
/// the full markdown body.
fn header_might_claim_id(path: &Path, stem: &str, target_id: &str, line: &mut String) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut stack_buf = [0u8; 1024];
    let mut filled = 0usize;
    while filled < 4 {
        match file.read(&mut stack_buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    if !stack_buf[..filled].starts_with(b"---\n") {
        return false;
    }
    let mut offset = 4usize;
    let mut seen_fields = 0usize;
    loop {
        while let Some(rel_nl) = stack_buf[offset..filled].iter().position(|&b| b == b'\n') {
            let raw_line = &stack_buf[offset..offset + rel_nl];
            offset += rel_nl + 1;
            if raw_line == b"---" {
                return matches!(PageId::from_file_stem(stem), Ok(parsed) if parsed.as_str() == target_id);
            }
            if seen_fields >= MAX_KEYS || raw_line.len() > MAX_LINE_BYTES {
                return false;
            }
            seen_fields += 1;
            if raw_line.starts_with(b"id:") {
                return match std::str::from_utf8(raw_line) {
                    Ok(text) => matches!(parse_id_field_value(text), Some(found_id) if found_id == target_id),
                    Err(_) => false,
                };
            }
        }
        if filled == stack_buf.len() {
            break;
        }
        match file.read(&mut stack_buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }

    // Header extended beyond 1 KiB; rewind to the start of the partial line so
    // `BufReader::read_line` never splits a multi-byte UTF-8 character.
    if file.seek(SeekFrom::Start(offset as u64)).is_err() {
        return false;
    }
    let mut reader = std::io::BufReader::new(file);
    loop {
        line.clear();
        match reader.read_line(line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            return false;
        }
        let trimmed = &line[..line.len() - 1];
        if trimmed == "---" {
            return matches!(PageId::from_file_stem(stem), Ok(parsed) if parsed.as_str() == target_id);
        }
        if seen_fields >= MAX_KEYS || trimmed.len() > MAX_LINE_BYTES {
            return false;
        }
        seen_fields += 1;
        if let Some(found_id) = parse_id_field_value(trimmed) {
            return found_id == target_id;
        }
    }
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
