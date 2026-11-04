//! Deterministic logical state to sealed originals.
use crate::legacy::{LegacyDiagnostic, corrupt};
use crate::legacy_replay::Replay;
use crate::*;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub(crate) fn build(id: &SessionId, state: &mut Replay) -> Result<(String, Vec<SealedEvent>), StoreError> {
    let digest = sha256_hex(
        canonical_json(&json!({
            "metadata":state.metadata, "messages":state.messages,
            "auxiliary":state.auxiliary, "compaction":state.compaction, "archived":state.archived,
        }))?
        .as_bytes(),
    );
    let origin = legacy_origin_id(id.as_str(), &digest)?;
    let mut builder = Builder {
        id,
        origin,
        created: state.metadata.created_at,
        events: Vec::new(),
    };
    builder.push(
        KnownEventKind::SessionCreated,
        json!(SessionCreatedPayload {
            branch_root: false,
            reset_provenance: None,
            metadata: state.metadata.clone(),
        }),
        None,
    )?;
    for raw in &state.messages {
        let time: Option<DateTime<Utc>> = serde_json::from_value(raw.get("timestamp").cloned().unwrap_or(Value::Null))
            .map_err(|e| corrupt(e.to_string()))?;
        builder.push(KnownEventKind::MessageAppended, json!({"message":raw}), time)?;
    }
    if let Some(raw) = &state.compaction {
        let converted = compaction(&builder, state, raw);
        match converted {
            Ok(payload) => builder.push(KnownEventKind::Compacted, json!(payload), None)?,
            Err(error) => {
                // Legacy boundary/counts cannot always satisfy the stricter new graph.
                // Keep the exact artifact, but do not fabricate valid source coverage.
                state
                    .diagnostics
                    .push(LegacyDiagnostic::CompactionPreservedInert { reason: error.message });
            }
        }
    }
    builder.push(
        KnownEventKind::AuxiliaryRecorded,
        json!({"aux_kind":"legacy","schema_version":1,"value":state.auxiliary}),
        None,
    )?;
    if state.archived {
        builder.push(
            KnownEventKind::ArchiveChanged,
            json!({"archived":true,"reason":"legacy archive"}),
            None,
        )?;
    }
    graph(&builder)?.replay(None)?;
    Ok((digest, builder.events))
}

struct Builder<'a> {
    id: &'a SessionId,
    origin: OriginId,
    created: DateTime<Utc>,
    events: Vec<SealedEvent>,
}
impl Builder<'_> {
    // Kind, payload and source time form one event; identity/order are owned here.
    fn push(&mut self, kind: KnownEventKind, payload: Value, time: Option<DateTime<Utc>>) -> Result<(), StoreError> {
        let ordinal = u64::try_from(self.events.len()).map_err(|e| corrupt(e.to_string()))?;
        self.events.push(
            EventDraft::new(
                self.id.clone(),
                legacy_event_id(self.id.as_str(), kind.as_str(), ordinal)?,
                self.events.last().map(|e| e.envelope().event_id.clone()),
                self.origin.clone(),
                pick_legacy_timestamp(time, Some(self.created)),
                EventKind::new(kind),
                payload,
            )
            .seal()?,
        );
        Ok(())
    }
}

fn graph(builder: &Builder<'_>) -> Result<EventGraph, StoreError> {
    let envelopes = builder.events.iter().map(|e| e.envelope().clone()).collect::<Vec<_>>();
    EventGraph::with_blobs(builder.id, &envelopes, |event| {
        builder
            .events
            .iter()
            .find(|e| e.envelope().event_id == event.event_id)
            .and_then(|e| e.blob())
            .map(<[u8]>::to_vec)
            .ok_or_else(|| corrupt("sealed migration blob missing"))
    })
}

fn compaction(builder: &Builder<'_>, state: &Replay, raw: &Value) -> Result<CompactedPayload, StoreError> {
    let old: horon_agent_session_types::StoredCompactionState =
        serde_json::from_value(raw.clone()).map_err(|e| corrupt(e.to_string()))?;
    let count = old.compacted_count.min(state.messages.len());
    // Index zero is SessionCreated: a transfer summary covers no child messages.
    // Its entire local history remains after that real source boundary.
    let graph = graph(builder)?;
    let head = builder
        .events
        .last()
        .ok_or_else(|| corrupt("source head absent"))?
        .envelope()
        .event_id
        .clone();
    let covered = builder
        .events
        .get(count)
        .ok_or_else(|| corrupt("coverage absent"))?
        .envelope()
        .event_id
        .clone();
    let mut native = match raw.get("native_window") {
        Some(Value::Array(items)) => items.clone(),
        None | Some(Value::Null) => Vec::new(),
        Some(_) => return Err(corrupt("native window must be an ordered array")),
    };
    if native.is_empty()
        && let Some(encrypted) = old.openai_encrypted_content
    {
        native.push(json!({"type":"compaction","encrypted_content":encrypted}));
    }
    let payload = CompactedPayload {
        summary_text: old.summary_text,
        source_head_id: Some(head.clone()),
        covered_through_event_id: covered.clone(),
        source_prefix_digest: graph.prefix_digest(&head, &covered, None)?,
        first_kept_event_id: builder.events.get(count + 1).map(|e| e.envelope().event_id.clone()),
        prior_compaction_id: None,
        provider: state
            .metadata
            .provider_key
            .clone()
            .ok_or_else(|| corrupt("legacy compaction provider absent"))?,
        model: state
            .metadata
            .model
            .clone()
            .ok_or_else(|| corrupt("legacy compaction model absent"))?,
        route: state
            .metadata
            .route_api_method
            .clone()
            .ok_or_else(|| corrupt("legacy compaction route absent"))?,
        settings_digest: sha256_hex(canonical_json(raw)?.as_bytes()),
        generation: 0,
        input_tokens: None,
        output_tokens: None,
        outcome: if native.is_empty() {
            CompactionOutcome::Summary
        } else {
            CompactionOutcome::Native
        },
        native_window: native,
        restoration: None,
    };
    let mut events = builder.events.iter().map(|e| e.envelope().clone()).collect::<Vec<_>>();
    let candidate = EventDraft::new(
        builder.id.clone(),
        legacy_event_id(
            builder.id.as_str(),
            "compacted",
            u64::try_from(events.len()).map_err(|e| corrupt(e.to_string()))?,
        )?,
        Some(head),
        builder.origin.clone(),
        builder.created,
        EventKind::new(KnownEventKind::Compacted),
        json!(payload),
    )
    .seal()?;
    events.push(candidate.envelope().clone());
    EventGraph::with_blobs(builder.id, &events, |event| {
        if event.event_id == candidate.envelope().event_id {
            return candidate
                .blob()
                .map(<[u8]>::to_vec)
                .ok_or_else(|| corrupt("compaction blob absent"));
        }
        builder
            .events
            .iter()
            .find(|e| e.envelope().event_id == event.event_id)
            .and_then(|e| e.blob())
            .map(<[u8]>::to_vec)
            .ok_or_else(|| corrupt("blob absent"))
    })?
    .replay(None)?;
    Ok(payload)
}
