//! Per-user memory wiki: markdown files as the truth, SQLite as the index.
//!
//! Layout under `root` (normally `$HORON_HOME/memory`):
//!
//! ```text
//! root/
//!   wiki/
//!     preferences/<id>.md
//!     people/<id>.md           # kind person
//!     entities/<id>.md
//!     facts/<id>.md
//!     procedures/<id>.md
//!     corrections/<id>.md
//!     lessons/<id>.md
//!     custom/<id>.md
//!     events/<YYYY>/<MM>/<id>.md
//!     preferences/<project>/<id>.md   # project-scoped page
//!   index.sqlite               # derived: pages, FTS, links, entities, versions
//! ```
//!
//! The file is authoritative. [`MemoryWiki::open`] reconciles the index with the
//! files on every open (stamps let it skip files that did not change), prunes
//! rows whose file vanished, and rebuilds from scratch when the index is absent,
//! carries another schema version, or is corrupt. Losing `index.sqlite` therefore
//! costs a rescan, not data.
//!
//! Writes take the file first (temp file, fsync, rename, owner-only permissions)
//! and then commit one index transaction, so a crash between the two leaves a
//! page the next open indexes. Deletes take the file first for the same reason.
//!
//! The public surface is the one the memory tool and the export path need:
//! [`MemoryWiki::write_page`], [`MemoryWiki::delete_page`],
//! [`MemoryWiki::page`], [`MemoryWiki::list`], [`MemoryWiki::search`],
//! [`MemoryWiki::reindex`], and the row accessors for export.

mod delete;
mod document;
mod frontmatter;
mod index;
mod reindex;
mod schema;
mod search;
mod text;

#[cfg(test)]
#[path = "document_tests.rs"]
mod document_tests;
#[cfg(test)]
#[path = "eval_tests.rs"]
mod eval_tests;
#[cfg(test)]
#[path = "frontmatter_tests.rs"]
mod frontmatter_tests;
#[cfg(test)]
#[path = "search_tests.rs"]
mod search_tests;
#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
#[cfg(test)]
#[path = "tests_support.rs"]
mod tests_support;
#[cfg(test)]
#[path = "text_tests.rs"]
mod text_tests;

pub use delete::{DeleteOutcome, safe_join};
pub use document::{MAX_BODY_BYTES, PAGE_SCHEMA};
pub use index::{FileStamp, IndexCounts, LinkRow, PageRow, VersionRow};
pub use reindex::{ReindexReport, SkippedPage};
pub use search::{DEFAULT_LIMIT, SearchHit, SearchQuery, SearchResults, StreamRanks};

use anyhow::{Context, Result, anyhow};
use horon_agent_memory_types::MemoryPage;
use horon_agent_memory_types::page::{PageId, PageKind};
use horon_agent_time::Timestamp;
use schema::Index;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The store handle.
pub struct MemoryWiki {
    root: PathBuf,
    index: Index,
    last_report: ReindexReport,
}

/// What one write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Path of the page file, relative to the wiki root, `/` separated.
    pub path: String,
    /// Version number written for the page body.
    pub revision: i64,
}

/// Which pages [`MemoryWiki::list`] returns.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Restrict to these kinds; empty means every kind.
    pub kinds: Vec<PageKind>,
    /// Restrict to one project key.
    pub project: Option<String>,
    /// Restrict to pages carrying every one of these tags.
    pub tags: Vec<String>,
    /// Include pages that were superseded.
    pub include_inactive: bool,
    /// Most pages to return.
    pub limit: Option<usize>,
}

