//! Host-local listing index. Session locks are acquired only outside catalog locks.
use crate::blobs::new_file;
use crate::blobs::{Disk, checked_file, log_file};
use crate::cache_io::{Stamp, decode, encode, publish_derived};
use crate::catalog_row::CatalogEntry;
pub use crate::catalog_row::{SessionListing, SkippedEntry, Summary};
use crate::checkpoint::SessionCache;
use crate::*;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;

/// Exact dead-byte thresholds; equality does not compact.
pub fn needs_compaction(dead: u64, live: u64) -> bool {
    dead > live.saturating_mul(2) || dead > 64 * 1024 * 1024
}

struct CatalogLock(File);

impl Drop for CatalogLock {
    fn drop(&mut self) {
        // A concurrently spawned child can retain the same open file description
        // until exec. Closing only our handle would leave the listing locked.
        if let Err(error) = self.0.unlock() {
            eprintln!("could not release the session catalog lock: {error}");
        }
    }
}

fn lock(paths: &SessionPaths) -> Result<CatalogLock, StoreError> {
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
    Ok(CatalogLock(file))
}
/// The session directories one listing can read, and the entries it refused.
struct Scan {
    ids: Vec<SessionId>,
    skipped: Vec<SkippedEntry>,
}

/// Whether a refusal is about this one entry rather than about the store.
///
/// A path the private-path check refuses says nothing about the other sessions:
/// an older build left it permissive, and hiding every session behind it is
/// worse than leaving it out. A malformed identity or an unreadable store stays
/// fatal, because neither is confined to the entry that raised it.
fn entry_local(error: &StoreError) -> bool {
    error.code == StoreErrorCode::UnsafePath
}

