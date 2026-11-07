//! Host-local listing index. Session locks are acquired only outside catalog locks.
use crate::blobs::new_file;
use crate::blobs::{Disk, checked_file, log_file};
use crate::cache_io::{Stamp, decode, encode, publish_derived};
use crate::catalog_row::CatalogEntry;
pub use crate::catalog_row::Summary;
use crate::checkpoint::SessionCache;
use crate::*;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;

/// Exact dead-byte thresholds; equality does not compact.
pub fn needs_compaction(dead: u64, live: u64) -> bool {
    dead > live.saturating_mul(2) || dead > 64 * 1024 * 1024
}

fn lock(paths: &SessionPaths) -> Result<File, StoreError> {
    // The store root is the anchor for every other private path: a permissive
    // root would let another user replace the catalog underneath this lock.
    // Windows keeps it private through the agent home's inherited ACL, so only
    // its type is verified there.
    if !horon_agent_platform::fs::is_private_dir(&fs::symlink_metadata(paths.root())?) {
        return Err(crate::blobs::unsafe_path(paths.root()));
    }
    let path = paths.root().join(".catalog.lock");
    let file = match new_file(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => checked_file(&path, true)?,
        Err(e) => return Err(e.into()),
    };
    crate::writer::lock_exclusive_named(&file, "catalog index")?;
    Ok(file)
}
fn sessions(paths: &SessionPaths) -> Result<Vec<SessionId>, StoreError> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(paths.root())? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        // Preserved legacy files are not session directories. file_type does not
        // follow symlinks, so those still reach the existing unsafe-path guard.
        if entry.file_type()?.is_file() {
            continue;
        }
        // This sibling contains edit-counter sidecars, not session originals.
        if name == "edit-stats" {
            crate::legacy_bundle::safe_path(&entry.path())?;
            continue;
        }
        let id = SessionId::parse(name.to_string_lossy().as_ref())?;
        let directory = paths.session_dir(&id)?;
        if directory.join("events.jsonl").exists() {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn rows(bytes: &[u8]) -> Result<BTreeMap<SessionId, CatalogEntry>, StoreError> {
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err(crate::recovery::corrupt("truncated catalog"));
    }
    let mut rows = BTreeMap::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let row: CatalogEntry = decode(line)?;
        rows.insert(row.session_id.clone(), row);
    }
    Ok(rows)
}
fn row_bytes(rows: &BTreeMap<SessionId, CatalogEntry>) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    for row in rows.values() {
        bytes.extend(encode(row)?);
        bytes.push(b'\n');
    }
    Ok(bytes)
}
fn summaries(paths: &SessionPaths) -> Result<BTreeMap<SessionId, CatalogEntry>, StoreError> {
    let probe = ReadProbe::default();
    let mut result = BTreeMap::new();
    for id in sessions(paths)? {
        let file = paths.session_dir(&id)?.join("summary.json");
        if let Ok(row) = probe.bounded(&file).and_then(|b| decode::<CatalogEntry>(&b))
            && row.session_id == id
        {
            result.insert(id, row);
        }
    }
    Ok(result)
}

/// Outcome of one session's listing-row publication.
///
/// A row and its .catalog.dirty marker are one unit: a deferred row is neither
/// written nor cleared, so the next listing or cache repair publishes it instead
/// of reporting a stale entry as current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowWrite {
    /// The row is durable in the shared index and its marker is cleared.
    Written,
    /// Another process owns the catalog; nothing was written and the row stays due.
    Deferred,
}

/// The typed ownership contention that defers one listing row.
///
/// The code stays StoreErrorCode::Busy, because retrying is the right response,
/// while the message names the catalog rather than a session writer.
pub(crate) fn deferred_row() -> StoreError {
    StoreError::busy("catalog index is owned by another writer; listing row is due")
}

