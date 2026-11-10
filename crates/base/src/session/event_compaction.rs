//! Mechanical conversion of the existing compaction state to event boundaries.
use super::{Session, StoredCompactionState};
use anyhow::{Context, Result};
use horon_agent_session_store as store;
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RouteIdentity {
    model: Option<String>,
    provider: Option<String>,
    route: Option<String>,
}

pub(super) fn stored_compaction(
    payload: &store::CompactedPayload,
    covered_messages: usize,
    total_messages: usize,
) -> StoredCompactionState {
    StoredCompactionState {
        summary_text: payload.summary_text.clone(),
        openai_encrypted_content: payload.native_window.iter().find_map(|item| {
            (item.get("type").and_then(|v| v.as_str()) == Some("compaction"))
                .then(|| {
                    item.get("encrypted_content")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                })
                .flatten()
        }),
        native_window: payload.native_window.clone(),
        native_route: (!payload.native_window.is_empty()).then(|| crate::session::StoredNativeRoute {
            provider: payload.provider.clone(),
            model: payload.model.clone(),
            route: (!payload.route.is_empty()).then(|| payload.route.clone()),
        }),
        covers_up_to_turn: covered_messages,
        original_turn_count: total_messages,
        compacted_count: covered_messages,
        structured: Some(crate::session::StoredSummaryMetadata {
            input_tokens: payload.input_tokens,
            output_tokens: payload.output_tokens,
            restoration: payload
                .restoration
                .as_ref()
                .map(|manifest| crate::session::StoredRestorationManifest {
                    entries: manifest
                        .entries
                        .iter()
                        .map(|entry| crate::session::StoredRestorationEntry {
                            path: entry.path.clone(),
                            kind: entry.kind.clone(),
                            note: entry.note.clone(),
                        })
                        .collect(),
                    total_bytes: manifest.total_bytes,
                }),
            lossy_emergency: payload.outcome == store::CompactionOutcome::LossyEmergency,
            recovery_blocked: None,
            handoff: payload.outcome == store::CompactionOutcome::Handoff,
        })
        .filter(|meta| {
            meta.input_tokens.is_some()
                || meta.output_tokens.is_some()
                || meta.restoration.is_some()
                || meta.lossy_emergency
                || meta.handoff
        }),
    }
}

impl Session {
    pub(super) fn restore_event_compaction(&mut self, payload: &store::CompactedPayload, counts: (usize, usize)) {
        let mut projected = stored_compaction(payload, counts.0, counts.1);
        // The compacted event carries the summary, not the local recovery
        // refusal, so the marker recorded beside it must survive the projection.
        if let (Some(saved), Some(meta)) = (
            self.compaction
                .as_ref()
                .and_then(|saved| saved.structured.as_ref().and_then(|meta| meta.recovery_blocked.clone())),
            projected.structured.as_mut(),
        ) {
            meta.recovery_blocked = Some(saved);
        }
        if self.compaction.as_ref().is_none_or(|saved| {
            saved.summary_text != projected.summary_text
                || saved.openai_encrypted_content != projected.openai_encrypted_content
                || saved.compacted_count != projected.compacted_count
                || saved.structured.as_ref().is_some_and(|meta| meta.lossy_emergency)
                    != projected.structured.as_ref().is_some_and(|meta| meta.lossy_emergency)
        }) {
            self.compaction = Some(projected);
        }
    }

    pub(super) fn route_identity(&self) -> RouteIdentity {
        RouteIdentity {
            model: self.model.clone(),
            provider: self.provider_key.clone(),
            route: self.route_api_method.clone(),
        }
    }

