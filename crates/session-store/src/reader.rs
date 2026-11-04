//! The single enumeration and raw-read surface for session originals.
//!
//! Every consumer that used to glob `sessions/*.json` or `*.journal.jsonl`
//! discovers sessions here instead, so the canonical layout
//! (`sessions/<id>/events.jsonl`) and the preserved legacy layout
//! (`sessions/<id>.json` plus `<id>.journal.jsonl`) are found by one rule.
//! Reading a legacy original for replay stays in [`crate::legacy`]; this module
//! only decides which layout a session has and returns its raw bytes.
use crate::*;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

/// Reserved child of the sessions root that holds edit counters, not sessions.
const EDIT_STATS_DIR: &str = "edit-stats";

/// Which on-disk layout holds a session's original history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceLayout {
    /// `sessions/<id>/events.jsonl`
    Canonical,
    /// `sessions/<id>.json` plus an optional `sessions/<id>.journal.jsonl`
    Legacy,
}

/// One discoverable session original.
#[derive(Clone, Debug)]
pub struct SessionSource {
    pub session_id: SessionId,
    pub layout: SourceLayout,
    /// Canonical log, or the legacy snapshot.
    pub snapshot: PathBuf,
    /// Legacy journal tail, present only when the legacy layout has one.
    pub journal: Option<PathBuf>,
    pub modified: SystemTime,
    pub size: u64,
}

impl SessionSource {
    /// Bytes covering the whole original history in this session's own layout.
    ///
    /// # Errors
    /// Propagates an unreadable snapshot or journal.
    pub fn raw(&self) -> Result<Vec<u8>, StoreError> {
        let mut raw = fs::read(&self.snapshot)?;
        if let Some(journal) = &self.journal {
            raw.push(b'\n');
            raw.extend_from_slice(&fs::read(journal)?);
        }
        Ok(raw)
    }
}

fn modified(path: &std::path::Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    metadata
        .is_file()
        .then(|| (metadata.modified().ok(), metadata.len()))
        .map(|(modified, len)| (modified.unwrap_or(SystemTime::UNIX_EPOCH), len))
}

fn canonical_source(paths: &SessionPaths, id: SessionId) -> Result<Option<SessionSource>, StoreError> {
    let log = paths.events_log(&id)?;
    let Some((modified, size)) = modified(&log) else {
        return Ok(None);
    };
    Ok(Some(SessionSource {
        session_id: id,
        layout: SourceLayout::Canonical,
        snapshot: log,
        journal: None,
        modified,
        size,
    }))
}

fn legacy_source(root: &std::path::Path, id: SessionId) -> Result<Option<SessionSource>, StoreError> {
    let snapshot = root.join(format!("{}.json", id.as_str()));
    let Some((snapshot_modified, snapshot_size)) = modified(&snapshot) else {
        return Ok(None);
    };
    let journal = root.join(format!("{}.journal.jsonl", id.as_str()));
    let tail = modified(&journal);
    Ok(Some(SessionSource {
        session_id: id,
        layout: SourceLayout::Legacy,
        snapshot,
        journal: tail.is_some().then_some(journal),
        modified: tail.map_or(snapshot_modified, |(modified, _)| modified.max(snapshot_modified)),
        size: snapshot_size + tail.map_or(0, |(_, len)| len + 1),
    }))
}

/// Resolve one session id to its preferred layout, canonical first.
///
/// # Errors
/// Propagates an unsafe session id or an unreadable root entry.
pub fn open(paths: &SessionPaths, id: &SessionId) -> Result<Option<SessionSource>, StoreError> {
    if let Some(source) = canonical_source(paths, id.clone())? {
        return Ok(Some(source));
    }
    legacy_source(paths.root(), id.clone())
}

/// The layout a session id currently uses, without reading its history.
///
/// # Errors
/// Propagates an unsafe session id.
pub fn layout(paths: &SessionPaths, id: &SessionId) -> Result<Option<SourceLayout>, StoreError> {
    Ok(open(paths, id)?.map(|source| source.layout))
}

/// Enumerate every session original under the root, canonical layout first.
///
/// A preserved legacy snapshot beside a canonical log is not a second session:
/// the log wins, and the legacy original stays untouched on disk for rollback.
///
/// # Errors
/// Propagates an unreadable sessions root or an unsafe entry name.
pub fn sources(paths: &SessionPaths) -> Result<Vec<SessionSource>, StoreError> {
    let root = paths.root();
    if !root.try_exists()? {
        return Ok(Vec::new());
    }
    let mut found: std::collections::BTreeMap<SessionId, SessionSource> = std::collections::BTreeMap::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if name == EDIT_STATS_DIR {
                continue;
            }
            let Ok(id) = SessionId::parse(name.as_ref()) else {
                continue;
            };
            if let Some(source) = canonical_source(paths, id)? {
                found.insert(source.session_id.clone(), source);
            }
        } else if file_type.is_file() {
            let Some(stem) = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str().map(ToOwned::to_owned))
            else {
                continue;
            };
            if entry.path().extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(id) = SessionId::parse(&stem) else {
                continue;
            };
            if found.contains_key(&id) {
                continue;
            }
            if let Some(source) = legacy_source(root, id)? {
                found.insert(source.session_id.clone(), source);
            }
        }
    }
    Ok(found.into_values().collect())
}
