//! Inventory and explicit removal of the derived cache files of inactive sessions.
//!
//! Only files the store can rebuild from `events.jsonl` are touched; originals,
//! blobs, the runtime journal and the shared catalog index are never read here
//! for removal. A session is inactive when its writer lock can be taken without
//! waiting, and the lock is held while its files are measured and removed, so a
//! writer cannot start in between. Removal is explicit: without `apply` nothing
//! is written.

use std::fs;

use crate::blobs::checked_file;
use crate::reader::{SourceLayout, sources};
use crate::writer::lock_exclusive;
use crate::{SessionId, SessionPaths, StoreError, StoreErrorCode};

/// The per-session derived files (see `packages/docs-site/reference/protocol/session-store.mdx`, "Layout").
const DERIVED_FILES: [&str; 4] = [
    "checkpoint.json",
    "sparse-index.jsonl",
    "summary.json",
    "facade-cache.json",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheFile {
    pub session_id: SessionId,
    pub name: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct ReclaimReport {
    /// Files an `apply` run would remove, or removed when `apply` was set.
    pub files: Vec<CacheFile>,
    /// Sessions skipped because a writer owns them.
    pub active: Vec<SessionId>,
}

impl ReclaimReport {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }
}

/// List the derived files of every inactive canonical session, and remove them
/// when `apply` is set.
///
/// # Errors
/// An unreadable store root or session directory. A file that is not a plain
/// regular file (a symbolic link, say) is skipped, never followed.
pub fn reclaim_derived(paths: &SessionPaths, apply: bool) -> Result<ReclaimReport, StoreError> {
    let mut report = ReclaimReport::default();
    for source in sources(paths)? {
        if source.layout != SourceLayout::Canonical {
            continue;
        }
        let directory = paths.session_dir(&source.session_id)?;
        let lock = match checked_file(&directory.join(".writer.lock"), true) {
            Ok(lock) => lock,
            // No lock file means no writer ever opened this session.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        match lock_exclusive(&lock) {
            Ok(()) => {}
            Err(error) if error.code == StoreErrorCode::Busy => {
                report.active.push(source.session_id);
                continue;
            }
            Err(error) => return Err(error),
        }
        for name in DERIVED_FILES {
            let path = directory.join(name);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_file() {
                continue;
            }
            if apply {
                fs::remove_file(&path)?;
            }
            report.files.push(CacheFile {
                session_id: source.session_id.clone(),
                name,
                bytes: metadata.len(),
            });
        }
        // The guard is the open file; dropping it releases the session.
        drop(lock);
    }
    Ok(report)
}
