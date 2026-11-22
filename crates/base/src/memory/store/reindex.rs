//! Rebuilding the index from the markdown files.
//!
//! The markdown files are the source of truth, so this walk is the only way the
//! index ever learns about a page it did not write itself: a page copied in, a
//! page edited by hand, an index deleted by a backup restore. It also repairs a
//! crash between "file written" and "index committed", and it prunes rows whose
//! file is gone, which is what makes a delete final.
//!
//! One malformed page never stops the walk. It is reported and left alone, so
//! its previous index rows stay as they were instead of turning into data loss.

use super::document::{self, MAX_BODY_BYTES};
use super::index::FileStamp;
use super::schema::Index;
use anyhow::{Context, Result, anyhow};
use horon_agent_memory_types::page::MemoryPage;
use horon_agent_time::Timestamp;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Largest page file read, in bytes: the body cap plus frontmatter headroom.
pub const MAX_PAGE_FILE_BYTES: u64 = MAX_BODY_BYTES as u64 + 64 * 1024;
/// Deepest directory level the walk descends.
const MAX_DEPTH: usize = 8;

/// A page file the walk found.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// Path relative to the wiki root, with `/` separators.
    pub relative: String,
    /// Absolute path on disk.
    pub absolute: PathBuf,
    /// Stamp taken from the filesystem.
    pub stamp: FileStamp,
}

/// A page the walk refused to index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPage {
    /// Path relative to the wiki root.
    pub path: String,
    /// Why the page was skipped.
    pub reason: String,
}

/// What one rebuild did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReindexReport {
    /// Page files found.
    pub files: usize,
    /// Pages written to the index.
    pub indexed: usize,
    /// Pages whose stamp matched, so they were not re-read.
    pub unchanged: usize,
    /// Index rows dropped because their file is gone.
    pub pruned: usize,
    /// Files that could not be indexed, with the reason.
    pub skipped: Vec<SkippedPage>,
}

/// Walk the wiki and make the index match it.
///
/// With `force`, every file is re-read and the index tables are cleared first;
/// without it, a file whose stamp matches the recorded one is left alone.
pub fn sync(index: &mut Index, wiki_root: &Path, force: bool) -> Result<ReindexReport> {
    if force {
        clear(index)?;
    }
    let files = scan(wiki_root)?;
    let mut report = ReindexReport {
        files: files.len(),
        ..ReindexReport::default()
    };
    let mut seen_paths: HashSet<String> = HashSet::with_capacity(files.len());
    let mut ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for file in &files {
        seen_paths.insert(file.relative.clone());
        let known = if force { None } else { index.file_stamp(&file.relative)? };
        if known == Some(file.stamp) {
            report.unchanged += 1;
            continue;
        }
        match read_page(file) {
            Ok(page) => {
                let id = page.id.as_str().to_string();
                if let Some(first) = ids.get(&id) {
                    report.skipped.push(SkippedPage {
                        path: file.relative.clone(),
                        reason: format!("page id {id} is already stored at {first}"),
                    });
                    continue;
                }
                ids.insert(id, file.relative.clone());
                index
                    .upsert_page(&page, &file.relative, file.stamp)
                    .with_context(|| format!("cannot index {}", file.relative))?;
                report.indexed += 1;
            }
            Err(error) => report.skipped.push(SkippedPage {
                path: file.relative.clone(),
                reason: format!("{error:#}"),
            }),
        }
    }
    report.pruned = prune_missing(index, &seen_paths)?;
    Ok(report)
}

/// Remove index rows whose file is no longer on disk.
fn prune_missing(index: &mut Index, seen_paths: &HashSet<String>) -> Result<usize> {
    let mut stale: Vec<(String, Option<String>)> = Vec::new();
    for (path, _) in index.file_stamps()? {
        if !seen_paths.contains(&path) {
            let id = index.page_id_for_path(&path)?;
            stale.push((path, id));
        }
    }
    stale.sort();
    let mut pruned = 0;
    for (path, id) in stale {
        match id {
            Some(id) => {
                if index.remove_page(&id)? {
                    pruned += 1;
                }
            }
            None => {
                index.remove_file_stamp(&path)?;
            }
        }
    }
    Ok(pruned)
}

/// Empty every derived table. The caller follows with a forced sync.
pub fn clear(index: &mut Index) -> Result<()> {
    let tx = index
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("cannot start an index rebuild")?;
    for table in ["pages", "tags", "links", "entities", "versions", "files"] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    for table in ["pages_fts", "pages_bigram", "pages_grams"] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    tx.commit().context("cannot commit an index rebuild")?;
    Ok(())
}

/// Every page file under `wiki_root`, sorted by relative path.
pub fn scan(wiki_root: &Path) -> Result<Vec<ScannedFile>> {
    let mut out = Vec::new();
    if !wiki_root.exists() {
        return Ok(out);
    }
    let mut directories = vec![(wiki_root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .with_context(|| format!("cannot read the wiki directory {}", directory.display()))?;
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_symlink() {
                // A symlink is never followed: the wiki root is the tenant
                // boundary and a link could point anywhere.
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
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                // Temp files from an interrupted write, and editor droppings.
                continue;
            }
            let Some(relative) = relative_slash_path(wiki_root, &path) else {
                continue;
            };
            let metadata = entry.metadata()?;
            if metadata.len() > MAX_PAGE_FILE_BYTES {
                out.push(ScannedFile {
                    relative,
                    absolute: path,
                    stamp: FileStamp {
                        mtime_ms: -1,
                        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                    },
                });
                continue;
            }
            out.push(ScannedFile {
                relative,
                absolute: path,
                stamp: stamp_of(&metadata),
            });
        }
    }
    out.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(out)
}

/// The index stamp of a file metadata value.
pub fn stamp_of(metadata: &std::fs::Metadata) -> FileStamp {
    let mtime_ms = match metadata.modified() {
        Ok(time) => match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        },
        Err(_) => 0,
    };
    FileStamp {
        mtime_ms,
        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
    }
}

fn read_page(file: &ScannedFile) -> Result<MemoryPage> {
    let bytes = std::fs::read(&file.absolute).with_context(|| format!("cannot read {}", file.relative))?;
    if bytes.len() as u64 > MAX_PAGE_FILE_BYTES {
        return Err(anyhow!("page file is over {MAX_PAGE_FILE_BYTES} bytes"));
    }
    let text = String::from_utf8(bytes).map_err(|_| anyhow!("page file is not UTF-8"))?;
    let stem = file
        .absolute
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("page file has no UTF-8 name"))?;
    let fallback = Timestamp::from_timestamp_millis(file.stamp.mtime_ms.max(0)).unwrap_or(Timestamp::UNIX_EPOCH);
    document::decode(&text, stem, fallback)
}

/// `path` relative to `root` with `/` separators.
pub fn relative_slash_path(root: &Path, path: &Path) -> Option<String> {
    let relative = match path.strip_prefix(root) {
        Ok(relative) => relative,
        Err(_) => return None,
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        parts.push(component.as_os_str().to_str()?.to_string());
    }
    Some(parts.join("/"))
}
