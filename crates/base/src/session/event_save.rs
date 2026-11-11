use super::{Session, event_actor::Writer, event_store};
use anyhow::{Context, Result, bail};
use horon_agent_session_store as store;
use serde_json::{Value, json};

impl Session {
    pub(super) fn save_events(&mut self, force: bool) -> Result<()> {
        if let Some(block) = self.migration_lease_block() {
            bail!("session migration ownership conflict: {block}");
        }
        if !force
            && !self.persist_state.snapshot_exists
            && !self.messages.iter().any(super::is_visible_conversation_message)
            && !self.saved
            && self.custom_title.is_none()
            && self.title.is_none()
            && self.parent_id.is_none()
            && self.system_prompt.is_none()
            && !self.is_debug
        {
            return Ok(());
        }
        if self.event_state.bound_id.as_deref().is_some_and(|id| id != self.id) {
            self.messages = self.history_messages()?;
            self.event_state = Default::default();
            self.reset_persist_state(false);
        }
        let paths = self
            .event_state
            .paths
            .clone()
            .map(Ok)
            .unwrap_or_else(event_store::paths)?;
        let id = store::SessionId::parse(&self.id)?;
        let writer = match &self.event_state.writer {
            Some(writer) => writer.clone(),
            None => Writer::open(paths.clone(), id.clone())?,
        };
        self.event_state.writer = Some(writer.clone());
        let mut operation = writer.operation()?;
        anyhow::ensure!(
            !operation.retry_required,
            store::StoreError::busy("retry the original import page")
        );
        if let Some(error) = &self.event_state.blocked {
            return Err(error.clone().into());
        }
        #[cfg(any(test, feature = "test-support"))]
        writer.prepare()?;
        self.refresh_agent_model_overrides()?;
        super::event_origin::validate_legacy_source(&paths, &id)?;
        // Retry the exact reserved bytes before preparing any newly dirty changes.
        let retried = !self.event_state.pending.is_empty();
        if retried {
            self.flush_events(&paths, &id)?;
        }
        // Another writer can append metadata to a session a live turn owns — the
        // ambient resume marker does — without touching the transcript. Those
        // events replay to a no-op, so chain this save onto them instead of
        // dropping the turn's own output. Anything else is a real conflict and
        // still fails: adopting a transcript event would interleave two
        // conversations.
        let current_source = event_store::source_hash(&paths, &id)?;
        if current_source != self.event_state.source_hash {
            match event_store::adopt_metadata_tail(&paths, &id, self.event_state.head.as_ref())? {
                Some(head) => {
                    self.event_state.head = Some(head);
                    self.event_state.source_hash = current_source.clone();
                }
                None => bail!("SourceConflict: session originals changed since this facade was loaded"),
            }
        }
        self.updated_at = chrono::Utc::now();
        let compaction_changed =
            self.event_state.auxiliary.get("compaction") != Some(&serde_json::to_value(&self.compaction)?);
        let identity_changed = self
            .event_state
            .identity
            .as_ref()
            .is_some_and(|identity| identity != &self.route_identity());
        if identity_changed {
            operation.advance()?;
        }
        self.event_state.generation = operation.snapshot().generation;
        let incremental = !identity_changed
            && !self.event_state.reset_requested
            && !retried
            && self.persist_state.messages_mode != super::PersistVectorMode::Full
            && self.messages.len() >= self.persist_state.messages_len
            && !compaction_changed;
        let desired = if self.persist_state.transcript_stripped || incremental {
            None
        } else {
            Some(self.history_messages()?)
        };
        let metadata = self.event_metadata()?;
        let mut head = self.event_state.head.clone();
        // A save that adds no message, no auxiliary value and no metadata edit
        // beyond its own clock has nothing for the log; the close at the end of a
        // run is one. Only the local runtime binding (status, pid) can still move,
        // so that alone is published and the event commit is skipped.
        if incremental
            && head.is_some()
            && self.messages.len() == self.persist_state.messages_len
            && self.event_state.metadata.as_ref().is_some_and(|committed| {
                let mut unchanged = metadata.clone();
                unchanged.updated_at = committed.updated_at;
                &unchanged == committed
            })
            && Some(store::sha256_hex(
                store::canonical_json(&self.auxiliary_value())?.as_bytes(),
            )) == self.event_state.auxiliary_digest
        {
            if let Some(committed) = &self.event_state.metadata {
                self.updated_at = committed.updated_at;
            }
            let runtime = super::event_wire::LocalRuntime::capture(self);
            let confirmed_binding = self.event_state.confirmed_binding.clone();
            let (runtime_paths, runtime_id) = (paths.clone(), id.clone());
            writer.call(move |_| {
                if runtime.publish(&runtime_paths, &runtime_id, confirmed_binding.as_ref())? {
                    event_store::sync_local_state(&runtime_paths, &runtime_id)?;
                }
                Ok(())
            })?;
            self.event_state.local_binding = Some(super::event_wire::LocalRuntime::capture(self));
            self.reset_persist_state(true);
            return Ok(());
        }
        let origin_paths = paths.clone();
        let origin = writer.call(move |_| super::event_origin::origin(&origin_paths))?;
        let old_graph = if !incremental && current_source.is_some() {
            Some(event_store::graph(&paths, &id)?)
        } else {
            None
        };
        let projection = old_graph.as_ref().map(|g| g.replay(head.as_ref())).transpose()?;
        let old = projection.as_ref().map(|p| p.messages()).unwrap_or(&[]);
        let mut prepared = Vec::new();
        let mut push = |kind, payload: Value, parent: &mut Option<store::EventId>| -> Result<()> {
            let event = store::EventDraft::new(
                id.clone(),
                store::EventId::new_v4(),
                parent.clone(),
                origin.clone(),
                self.updated_at,
                store::EventKind::new(kind),
                payload,
            )
            .seal()?;
            *parent = Some(event.envelope().event_id.clone());
            prepared.push(event);
            Ok(())
        };
        if head.is_none() {
            push(
                store::KnownEventKind::SessionCreated,
                json!({
                    "branch_root":false, "metadata":metadata
                }),
                &mut head,
            )?;
        }
        let mut tools = self.event_state.tools.clone();
        if incremental && !self.persist_state.transcript_stripped {
            for message in &self.messages[self.persist_state.messages_len..] {
                tools.append(message)?;
                push(
                    store::KnownEventKind::MessageAppended,
                    json!({"message":message}),
                    &mut head,
                )?;
            }
        }
        if let Some(desired) = desired {
            let common = old
                .iter()
                .zip(&desired)
                .take_while(|(a, b)| a.message.id == b.id)
                .count();
            let structural = common < old.len() || self.event_state.reset_requested;
            if structural {
                self.event_state.explicit_head = true;
                if common == 0 {
                    let cleared = head.clone();
                    head = None;
                    push(
                        store::KnownEventKind::SessionCreated,
                        json!({
                            "branch_root":true, "metadata":metadata,
                            "reset_provenance":{"reset_at":self.updated_at,"cleared_head_event_id":cleared,"reason":if self.event_state.reset_requested { "clear" } else { "replace" }}
                        }),
                        &mut head,
                    )?;
                } else {
                    head = Some(old[common - 1].event_id.clone());
                }
            }
            let branch_base = if structural && common > 0 {
                Some(
                    old_graph
                        .as_ref()
                        .context("branch source missing")?
                        .replay(head.as_ref())?,
                )
            } else {
                None
            };
            for (index, message) in desired.iter().enumerate() {
                if index >= common {
                    push(
                        store::KnownEventKind::MessageAppended,
                        json!({"message":message}),
                        &mut head,
                    )?;
                } else if serde_json::to_value(
                    &branch_base.as_ref().map(|p| p.messages()).unwrap_or(old)[index].message,
                )? != serde_json::to_value(message)?
                {
                    push(
                        store::KnownEventKind::MessageRevised,
                        json!({
                            "target_event_id":old[index].event_id,"replacement":message
                        }),
                        &mut head,
                    )?;
                }
            }
        }
        push(
            store::KnownEventKind::MetadataChanged,
            json!({"metadata":metadata}),
            &mut head,
        )?;
        let auxiliary = self.auxiliary_value();
        let auxiliary_digest = store::sha256_hex(store::canonical_json(&auxiliary)?.as_bytes());
        if Some(&auxiliary_digest) != self.event_state.auxiliary_digest.as_ref() {
            push(
                store::KnownEventKind::AuxiliaryRecorded,
                json!({
                    "aux_kind":"legacy","schema_version":1,"value":auxiliary
                }),
                &mut head,
            )?;
        }
        if !incremental {
            self.prepare_compaction(&paths, &mut prepared)?;
            let view = super::event_compaction::validate_prepared(&paths, &prepared)?;
            if let Some(reset) = self.compaction_reset_event(&view, &origin)? {
                let fresh_root = reset.envelope().parent_event_id.is_none();
                self.event_state.explicit_head |= fresh_root;
                let parent = reset.envelope().event_id.clone();
                prepared.push(reset);
                if fresh_root {
                    prepared.push(
                        store::EventDraft::new(
                            id.clone(),
                            store::EventId::new_v4(),
                            Some(parent),
                            origin.clone(),
                            self.updated_at,
                            store::EventKind::new(store::KnownEventKind::AuxiliaryRecorded),
                            json!({"aux_kind":"legacy","schema_version":1,"value":auxiliary}),
                        )
                        .seal()?,
                    );
                }
            }
        }
        self.event_state.pending_base_len = if paths.events_log(&id)?.exists() {
            usize::try_from(std::fs::metadata(paths.events_log(&id)?)?.len())?
        } else {
            0
        };
        self.event_state.pending = prepared;
        self.flush_events(&paths, &id)?;
        self.event_state.paths = Some(paths.clone());
        self.event_state.bound_id = Some(self.id.clone());
        if !self.persist_state.transcript_stripped {
            self.event_state.auxiliary = json!({"compaction":self.compaction});
        }
        self.event_state.auxiliary_digest = Some(auxiliary_digest);
        if !incremental {
            self.refresh_active_context()?;
        } else {
            self.event_state.tools = tools;
        }
        self.event_state.identity = Some(self.route_identity());
        self.event_state.source_metadata = Some(metadata.clone());
        self.event_state.metadata = Some(metadata);
        self.event_state.local_binding = Some(super::event_wire::LocalRuntime::capture(self));
        self.event_state.reset_requested = false;
        self.reset_persist_state(true);
        if let Err(error) = super::event_cache::publish(self) {
            crate::logging::warn(&format!("Session committed; auxiliary cache needs repair: {error}"));
        }
        Ok(())
    }
}