fn sessions(paths: &SessionPaths) -> Result<Scan, StoreError> {
    let mut ids = Vec::new();
    let mut skipped = Vec::new();
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
            match crate::legacy_bundle::safe_path(&entry.path()) {
                Ok(()) => continue,
                Err(error) if entry_local(&error) => {
                    skipped.push(SkippedEntry {
                        path: entry.path(),
                        reason: error.message,
                    });
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        let id = SessionId::parse(name.to_string_lossy().as_ref())?;
        let directory = match paths.session_dir(&id) {
            Ok(directory) => directory,
            Err(error) if entry_local(&error) => {
                skipped.push(SkippedEntry {
                    path: entry.path(),
                    reason: error.message,
                });
                continue;
            }
            Err(error) => return Err(error),
        };
        if directory.join("events.jsonl").exists() {
            ids.push(id);
        }
    }
    Ok(Scan { ids, skipped })
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
    for id in sessions(paths)?.ids {
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
/// [`list`] skips a directory the private-path check refuses and fails as a whole
/// only when a listed session cannot be summarized, which is the right contract
/// for a caller that must report damage. A caller that has to resolve one session
/// on its own goes through here.
/// # Errors
/// Propagates the same errors as [`list`] for this one session.
pub fn row(paths: &SessionPaths, id: &SessionId, probe: ReadProbe) -> Result<Summary, StoreError> {
    crate::legacy_completion::check(paths, &probe)?;
    let cache = SessionCache::new(paths.clone(), probe.clone());
    cache.catalog_entry(id)?.materialize(paths, &probe)
}

/// Lists bounded summary metadata and reports the entries it refused.
///
/// One directory the private-path check refuses must not hide every other
/// session: the entry is skipped, its reason names the path and mode, and the
/// rest of the listing is returned as usual. [`list`] drops that report; a caller
/// with a log calls this and logs [`SkippedEntry::warning`].
/// # Errors
/// Dirty sessions owned by another writer return Busy, never stale success.
pub fn list_with_skipped(paths: &SessionPaths, probe: ReadProbe) -> Result<SessionListing, StoreError> {
    crate::legacy_completion::check(paths, &probe)?;
    let snapshot = {
        let _lock = lock(paths).map_err(|error| catalog_error("read the catalog lock", error))?;
        probe
            .read(&paths.root().join(".catalog.index.jsonl"))
            .and_then(|b| rows(&b))
    };
    let mut repair_catalog = snapshot.is_err();
    let mut snapshot = snapshot.unwrap_or_default();
    let cache = SessionCache::new(paths.clone(), probe.clone());
    let scan = sessions(paths).map_err(|error| catalog_error("scan session directories", error))?;
    let ids = scan.ids;
    let live: std::collections::HashSet<SessionId> = ids.iter().cloned().collect();
    snapshot.retain(|id, _| live.contains(id));
    for id in &ids {
        let source = Stamp::read(&paths.events_log(id)?)?;
        let valid = snapshot.get(id).is_some_and(|s| s.source == source);
        let summary = paths.session_dir(id)?.join("summary.json");
        let summary_valid = probe
            .bounded(&summary)
            .and_then(|b| decode::<CatalogEntry>(&b))
            .is_ok_and(|s| s.session_id == *id && s.source == source);
        if !valid || !summary_valid || crate::cache_io::dirty_present(paths, Some(id))? {
            repair_catalog |= !valid;
            let row = cache
                .catalog_entry(id)
                .map_err(|error| catalog_error("read session metadata", error))?;
            snapshot.insert(id.clone(), row);
        }
    }
    // Final snapshot is a linearization point. Never hold this lock during repair.
    let _lock = lock(paths).map_err(|error| catalog_error("take the final catalog lock", error))?;
    if let Ok(current) = probe
        .read(&paths.root().join(".catalog.index.jsonl"))
        .and_then(|b| rows(&b))
    {
        // Only rows whose original still exists may rejoin the snapshot. The
        // index is derived and may still carry a row for a session that was
        // removed since it was written; merging those back would list a
        // deleted session in every later listing, because nothing rewrites the
        // index until some other session changes.
        snapshot.extend(current.into_iter().filter(|(id, _)| live.contains(id)));
    }
    // A marker means that session's row is due, so this listing is not
    // authoritative for it. A marker left by a session this listing does not
    // list — one it skipped, or one removed since — is not contention: reading
    // it as contention would let one stale entry block the whole store again.
    let mut due = false;
    for id in &ids {
        if crate::cache_io::dirty_present(paths, Some(id))? {
            due = true;
            break;
        }
    }
    if due {
        return Err(StoreError::busy("catalog changed during listing; retry"));
    }
    if repair_catalog {
        let mut disk = Disk::new(Box::new(|_| Ok(())), paths.sync_mode());
        publish_derived(
            &mut disk,
            &paths.root().join(".catalog.index.jsonl"),
            &row_bytes(&snapshot)?,
            &probe,
        )
        .map_err(|error| catalog_error("publish the derived session catalog", error))?;
    }
    drop(_lock);
    let mut result: Vec<_> = snapshot
        .into_values()
        .map(|entry| entry.materialize(paths, &probe))
        .collect::<Result<_, _>>()
        .map_err(|error| catalog_error("read catalog entries", error))?;
    result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.session_id.cmp(&b.session_id)));
    Ok(SessionListing {
        rows: result,
        skipped: scan.skipped,
    })
}

fn catalog_error(operation: &str, mut error: StoreError) -> StoreError {
    if error.code == crate::StoreErrorCode::Io {
        error.message = format!("could not {operation}: {}", error.message);
    }
    error
}

/// Lists bounded summary metadata; never parses a transcript on a clean hit.
///
/// A caller that has to report what the listing refused uses
/// [`list_with_skipped`]; this form keeps only the rows.
/// # Errors
/// Dirty sessions owned by another writer return Busy, never stale success.
pub fn list(paths: &SessionPaths, probe: ReadProbe) -> Result<Vec<Summary>, StoreError> {
    Ok(list_with_skipped(paths, probe)?.rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_catalog_owner_releases_the_listing_even_with_a_duplicate_handle() -> Result<(), StoreError> {
        let root = std::env::temp_dir()
            .canonicalize()?
            .join(format!("catalog-owner-{}", uuid::Uuid::new_v4()));
        horon_agent_platform::fs::create_private_dir(&root)?;
        let paths = SessionPaths::new(&root);
        let owner = lock(&paths)?;
        let duplicate = owner.0.try_clone()?;
        let held = list(&paths, ReadProbe::default());
        drop(owner);
        let released = list(&paths, ReadProbe::default());
        drop(duplicate);
        fs::remove_dir_all(root)?;
        assert_eq!(held.unwrap_err().code, StoreErrorCode::Busy);
        assert!(released?.is_empty());
        Ok(())
    }
}