    pub(super) fn compaction_reset_event(
        &self,
        old: &store::Projection,
        origin: &store::OriginId,
    ) -> Result<Option<store::SealedEvent>> {
        if self.persist_state.transcript_stripped || self.compaction.is_some() || old.compaction().is_none() {
            return Ok(None);
        }
        let (kind, parent, payload) = if let Some(last) = old.messages().last() {
            (
                store::KnownEventKind::MessageRevised,
                Some(old.head().clone()),
                json!({
                    "target_event_id":last.event_id,"replacement":last.message
                }),
            )
        } else {
            (
                store::KnownEventKind::SessionCreated,
                None,
                json!({
                    "branch_root":true,"metadata":self.event_metadata()?,
                    "reset_provenance":{"reset_at":self.updated_at,"cleared_head_event_id":old.head(),"reason":"compaction_reset"}
                }),
            )
        };
        Ok(Some(
            store::EventDraft::new(
                store::SessionId::parse(&self.id)?,
                store::EventId::new_v4(),
                parent,
                origin.clone(),
                self.updated_at,
                store::EventKind::new(kind),
                payload,
            )
            .seal()?,
        ))
    }

    pub fn active_history_offset(&self) -> usize {
        self.event_state.active_start
    }

    pub fn compaction_for_active_context(&self) -> Option<StoredCompactionState> {
        if self.event_state.paths.is_some() && !self.event_state.compaction_executable {
            return None;
        }
        let mut state = self.compaction.clone()?;
        state.compacted_count = state.compacted_count.saturating_sub(self.event_state.active_start);
        Some(state)
    }

