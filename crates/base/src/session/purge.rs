//! Permanent removal of one stored session and every file keyed by its id.
//!
//! Archiving hides a session and keeps it loadable; this is the other
//! operation, the one an operator asks for when the transcript itself must go.
//! The transcript is only part of what a session owns: a turn leaves todos, a
//! goal attachment, interrupts, a writer lease, presence markers, notes, a
//! runtime journal and edit statistics behind, and each of those names the
//! session id in its filename. Collection therefore walks every such location
//! rather than stopping at the event log.
//!
//! Nothing outside the session's own files is touched. Process-wide state
//! (`managed-skills`, `models`, approvals, configuration, the usage database)
//! is not a session's, and deleting one session must not disturb another.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::storage;

/// What one purge removed, for the caller's report and for tests.
///
/// Paths that were already absent are not counted: a second purge of the same
/// id reports zero, which is how idempotence is observable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PurgeReport {
    /// Files and directories that existed and were removed.
    pub removed: Vec<PathBuf>,
}

impl PurgeReport {
    /// How many entries were removed.
    pub fn removed_count(&self) -> usize {
        self.removed.len()
    }
}

/// Whether `value` is a session id the store would accept as a path component.
///
/// A delete resolves its target with this rather than by loading the session:
/// a corrupt session has to be deletable, and an id that is already gone has to
/// resolve so that deleting it twice succeeds. The check is the store's own, so
/// it also refuses anything that could name a path outside the store.
pub fn is_store_session_id(value: &str) -> bool {
    horon_agent_session_store::SessionId::parse(value).is_ok()
}

/// Remove one session's stored state and return what was removed.
///
/// The id is validated by the session store's own rules, so a caller cannot
/// name a parent directory. Errors are I/O failures other than "already gone":
/// a purge that removed half the session reports the failure instead of
/// pretending the session is gone.
pub fn purge_session_state(session_id: &str) -> Result<PurgeReport> {
    // Validate with the same parser every other path builder uses, so a purge
    // cannot address a path the store itself would refuse.
    horon_agent_session_store::SessionId::parse(session_id)?;
    let home = storage::horon_agent_dir()?;
    let mut report = PurgeReport::default();

    // The transcript, plus the older JSON, journal and artifacts behind it.
    // The directory is recorded only when it existed, so a second purge of the
    // same id reports nothing removed.
    //
    // A session that was created and never persisted has no store root at all,
    // and the store's own path builder refuses to resolve one. Nothing exists to
    // remove in that case, which is the same state as a second purge.
    let transcript = home.join("sessions").join(session_id);
    let transcript_existed = transcript.symlink_metadata().is_ok();
    if home.join("sessions").exists() {
        super::discard_session_original(session_id)?;
    }
    if transcript_existed {
        report.removed.push(transcript);
    }

    // Session-keyed files outside the transcript. Each entry pairs a path with
    // whether it is a directory; a missing path is not an error.
    let mut targets: Vec<(PathBuf, bool)> = Vec::new();

    for suffix in ["", "-goals", "-plan", "-gate-observations", "-review-state"] {
        targets.push((home.join("todos").join(format!("{session_id}{suffix}.json")), false));
    }
    targets.push((home.join("goals/sessions").join(format!("{session_id}.json")), false));
    targets.push((
        home.join("pending-soft-interrupts").join(format!("{session_id}.json")),
        false,
    ));
    targets.push((home.join("session_leases").join(format!("{session_id}.json")), false));
    targets.push((home.join("active_pids").join(session_id), false));
    targets.push((home.join("streaming_pids").join(session_id), false));
    targets.push((home.join("internal_pids").join(session_id), false));
    targets.push((home.join("context-notes").join(format!("{session_id}.md")), false));
    targets.push((home.join("durable-runtime").join(session_id), true));
    targets.push((home.join("failover-budget").join(format!("{session_id}.json")), false));
    // Edit statistics live beside the store root, not under the home root.
    targets.push((
        home.join("sessions/edit-stats").join(format!("{session_id}.json")),
        false,
    ));
    targets.push((
        home.join("sessions/edit-stats").join(format!("{session_id}.lock")),
        false,
    ));
    // A deferred catalog row is published from its dirty marker, so the marker
    // must go with the row it names.
    targets.push((home.join("sessions/.catalog.dirty").join(session_id), false));

    for (path, directory) in targets {
        if remove_target(&path, directory)? {
            report.removed.push(path);
        }
    }

    Ok(report)
}

/// Remove one file or directory. `Ok(false)` means it was already absent.
fn remove_target(path: &Path, directory: bool) -> Result<bool> {
    let result = if directory {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            // A directory target may exist as a file or the other way round
            // after an interrupted migration; fall back rather than fail.
            let fallback = if directory {
                std::fs::remove_file(path)
            } else {
                std::fs::remove_dir_all(path)
            };
            match fallback {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(_) => Err(error.into()),
            }
        }
    }
}
