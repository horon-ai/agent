//! Reading the pre-wiki JSON memory files into the store.
//!
//! Before the markdown wiki, memory was two JSON files — `memory/global.json`
//! and `memory/projects/<hash>.json` — each a whole `MemoryGraph`. They are now
//! a migration source only: nothing in this module rewrites or deletes one, and
//! their `.bak` files are left alone too. Imports are additive and idempotent.
//!
//! What makes a re-run safe:
//!
//! * Each source file is recorded in `memory/migration.json` with its size,
//!   SHA-256, entry and edge counts and when it was imported. A file whose bytes
//!   did not change is skipped without being parsed, so a second run costs one
//!   hash.
//! * Every page is written by the id the source carries, one write per page, so
//!   a run that stopped halfway leaves the pages it already wrote and the next
//!   run writes only the rest.
//! * When a page already exists in the wiki and is newer than the source entry,
//!   the wiki copy wins; the older source entry is counted as skipped rather
//!   than overwritten. That keeps a store that has been in use ahead of a stale
//!   JSON file.
//!
//! A file that is not a version-2 `MemoryGraph` is refused before anything is
//! written, naming the file and the version it claimed. The version trap the old
//! code had — a graph file of another version fell through to a `MemoryStore`
//! parse error — becomes one clear refusal here instead of a clobber.

use super::Store;
use super::graph_pages::page_for_entry;
use super::store::MemoryWiki;
use crate::memory_graph::MemoryGraph;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Version of the migration record file. A different version is reported and
/// the record is re-imported rather than trusted.
const RECORD_VERSION: u32 = 1;

/// Name of the migration record inside the store root.
const RECORD_FILE: &str = "migration.json";

/// What one import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ImportReport {
    /// Pages written from the source file.
    pub imported: usize,
    /// Source entries skipped because the wiki copy was newer.
    pub skipped: usize,
    /// Source entries that could not become a page, with the reason.
    pub refused: Vec<String>,
    /// Whether the file was skipped outright because its bytes were unchanged.
    pub already_imported: bool,
}

/// The recorded state of one imported source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MigrationRecord {
    /// Path of the source file, for a person reading the record.
    pub source: String,
    /// Size in bytes of the imported file.
    pub bytes: u64,
    /// SHA-256 of the imported bytes, lowercase hex.
    pub sha256: String,
    /// Entries the file held.
    pub entries: usize,
    /// Edges the file held.
    pub edges: usize,
    /// Pages actually written.
    pub imported: usize,
    /// Source entries the wiki already had newer.
    pub skipped: usize,
    /// When the import ran, RFC 3339 in UTC.
    pub imported_at: String,
    /// `horon-agent` version that ran it.
    pub tool_version: String,
}

/// The whole migration record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MigrationLog {
    /// Record format version.
    pub version: u32,
    /// One record per imported source file, keyed by the file's path.
    pub sources: BTreeMap<String, MigrationRecord>,
}

impl Default for MigrationLog {
    fn default() -> Self {
        Self {
            version: RECORD_VERSION,
            sources: BTreeMap::new(),
        }
    }
}

