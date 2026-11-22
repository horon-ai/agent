//! Writes and reads against the derived index.
//!
//! Every mutating call owns one `BEGIN IMMEDIATE` transaction, so two writers
//! serialise inside SQLite instead of losing each other's rows. Nothing here
//! touches the markdown files: the store writes the file first and calls these
//! functions with the page that is already durable, so a process that dies
//! between the two leaves an index the next rescan repairs.

use super::schema::Index;
use super::text::{bigram_expansion, normalize_entity};
use anyhow::{Context, Result};
use horon_agent_memory_types::MemoryPage;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

/// One page as the index knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRow {
    /// Page id.
    pub id: String,
    /// Path relative to the wiki root.
    pub path: String,
    /// Page kind text.
    pub kind: String,
    /// Memory category text.
    pub category: String,
    /// Project key, when the page is project-scoped.
    pub project: Option<String>,
    /// Page title.
    pub title: String,
    /// Last write, in milliseconds since the Unix epoch.
    pub updated_ms: i64,
    /// First write, in milliseconds since the Unix epoch.
    pub created_ms: i64,
    /// Whether the page is current.
    pub active: bool,
    /// First valid date.
    pub valid_from: Option<String>,
    /// Last valid date.
    pub valid_to: Option<String>,
    /// Where the page came from.
    pub source: Option<String>,
}

/// The stamp a page file had when it was indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// File size in bytes.
    pub size: i64,
}

/// One link as the index stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRow {
    /// Source page id.
    pub src_id: String,
    /// Destination page id.
    pub dst_id: String,
    /// Link kind text.
    pub kind: String,
    /// Traversal weight.
    pub weight: f32,
}

/// One stored version of a page body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRow {
    /// Revision number, starting at 1.
    pub rev: i64,
    /// The body as written in that revision.
    pub body: String,
    /// When the revision was written.
    pub created_at: String,
}

/// Row counts, for `stats` and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexCounts {
    /// Pages in the index.
    pub pages: i64,
    /// Links between pages.
    pub links: i64,
    /// Entity rows.
    pub entities: i64,
    /// Stored versions.
    pub versions: i64,
}