impl MemoryWiki {
    /// Open the store under `root`, creating it and reconciling the index with
    /// the page files.
    ///
    /// A page that cannot be parsed is reported in
    /// [`MemoryWiki::last_report`] and skipped; it never fails the open, so one
    /// damaged file cannot lock the user out of the rest of the wiki.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let wiki = root.join("wiki");
        ensure_private_dir(&root)?;
        ensure_private_dir(&wiki)?;
        let mut index = Index::open(&root.join(schema::INDEX_FILE_NAME))?;
        let force = index.is_fresh();
        index.clear_fresh();
        let report = reindex::sync(&mut index, &wiki, force)?;
        Ok(Self {
            root,
            index,
            last_report: report,
        })
    }

    /// The store root (`$HORON_HOME/memory`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory that holds the page files.
    pub fn wiki_root(&self) -> PathBuf {
        self.root.join("wiki")
    }

    /// What the last reconcile did.
    pub fn last_report(&self) -> &ReindexReport {
        &self.last_report
    }

    /// Reconcile changed and removed files without clearing the index.
    pub fn resync(&mut self) -> Result<ReindexReport> {
        let wiki = self.wiki_root();
        let report = reindex::sync(&mut self.index, &wiki, false)?;
        self.last_report = report.clone();
        Ok(report)
    }

    /// Rebuild the whole index from the page files.
    ///
    /// The result is deterministic: pages are read in path order, and the
    /// fusion's tie-breaks are explicit, so the same files answer the same
    /// query the same way before and after.
    pub fn reindex(&mut self) -> Result<ReindexReport> {
        let wiki = self.wiki_root();
        let report = reindex::sync(&mut self.index, &wiki, true)?;
        self.last_report = report.clone();
        Ok(report)
    }

    /// Write one page, creating it or replacing it.
    ///
    /// The file is written atomically with owner-only permissions, then the
    /// index is updated in one transaction and a version row is added. A page
    /// whose kind or project changes moves to the new path, and the old file is
    /// removed after the new one is indexed.
    pub fn write_page(&mut self, page: &MemoryPage) -> Result<WriteOutcome> {
        document::validate(page)?;
        let relative = relative_slash(&document::relative_path(page)?)?;
        let text = document::encode(page)?;
        let wiki = self.wiki_root();
        let target = delete::safe_join(&wiki, &relative)?;
        let previous = self.index.page_row(page.id.as_str())?.map(|row| row.path);
        write_atomic(&target, text.as_bytes())?;
        let metadata =
            std::fs::metadata(&target).with_context(|| format!("cannot stat the page file {}", target.display()))?;
        let stamp = reindex::stamp_of(&metadata);
        let revision = self.index.upsert_page(page, &relative, stamp)?;
        if let Some(previous) = previous
            && previous != relative
        {
            let old = delete::safe_join(&wiki, &previous)?;
            match std::fs::remove_file(&old) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(
                        anyhow!(error).context(format!("cannot remove the previous page file {}", old.display()))
                    );
                }
            }
        }
        Ok(WriteOutcome {
            path: relative,
            revision,
        })
    }

    /// Delete one page: the file first, then every index row, then a vacuum.
    pub fn delete_page(&mut self, id: &PageId) -> Result<DeleteOutcome> {
        let wiki = self.wiki_root();
        delete::delete_page(&mut self.index, &wiki, id.as_str())
    }

    /// Read one page from its markdown file.
    pub fn page(&self, id: &PageId) -> Result<Option<MemoryPage>> {
        let Some(row) = self.index.page_row(id.as_str())? else {
            return Ok(None);
        };
        let path = delete::safe_join(&self.wiki_root(), &row.path)?;
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(anyhow!(error).context(format!("cannot stat the page file {}", path.display())));
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(anyhow!("page file {} is a symlink", path.display()));
        }
        let bytes = std::fs::read(&path).with_context(|| format!("cannot read the page file {}", path.display()))?;
        let text = String::from_utf8(bytes).map_err(|_| anyhow!("page file {} is not UTF-8", path.display()))?;
        let fallback = reindex::stamp_of(&metadata);
        let fallback_time =
            Timestamp::from_timestamp_millis(fallback.mtime_ns.max(0) / 1_000_000).unwrap_or(Timestamp::UNIX_EPOCH);
        document::decode(&text, id.file_stem().as_str(), fallback_time).map(Some)
    }

    /// Pages matching `filter`, newest first, with the id as the final tie-break.
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<PageRow>> {
        let mut rows = self.index.rows()?;
        rows.retain(|row| {
            if !filter.kinds.is_empty() {
                let kind = PageKind::parse(&row.kind);
                if !kind.is_some_and(|kind| filter.kinds.contains(&kind)) {
                    return false;
                }
            }
            if let Some(project) = &filter.project
                && row.project.as_deref() != Some(project.as_str())
            {
                return false;
            }
            if !filter.include_inactive && !row.active {
                return false;
            }
            true
        });
        if !filter.tags.is_empty() {
            let pairs = self.index.all_tags()?;
            rows.retain(|row| {
                filter.tags.iter().all(|tag| {
                    pairs
                        .iter()
                        .any(|(page_id, stored)| page_id == &row.id && stored == tag)
                })
            });
        }
        rows.sort_by(|left, right| {
            right
                .updated_ms
                .cmp(&left.updated_ms)
                .then_with(|| right.created_ms.cmp(&left.created_ms))
                .then_with(|| left.id.cmp(&right.id))
        });
        if let Some(limit) = filter.limit {
            rows.truncate(limit);
        }
        Ok(rows)
    }

    /// Search the index.
    pub fn search(&self, query: &SearchQuery) -> Result<SearchResults> {
        search::search(&self.index, query)
    }

    /// Index row counts.
    pub fn stats(&self) -> Result<IndexCounts> {
        self.index.counts()
    }

    /// A counter that changes on every write to this store.
    ///
    /// It moves inside the write transaction, so a caller can key a cache on it
    /// and be sure the next read after any write (by this handle, another
    /// thread, another process) misses instead of answering from before that
    /// write. A write that changed nothing did not move it.
    pub fn revision(&self) -> Result<u64> {
        self.index.revision()
    }

    /// Every page of one scope, read from its markdown file.
    ///
    /// `project` selects one project key or, for `None`, the user-level pages.
    /// A page that cannot be read is reported and skipped, the same way
    /// [`MemoryWiki::page`] reports one damaged file, so a single broken page
    /// does not hide the rest of the scope.
    pub fn scope_pages(&self, project: Option<&str>) -> Result<Vec<MemoryPage>> {
        // `ListFilter::project` cannot express "the user-level pages": `None`
        // there means "do not filter", so the scope is applied to the rows here.
        let rows = self.list(&ListFilter {
            include_inactive: true,
            ..ListFilter::default()
        })?;
        let rows: Vec<PageRow> = rows
            .into_iter()
            .filter(|row| row.project.as_deref() == project)
            .collect();
        let mut pages = Vec::with_capacity(rows.len());
        for row in rows {
            let Ok(id) = PageId::parse(&row.id) else {
                crate::logging::warn(&format!("memory index holds the unstorable page id {}", row.id));
                continue;
            };
            match self.page(&id) {
                Ok(Some(page)) => pages.push(page),
                Ok(None) => {}
                Err(error) => {
                    crate::logging::warn(&format!("cannot read memory page {}: {error}", row.id));
                }
            }
        }
        Ok(pages)
    }

    /// The absolute path of one page file, when the page is indexed.
    pub fn page_path(&self, id: &PageId) -> Result<Option<PathBuf>> {
        let Some(row) = self.index.page_row(id.as_str())? else {
            return Ok(None);
        };
        Ok(Some(delete::safe_join(&self.wiki_root(), &row.path)?))
    }

    /// The stored body of one page, from the index.
    pub fn stored_body(&self, id: &PageId) -> Result<Option<String>> {
        self.index.page_body(id.as_str())
    }

    /// Every stored version of one page, oldest first.
    pub fn versions(&self, id: &PageId) -> Result<Vec<VersionRow>> {
        self.index.versions(id.as_str())
    }
}