impl MigrationLog {
    fn load(root: &Path) -> Result<Self> {
        let path = root.join(RECORD_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        match crate::storage::read_json::<Self>(&path) {
            Ok(log) if log.version == RECORD_VERSION => Ok(log),
            Ok(log) => {
                crate::logging::info(&format!(
                    "Ignoring the memory migration record {} written by version {}",
                    path.display(),
                    log.version
                ));
                Ok(Self::default())
            }
            Err(error) => {
                crate::logging::info(&format!(
                    "Ignoring the unreadable memory migration record {}: {error}",
                    path.display()
                ));
                Ok(Self::default())
            }
        }
    }

    fn save(&self, root: &Path) -> Result<()> {
        let path = root.join(RECORD_FILE);
        if let Some(parent) = path.parent() {
            super::backend::ensure_private_dir(parent)?;
        }
        crate::storage::write_json(&path, self)
            .with_context(|| format!("cannot write the memory migration record {}", path.display()))
    }
}

/// Record one completed import in `memory/migration.json`.
pub(crate) fn record_import(root: &Path, record: MigrationRecord) -> Result<()> {
    let mut log = MigrationLog::load(root)?;
    log.sources.insert(record.source.clone(), record);
    log.save(root)
}

/// Parse one pre-wiki graph file, refusing anything that is not a version-2 graph.
pub(crate) fn parse_graph(path: &Path, bytes: &[u8]) -> Result<MemoryGraph> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).with_context(|| format!("memory file {} is not valid JSON", path.display()))?;
    let claimed = value.get("graph_version").and_then(serde_json::Value::as_u64);
    if claimed != Some(u64::from(crate::memory_graph::GRAPH_VERSION)) {
        match claimed {
            Some(version) => bail!(
                "memory file {} is graph version {version}; this build migrates only version {} and leaves the file untouched",
                path.display(),
                crate::memory_graph::GRAPH_VERSION
            ),
            None => bail!(
                "memory file {} is not a memory graph (no graph_version field); leaving it untouched",
                path.display()
            ),
        }
    }
    serde_json::from_value(value)
        .with_context(|| format!("memory file {} is not a readable memory graph", path.display()))
}

/// Import one pre-wiki source file into `project`'s namespace.
///
/// Returns what happened and the record to store, if the file was imported (an
/// unchanged file answers `None`). The source file is read and hashed only.
pub(crate) fn import_file(
    wiki: &mut MemoryWiki,
    root: &Path,
    path: &Path,
    project: Option<&str>,
) -> Result<(ImportReport, Option<MigrationRecord>)> {
    import_file_with(wiki, root, path, project, &mut |_| Ok(()))
}

/// [`import_file`] with a hook called before each page write.
///
/// The hook receives the number of pages already written for this file; a test
/// uses it to fail in the middle of an import and prove the source is untouched
/// and the next run completes.
pub(crate) fn import_file_with(
    wiki: &mut MemoryWiki,
    root: &Path,
    path: &Path,
    project: Option<&str>,
    before_write: &mut dyn FnMut(usize) -> Result<()>,
) -> Result<(ImportReport, Option<MigrationRecord>)> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((ImportReport::default(), None));
        }
        Err(error) => {
            return Err(anyhow::anyhow!(error).context(format!("cannot read the memory file {}", path.display())));
        }
    };
    let digest = hex::encode(Sha256::digest(&bytes));
    let key = path.display().to_string();
    if let Some(record) = MigrationLog::load(root)?.sources.get(&key).cloned()
        && record.sha256 == digest
        && record.bytes == bytes.len() as u64
    {
        return Ok((
            ImportReport {
                already_imported: true,
                ..ImportReport::default()
            },
            None,
        ));
    }

    let graph = parse_graph(path, &bytes)?;
    let mut report = ImportReport::default();
    let mut ids: Vec<String> = graph.memories.keys().cloned().collect();
    ids.sort();
    for id in &ids {
        let Some(entry) = graph.memories.get(id) else {
            continue;
        };
        let page = match page_for_entry(entry, project, &graph) {
            Ok(page) => page,
            Err(error) => {
                report.refused.push(format!("{id}: {error}"));
                continue;
            }
        };
        match wiki.page(&page.id)? {
            Some(existing) if existing.updated_at >= page.updated_at => {
                report.skipped += 1;
                continue;
            }
            _ => {}
        }
        before_write(report.imported)?;
        match wiki.write_page(&page) {
            Ok(_) => report.imported += 1,
            Err(error) => report.refused.push(format!("{id}: {error}")),
        }
    }

    let record = MigrationRecord {
        source: key,
        bytes: bytes.len() as u64,
        sha256: digest,
        entries: graph.memories.len(),
        edges: graph.edge_count(),
        imported: report.imported,
        skipped: report.skipped,
        imported_at: horon_agent_time::Timestamp::now().to_string(),
        tool_version: horon_agent_build_meta::version().to_string(),
    };
    Ok((report, Some(record)))
}