impl Index {
    /// Insert or update one page and record the file stamp and a new version.
    ///
    /// The transaction is `IMMEDIATE`, so a concurrent writer waits here
    /// instead of interleaving. Returns the revision number written.
    pub fn upsert_page(&mut self, page: &MemoryPage, path: &str, stamp: FileStamp) -> Result<i64> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("cannot start a memory index write")?;
        let id = page.id.as_str();
        let existing: Option<i64> = tx
            .query_row("SELECT rowid FROM pages WHERE id = ?1", [id], |row| row.get(0))
            .optional()?;
        if let Some(rowid) = existing {
            clear_page_rows(&tx, rowid, id)?;
        }
        // A page whose frontmatter id was edited keeps its file path, so the
        // row (and its full-text rows) for the previous id must go before the
        // path can be claimed again. Its versions go too: they belong to a page
        // id that no longer exists.
        let path_owner: Option<(i64, String)> = tx
            .query_row("SELECT rowid, id FROM pages WHERE path = ?1", [path], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        if let Some((rowid, previous_id)) = path_owner {
            clear_page_rows(&tx, rowid, &previous_id)?;
            tx.execute("DELETE FROM versions WHERE page_id = ?1", [&previous_id])?;
            tx.execute("DELETE FROM files WHERE page_id = ?1", [&previous_id])?;
        }
        let tags_json = serde_json::to_string(&page.tags)?;
        let entities_json = serde_json::to_string(&page.entities)?;
        tx.execute(
            "INSERT INTO pages (id, path, kind, category, project, title, body, tags, entities,
                                updated_ms, created_ms, active, valid_from, valid_to, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                id,
                path,
                page.kind.as_str(),
                page.category.to_string(),
                page.project,
                page.title,
                page.body,
                tags_json,
                entities_json,
                page.updated_at.timestamp_millis(),
                page.created_at.timestamp_millis(),
                page.active,
                page.valid_from,
                page.valid_to,
                page.source,
            ],
        )?;
        let rowid: i64 = tx.query_row("SELECT rowid FROM pages WHERE id = ?1", [id], |row| row.get(0))?;
        let grams = bigram_expansion(&format!("{} {}", page.title, page.body));
        tx.execute(
            "INSERT INTO pages_fts (rowid, id, title, body, tags, entities) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rowid,
                id,
                page.title,
                page.body,
                page.tags.join(" "),
                page.entities.join(" ")
            ],
        )?;
        tx.execute(
            "INSERT INTO pages_bigram (rowid, id, title, body) VALUES (?1, ?2, ?3, ?4)",
            params![rowid, id, page.title, page.body],
        )?;
        tx.execute(
            "INSERT INTO pages_grams (rowid, id, grams) VALUES (?1, ?2, ?3)",
            params![rowid, id, grams],
        )?;
        for tag in &page.tags {
            tx.execute(
                "INSERT OR REPLACE INTO tags (page_id, tag) VALUES (?1, ?2)",
                params![id, tag],
            )?;
        }
        for entity in &page.entities {
            tx.execute(
                "INSERT OR REPLACE INTO entities (page_id, name, name_norm) VALUES (?1, ?2, ?3)",
                params![id, entity, normalize_entity(entity)],
            )?;
        }
        for link in &page.links {
            tx.execute(
                "INSERT OR REPLACE INTO links (src_id, dst_id, kind, weight) VALUES (?1, ?2, ?3, ?4)",
                params![
                    id,
                    link.dst.as_str(),
                    link.kind.as_str(),
                    f64::from(link.traversal_weight())
                ],
            )?;
        }
        tx.execute("DELETE FROM files WHERE page_id = ?1", [id])?;
        tx.execute(
            "INSERT OR REPLACE INTO files (path, page_id, mtime_ns, size) VALUES (?1, ?2, ?3, ?4)",
            params![path, id, stamp.mtime_ns, stamp.size],
        )?;
        let rev: i64 = tx.query_row(
            "SELECT COALESCE(MAX(rev), 0) + 1 FROM versions WHERE page_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO versions (page_id, rev, body, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, rev, page.body, page.updated_at.to_string()],
        )?;
        tx.commit().context("cannot commit a memory index write")?;
        Ok(rev)
    }

    /// Remove every row of one page, including its versions and file stamp.
    ///
    /// Returns whether the page had an index row. The caller removes the
    /// markdown file first, so a failure here leaves a stale row that the next
    /// rescan prunes rather than a resurrected page.
    pub fn remove_page(&mut self, id: &str) -> Result<bool> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("cannot start a memory index delete")?;
        let existing: Option<i64> = tx
            .query_row("SELECT rowid FROM pages WHERE id = ?1", [id], |row| row.get(0))
            .optional()?;
        let Some(rowid) = existing else {
            tx.commit().context("cannot commit a memory index delete")?;
            return Ok(false);
        };
        clear_page_rows(&tx, rowid, id)?;
        tx.execute("DELETE FROM versions WHERE page_id = ?1", [id])?;
        tx.execute("DELETE FROM files WHERE page_id = ?1", [id])?;
        tx.commit().context("cannot commit a memory index delete")?;
        Ok(true)
    }

    /// One page row by id.
    pub fn page_row(&self, id: &str) -> Result<Option<PageRow>> {
        let mut statement = self.conn().prepare(PAGE_ROW_SELECT)?;
        Ok(statement.query_row([id], page_row_from).optional()?)
    }

    /// Every page row, ordered by id.
    pub fn rows(&self) -> Result<Vec<PageRow>> {
        let mut statement = self
            .conn()
            .prepare(&format!("{PAGE_ROW_SELECT_BASE} ORDER BY id ASC"))?;
        let rows = statement.query_map([], page_row_from)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The stored body of one page, for export and version inspection.
    pub fn page_body(&self, id: &str) -> Result<Option<String>> {
        let mut statement = self.conn().prepare("SELECT body FROM pages WHERE id = ?1")?;
        Ok(statement.query_row([id], |row| row.get(0)).optional()?)
    }

    /// The file stamp recorded for one path.
    pub fn file_stamp(&self, path: &str) -> Result<Option<FileStamp>> {
        let mut statement = self
            .conn()
            .prepare("SELECT mtime_ns, size FROM files WHERE path = ?1")?;
        let mut rows = statement.query([path])?;
        match rows.next()? {
            Some(row) => Ok(Some(FileStamp {
                mtime_ns: row.get(0)?,
                size: row.get(1)?,
            })),
            None => Ok(None),
        }
    }

    /// Every recorded file stamp, as `(path, stamp)`.
    pub fn file_stamps(&self) -> Result<Vec<(String, FileStamp)>> {
        let mut statement = self.conn().prepare("SELECT path, mtime_ns, size FROM files")?;
        let rows = statement.query_map([], |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                FileStamp {
                    mtime_ns: row.get(1)?,
                    size: row.get(2)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The page id recorded for a path, when the file is indexed.
    pub fn page_id_for_path(&self, path: &str) -> Result<Option<String>> {
        let mut statement = self.conn().prepare("SELECT page_id FROM files WHERE path = ?1")?;
        Ok(statement.query_row([path], |row| row.get(0)).optional()?)
    }

    /// Every `(page id, tag)` pair, for tag filters.
    pub fn all_tags(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.conn().prepare("SELECT page_id, tag FROM tags")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The path recorded for a page id, even when the page row is gone.
    pub fn recorded_path_for_id(&self, id: &str) -> Result<Option<String>> {
        let mut statement = self
            .conn()
            .prepare("SELECT path FROM files WHERE page_id = ?1 ORDER BY path ASC LIMIT 1")?;
        Ok(statement.query_row([id], |row| row.get(0)).optional()?)
    }

    /// Forget the stamp of a path that has no page row.
    pub fn remove_file_stamp(&mut self, path: &str) -> Result<()> {
        self.conn_mut().execute("DELETE FROM files WHERE path = ?1", [path])?;
        Ok(())
    }

    /// Links leaving `id`.
    pub fn links_from(&self, id: &str) -> Result<Vec<LinkRow>> {
        self.links("SELECT src_id, dst_id, kind, weight FROM links WHERE src_id = ?1", id)
    }

    /// Links arriving at `id`.
    pub fn links_to(&self, id: &str) -> Result<Vec<LinkRow>> {
        self.links("SELECT src_id, dst_id, kind, weight FROM links WHERE dst_id = ?1", id)
    }

    fn links(&self, sql: &str, id: &str) -> Result<Vec<LinkRow>> {
        let mut statement = self.conn().prepare(sql)?;
        let rows = statement.query_map([id], |row| {
            Ok(LinkRow {
                src_id: row.get(0)?,
                dst_id: row.get(1)?,
                kind: row.get(2)?,
                weight: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Versions of one page, oldest first.
    pub fn versions(&self, id: &str) -> Result<Vec<VersionRow>> {
        let mut statement = self
            .conn()
            .prepare("SELECT rev, body, created_at FROM versions WHERE page_id = ?1 ORDER BY rev ASC")?;
        let rows = statement.query_map([id], |row| {
            Ok(VersionRow {
                rev: row.get(0)?,
                body: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// How many pages, links, entities and versions the index holds.
    pub fn counts(&self) -> Result<IndexCounts> {
        let count = |sql: &str| -> Result<i64> {
            let mut statement = self.conn().prepare(sql)?;
            Ok(statement.query_row([], |row| row.get(0))?)
        };
        Ok(IndexCounts {
            pages: count("SELECT COUNT(*) FROM pages")?,
            links: count("SELECT COUNT(*) FROM links")?,
            entities: count("SELECT COUNT(*) FROM entities")?,
            versions: count("SELECT COUNT(*) FROM versions")?,
        })
    }
}

const PAGE_ROW_SELECT_BASE: &str = "SELECT id, path, kind, category, project, title, updated_ms, created_ms,
        active, valid_from, valid_to, source FROM pages";
const PAGE_ROW_SELECT: &str = "SELECT id, path, kind, category, project, title, updated_ms, created_ms,
        active, valid_from, valid_to, source FROM pages WHERE id = ?1";

pub(super) fn page_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageRow> {
    Ok(PageRow {
        id: row.get(0)?,
        path: row.get(1)?,
        kind: row.get(2)?,
        category: row.get(3)?,
        project: row.get(4)?,
        title: row.get(5)?,
        updated_ms: row.get(6)?,
        created_ms: row.get(7)?,
        active: row.get(8)?,
        valid_from: row.get(9)?,
        valid_to: row.get(10)?,
        source: row.get(11)?,
    })
}

/// Remove the rows that belong to one page, keeping its version history.
fn clear_page_rows(tx: &rusqlite::Transaction<'_>, rowid: i64, id: &str) -> Result<()> {
    tx.execute("DELETE FROM pages_fts WHERE rowid = ?1", [rowid])?;
    tx.execute("DELETE FROM pages_bigram WHERE rowid = ?1", [rowid])?;
    tx.execute("DELETE FROM pages_grams WHERE rowid = ?1", [rowid])?;
    tx.execute("DELETE FROM links WHERE src_id = ?1 OR dst_id = ?1", [id])?;
    tx.execute("DELETE FROM entities WHERE page_id = ?1", [id])?;
    tx.execute("DELETE FROM tags WHERE page_id = ?1", [id])?;
    tx.execute("DELETE FROM pages WHERE rowid = ?1", [rowid])?;
    Ok(())
}
