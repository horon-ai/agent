//! The write fence and the quiesce step a volume snapshot needs.
//!
//! A Fly Volume snapshot is at best crash-consistent: the platform documents no
//! filesystem freeze, and the markdown pages and the SQLite index are two
//! separate durability domains. Restoring a snapshot taken under load costs the
//! page tail and leaves an index the next open repairs, which is recoverable but
//! not a clean backup. This module is the clean version:
//!
//! 1. [`quiesce`] takes the store's writer lock, which waits out an in-flight
//!    write instead of splitting one;
//! 2. it checkpoints the index write-ahead log with `TRUNCATE`, so every
//!    committed page row is in `index.sqlite` and no `-wal` file holds text that
//!    a later `VACUUM` cannot reach;
//! 3. it fsyncs the index and every page file, so the kernel has the bytes;
//! 4. it writes [`FENCE_FILE`] atomically, which makes every store write refuse
//!    until [`resume`] removes it.
//!
//! The fence is a file rather than process memory on purpose: the process that
//! quiesced a volume may not be the process that serves it afterwards, and a
//! restart must not silently lift a fence the control plane is relying on.
//!
//! **The data-loss window when this is skipped.** A snapshot taken while writes
//! are in flight can hold a page file from before its index row, or the reverse,
//! and can hold a `-wal` the restored reader never replays. Nothing is
//! unrecoverable (the markdown is the truth and the index rebuilds), but the
//! snapshot may miss the last seconds of writes and may need a full reindex on
//! first open. A snapshot taken after `quiesce` misses nothing that the store
//! acknowledged before the call returned.
//!
//! Quiescing is idempotent: a second call re-checkpoints and rewrites the fence
//! with a newer revision and timestamp, and reports the same `revision` only
//! when no write happened in between.

use super::{VolumeLayout, now_rfc3339, write_private_file};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The fence file, inside the store root so it travels with the volume.
pub const FENCE_FILE: &str = ".write-fence.json";

/// The record a quiesced volume carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceRecord {
    /// Always `1`; a future shape bumps it and an older reader refuses.
    pub version: u32,
    /// When the fence was written, RFC 3339.
    pub fenced_at: String,
    /// The store revision the checkpoint ended at.
    pub revision: u64,
    /// How many page files were fsynced.
    pub page_files: usize,
    /// The operator-facing token that names this checkpoint: `<revision>@<fenced_at>`.
    pub token: String,
}

/// What one quiesce did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuiesceReport {
    /// The fence now in force.
    pub fence: FenceRecord,
    /// Whether a fence was already in force when the call started.
    pub already_fenced: bool,
}

/// The fence version this build writes.
const FENCE_VERSION: u32 = 1;

impl VolumeLayout {
    /// The fence file of this volume.
    pub fn fence_path(&self) -> std::path::PathBuf {
        self.store_root().join(FENCE_FILE)
    }
}

/// Read the fence in force for this volume, if any.
pub fn read_fence(layout: &VolumeLayout) -> Result<Option<FenceRecord>> {
    read_fence_at(&layout.store_root())
}

/// Read the fence at a store root, if any.
pub fn read_fence_at(store_root: &Path) -> Result<Option<FenceRecord>> {
    let path = store_root.join(FENCE_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(anyhow!(error).context(format!("cannot read the fence {}", path.display()))),
    };
    let record: FenceRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("the fence {} is not a valid fence record", path.display()))?;
    if record.version != FENCE_VERSION {
        return Err(anyhow!(
            "the fence {} carries version {} and this build writes {FENCE_VERSION}",
            path.display(),
            record.version
        ));
    }
    Ok(Some(record))
}

/// Fence a volume that has no store yet, so deletion can run on it.
///
/// A volume that never served memory still has to be deletable, so the deletion
/// job fences it without a checkpoint. Returns the fence written.
pub fn fence_without_store(layout: &VolumeLayout) -> Result<FenceRecord> {
    let store_root = layout.store_root();
    std::fs::create_dir_all(&store_root)
        .with_context(|| format!("cannot create the store root {}", store_root.display()))?;
    horon_agent_platform::fs::set_dir_permissions_owner_only(&store_root)
        .with_context(|| format!("cannot restrict the store root {}", store_root.display()))?;
    let fenced_at = now_rfc3339();
    let fence = FenceRecord {
        version: FENCE_VERSION,
        token: format!("0@{fenced_at}"),
        fenced_at,
        revision: 0,
        page_files: 0,
    };
    write_private_file(
        &store_root.join(FENCE_FILE),
        &serde_json::to_vec_pretty(&fence).context("cannot encode the fence record")?,
    )?;
    Ok(fence)
}

