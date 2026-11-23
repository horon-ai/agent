//! The derived SQLite index: opening it, its schema, and corruption recovery.
//!
//! The index is derived data. The markdown files are the source of truth, so
//! every failure mode here rebuilds from the files instead of failing the store:
//! a missing file, a foreign schema version, a file that is not a database, and
//! a corrupt b-tree all reset the index and let the caller reindex. The one
//! failure that is not reset is a lock held by another writer: deleting a file
//! another connection is using would lose their work, so it surfaces instead.

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, Error as SqliteError, ErrorCode};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Schema version written to `PRAGMA user_version`. Bump it whenever the
/// tables below change shape; an index with another version is rebuilt.
pub const SCHEMA_VERSION: i64 = 1;

/// File name of the index inside the memory directory.
pub const INDEX_FILE_NAME: &str = "index.sqlite";

/// How long a write waits for another writer's transaction.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The three full-text tables plus the page tables.
///
/// `pages_fts` is the token index (`unicode61`), `pages_bigram` the substring
/// index (`trigram`, used for Korean and CJK text without spaces), and
/// `pages_grams` the overlapping-bigram expansion that lets an unspaced query
/// match words that are separated in the page text. `files` remembers the
/// modification stamp of every indexed page file so a rescan can skip unchanged
/// files.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pages (
  rowid INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  path TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL,
  category TEXT NOT NULL,
  project TEXT,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  tags TEXT NOT NULL,
  entities TEXT NOT NULL,
  updated_ms INTEGER NOT NULL,
  created_ms INTEGER NOT NULL,
  active INTEGER NOT NULL,
  valid_from TEXT,
  valid_to TEXT,
  source TEXT
);
CREATE TABLE IF NOT EXISTS tags (
  page_id TEXT NOT NULL,
  tag TEXT NOT NULL,
  PRIMARY KEY (page_id, tag)
);
CREATE INDEX IF NOT EXISTS tags_tag ON tags(tag);
CREATE TABLE IF NOT EXISTS links (
  src_id TEXT NOT NULL,
  dst_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  weight REAL NOT NULL,
  PRIMARY KEY (src_id, dst_id, kind)
);
CREATE INDEX IF NOT EXISTS links_dst ON links(dst_id);
CREATE TABLE IF NOT EXISTS entities (
  page_id TEXT NOT NULL,
  name TEXT NOT NULL,
  name_norm TEXT NOT NULL,
  PRIMARY KEY (page_id, name)
);
CREATE INDEX IF NOT EXISTS entities_norm ON entities(name_norm);
CREATE TABLE IF NOT EXISTS versions (
  page_id TEXT NOT NULL,
  rev INTEGER NOT NULL,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (page_id, rev)
);
CREATE TABLE IF NOT EXISTS files (
  path TEXT PRIMARY KEY,
  page_id TEXT,
  mtime_ns INTEGER NOT NULL,
  size INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
  id UNINDEXED, title, body, tags, entities,
  tokenize='unicode61 remove_diacritics 2'
);
CREATE VIRTUAL TABLE IF NOT EXISTS pages_bigram USING fts5(
  id UNINDEXED, title, body,
  tokenize='trigram'
);
CREATE VIRTUAL TABLE IF NOT EXISTS pages_grams USING fts5(
  id UNINDEXED, grams,
  tokenize='unicode61 remove_diacritics 2'
);
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

/// The full-text tables whose deleted terms must be overwritten.
const SECURE_DELETE_TABLES: [&str; 3] = ["pages_fts", "pages_bigram", "pages_grams"];

/// An open index connection.
pub struct Index {
    conn: Connection,
    path: PathBuf,
    /// Whether this call created the index from nothing, so the caller must
    /// index every page file.
    fresh: bool,
}

impl Index {
    /// Open the index at `path`, creating or resetting it when needed.
    ///
    /// `fresh` in the result is true when the file was absent, carried another
    /// schema version, or was corrupt: the caller then indexes every page file
    /// instead of trusting the `files` table.
    pub fn open(path: &Path) -> Result<Self> {
        match Self::try_open(path) {
            Ok(index) => Ok(index),
            Err(error) if is_corruption(&error) => {
                reset_index_files(path)?;
                Self::try_open(path)
            }
            Err(error) => Err(error),
        }
    }

