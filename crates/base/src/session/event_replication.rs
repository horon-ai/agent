//! Host-local replica admission. The existing actor remains the only writer.
use super::{Session, event_store};
use anyhow::{Context, Result, ensure};
use std::sync::Arc;

use event_store_api as store;
pub use horon_agent_session_store as event_store_api;
use store::replication::{SessionEventPage, SessionImportResult};

/// A process-local writer identity; never serialized or imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGeneration {
    pub incarnation: store::EventId,
    pub generation: u64,
}

/// Captured original coverage and execution identity for an asynchronous job.
#[derive(Clone, Debug)]
pub struct SessionEventJob {
    pub session_id: store::SessionId,
    pub generation: SessionGeneration,
    pub source_head: store::EventId,
    pub covered_through: store::EventId,
    pub prior_compaction: Option<store::EventId>,
    pub prefix_digest: String,
    pub settings_digest: String,
    identity: super::event_compaction::RouteIdentity,
}

impl Session {
    /// Import without creating an Agent, registering a PID, or adopting a remote cwd.
    pub fn import_replica_for_id(page: SessionEventPage, retry_cursor: Option<String>) -> Result<SessionImportResult> {
        // Loading a façade first could queue cache repair behind an active save.
        // Acquire admission in import_replica before reading any mutable binding.
        let mut target = Self::create_with_id(page.session_id.as_str().into(), None, None);
        target.status = super::SessionStatus::Closed;
        target.last_pid = None;
        target.last_active_at = None;
        target.working_dir = None;
        target.migration_epoch = 0;
        target.import_replica(page, retry_cursor)
    }

