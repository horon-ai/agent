//! Durable archive and restore records. Hiding a session is an event, never a
//! deletion: the original log, blobs and every earlier archive record stay.
use crate::*;
use chrono::Utc;
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;

/// What recording an archive or restore intent did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveOutcome {
    /// A new `ArchiveChanged` event is durable at the selected head.
    Recorded {
        event_id: EventId,
        cache_warning: Option<String>,
    },
    /// The selected head already carries the requested visibility.
    Unchanged { head: EventId },
}

impl ArchiveOutcome {
    /// The new event id, absent when nothing had to change.
    pub fn event_id(&self) -> Option<&EventId> {
        match self {
            Self::Recorded { event_id, .. } => Some(event_id),
            Self::Unchanged { .. } => None,
        }
    }
}

/// The install origin recorded beside the store root, created on first use.
///
/// A per-install identity, not a credential. The file is owner-only and never
/// rewritten once it exists.
///
/// # Errors
/// Refuses a symlinked origin file, a malformed value, or a missing store root.
pub fn origin_id(paths: &SessionPaths) -> Result<OriginId, StoreError> {
    let path = paths.root().join(".origin-id");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    horon_agent_platform::fs::request_owner_only_mode(&mut options);
    match options.open(&path) {
        Ok(mut file) => {
            let id = OriginId::new_v4();
            file.write_all(id.as_str().as_bytes())?;
            crate::blobs::sync_file(&file, paths.sync_mode())?;
            crate::blobs::sync_directory(paths.root(), paths.sync_mode())?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(crate::blobs::unsafe_path(&path));
            }
            OriginId::parse(std::fs::read_to_string(&path)?)
        }
        Err(error) => Err(error.into()),
    }
}

/// The host-local head this machine last selected, mirroring the facade rule:
/// an explicit `false` flag means no selection, so diverged heads stay refused.
fn selected(paths: &SessionPaths, id: &SessionId) -> Result<Option<EventId>, StoreError> {
    let directory = paths.session_dir(id)?;
    match std::fs::read(directory.join("local-head-explicit.json")) {
        Ok(bytes) => {
            if !serde_json::from_slice::<bool>(&bytes).map_err(crate::cache_io::json_error)? {
                return Ok(None);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match std::fs::read(directory.join("local-head.json")) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice::<EventId>(&bytes).map_err(crate::cache_io::json_error)?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Append one `ArchiveChanged` event to the session's selected head.
///
/// The exclusive session writer is taken before the head is read, so no other
/// process can append between the replay and the record. A live writer is
/// `Busy`, an absent original is `UnknownSession`, and diverged heads are
/// `ConflictHeads` rather than a silent choice of one branch.
///
/// # Errors
/// Returns the store error for a busy session, an absent log, diverged heads,
/// or failed durability. The original events are never modified.
pub fn record(
    paths: &SessionPaths,
    id: &SessionId,
    archived: bool,
    reason: &str,
) -> Result<ArchiveOutcome, StoreError> {
    let log = paths.events_log(id)?;
    match std::fs::symlink_metadata(&log) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(crate::blobs::unsafe_path(&log)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::unknown_session("session has no original event log"));
        }
        Err(error) => return Err(error.into()),
    }
    let mut writer = BlockingWriter::open(paths.clone(), id.clone())?;
    let graph = EventGraph::open(paths, id)?;
    let head = graph.selected_head(selected(paths, id)?.as_ref())?;
    let current = graph.replay(Some(&head))?.archive().is_some_and(|state| state.archived);
    if current == archived {
        return Ok(ArchiveOutcome::Unchanged { head });
    }
    let event = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        Some(head),
        origin_id(paths)?,
        Utc::now(),
        EventKind::new(KnownEventKind::ArchiveChanged),
        json!({"archived": archived, "reason": reason}),
    )
    .seal()?;
    let event_id = event.envelope().event_id.clone();
    writer.append(&event)?;
    // The host-local selection file, when one exists, must follow the appended
    // child or the next facade load would replay the parent and never see this
    // record. The flag itself stays exactly as the facade wrote it.
    let head_file = paths.session_dir(id)?.join("local-head.json");
    let mut warning = None;
    if head_file.symlink_metadata().is_ok()
        && let Err(error) = crate::cache_io::publish(
            &mut crate::blobs::Disk::new(Box::new(|_| Ok(())), paths.sync_mode()),
            &head_file,
            &serde_json::to_vec(&event_id).map_err(crate::cache_io::json_error)?,
            &ReadProbe::default(),
        )
    {
        warning = Some(format!("local head: {:?}: {}", error.code, error.message));
    }
    // The original commit stands even when the derived cache cannot be rebuilt.
    if let Err(error) = writer.refresh_cache(Some(&event_id), ReadProbe::default()) {
        warning = Some(format!("cache: {:?}: {}", error.code, error.message));
    }
    Ok(ArchiveOutcome::Recorded {
        event_id,
        cache_warning: warning,
    })
}

/// Current visibility from the durable record, if one was ever written.
///
/// # Errors
/// Propagates original damage and missing blobs.
pub fn visibility(paths: &SessionPaths, id: &SessionId) -> Result<Option<bool>, StoreError> {
    let graph = EventGraph::open(paths, id)?;
    let selected = selected(paths, id)?;
    graph
        .replay(selected.as_ref())
        .map(|view| view.archive().map(|state| state.archived))
        .or_else(|error| {
            if error.code == StoreErrorCode::ConflictHeads {
                graph
                    .shared_head()
                    .and_then(|head| graph.replay(Some(&head)))
                    .map(|view| view.archive().map(|state| state.archived))
            } else {
                Err(error)
            }
        })
}
