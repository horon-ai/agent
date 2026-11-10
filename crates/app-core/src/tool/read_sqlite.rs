//! SQLite reads: `read app.db` lists tables with row counts, and
//! `read app.db:users` returns that table's schema plus a page of rows.
//!
//! The database logic lives in the base crate ([`horon_agent_base::sqlite_read`]);
//! this reader only decides what to show and pages it. Every query runs through
//! [`ReadOnlyDb`], which opens the file with `SQLITE_OPEN_READ_ONLY`, so a read
//! never writes to the database.
//!
//! `?limit=` and `?offset=` page the rows. Both are part of the path, so the
//! query string is stripped before the member name is looked up and the same
//! grammar stays available to a future `read` form.

use super::ReadInput;
use crate::tool::{ToolContext, ToolOutput};
use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};

/// Default rows per page when the call names no `limit`.
const DEFAULT_ROW_LIMIT: usize = 50;
/// Longest table list rendered; the rest is a count, so a huge schema cannot
/// spend a whole context window.
const MAX_LISTED_TABLES: usize = 200;
/// Cells rendered per row line before the row is cut.
const MAX_CELLS_PER_ROW: usize = 24;

/// One parsed SQLite read request.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SqliteTarget {
    pub path: PathBuf,
    pub table: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// Whether `path` is a SQLite database: extension plus the 16-byte header.
pub(super) fn is_sqlite(path: &Path) -> bool {
    let has_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            matches!(extension.as_str(), "sqlite" | "sqlite3" | "db" | "db3")
        });
    if !has_extension {
        return false;
    }
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut header = [0u8; 16];
    match std::io::Read::read_exact(&mut file, &mut header) {
        Ok(()) => horon_agent_base::sqlite_read::has_sqlite_magic(&header),
        Err(_) => false,
    }
}

/// Resolves `raw` to a database and an optional table, or `None` when the
/// request is not a SQLite read.
///
/// An existing file named exactly `raw` always wins, so a file whose name
/// contains a colon still reads as a file. Otherwise each colon is tried from
/// the right and the first prefix that is a SQLite database names the database.
pub(super) fn resolve(raw: &str, cwd: &Path) -> Option<SqliteTarget> {
    let whole = absolute(raw, cwd);
    if is_sqlite(&whole) {
        return Some(SqliteTarget {
            path: whole,
            table: None,
            limit: DEFAULT_ROW_LIMIT,
            offset: 0,
        });
    }
    for (index, _) in raw.rmatch_indices(':') {
        let (prefix, rest) = (&raw[..index], &raw[index + 1..]);
        if prefix.is_empty() || rest.is_empty() || rest.contains("://") {
            continue;
        }
        let candidate = absolute(prefix, cwd);
        if !is_sqlite(&candidate) {
            continue;
        }
        let (table, query) = match rest.split_once('?') {
            Some((table, query)) => (table, Some(query)),
            None => (rest, None),
        };
        if table.is_empty() {
            continue;
        }
        let (limit, offset) = parse_page(query);
        return Some(SqliteTarget {
            path: candidate,
            table: Some(table.to_string()),
            limit,
            offset,
        });
    }
    None
}

fn absolute(raw: &str, cwd: &Path) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    }
}

/// Parses `limit=&offset=` from a query string, ignoring unknown keys.
///
/// A value that is not a positive number leaves that field at its default
/// rather than failing: a wrong page hint should not cost the caller the read,
/// and the rendered header always states the page that was actually used.
pub(super) fn parse_page(query: Option<&str>) -> (usize, usize) {
    let mut limit = DEFAULT_ROW_LIMIT;
    let mut offset = 0usize;
    let Some(query) = query else {
        return (limit, offset);
    };
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let parsed = value.parse::<usize>();
        match (key, parsed) {
            ("limit", Ok(value)) if value > 0 => limit = value,
            ("offset", Ok(value)) => offset = value,
            ("limit" | "offset", Err(_)) => {}
            _ => {}
        }
    }
    (limit, offset)
}