    pub(super) fn prepare_compaction(
        &self,
        paths: &store::SessionPaths,
        events: &mut Vec<store::SealedEvent>,
    ) -> Result<()> {
        let Some(state) = &self.compaction else {
            return Ok(());
        };
        if !state.has_summary() {
            return Ok(());
        }
        if self.event_state.auxiliary.get("compaction") == Some(&serde_json::to_value(state)?) {
            return Ok(());
        }
        // Old records without source identity remain exact inert auxiliary data.
        let (Some(provider), Some(model), Some(route)) = (&self.provider_key, &self.model, &self.route_api_method)
        else {
            return Ok(());
        };
        let id = store::SessionId::parse(&self.id)?;
        let mut originals = if paths.events_log(&id)?.exists() {
            let report = store::full_scan(paths, &id)?;
            if let Some(error) = report.damage {
                return Err(error.into());
            }
            report.events
        } else {
            Vec::new()
        };
        originals.extend(events.iter().map(|e| e.envelope().clone()));
        let graph = store::EventGraph::with_blobs(&id, &originals, |event| {
            if let Some(bytes) = events
                .iter()
                .find(|e| e.envelope().event_id == event.event_id)
                .and_then(|e| e.blob())
            {
                return Ok(bytes.to_vec());
            }
            let reference = event
                .payload_ref
                .as_ref()
                .ok_or_else(|| store::StoreError::unknown_session("blob missing"))?;
            Ok(std::fs::read(paths.blob_file(&id, &reference.sha256)?)?)
        })?;
        let parent = events.last().context("compaction source absent")?.envelope();
        let projection = graph.replay(Some(&parent.event_id))?;
        let chain = graph.ancestry(&parent.event_id)?;
        let count = state.compacted_count.min(projection.messages().len());
        let covered = if count == 0 {
            chain.first().context("session root missing")?.clone()
        } else {
            projection.messages()[count - 1].event_id.clone()
        };
        let covered_position = chain.iter().position(|id| id == &covered).context("coverage missing")?;
        let prior = projection.compaction().map(|(id, _)| id.clone());
        // Usage and restoration are per-run state, not settings; leaving them
        // out keeps the digest stable when the same summary is republished.
        let digest_state = StoredCompactionState {
            structured: None,
            ..state.clone()
        };
        // Persist the retained window whole and in order. A window-less legacy
        // state keeps its single compaction item; anything else would drop the
        // items the server retained.
        let native_window = if state.native_window.is_empty() {
            state
                .openai_encrypted_content
                .as_ref()
                .map(|content| vec![json!({"type":"compaction","encrypted_content":content})])
                .unwrap_or_default()
        } else {
            state.native_window.clone()
        };
        let payload = store::CompactedPayload {
            summary_text: state.summary_text.clone(),
            source_head_id: Some(parent.event_id.clone()),
            covered_through_event_id: covered.clone(),
            source_prefix_digest: graph.prefix_digest(&parent.event_id, &covered, prior.as_ref())?,
            first_kept_event_id: chain.get(covered_position + 1).cloned(),
            prior_compaction_id: prior,
            provider: provider.clone(),
            model: model.clone(),
            route: route.clone(),
            settings_digest: store::sha256_hex(serde_json::to_string(&digest_state)?.as_bytes()),
            generation: self.event_state.generation,
            input_tokens: state.structured.as_ref().and_then(|meta| meta.input_tokens),
            output_tokens: state.structured.as_ref().and_then(|meta| meta.output_tokens),
            outcome: if state.structured.as_ref().is_some_and(|meta| meta.lossy_emergency) {
                store::CompactionOutcome::LossyEmergency
            } else if state.structured.as_ref().is_some_and(|meta| meta.handoff) {
                store::CompactionOutcome::Handoff
            } else if native_window.is_empty() {
                store::CompactionOutcome::Summary
            } else {
                store::CompactionOutcome::Native
            },
            native_window,
            restoration: state
                .structured
                .as_ref()
                .and_then(|meta| meta.restoration.as_ref())
                .map(|manifest| store::RestorationManifest {
                    entries: manifest
                        .entries
                        .iter()
                        .map(|entry| store::RestorationEntry {
                            path: entry.path.clone(),
                            kind: entry.kind.clone(),
                            note: entry.note.clone(),
                        })
                        .collect(),
                    total_bytes: manifest.total_bytes,
                }),
        };
        let event = store::EventDraft::new(
            id,
            store::EventId::new_v4(),
            Some(parent.event_id.clone()),
            parent.origin_id.clone(),
            self.updated_at,
            store::EventKind::new(store::KnownEventKind::Compacted),
            json!(payload),
        )
        .seal()?;
        originals.push(event.envelope().clone());
        store::EventGraph::with_blobs(&event.envelope().session_id, &originals, |record| {
            if record.event_id == event.envelope().event_id
                && let Some(bytes) = event.blob()
            {
                return Ok(bytes.to_vec());
            }
            if let Some(bytes) = events
                .iter()
                .find(|e| e.envelope().event_id == record.event_id)
                .and_then(|e| e.blob())
            {
                return Ok(bytes.to_vec());
            }
            let reference = record
                .payload_ref
                .as_ref()
                .ok_or_else(|| store::StoreError::unknown_session("blob missing"))?;
            Ok(std::fs::read(paths.blob_file(&record.session_id, &reference.sha256)?)?)
        })?
        .replay(Some(&event.envelope().event_id))?;
        events.push(event);
        Ok(())
    }
}

/// Refuse invalid application mutations before any canonical record is appended.
pub(super) fn validate_prepared(
    paths: &store::SessionPaths,
    events: &[store::SealedEvent],
) -> Result<store::Projection> {
    let head = events.last().context("empty prepared mutation")?.envelope();
    let mut originals = if paths.events_log(&head.session_id)?.exists() {
        let report = store::full_scan(paths, &head.session_id)?;
        if let Some(error) = report.damage {
            return Err(error.into());
        }
        report.events
    } else {
        Vec::new()
    };
    originals.extend(events.iter().map(|event| event.envelope().clone()));
    let projection = store::EventGraph::with_blobs(&head.session_id, &originals, |record| {
        if let Some(bytes) = events
            .iter()
            .find(|e| e.envelope().event_id == record.event_id)
            .and_then(|e| e.blob())
        {
            return Ok(bytes.to_vec());
        }
        let reference = record
            .payload_ref
            .as_ref()
            .ok_or_else(|| store::StoreError::unknown_session("blob missing"))?;
        Ok(std::fs::read(paths.blob_file(&record.session_id, &reference.sha256)?)?)
    })?
    .replay(Some(&head.event_id))?;
    Ok(projection)
}