    /// Reuse this session's writer; never queue an import behind an active save.
    pub fn import_replica(
        &mut self,
        page: SessionEventPage,
        retry_cursor: Option<String>,
    ) -> Result<SessionImportResult> {
        ensure!(
            page.session_id.as_str() == self.id,
            store::StoreError::invalid_request(
                store::StoreErrorCode::InvalidRecord,
                "replica session identity mismatch"
            )
        );
        let paths = self
            .event_state
            .paths
            .clone()
            .map(Ok)
            .unwrap_or_else(event_store::paths)?;
        let id = page.session_id.clone();
        let writer = match &self.event_state.writer {
            Some(writer) => Arc::clone(writer),
            None => super::event_actor::Writer::open_for_import(paths.clone(), id.clone())?,
        };
        self.event_state.writer = Some(Arc::clone(&writer));
        let mut operation = writer.try_operation()?;
        let recovering = operation.retry_required || self.event_state.blocked.is_some();
        ensure!(
            self.event_state.pending.is_empty(),
            store::StoreError::busy("finish the reserved local save before importing")
        );
        ensure!(
            !self.has_unsaved_event_changes()?,
            store::StoreError::busy("save local changes before importing")
        );
        super::event_origin::validate_legacy_source(&paths, &id)?;
        if !self.persist_state.snapshot_exists {
            self.restore_local_runtime(&paths, &id)?;
            self.event_state.head = event_store::selected(&paths, &id)?;
            self.event_state.explicit_head = event_store::explicit_selection(&paths, &id)?;
        }
        let mut local = super::event_wire::LocalRuntime::capture(self);
        local.provider_session_id = None;
        let explicit = self.event_state.explicit_head;
        let selected = self.event_state.head.clone();
        let import_paths = paths.clone();
        let import_id = id.clone();
        let (outcome, changed, after) = writer.call(move |owned| {
            let before = event_store::source_hash(&import_paths, &import_id)?;
            let outcome = owned.import_events_resuming(&page, retry_cursor.as_deref());
            let after = event_store::source_hash(&import_paths, &import_id);
            // A failed observation must not turn a committed-prefix error into success.
            let changed = after.as_ref().map_or(true, |hash| hash != &before);
            Ok((outcome, changed, after))
        })?;
        operation.retry_required = outcome.as_ref().err().is_some_and(|error| {
            error.details.retry_cursor.is_some()
                || matches!(
                    error.code,
                    store::StoreErrorCode::Io | store::StoreErrorCode::DurabilityUnsupported
                )
        });
        let stale_view = after.as_ref().is_ok_and(|hash| hash != &self.event_state.source_hash);
        if changed || operation.retry_required || recovering || stale_view {
            if changed || operation.retry_required {
                operation.advance()?;
            }
            self.event_state.blocked = Some(store::StoreError::busy("replica refresh required"));
            let refresh = (|| -> Result<()> {
                after?;
                let graph = event_store::graph(&paths, &id)?;
                let head = graph.selected_head(if explicit { selected.as_ref() } else { None })?;
                let publish_paths = paths.clone();
                let publish_id = id.clone();
                writer.call(move |owned| {
                    let runtime_changed = local.publish(&publish_paths, &publish_id, None)?;
                    let selected_changed = event_store::select(&publish_paths, &publish_id, &head, explicit)?;
                    if runtime_changed || selected_changed {
                        event_store::sync_local_state(&publish_paths, &publish_id)?;
                    }
                    owned.refresh_cache(Some(&head), store::ReadProbe::default())?;
                    Ok(())
                })?;
                let mut restored = Self::load_events(paths, id)?;
                restored.event_state.writer = Some(Arc::clone(&writer));
                restored.event_state.explicit_head = explicit;
                if operation.retry_required {
                    restored.event_state.blocked = Some(store::StoreError::busy("retry the original import page"));
                }
                *self = restored;
                Ok(())
            })();
            if let Err(error) = refresh {
                let mut refresh_error = error.downcast_ref::<store::StoreError>().cloned().unwrap_or_else(|| {
                    let code = if error.downcast_ref::<std::io::Error>().is_some() {
                        store::StoreErrorCode::Io
                    } else {
                        store::StoreErrorCode::Internal
                    };
                    store::StoreError::internal(code, error.to_string())
                });
                match &outcome {
                    Ok(committed) => {
                        refresh_error.message = format!(
                            "replica events synchronized; runtime refresh failed: {}",
                            refresh_error.message
                        );
                        refresh_error.details = store::StoreErrorDetails {
                            accepted_ids: committed.accepted_ids.iter().map(|id| id.as_str().to_owned()).collect(),
                            retry_cursor: committed.retry_cursor.clone(),
                            conflict_heads: committed
                                .conflict_heads
                                .iter()
                                .map(|id| id.as_str().to_owned())
                                .collect(),
                        };
                        self.event_state.blocked = Some(refresh_error.clone());
                        return Err(refresh_error.into());
                    }
                    Err(import_error) => {
                        refresh_error.details = import_error.details.clone();
                        self.event_state.blocked = Some(refresh_error);
                    }
                }
            }
        }
        outcome.map_err(Into::into)
    }

    /// Capture only a durable prefix. A token does not keep the writer alive.
    pub fn capture_event_job(&mut self, covered_through: &str, settings_digest: &str) -> Result<SessionEventJob> {
        self.save_prepared()?;
        let writer = self.event_writer()?;
        let operation = writer.try_operation()?;
        ensure!(
            !operation.retry_required,
            store::StoreError::busy("retry the original import page")
        );
        self.ensure_original_current()?;
        let paths = self.event_state.paths.as_ref().context("session paths missing")?;
        let id = store::SessionId::parse(&self.id)?;
        let graph = event_store::graph(paths, &id)?;
        let source = graph.selected_head(self.event_state.head.as_ref())?;
        let projection = graph.replay(Some(&source))?;
        let covered = store::EventId::parse(covered_through)?;
        let prior = projection.compaction().map(|(id, _)| id.clone());
        Ok(SessionEventJob {
            session_id: id,
            generation: operation.snapshot(),
            prefix_digest: graph.prefix_digest(&source, &covered, prior.as_ref())?,
            source_head: source,
            covered_through: covered,
            prior_compaction: prior,
            settings_digest: settings_digest.into(),
            identity: self.route_identity(),
        })
    }

