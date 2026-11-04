use anyhow::Result;
use std::path::{Path, PathBuf};

use super::PersistVectorMode;
use crate::storage;

pub(crate) fn session_path_in_dir(base: &std::path::Path, session_id: &str) -> PathBuf {
    base.join("sessions").join(format!("{}.json", session_id))
}

pub(super) use crate::process_memory::estimate_json_bytes;

#[cfg(test)]
pub(super) fn file_len_or_zero(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

pub(super) fn persist_vector_mode_label(mode: PersistVectorMode) -> &'static str {
    match mode {
        PersistVectorMode::Clean => "clean",
        PersistVectorMode::Append => "append",
        PersistVectorMode::Full => "full",
    }
}

pub fn session_path(session_id: &str) -> Result<PathBuf> {
    let base = storage::horon_agent_dir()?;
    Ok(session_path_in_dir(&base, session_id))
}

/// The store root every reader enumerates, with the OS temp-dir alias resolved.
pub fn store_paths() -> Result<horon_agent_session_store::SessionPaths> {
    let home = storage::horon_agent_dir()?;
    let home = if home.exists() { home.canonicalize()? } else { home };
    Ok(horon_agent_session_store::SessionPaths::new(home.join("sessions")))
}

pub fn session_journal_path_from_snapshot(path: &Path) -> PathBuf {
    let mut name = path.file_stem().map(|stem| stem.to_os_string()).unwrap_or_default();
    name.push(".journal.jsonl");
    path.with_file_name(name)
}

pub fn session_journal_path(session_id: &str) -> Result<PathBuf> {
    Ok(session_journal_path_from_snapshot(&session_path(session_id)?))
}

/// Remove every original this process may have written for a session that never
/// reached the user.
///
/// A prepared session can exist in either layout: the event store's
/// `sessions/<id>/` directory, or the legacy `sessions/<id>.json` plus journal
/// that an older build wrote. Cleanup has to cover both, otherwise a launch that
/// fails leaves an orphan the catalog still lists.
pub fn discard_session_original(session_id: &str) -> Result<()> {
    let id = horon_agent_session_store::SessionId::parse(session_id)?;
    let paths = super::event_store::paths()?;
    let directory = paths.session_dir(&id)?;
    match std::fs::remove_dir_all(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for path in [session_path(session_id)?, session_journal_path(session_id)?] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // Artifacts belong to the transcript that pointed at them; once the
    // transcript is gone they have no reader left.
    if let Ok(dir) = crate::artifacts::session_dir(session_id) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

pub fn session_exists(session_id: &str) -> bool {
    if let Ok(paths) = super::event_store::paths()
        && let Ok(id) = horon_agent_session_store::SessionId::parse(session_id)
        && let Ok(path) = paths.events_log(&id)
        && path.exists()
    {
        return true;
    }
    session_path(session_id).map(|path| path.exists()).unwrap_or(false)
}