/// Quiesce the volume: checkpoint, fsync, and fence further writes.
pub fn quiesce(layout: &VolumeLayout) -> Result<QuiesceReport> {
    let store_root = layout.store_root();
    if !store_root.is_dir() {
        return Err(anyhow!(
            "the memory store {} does not exist yet; there is nothing to quiesce",
            store_root.display()
        ));
    }
    let already_fenced = read_fence_at(&store_root)?.is_some();
    // One writer at a time: an in-flight page write finishes before the
    // checkpoint, so the fence never splits a file from its index row.
    let lock = crate::memory::write_lock(&store_root)?;
    let revision = checkpoint_store(&store_root)?;
    let page_files = sync_page_files(&store_root.join("wiki"))?;
    sync_index(&store_root)?;
    let fenced_at = now_rfc3339();
    let fence = FenceRecord {
        version: FENCE_VERSION,
        token: format!("{revision}@{fenced_at}"),
        fenced_at,
        revision,
        page_files,
    };
    write_private_file(
        &store_root.join(FENCE_FILE),
        &serde_json::to_vec_pretty(&fence).context("cannot encode the fence record")?,
    )?;
    drop(lock);
    crate::logging::info(&format!(
        "memory volume quiesced at revision {revision} ({} page file(s))",
        fence.page_files
    ));
    Ok(QuiesceReport { fence, already_fenced })
}

/// Lift the fence so the store accepts writes again.
///
/// Returns whether a fence was in force. Idempotent: lifting a volume that is
/// not fenced succeeds and reports `false`.
pub fn resume(layout: &VolumeLayout) -> Result<bool> {
    let path = layout.fence_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {
            crate::logging::info("memory volume fence lifted");
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(anyhow!(error).context(format!("cannot lift the fence {}", path.display()))),
    }
}

/// Refuse a write on a fenced store, naming the checkpoint to resume from.
///
/// Every store mutation calls this before it touches a file, so a quiesced
/// volume is read-only for the whole process, not just for the command that
/// fenced it.
pub(crate) fn ensure_writable(store_root: &Path) -> Result<()> {
    match read_fence_at(store_root)? {
        Some(fence) => Err(anyhow!(
            "the memory volume is quiesced for a snapshot (checkpoint {}, written {}); \
             run `horon volume resume` before writing",
            fence.token,
            fence.fenced_at
        )),
        None => Ok(()),
    }
}

/// Whether the store at `store_root` is fenced.
pub fn is_fenced(store_root: &Path) -> Result<bool> {
    Ok(read_fence_at(store_root)?.is_some())
}

/// Checkpoint the index log and return the store revision it ended at.
fn checkpoint_store(store_root: &Path) -> Result<u64> {
    let index_path = store_root.join(crate::memory::store::INDEX_FILE_NAME);
    if !index_path.exists() {
        // No index yet means no rows to flush; the revision is what a fresh
        // store reports.
        return Ok(0);
    }
    let index = crate::memory::store::IndexHandle::open_existing(&index_path)?;
    index.checkpoint_truncate()?;
    index.sync()?;
    index.revision()
}

/// fsync the index file itself, after the checkpoint closed the log.
fn sync_index(store_root: &Path) -> Result<()> {
    let index_path = store_root.join(crate::memory::store::INDEX_FILE_NAME);
    if index_path.exists() {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .open(&index_path)
            .with_context(|| format!("cannot reopen the index {}", index_path.display()))?;
        file.sync_all()
            .with_context(|| format!("cannot flush the index {}", index_path.display()))?;
    }
    Ok(())
}

/// fsync every page file under the wiki root, returning how many were flushed.
fn sync_page_files(wiki_root: &Path) -> Result<usize> {
    let files = crate::memory::store::scan_wiki(wiki_root)?;
    let mut flushed = 0;
    for file in files {
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .open(&file.absolute)
            .with_context(|| format!("cannot reopen the page file {}", file.relative))?;
        handle
            .sync_all()
            .with_context(|| format!("cannot flush the page file {}", file.relative))?;
        flushed += 1;
    }
    Ok(flushed)
}