/// Create a directory owner-only, and tighten an existing one.
fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("cannot create the memory directory {}", path.display()))?;
    horon_agent_platform::fs::set_dir_permissions_owner_only(path)
        .with_context(|| format!("cannot restrict the memory directory {}", path.display()))?;
    Ok(())
}

/// Join path components with `/` for storage in the index.
fn relative_slash(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                let Some(text) = part.to_str() else {
                    return Err(anyhow!("page path component is not UTF-8"));
                };
                parts.push(text.to_string());
            }
            std::path::Component::CurDir => {}
            _ => return Err(anyhow!("page path is not a plain relative path")),
        }
    }
    Ok(parts.join("/"))
}

/// Write `bytes` to `path` atomically: temp file in the same directory, fsync,
/// owner-only permissions, rename over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Err(anyhow!("page path {} has no parent directory", path.display()));
    };
    ensure_private_dir(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("page path {} has no UTF-8 file name", path.display()))?;
    let nonce: u64 = rand::random();
    let temp = parent.join(format!(".{file_name}.tmp.{}.{nonce}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        horon_agent_platform::fs::request_owner_only_mode(&mut options);
        let mut file = options
            .open(&temp)
            .with_context(|| format!("cannot create {}", temp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("cannot flush {}", temp.display()))?;
        horon_agent_platform::fs::set_file_permissions_owner_only(&temp)
            .with_context(|| format!("cannot restrict {}", temp.display()))?;
        std::fs::rename(&temp, path).with_context(|| format!("cannot move {} into place", path.display()))?;
        horon_agent_platform::fs::sync_parent_directory(path)
            .with_context(|| format!("cannot flush the directory of {}", path.display()))?;
        Ok(())
    })();
    if result.is_err()
        && let Err(cleanup) = std::fs::remove_file(&temp)
        && cleanup.kind() != std::io::ErrorKind::NotFound
    {
        return Err(anyhow!(cleanup).context(format!("cannot remove the temp file {}", temp.display())));
    }
    result
}
