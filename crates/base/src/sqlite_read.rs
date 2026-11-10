//! Read-only SQLite inspection for the `read` tool.
//!
//! The `read` tool lists a database's tables and then one table's schema plus a
//! page of rows. Keeping the SQL here means the tool layer holds no database
//! logic, and every query opens the file with `SQLITE_OPEN_READ_ONLY`, so a read
//! can never write to the file it inspects (a WAL is not replayed into it, no
//! journal is created, and the bytes stay the same).
//!
//! A database that is locked, encrypted, or not a database fails here with a
//! message naming the path instead of surfacing a raw `rusqlite` error.

use anyhow::{Context, Result, anyhow};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// How long a read waits for another connection's lock before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest cell body rendered before it is cut. A blob or a huge text cell is
/// not something a read should spend a context window on.
pub const MAX_CELL_CHARS: usize = 300;

/// One table as listed by [`ReadOnlyDb::tables`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    /// `COUNT(*)` for the table, read at the same time as the name.
    pub rows: i64,
}

/// A table's `CREATE TABLE` statement and its declared columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub sql: Option<String>,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub decl_type: String,
    pub not_null: bool,
    pub default: Option<String>,
    pub primary_key: bool,
}

/// One result cell, already rendered for display.
pub type Cell = String;

/// An open read-only database. Dropping it closes the connection.
pub struct ReadOnlyDb {
    conn: Connection,
}

impl ReadOnlyDb {
    /// Open `path` read-only and prove it is a database this build can query.
    ///
    /// The probe runs a catalog query, so an encrypted file (which reads as
    /// noise) and a file that is not a database both fail here with a message
    /// that names the path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .with_context(|| format!("cannot open SQLite database read-only: {}", path.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get::<_, i64>(0))
            .map_err(|error| {
                anyhow!(
                    "{} is not a readable SQLite database (locked or encrypted?): {error}",
                    path.display()
                )
            })?;
        Ok(Self { conn })
    }

    /// Tables in name order, each with its row count.
    ///
    /// `sqlite_%` internal tables are left out: they are catalog bookkeeping,
    /// not the user's data.
    pub fn tables(&self) -> Result<Vec<TableInfo>> {
        let mut statement = self.conn.prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut tables = Vec::with_capacity(names.len());
        for name in names {
            let rows = self.count_rows(&name)?;
            tables.push(TableInfo { name, rows });
        }
        Ok(tables)
    }

    /// `COUNT(*)` for one table, for a page header.
    pub fn count_rows(&self, table: &str) -> Result<i64> {
        let sql = format!("SELECT COUNT(*) FROM {}", quote_ident(table));
        Ok(self.conn.query_row(&sql, [], |row| row.get(0))?)
    }

    /// Whether `table` exists as a user table.
    pub fn has_table(&self, table: &str) -> Result<bool> {
        let found: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )?;
        Ok(found > 0)
    }

    /// The table's `CREATE TABLE` text and its declared columns.
    pub fn schema(&self, table: &str) -> Result<TableSchema> {
        let sql: Option<String> = match self.conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, Option<String>>(0),
        ) {
            Ok(sql) => sql,
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => return Err(error.into()),
        };
        let mut statement = self
            .conn
            .prepare(&format!("PRAGMA table_info({})", quote_string(table)))?;
        let columns = statement
            .query_map([], |row| {
                Ok(ColumnInfo {
                    name: row.get(1)?,
                    decl_type: row.get(2)?,
                    not_null: row.get::<_, i64>(3)? != 0,
                    default: row.get(4)?,
                    primary_key: row.get::<_, i64>(5)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(TableSchema { sql, columns })
    }

    /// Column names in declaration order, for the row header.
    pub fn columns(&self, table: &str) -> Result<Vec<String>> {
        let statement = self
            .conn
            .prepare(&format!("SELECT * FROM {} LIMIT 0", quote_ident(table)))?;
        Ok(statement.column_names().into_iter().map(str::to_string).collect())
    }

    /// One page of rows, each cell rendered through [`render_value`].
    pub fn rows(&self, table: &str, limit: usize, offset: usize) -> Result<Vec<Vec<Cell>>> {
        let mut statement = self
            .conn
            .prepare(&format!("SELECT * FROM {} LIMIT ?1 OFFSET ?2", quote_ident(table)))?;
        let column_count = statement.column_count();
        let mut rows = statement.query(rusqlite::params![limit as i64, offset as i64])?;
        let mut out: Vec<Vec<Cell>> = Vec::new();
        while let Some(row) = rows.next()? {
            let mut cells = Vec::with_capacity(column_count);
            for index in 0..column_count {
                cells.push(render_value(row.get_ref(index)?));
            }
            out.push(cells);
        }
        Ok(out)
    }
}

/// `true` when the stream starts with the SQLite file header.
///
/// The magic check runs beside the extension check so a database copied to a
/// name without an extension is still recognized, and a text file called
/// `.sqlite` is not.
pub fn has_sqlite_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SQLite format 3\0")
}

/// Render one cell as one line: `NULL` for a null, a size note for a blob, and
/// the text itself (cut at [`MAX_CELL_CHARS`]) otherwise.
pub fn render_value(value: ValueRef<'_>) -> Cell {
    match value {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(number) => number.to_string(),
        ValueRef::Real(number) => number.to_string(),
        ValueRef::Text(bytes) => cut_cell(&String::from_utf8_lossy(bytes)),
        ValueRef::Blob(bytes) => format!("<blob {} bytes>", bytes.len()),
    }
}

fn cut_cell(text: &str) -> Cell {
    let single_line: String = text
        .chars()
        .map(|ch| {
            if ch == '\n' || ch == '\r' || ch == '\t' {
                ' '
            } else {
                ch
            }
        })
        .collect();
    if single_line.chars().count() <= MAX_CELL_CHARS {
        return single_line;
    }
    let cut: String = single_line.chars().take(MAX_CELL_CHARS).collect();
    format!("{cut}…")
}

/// Quote an identifier for a statement: double quotes, doubled inside.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quote a string literal: single quotes, doubled inside.
fn quote_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
#[path = "sqlite_read_tests.rs"]
mod sqlite_read_tests;