    fn try_open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create the memory directory {}", parent.display()))?;
            horon_agent_platform::fs::set_dir_permissions_owner_only(parent)
                .with_context(|| format!("cannot restrict the memory directory {}", parent.display()))?;
        }
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("cannot open the memory index {}", path.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == SCHEMA_VERSION {
            configure(&conn)?;
            return Ok(Self {
                conn,
                path: path.to_path_buf(),
                fresh: false,
            });
        }
        // Another version, a database created by something else, or a file that
        // is not a database: start over. The markdown files are the truth.
        drop(conn);
        reset_index_files(path)?;
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("cannot create the memory index {}", path.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        configure(&conn)?;
        conn.execute_batch(SCHEMA)
            .with_context(|| format!("cannot create the memory index schema in {}", path.display()))?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        set_secure_delete(&conn).with_context(|| format!("cannot enable FTS5 secure delete in {}", path.display()))?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            fresh: true,
        })
    }

    /// The index file this handle opened.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// fsync the database file, after a checkpoint closed the log.
    ///
    /// A quiesce calls this so the bytes reach the device before a snapshot is
    /// taken. SQLite's own `synchronous=NORMAL` makes WAL commits durable at
    /// checkpoint time, which is why the checkpoint runs first.
    pub fn sync_to_disk(&self) -> Result<()> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .open(&self.path)
            .with_context(|| format!("cannot reopen the index {}", self.path.display()))?;
        file.sync_all()
            .with_context(|| format!("cannot flush the index {}", self.path.display()))?;
        Ok(())
    }

    /// The connection, for the modules that query it.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// A mutable connection, for a transaction.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Whether this open created an empty index that must be filled from the
    /// markdown files.
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// Mark the index as filled, so a later caller does not reindex again.
    pub fn clear_fresh(&mut self) {
        self.fresh = false;
    }

    /// Re-enable FTS5 secure delete and checkpoint the write-ahead log, so
    /// deleted page text is not left in the database or its log.
    pub fn purge_deleted(&self) -> Result<()> {
        set_secure_delete(&self.conn)?;
        self.conn.execute_batch(
            "INSERT INTO pages_fts(pages_fts) VALUES('optimize');
            INSERT INTO pages_bigram(pages_bigram) VALUES('optimize');
            INSERT INTO pages_grams(pages_grams) VALUES('optimize');",
        )?;
        self.conn.execute_batch("PRAGMA secure_delete=ON;")?;
        self.conn.execute_batch("VACUUM;")?;
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .with_context(|| format!("cannot truncate the index log for {}", self.path.display()))?;
        Ok(())
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA secure_delete=ON;
         PRAGMA trusted_schema=OFF;",
    )?;
    Ok(())
}

fn set_secure_delete(conn: &Connection) -> Result<()> {
    for table in SECURE_DELETE_TABLES {
        conn.execute(
            &format!("INSERT INTO {table}({table}, rank) VALUES('secure-delete', 1)"),
            [],
        )
        .with_context(|| format!("FTS5 table {table} does not accept the secure-delete option"))?;
    }
    Ok(())
}

/// Whether `error` says the file is not a usable database.
fn is_corruption(error: &anyhow::Error) -> bool {
    let Some(sqlite) = error.downcast_ref::<SqliteError>() else {
        return false;
    };
    match sqlite {
        SqliteError::SqliteFailure(code, _) => {
            matches!(code.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
        }
        _ => false,
    }
}

/// Remove the index and its write-ahead log. Used only when the file is not a
/// usable index; a locked file surfaces instead.
fn reset_index_files(path: &Path) -> Result<()> {
    for candidate in sidecar_paths(path) {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow!(error).context(format!(
                    "cannot reset the memory index {} ({})",
                    path.display(),
                    candidate.display()
                )));
            }
        }
    }
    Ok(())
}

/// The index file plus the two files SQLite keeps next to a WAL database.
fn sidecar_paths(path: &Path) -> [PathBuf; 3] {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    [path.to_path_buf(), PathBuf::from(wal), PathBuf::from(shm)]
}