    /// Revalidate and append under the same reservation; no check/apply race.
    pub fn apply_event_job(
        &mut self,
        job: &SessionEventJob,
        mut payload: store::CompactedPayload,
        settings_digest: &str,
    ) -> Result<()> {
        let writer = self.event_writer()?;
        let operation = writer.try_operation()?;
        ensure!(
            !operation.retry_required,
            store::StoreError::busy("retry the original import page")
        );
        self.ensure_original_current()?;
        ensure!(
            !self.has_unsaved_event_changes()?,
            store::StoreError::busy("save local changes before applying an event job")
        );
        ensure!(
            job.session_id.as_str() == self.id
                && job.generation == operation.snapshot()
                && job.identity == self.route_identity()
                && job.settings_digest == settings_digest,
            store::StoreError::invalid_request(store::StoreErrorCode::SourceConflict, "stale event job")
        );
        let paths = self.event_state.paths.clone().context("session paths missing")?;
        let graph = event_store::graph(&paths, &job.session_id)?;
        let head = graph.selected_head(self.event_state.head.as_ref())?;
        let chain = graph.ancestry(&head)?;
        let start = chain
            .iter()
            .position(|id| id == &job.source_head)
            .context("captured head is no longer an ancestor")?;
        let projection = graph.replay(Some(&head))?;
        ensure!(
            graph.prefix_digest(&head, &job.covered_through, job.prior_compaction.as_ref())? == job.prefix_digest
                && projection.compaction().map(|(id, _)| id) == job.prior_compaction.as_ref(),
            store::StoreError::invalid_request(store::StoreErrorCode::SourceConflict, "job prefix changed")
        );
        for id in &chain[start + 1..] {
            let event = graph.event(id).context("missing suffix event")?;
            ensure!(
                matches!(
                    event.kind.known(),
                    Some(
                        store::KnownEventKind::MessageAppended
                            | store::KnownEventKind::MetadataChanged
                            | store::KnownEventKind::AuxiliaryRecorded
                    )
                ),
                store::StoreError::invalid_request(
                    store::StoreErrorCode::SourceConflict,
                    "job suffix is not append-only"
                )
            );
        }
        let covered = chain
            .iter()
            .position(|id| id == &job.covered_through)
            .context("coverage missing")?;
        payload.source_head_id = Some(head.clone());
        payload.covered_through_event_id = job.covered_through.clone();
        payload.prior_compaction_id = job.prior_compaction.clone();
        payload.source_prefix_digest = job.prefix_digest.clone();
        payload.first_kept_event_id = chain.get(covered + 1).cloned();
        payload.settings_digest = settings_digest.into();
        payload.generation = job.generation.generation;
        let count = projection
            .messages()
            .iter()
            .filter(|message| chain[..=covered].contains(&message.event_id))
            .count();
        let stored = super::event_compaction::stored_compaction(&payload, count, projection.messages().len());
        let origin_paths = paths.clone();
        let origin = writer.call(move |_| super::event_origin::origin(&origin_paths))?;
        let event = store::EventDraft::new(
            job.session_id.clone(),
            store::EventId::new_v4(),
            Some(head),
            origin,
            chrono::Utc::now(),
            store::EventKind::new(store::KnownEventKind::Compacted),
            serde_json::to_value(payload)?,
        )
        .seal()?;
        super::event_compaction::validate_prepared(&paths, std::slice::from_ref(&event))?;
        self.event_state.pending_base_len =
            usize::try_from(std::fs::metadata(paths.events_log(&job.session_id)?)?.len())?;
        self.event_state.pending = vec![event];
        self.flush_events(&paths, &job.session_id)?;
        self.compaction = Some(stored);
        self.event_state.auxiliary = serde_json::json!({"compaction": self.compaction});
        self.event_state.auxiliary_digest = Some(store::sha256_hex(
            store::canonical_json(&self.auxiliary_value())?.as_bytes(),
        ));
        self.refresh_active_context()?;
        self.reset_persist_state(true);
        super::event_cache::publish(self)?;
        Ok(())
    }
}