pub(crate) fn publish_row(
    paths: &SessionPaths,
    row: &CatalogEntry,
    disk: &mut Disk,
    probe: &ReadProbe,
) -> Result<RowWrite, StoreError> {
    // The catalog is a shared, rebuildable listing index: another process
    // holding it delays one row, and it never invalidates this session's own
    // checkpoint. The row stays due through its dirty marker.
    let _lock = match lock(paths) {
        Ok(lock) => lock,
        Err(error) if error.code == StoreErrorCode::Busy => return Ok(RowWrite::Deferred),
        Err(error) => return Err(error),
    };
    let path = paths.root().join(".catalog.index.jsonl");
    let old = probe.read(&path);
    let (mut current, total, repair) = match old.and_then(|b| rows(&b).map(|r| (r, b.len()))) {
        Ok((rows, len)) => (
            rows,
            u64::try_from(len).map_err(|_| crate::recovery::corrupt("catalog size"))?,
            false,
        ),
        Err(_) => (summaries(paths)?, 0, true),
    };
    current.insert(row.session_id.clone(), row.clone());
    let live = row_bytes(&current)?;
    let mut line = encode(row)?;
    line.push(b'\n');
    let added = u64::try_from(line.len()).map_err(|_| crate::recovery::corrupt("row size"))?;
    let live_size = u64::try_from(live.len()).map_err(|_| crate::recovery::corrupt("catalog size"))?;
    disk.hit(IoPoint::CatalogWrite)?;
    if repair || needs_compaction(total.saturating_add(added).saturating_sub(live_size), live_size) {
        publish_derived(disk, &path, &live, probe)?;
    } else {
        let (mut file, _) = log_file(&path)?;
        file.write_all(&line)?;
        // A listing row is derived: the dirty marker beside the original says
        // it is due, and the next listing or repair republishes it. Nothing is
        // flushed here, so a crash can only cost a rebuild.
        disk.hit(IoPoint::CatalogRowStaged)?;
        probe.note_write(
            &path,
            total,
            usize::try_from(added).map_err(|_| crate::recovery::corrupt("row size"))?,
        )?;
    }
    disk.hit(IoPoint::CatalogPublished)?;
    let dirty = paths.root().join(".catalog.dirty").join(row.session_id.as_str());
    match checked_file(&dirty, false) {
        Ok(_) => {
            fs::remove_file(&dirty)?;
            disk.hit(IoPoint::DirtyRemoved)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(RowWrite::Written)
}

/// One session's listing row, so a caller can isolate an unreadable original.
///
/// [`list`] fails as a whole when any session cannot be summarized, which is the
/// right contract for a caller that must report damage. A caller that only has to
/// keep the rest of the listing (startup, a picker) resolves rows individually
/// through here and drops the sessions it cannot read.
/// # Errors
/// Propagates the same errors as [`list`] for this one session.
pub fn row(paths: &SessionPaths, id: &SessionId, probe: ReadProbe) -> Result<Summary, StoreError> {
    crate::legacy_completion::check(paths, &probe)?;
    let cache = SessionCache::new(paths.clone(), probe.clone());
    cache.catalog_entry(id)?.materialize(paths, &probe)
}

/// Lists bounded summary metadata; never parses a transcript on a clean hit.
/// # Errors
/// Dirty sessions owned by another writer return Busy, never stale success.
pub fn list(paths: &SessionPaths, probe: ReadProbe) -> Result<Vec<Summary>, StoreError> {
    crate::legacy_completion::check(paths, &probe)?;
    let snapshot = {
        let _lock = lock(paths)?;
        probe
            .read(&paths.root().join(".catalog.index.jsonl"))
            .and_then(|b| rows(&b))
    };
    let mut repair_catalog = snapshot.is_err();
    let mut snapshot = snapshot.unwrap_or_default();
    let cache = SessionCache::new(paths.clone(), probe.clone());
    let ids = sessions(paths)?;
    snapshot.retain(|id, _| ids.contains(id));
    for id in ids {
        let source = Stamp::read(&paths.events_log(&id)?)?;
        let valid = snapshot.get(&id).is_some_and(|s| s.source == source);
        let summary = paths.session_dir(&id)?.join("summary.json");
        let summary_valid = probe
            .bounded(&summary)
            .and_then(|b| decode::<CatalogEntry>(&b))
            .is_ok_and(|s| s.session_id == id && s.source == source);
        if !valid || !summary_valid || crate::cache_io::dirty_present(paths, Some(&id))? {
            repair_catalog |= !valid;
            let row = cache.catalog_entry(&id)?;
            snapshot.insert(id, row);
        }
    }
    // Final snapshot is a linearization point. Never hold this lock during repair.
    let _lock = lock(paths)?;
    if let Ok(current) = probe
        .read(&paths.root().join(".catalog.index.jsonl"))
        .and_then(|b| rows(&b))
    {
        snapshot.extend(current);
    }
    if crate::cache_io::dirty_present(paths, None)? {
        return Err(StoreError::busy("catalog changed during listing; retry"));
    }
    if repair_catalog {
        let mut disk = Disk::new(Box::new(|_| Ok(())), paths.sync_mode());
        publish_derived(
            &mut disk,
            &paths.root().join(".catalog.index.jsonl"),
            &row_bytes(&snapshot)?,
            &probe,
        )?;
    }
    drop(_lock);
    let mut result: Vec<_> = snapshot
        .into_values()
        .map(|entry| entry.materialize(paths, &probe))
        .collect::<Result<_, _>>()?;
    result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.session_id.cmp(&b.session_id)));
    Ok(result)
}
