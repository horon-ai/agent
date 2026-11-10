//! Durable binding and transient original replay. No whole-history runtime cache.
use super::{Session, event_actor::Writer};
use anyhow::{Context, Result, bail};
use horon_agent_session_store as store;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Publish a host-local file the originals cannot rebuild.
///
/// The selected head, its explicit flag, the external-source receipt and the
/// runtime binding are local state the event log does not carry, so the file and
/// the rename into place are flushed with the configured durability level
/// before the caller continues.
pub(super) fn publish_local_durable<T: serde::Serialize>(
    path: &Path,
    value: &T,
    mode: horon_agent_platform::SyncMode,
) -> Result<()> {
    if publish_local_file(path, value, mode)? {
        sync_local_directory(path, mode)?;
    }
    Ok(())
}

/// Publish a host-local value that is rebuilt from the originals when it is lost.
///
/// A derived file is replaced by rename and never flushed: a crash can only
/// invalidate it, and the restore path then reconstructs it from the event log.
pub(super) fn publish_local_derived<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    publish_renamed(path, value, None)?;
    Ok(())
}

/// Write `value` into `path` by rename, reporting whether the bytes changed.
///
/// An unchanged file is already durable, so rewriting it would only add a flush
/// and a rename for state that did not move. The caller flushes the directory
/// once for all the files it published in one step.
pub(super) fn publish_local_file<T: serde::Serialize>(
    path: &Path,
    value: &T,
    mode: horon_agent_platform::SyncMode,
) -> Result<bool> {
    publish_renamed(path, value, Some(mode))
}

/// Replace `path` by rename, flushing the staged file when `mode` asks for it.
fn publish_renamed<T: serde::Serialize>(
    path: &Path,
    value: &T,
    mode: Option<horon_agent_platform::SyncMode>,
) -> Result<bool> {
    use std::io::Write;
    let bytes = serde_json::to_vec(value)?;
    let unchanged = matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.is_file())
        && std::fs::read(path).is_ok_and(|current| current == bytes);
    if unchanged {
        return Ok(false);
    }
    let directory = path.parent().context("local state parent missing")?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    if let Some(mode) = mode {
        horon_agent_platform::sync_file(temporary.as_file(), mode)?;
    }
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(true)
}

/// Flush one directory entry for the local files published in `path`.
fn sync_local_directory(path: &Path, mode: horon_agent_platform::SyncMode) -> Result<()> {
    let directory = path.parent().context("local state parent missing")?;
    horon_agent_platform::sync_dir(directory, mode)?;
    Ok(())
}

