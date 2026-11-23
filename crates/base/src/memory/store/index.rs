//! Writes and reads against the derived index.
//!
//! Every mutating call owns one `BEGIN IMMEDIATE` transaction, so two writers
//! serialise inside SQLite instead of losing each other's rows. Nothing here
//! touches the markdown files: the store writes the file first and calls these
//! functions with the page that is already durable, so a process that dies
//! between the two leaves an index the next rescan repairs.

use super::schema::Index;
use super::text::{bigram_expansion_parts, normalize_entity};
use anyhow::{Context, Result};
use horon_agent_memory_types::MemoryPage;
use horon_agent_memory_types::page::PageKind;
use rusqlite::{OptionalExtension, ToSql, Transaction, TransactionBehavior, params, params_from_iter};
use std::borrow::Cow;
use std::collections::HashSet;

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
        let rev = upsert_page_in_tx(&tx, page, path, stamp)?;
        tx.commit().context("cannot commit a memory index write")?;
        Ok(rev)
    }

    /// Insert or update a batch of pages in one `IMMEDIATE` transaction,
    /// optionally clearing all derived tables first.
    pub(super) fn upsert_batch(&mut self, clear_first: bool, entries: &[(MemoryPage, &str, FileStamp)]) -> Result<()> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("cannot start a memory batch index write")?;
        if clear_first {
            clear_in_tx(&tx)?;
        }
        for (page, path, stamp) in entries {
            upsert_page_in_tx(&tx, page, path, *stamp).with_context(|| format!("cannot index {path}"))?;
        }
        tx.commit().context("cannot commit a memory batch index write")?;
        Ok(())
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
        let removed = remove_page_in_tx(&tx, id)?;
        tx.commit().context("cannot commit a memory index delete")?;
        Ok(removed)
    }

    /// Remove stale pages and orphaned file stamps in one `IMMEDIATE` transaction.
    pub(super) fn prune_missing_batch(&mut self, stale: &[(String, Option<String>)]) -> Result<usize> {
        if stale.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("cannot start a memory index prune")?;
        let mut pruned = 0usize;
        let mut remove_stamp = tx.prepare_cached("DELETE FROM files WHERE path = ?1")?;
        for (path, id) in stale {
            match id {
                Some(id) => {
                    if remove_page_in_tx(&tx, id)? {
                        pruned += 1;
                    }
                }
                None => {
                    remove_stamp.execute([path])?;
                }
            }
        }
        drop(remove_stamp);
        tx.commit().context("cannot commit a memory index prune")?;
        Ok(pruned)
    }

    /// Clear all derived tables inside a single `IMMEDIATE` transaction.
    pub(super) fn clear_all(&mut self) -> Result<()> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("cannot start an index rebuild")?;
        clear_in_tx(&tx)?;
        tx.commit().context("cannot commit an index rebuild")?;
        Ok(())
    }

    /// One page row by id.
    pub fn page_row(&self, id: &str) -> Result<Option<PageRow>> {
        let mut statement = self.conn().prepare_cached(PAGE_ROW_SELECT)?;
        Ok(statement.query_row([id], page_row_from).optional()?)
    }

    /// Filtered and sorted page rows pushed down into SQLite.
    pub(super) fn list_rows(
        &self,
        kinds: &[PageKind],
        project: Option<&str>,
        tags: &[String],
        include_inactive: bool,
        limit: Option<usize>,
    ) -> Result<Vec<PageRow>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        enum Arg<'a> {
            Text(&'a str),
            Int(i64),
        }
        impl ToSql for Arg<'_> {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                match self {
                    Self::Text(text) => text.to_sql(),
                    Self::Int(num) => num.to_sql(),
                }
            }
        }
        let mut sql = String::from(
            "SELECT p.id, p.path, p.kind, p.category, p.project, p.title, p.updated_ms, p.created_ms,
                    p.active, p.valid_from, p.valid_to, p.source FROM pages p WHERE 1=1",
        );
        let mut params: Vec<Arg<'_>> = Vec::with_capacity(tags.len() + 2);
        if !include_inactive {
            sql.push_str(" AND p.active = 1");
        }
        if !kinds.is_empty() {
            sql.push_str(" AND p.kind IN (");
            for (idx, kind) in kinds.iter().enumerate() {
                if idx > 0 {
                    sql.push_str(", ");
                }
                sql.push('\'');
                sql.push_str(kind.as_str());
                sql.push('\'');
            }
            sql.push(')');
        }
        if let Some(project) = project {
            params.push(Arg::Text(project));
            sql.push_str(&format!(" AND p.project = ?{}", params.len()));
        }
        let mut unique_tags: Vec<&str> = Vec::with_capacity(tags.len());
        for tag in tags {
            if !unique_tags.contains(&tag.as_str()) {
                unique_tags.push(tag.as_str());
            }
        }
        for tag in unique_tags {
            params.push(Arg::Text(tag));
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM tags t WHERE t.page_id = p.id AND t.tag = ?{})",
                params.len()
            ));
        }
        sql.push_str(" ORDER BY p.updated_ms DESC, p.created_ms DESC, p.id ASC");
        if let Some(limit) = limit {
            params.push(Arg::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
            sql.push_str(&format!(" LIMIT ?{}", params.len()));
        }
        let mut statement = self.conn().prepare_cached(&sql)?;
        let rows = statement.query_map(params_from_iter(params.iter()), page_row_from)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The stored body of one page, for export and version inspection.
    pub fn page_body(&self, id: &str) -> Result<Option<String>> {
        let mut statement = self.conn().prepare_cached("SELECT body FROM pages WHERE id = ?1")?;
        Ok(statement.query_row([id], |row| row.get(0)).optional()?)
    }

    /// Every recorded file stamp with its `page_id`, as `(path, page_id, stamp)`.
    pub(super) fn file_stamps_with_ids(&self) -> Result<Vec<(String, Option<String>, FileStamp)>> {
        let mut statement = self
            .conn()
            .prepare_cached("SELECT path, page_id, mtime_ns, size FROM files")?;
        let rows = statement.query_map([], |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                FileStamp {
                    mtime_ns: row.get(2)?,
                    size: row.get(3)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Candidate page ids among `ids` carrying every tag in `tags`.
    pub(super) fn page_ids_with_all_tags(&self, ids: &[&str], tags: &[&str]) -> Result<HashSet<String>> {
        if ids.is_empty() || tags.is_empty() {
            return Ok(HashSet::new());
        }
        let mut unique_tags: Vec<&str> = Vec::with_capacity(tags.len());
        for tag in tags {
            if !unique_tags.contains(tag) {
                unique_tags.push(tag);
            }
        }
        let mut sql = String::from("SELECT page_id FROM tags WHERE page_id IN (");
        let mut params: Vec<&str> = Vec::with_capacity(ids.len() + unique_tags.len());
        for (idx, id) in ids.iter().enumerate() {
            if idx > 0 {
                sql.push_str(", ");
            }
            params.push(id);
            sql.push_str(&format!("?{}", params.len()));
        }
        sql.push_str(") AND tag IN (");
        for (idx, tag) in unique_tags.iter().enumerate() {
            if idx > 0 {
                sql.push_str(", ");
            }
            params.push(tag);
            sql.push_str(&format!("?{}", params.len()));
        }
        sql.push_str(&format!(
            ") GROUP BY page_id HAVING COUNT(DISTINCT tag) = {}",
            unique_tags.len()
        ));
        let mut statement = self.conn().prepare_cached(&sql)?;
        let rows = statement.query_map(params_from_iter(params.iter()), |row| row.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// All edges touching any of `seeds`, in one query.
    pub(super) fn links_for_seeds(&self, seeds: &[&str]) -> Result<Vec<(String, String, f32)>> {
        if seeds.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from("SELECT src_id, dst_id, weight FROM links WHERE src_id IN (");
        for idx in 1..=seeds.len() {
            if idx > 1 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("?{idx}"));
        }
        sql.push_str(") OR dst_id IN (");
        for idx in 1..=seeds.len() {
            if idx > 1 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("?{idx}"));
        }
        sql.push(')');
        let mut statement = self.conn().prepare_cached(&sql)?;
        let rows = statement.query_map(params_from_iter(seeds.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
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
            .prepare_cached("SELECT path FROM files WHERE page_id = ?1 ORDER BY path ASC LIMIT 1")?;
        Ok(statement.query_row([id], |row| row.get(0)).optional()?)
    }

    /// Versions of one page, oldest first.
    pub fn versions(&self, id: &str) -> Result<Vec<VersionRow>> {
        let mut statement = self
            .conn()
            .prepare_cached("SELECT rev, body, created_at FROM versions WHERE page_id = ?1 ORDER BY rev ASC")?;
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
fn clear_page_rows(tx: &Transaction<'_>, rowid: i64, id: &str) -> Result<()> {
    tx.prepare_cached("DELETE FROM pages_fts WHERE rowid = ?1")?
        .execute([rowid])?;
    tx.prepare_cached("DELETE FROM pages_bigram WHERE rowid = ?1")?
        .execute([rowid])?;
    tx.prepare_cached("DELETE FROM pages_grams WHERE rowid = ?1")?
        .execute([rowid])?;
    tx.prepare_cached("DELETE FROM links WHERE src_id = ?1 OR dst_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM entities WHERE page_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM tags WHERE page_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM pages WHERE rowid = ?1")?
        .execute([rowid])?;
    Ok(())
}

fn join_space(items: &[String]) -> Cow<'_, str> {
    match items {
        [] => Cow::Borrowed(""),
        [single] => Cow::Borrowed(single.as_str()),
        _ => Cow::Owned(items.join(" ")),
    }
}

fn json_string_list(items: &[String]) -> Result<Cow<'_, str>> {
    if items.is_empty() {
        Ok(Cow::Borrowed("[]"))
    } else {
        Ok(Cow::Owned(serde_json::to_string(items)?))
    }
}

fn clear_in_tx(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DELETE FROM pages;
         DELETE FROM tags;
         DELETE FROM links;
         DELETE FROM entities;
         DELETE FROM versions;
         DELETE FROM files;
         DELETE FROM pages_fts;
         DELETE FROM pages_bigram;
         DELETE FROM pages_grams;",
    )?;
    Ok(())
}

fn remove_page_in_tx(tx: &Transaction<'_>, id: &str) -> Result<bool> {
    let existing: Option<i64> = tx
        .prepare_cached("SELECT rowid FROM pages WHERE id = ?1")?
        .query_row([id], |row| row.get(0))
        .optional()?;
    let Some(rowid) = existing else {
        return Ok(false);
    };
    clear_page_rows(tx, rowid, id)?;
    tx.prepare_cached("DELETE FROM versions WHERE page_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM files WHERE page_id = ?1")?
        .execute([id])?;
    Ok(true)
}

fn upsert_page_in_tx(tx: &Transaction<'_>, page: &MemoryPage, path: &str, stamp: FileStamp) -> Result<i64> {
    let id = page.id.as_str();
    let existing: Option<i64> = tx
        .prepare_cached("SELECT rowid FROM pages WHERE id = ?1")?
        .query_row([id], |row| row.get(0))
        .optional()?;
    if let Some(rowid) = existing {
        clear_page_rows(tx, rowid, id)?;
    }
    // A page whose frontmatter id was edited keeps its file path, so the
    // row (and its full-text rows) for the previous id must go before the
    // path can be claimed again. Its versions go too: they belong to a page
    // id that no longer exists.
    let path_owner: Option<(i64, String)> = tx
        .prepare_cached("SELECT rowid, id FROM pages WHERE path = ?1")?
        .query_row([path], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    if let Some((rowid, previous_id)) = path_owner {
        clear_page_rows(tx, rowid, &previous_id)?;
        tx.prepare_cached("DELETE FROM versions WHERE page_id = ?1")?
            .execute([&previous_id])?;
        tx.prepare_cached("DELETE FROM files WHERE page_id = ?1")?
            .execute([&previous_id])?;
    }
    let tags_json = json_string_list(&page.tags)?;
    let entities_json = json_string_list(&page.entities)?;
    let category_text = page.category.to_string();
    tx.prepare_cached(
        "INSERT INTO pages (id, path, kind, category, project, title, body, tags, entities,
                            updated_ms, created_ms, active, valid_from, valid_to, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?
    .execute(params![
        id,
        path,
        page.kind.as_str(),
        category_text.as_str(),
        page.project,
        page.title,
        page.body,
        tags_json.as_ref(),
        entities_json.as_ref(),
        page.updated_at.timestamp_millis(),
        page.created_at.timestamp_millis(),
        page.active,
        page.valid_from,
        page.valid_to,
        page.source,
    ])?;
    let rowid = tx.last_insert_rowid();
    let grams = bigram_expansion_parts(&[&page.title, &page.body]);
    let tags_joined = join_space(&page.tags);
    let entities_joined = join_space(&page.entities);
    tx.prepare_cached(
        "INSERT INTO pages_fts (rowid, id, title, body, tags, entities) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![
        rowid,
        id,
        page.title,
        page.body,
        tags_joined.as_ref(),
        entities_joined.as_ref(),
    ])?;
    tx.prepare_cached("INSERT INTO pages_bigram (rowid, id, title, body) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![rowid, id, page.title, page.body])?;
    tx.prepare_cached("INSERT INTO pages_grams (rowid, id, grams) VALUES (?1, ?2, ?3)")?
        .execute(params![rowid, id, grams])?;
    if !page.tags.is_empty() {
        let mut tag_stmt = tx.prepare_cached("INSERT OR REPLACE INTO tags (page_id, tag) VALUES (?1, ?2)")?;
        for tag in &page.tags {
            tag_stmt.execute(params![id, tag])?;
        }
    }
    if !page.entities.is_empty() {
        let mut ent_stmt =
            tx.prepare_cached("INSERT OR REPLACE INTO entities (page_id, name, name_norm) VALUES (?1, ?2, ?3)")?;
        for entity in &page.entities {
            ent_stmt.execute(params![id, entity, normalize_entity(entity)])?;
        }
    }
    if !page.links.is_empty() {
        let mut link_stmt =
            tx.prepare_cached("INSERT OR REPLACE INTO links (src_id, dst_id, kind, weight) VALUES (?1, ?2, ?3, ?4)")?;
        for link in &page.links {
            link_stmt.execute(params![
                id,
                link.dst.as_str(),
                link.kind.as_str(),
                f64::from(link.traversal_weight())
            ])?;
        }
    }
    tx.prepare_cached("DELETE FROM files WHERE page_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("INSERT OR REPLACE INTO files (path, page_id, mtime_ns, size) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![path, id, stamp.mtime_ns, stamp.size])?;
    let rev: i64 = tx
        .prepare_cached("SELECT COALESCE(MAX(rev), 0) + 1 FROM versions WHERE page_id = ?1")?
        .query_row([id], |row| row.get(0))?;
    let updated_str = page.updated_at.to_string();
    tx.prepare_cached("INSERT INTO versions (page_id, rev, body, created_at) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![id, rev, page.body, updated_str.as_str()])?;
    Ok(rev)
}