/// Reads one SQLite request end to end.
pub(super) fn read_sqlite(params: &ReadInput, ctx: &ToolContext, cwd: &Path) -> Result<ToolOutput> {
    let target = resolve(&params.file_path, cwd).ok_or_else(|| anyhow!("not a SQLite read: {}", params.file_path))?;
    // The policy judged the raw argument, which still carries a table name.
    crate::tool::path_policy::enforce(
        "read",
        &serde_json::json!({ "file_path": target.path.to_string_lossy() }),
        ctx,
    )?;

    let db = horon_agent_base::sqlite_read::ReadOnlyDb::open(&target.path)
        .map_err(|error| anyhow!("{}: {error}", params.file_path))?;

    let Some(table) = target.table else {
        let tables = db.tables()?;
        return Ok(
            ToolOutput::new(render_table_listing(&params.file_path, &tables)).with_title(params.file_path.clone())
        );
    };

    if !db.has_table(&table)? {
        let available = db.tables()?;
        let names: Vec<&str> = available
            .iter()
            .take(MAX_LISTED_TABLES)
            .map(|table| table.name.as_str())
            .collect();
        bail!(
            "no table named {table:?} in {}. Tables: {}",
            target.path.display(),
            names.join(", ")
        );
    }

    let schema = db.schema(&table)?;
    let columns = db.columns(&table)?;
    let rows = db.rows(&table, target.limit, target.offset)?;
    let total = db.count_rows(&table)?;

    Ok(ToolOutput::new(render_table(
        &params.file_path,
        &table,
        &schema,
        &columns,
        &rows,
        total,
        (target.limit, target.offset),
    ))
    .with_title(format!("{}:{table}", params.file_path)))
}

fn render_table_listing(requested: &str, tables: &[horon_agent_base::sqlite_read::TableInfo]) -> String {
    let mut out = format!("SQLite database: {requested}\n{} tables:\n", tables.len());
    for table in tables.iter().take(MAX_LISTED_TABLES) {
        out.push_str(&format!("- {} ({} rows)\n", table.name, table.rows));
    }
    if tables.len() > MAX_LISTED_TABLES {
        out.push_str(&format!("- … {} more tables\n", tables.len() - MAX_LISTED_TABLES));
    }
    if tables.is_empty() {
        out.push_str("(no user tables)\n");
    }
    out
}

fn render_table(
    requested: &str,
    table: &str,
    schema: &horon_agent_base::sqlite_read::TableSchema,
    columns: &[String],
    rows: &[Vec<String>],
    total: i64,
    (limit, offset): (usize, usize),
) -> String {
    let mut out = format!("Table: {requested}:{table}\n");
    match &schema.sql {
        Some(sql) => out.push_str(&format!("\nSchema:\n{sql}\n")),
        None => out.push_str("\nSchema: (no stored CREATE TABLE statement)\n"),
    }
    if !schema.columns.is_empty() {
        out.push_str("\nColumns:\n");
        for column in &schema.columns {
            let mut flags = Vec::new();
            if column.primary_key {
                flags.push("primary key".to_string());
            }
            if column.not_null {
                flags.push("not null".to_string());
            }
            if let Some(default) = &column.default {
                flags.push(format!("default {default}"));
            }
            let suffix = if flags.is_empty() {
                String::new()
            } else {
                format!(" ({})", flags.join(", "))
            };
            out.push_str(&format!("- {} {}{suffix}\n", column.name, column.decl_type));
        }
    }

    if rows.is_empty() {
        out.push_str(&format!(
            "\nRows (offset {offset}, limit {limit}):\n(no rows on this page)\n"
        ));
        out.push_str(&format!("{total} rows total.\n"));
        return out;
    }

    out.push_str(&format!("\nRows (offset {offset}, limit {limit}):\n"));
    out.push_str(&columns.join(" | "));
    out.push('\n');
    for row in rows {
        let cells: Vec<&str> = row.iter().take(MAX_CELLS_PER_ROW).map(String::as_str).collect();
        let extra = row.len().saturating_sub(cells.len());
        out.push_str(&cells.join(" | "));
        if extra > 0 {
            out.push_str(&format!(" | … {extra} more columns"));
        }
        out.push('\n');
    }
    let shown_end = offset.saturating_add(rows.len());
    out.push_str(&format!("{shown_end} of {total} rows shown.\n"));
    if (shown_end as i64) < total {
        out.push_str(&format!(
            "Next page: {requested}:{table}?limit={limit}&offset={shown_end}\n"
        ));
    }
    out
}

#[cfg(test)]
#[path = "read_sqlite_tests.rs"]
mod tests;