/// One pre-wiki source file and the namespace its pages belong to.
#[derive(Debug, Clone)]
pub(crate) struct JsonSource {
    pub(crate) path: PathBuf,
    pub(crate) project: Option<String>,
}

/// Every pre-wiki source file under `root`, in a stable order.
///
/// Global comes first, then project files sorted by name. A project file's name
/// is a `DefaultHasher` value that cannot be inverted, so its pages are imported
/// under `pre-wiki-<hash>`; the first time that project is opened they move to its
/// stable key, a page-id-preserving rewrite.
pub(crate) fn json_sources(root: &Path) -> Vec<JsonSource> {
    let mut sources = Vec::new();
    for name in ["global.json", "test/test_global.json"] {
        let path = root.join(name);
        if path.is_file() {
            sources.push(JsonSource { path, project: None });
        }
    }
    for directory in ["projects", "test"] {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(root.join(directory)) {
            Ok(entries) => entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
                .filter(|path| !path_is_backup(path))
                .filter(|path| is_project_source(path))
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort();
        for path in files {
            let project = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(super::project_key::pre_wiki_namespace);
            sources.push(JsonSource { path, project });
        }
    }
    sources
}

/// `storage::write_json` rotates the previous version to `<name>.json.bak`; that
/// is a backup of a file next to it, not a source of its own.
fn path_is_backup(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".json.bak"))
}

/// Whether a JSON file under `projects/` or `test/` is a memory store rather than
/// something else beside it.
fn is_project_source(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // A test-mode memory file and the hash-named project files are the two
    // shapes the old store wrote. `test_notes.json`, a consolidation state and
    // the migration record are not memory graphs.
    if name.starts_with('.') || name == RECORD_FILE {
        return false;
    }
    !name.contains("consolidation") && !name.contains("notes")
}

/// The summary `memory migrate` prints.
#[derive(Debug, Clone, Default)]
pub struct MigrateReport {
    /// Source files found.
    pub files: usize,
    /// Source files already imported with the same bytes.
    pub unchanged: usize,
    /// Pages written across every file.
    pub imported: usize,
    /// Source entries the wiki already had newer.
    pub skipped: usize,
    /// Entries that could not be imported, each with its reason.
    pub refused: Vec<String>,
}

/// Import every pre-wiki source file found under the store root.
///
/// With `dry_run` every file is read, parsed and counted, and nothing is
/// written: no page, no record, not even the store directory.
pub(crate) fn migrate_all(store: &Store, dry_run: bool) -> Result<MigrateReport> {
    let root = store.root();
    let sources = json_sources(root);
    let mut report = MigrateReport {
        files: sources.len(),
        ..MigrateReport::default()
    };
    if dry_run {
        for source in &sources {
            let bytes = match std::fs::read(&source.path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(anyhow::anyhow!(error)
                        .context(format!("cannot read the memory file {}", source.path.display())));
                }
            };
            let graph = parse_graph(&source.path, &bytes)?;
            report.imported += graph.memories.len();
        }
        return Ok(report);
    }
    let (_lock, mut wiki) = store.migration_session()?;
    for source in &sources {
        let (result, record) = import_file(&mut wiki, root, &source.path, source.project.as_deref())?;
        report.imported += result.imported;
        report.skipped += result.skipped;
        report.refused.extend(result.refused);
        if result.already_imported {
            report.unchanged += 1;
        }
        if let Some(record) = record {
            record_import(root, record)?;
        }
    }
    Ok(report)
}

/// Move the pages one project's pre-wiki file imported under `pre-wiki-<hash>`
/// into that project's stable key.
///
/// A page keeps its id, so the rewrite moves the file and its index row instead
/// of creating a second copy.
pub(crate) fn adopt_namespace(wiki: &mut MemoryWiki, from: &str, to: &str) -> Result<usize> {
    if from == to {
        return Ok(0);
    }
    let pages = wiki.scope_pages(Some(from))?;
    let mut moved = 0;
    for mut page in pages {
        page.project = Some(to.to_string());
        wiki.write_page(&page)?;
        moved += 1;
    }
    Ok(moved)
}