/// Flush the session directory once for every local original a commit published.
///
/// Every caller that published a local file ([`publish_local_file`], [`select`],
/// the runtime binding) calls this once after the last of them, so one step
/// pays one directory flush however many of its files changed.
pub(super) fn sync_local_state(paths: &store::SessionPaths, id: &store::SessionId) -> Result<()> {
    horon_agent_platform::sync_dir(&paths.session_dir(id)?, paths.sync_mode())?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub(super) struct EventState {
    pub paths: Option<store::SessionPaths>,
    pub bound_id: Option<String>,
    pub head: Option<store::EventId>,
    pub source_hash: Option<String>,
    pub active_start: usize,
    pub pending: Vec<store::SealedEvent>,
    pub pending_base_len: usize,
    pub writer: Option<Arc<Writer>>,
    pub auxiliary: serde_json::Value,
    pub auxiliary_digest: Option<String>,
    pub strip_images: bool,
    pub image_budget: Option<usize>,
    pub auxiliary_stripped: bool,
    pub tools: super::event_tools::ToolChain,
    pub explicit_head: bool,
    pub prefix_usage: Option<crate::protocol::TokenUsageTotals>,
    pub prefix_visible: usize,
    pub compaction_executable: bool,
    pub identity: Option<super::event_compaction::RouteIdentity>,
    pub fork_source: Option<serde_json::Value>,
    pub external_source: Option<(PathBuf, String)>,
    pub metadata: Option<store::SessionMetadata>,
    pub source_metadata: Option<store::SessionMetadata>,
    pub local_binding: Option<super::event_wire::LocalRuntime>,
    /// The newest session binding the runtime journal carries, read at load or
    /// recorded when a run row carrying it is durable. A mirror publish of this
    /// binding takes its `rev` and needs no flush.
    pub confirmed_binding: Option<crate::durable_runtime::Binding>,
    pub blocked: Option<store::StoreError>,
    pub reset_requested: bool,
    pub generation: u64,
    /// Whether a caller that ends a turn may leave the derived cache of that
    /// last commit to the next reader. Only a one-shot run enables this.
    pub defer_final_cache: bool,
    /// Set just before the commit that closes a turn. The commit consumes it
    /// even when it fails, so the commit after that rebuilds normally.
    pub skip_cache_once: bool,
}

pub(super) fn validate_external(source: &(PathBuf, String)) -> Result<()> {
    let bytes = std::fs::read(&source.0).context("SourceConflict: explicit snapshot disappeared")?;
    anyhow::ensure!(
        store::sha256_hex(&bytes) == source.1,
        "SourceConflict: explicit snapshot changed"
    );
    Ok(())
}

pub(super) fn external_source(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Option<(PathBuf, String)>> {
    let receipt = paths.session_dir(id)?.join("local-explicit-source.json");
    match std::fs::read(receipt) {
        Ok(bytes) => {
            let source = serde_json::from_slice(&bytes)?;
            validate_external(&source)?;
            Ok(Some(source))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn paths() -> Result<store::SessionPaths> {
    super::storage_paths::store_paths()
}

pub(super) fn graph(paths: &store::SessionPaths, id: &store::SessionId) -> Result<store::EventGraph> {
    let report = store::full_scan(paths, id)?;
    if let Some(error) = report.damage {
        return Err(error.into());
    }
    if !report.torn_tail.is_empty() {
        bail!("incomplete original tail requires writer recovery");
    }
    Ok(store::EventGraph::with_blobs(id, &report.events, |event| {
        let reference = event
            .payload_ref
            .as_ref()
            .ok_or_else(|| store::StoreError::unknown_session("missing original blob reference"))?;
        let bytes = std::fs::read(paths.blob_file(id, &reference.sha256)?)?;
        horon_agent_perf_counters::record_blob_bytes_read(bytes.len() as u64);
        horon_agent_perf_counters::record_store_bytes_read(bytes.len() as u64);
        Ok(bytes)
    })?)
}

/// Digest of the whole event log, streamed through one fixed buffer.
///
/// The bytes hashed are exactly the bytes on disk, so the hex equals a
/// whole-file read fed to [`store::sha256_hex`]. Only the peak temporary
/// allocation changed: the log is no longer held in memory. The read volume is
/// unchanged - every byte is still hashed, and still counted.
pub(super) fn source_hash(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Option<String>> {
    let mut log = match std::fs::File::open(paths.events_log(id)?) {
        Ok(log) => log,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let digest = store::sha256_hex_stream(&mut log)?;
    horon_agent_perf_counters::record_hash_bytes_read(digest.bytes);
    horon_agent_perf_counters::record_store_bytes_read(digest.bytes);
    horon_agent_perf_counters::record_full_source_hash(digest.bytes);
    Ok(Some(digest.hex))
}

/// Adopt a metadata-only tail another writer appended after `head`.
///
/// A second writer can add metadata to a session a live turn owns without
/// touching the transcript: the ambient resume marker is exactly that, and it
/// records "a resume turn was created for this request" as a `MetadataChanged`
/// event that copies the metadata the head already projects. Replaying it is a
/// no-op, so chaining this facade's events onto it preserves the conversation
/// byte for byte.
///
/// Only `MetadataChanged` qualifies. `AuxiliaryRecorded` does not: its payload
/// is a snapshot the facade would then overwrite, so adopting it would discard
/// another writer's state. A transcript event would interleave two
/// conversations. Both stay conflicts. `None` means "do not adopt".
pub(super) fn adopt_metadata_tail(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    head: Option<&store::EventId>,
) -> Result<Option<store::EventId>> {
    // A facade that believes the log is empty has no prefix to extend: whatever
    // is on disk is a session it never saw, not a tail it can chain onto.
    let Some(head) = head else {
        return Ok(None);
    };
    let report = store::full_scan(paths, id)?;
    if report.damage.is_some() || !report.torn_tail.is_empty() {
        return Ok(None);
    }
    let Some(index) = report.events.iter().position(|event| &event.event_id == head) else {
        return Ok(None);
    };
    let tail = &report.events[index + 1..];
    let metadata_only = tail
        .iter()
        .all(|event| event.kind.as_str() == store::KnownEventKind::MetadataChanged.as_str());
    if !metadata_only {
        return Ok(None);
    }
    Ok(tail.last().map(|event| event.event_id.clone()))
}

pub(super) fn selected(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Option<store::EventId>> {
    if explicit_flag(paths, id)? == Some(false) {
        return Ok(None);
    }
    let file = paths.session_dir(id)?.join("local-head.json");
    match std::fs::read(file) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn explicit_flag(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Option<bool>> {
    match std::fs::read(paths.session_dir(id)?.join("local-head-explicit.json")) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn explicit_selection(paths: &store::SessionPaths, id: &store::SessionId) -> Result<bool> {
    match explicit_flag(paths, id)? {
        Some(explicit) => Ok(explicit),
        None => {
            let Some(head) = selected(paths, id)? else {
                return Ok(false);
            };
            let graph = graph(paths, id)?;
            Ok(graph.heads().len() > 1 || graph.heads().first() != Some(&head))
        }
    }
}

/// Record the selected head and its intent, reporting whether a file changed.
///
/// The files are flushed but their directory entry is not: a caller that sees
/// `true` must call [`sync_local_state`] before it reports the selection.
pub(super) fn select(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    head: &store::EventId,
    explicit: bool,
) -> Result<bool> {
    let directory = paths.session_dir(id)?;
    // A head is only meaningful when the selection is explicit: every reader
    // (selection, history paging, replica export) checks the flag first and
    // ignores the file when the flag says there is no selection. Recording an
    // implicit head would cost a flush per commit for a value nothing reads.
    let mut changed = false;
    let head_file = directory.join("local-head.json");
    let flag_file = directory.join("local-head-explicit.json");
    if explicit {
        changed |= publish_local_file(&head_file, head, paths.sync_mode())?;
    } else if absent(&flag_file)? && absent(&head_file)? {
        // With neither file present every reader already resolves "no explicit
        // selection": the flag defaults to false and there is no stored head to
        // fall back to. Writing `false` would add a file flush and a directory
        // flush for a value the absence already states, so a session that never
        // had an explicit head (every new session) keeps no flag file.
        return Ok(changed);
    }
    changed |= publish_local_file(&flag_file, &explicit, paths.sync_mode())?;
    Ok(changed)
}

/// Whether nothing exists at `path`, without following a link.
fn absent(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

impl Session {
    /// Allow a caller that ends a turn to leave the derived cache of that last
    /// commit to the next reader.
    ///
    /// Only a one-shot run sets this. Every other caller, and every reader,
    /// keeps the ordinary behavior: the commit that publishes originals also
    /// publishes the derived checkpoint and catalog row.
    pub fn set_defer_final_cache(&mut self, on: bool) {
        self.event_state.defer_final_cache = on;
    }

    /// Mark the commit that closes a turn as the last one whose derived cache
    /// may be left to the next reader.
    ///
    /// The mark is consumed by the next commit whatever its outcome; when the
    /// flag above is off, the mark changes nothing.
    pub fn mark_final_commit(&mut self) {
        self.event_state.skip_cache_once = true;
    }

    pub(super) fn event_writer(&mut self) -> Result<Arc<Writer>> {
        let paths = self.event_state.paths.clone().map(Ok).unwrap_or_else(paths)?;
        if let Some(writer) = &self.event_state.writer {
            return Ok(Arc::clone(writer));
        }
        let writer = Writer::open(paths, store::SessionId::parse(&self.id)?)?;
        self.event_state.writer = Some(Arc::clone(&writer));
        Ok(writer)
    }

    pub(super) fn has_unsaved_event_changes(&self) -> Result<bool> {
        let current = self.event_metadata()?;
        Ok(!self.event_state.pending.is_empty()
            || self.persist_state.messages_mode != super::PersistVectorMode::Clean
            || self.persist_state.env_snapshots_mode != super::PersistVectorMode::Clean
            || self.persist_state.memory_injections_mode != super::PersistVectorMode::Clean
            || self.persist_state.replay_events_mode != super::PersistVectorMode::Clean
            || self
                .event_state
                .metadata
                .as_ref()
                .is_some_and(|metadata| &current != metadata))
    }

    /// Refuse stale/uncertain projections before provider execution or job application.
    pub fn ensure_event_current(&self) -> Result<()> {
        let paths = self.event_state.paths.clone().map(Ok).unwrap_or_else(paths)?;
        let writer = match &self.event_state.writer {
            Some(writer) => Some(Arc::clone(writer)),
            None => Writer::registered(&paths, &store::SessionId::parse(&self.id)?)?,
        };
        if let Some(writer) = writer {
            let operation = writer.try_operation()?;
            anyhow::ensure!(
                !operation.retry_required,
                store::StoreError::busy("retry the original import page")
            );
            self.ensure_original_current()
        } else {
            self.ensure_original_current()
        }
    }

    pub(super) fn ensure_original_current(&self) -> Result<()> {
        if let Some(error) = &self.event_state.blocked {
            return Err(error.clone().into());
        }
        if let Some(paths) = &self.event_state.paths {
            let id = store::SessionId::parse(&self.id)?;
            anyhow::ensure!(
                source_hash(paths, &id)? == self.event_state.source_hash,
                store::StoreError::invalid_request(
                    store::StoreErrorCode::SourceConflict,
                    "session originals changed; reload"
                )
            );
            if let Some(selected) = selected(paths, &id)? {
                anyhow::ensure!(
                    self.event_state.head.as_ref() == Some(&selected),
                    store::StoreError::invalid_request(
                        store::StoreErrorCode::SourceConflict,
                        "local head changed; reload"
                    )
                );
            }
        }
        Ok(())
    }

    pub fn event_generation(&self) -> Result<super::SessionGeneration> {
        let writer = self.event_state.writer.as_ref().context("session writer missing")?;
        Ok(writer.operation()?.snapshot())
    }

    pub fn invalidate_event_generation(&mut self) -> Result<()> {
        if let Some(writer) = &self.event_state.writer {
            let mut operation = writer.operation()?;
            operation.advance()?;
            self.event_state.generation = operation.snapshot().generation;
        }
        Ok(())
    }

    pub(super) fn note_event_mutation(&mut self) {
        if let Err(error) = self.invalidate_event_generation() {
            self.event_state.blocked = Some(store::StoreError::internal(
                store::StoreErrorCode::Internal,
                error.to_string(),
            ));
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_event_preparation_hook(&mut self, hook: Box<dyn FnOnce() + Send>) -> Result<()> {
        self.event_writer()?.set_preparation_hook(hook)
    }

    pub fn event_log_path(&self) -> Result<PathBuf> {
        let paths = self.event_state.paths.clone().map(Ok).unwrap_or_else(paths)?;
        Ok(paths.events_log(&store::SessionId::parse(&self.id)?)?)
    }

    pub(super) fn load_event_path(path: &Path) -> Result<Self> {
        if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            let bytes = std::fs::read(path)?;
            let mut session: Self = serde_json::from_slice(&bytes)?;
            store::SessionId::parse(&session.id)?;
            let canonical_name = path.file_stem().and_then(|s| s.to_str()) == Some(&session.id);
            let sessions_parent = path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "sessions");
            if !canonical_name || !sessions_parent {
                session.event_state.external_source = Some((path.canonicalize()?, store::sha256_hex(&bytes)));
                session.status = super::SessionStatus::Closed;
                session.last_pid = None;
                session.refresh_agent_model_overrides()?;
                session.backfill_prompt_title();
                session.reset_persist_state(false);
                session.reset_provider_messages_cache();
                session.mark_memory_profile_dirty();
                return Ok(session);
            }
        }
        let (paths, id) = if path.file_name().is_some_and(|name| name == "events.jsonl") {
            let directory = path.parent().context("event directory missing")?;
            let id = directory
                .file_name()
                .and_then(|v| v.to_str())
                .context("session id missing")?;
            (
                store::SessionPaths::new(directory.parent().context("sessions root missing")?.to_path_buf())
                    .with_sync_mode(crate::config::config().storage_sync_mode()),
                store::SessionId::parse(id)?,
            )
        } else {
            let id = path
                .file_stem()
                .and_then(|v| v.to_str())
                .context("session id missing")?;
            let root = path.parent().context("sessions root missing")?;
            (
                store::SessionPaths::new(root.canonicalize()?)
                    .with_sync_mode(crate::config::config().storage_sync_mode()),
                store::SessionId::parse(id)?,
            )
        };
        if !paths.events_log(&id)?.exists() {
            if !path.exists() {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "session not found").into());
            }
            let home = paths.root().parent().context("legacy HOME missing")?;
            store::legacy::LegacySource::new(home, id.clone())?.migrate_for_runtime(&paths)?;
        }
        Self::load_events(paths, id)
    }
}
