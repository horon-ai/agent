//! Cold reconstruction releases the temporary full-history projection before returning.
use super::Session;
use super::event_actor::Writer;
use super::event_store::{EventState, graph, selected, source_hash};
use anyhow::{Context, Result};
use horon_agent_session_store as store;

impl Session {
    pub(super) fn load_events(paths: store::SessionPaths, id: store::SessionId) -> Result<Self> {
        super::event_origin::validate_legacy_source(&paths, &id)?;
        let external_source = super::event_store::external_source(&paths, &id)?;
        if let Some(mut session) = super::event_cache::restore(&paths, &id)? {
            session.event_state.external_source = external_source;
            return Ok(session);
        }
        let head = selected(&paths, &id)?;
        let cache = store::checkpoint::SessionCache::new(paths.clone(), store::ReadProbe::default());
        match cache.recent(&id, head.as_ref()) {
            Ok(_) => {}
            Err(error) if error.code == store::StoreErrorCode::Busy => {
                let writer = Writer::open(paths.clone(), id.clone())?;
                let selected = head.clone();
                writer.call(move |writer| Ok(writer.refresh_cache(selected.as_ref(), store::ReadProbe::default())?))?;
            }
            Err(error) => return Err(error.into()),
        }
        let event_graph = graph(&paths, &id)?;
        let projection = event_graph.replay(head.as_ref())?;
        let mut tools = super::event_tools::ToolChain::default();
        for message in projection.messages() {
            tools.append(&message.message)?;
        }
        let mut value = serde_json::to_value(projection.metadata())?;
        let object = value.as_object_mut().context("session metadata must be an object")?;
        object.insert("id".into(), serde_json::json!(id.as_str()));
        object.insert(
            "messages".into(),
            serde_json::json!(
                projection
                    .active_messages()
                    .iter()
                    .map(|m| &m.message)
                    .collect::<Vec<_>>()
            ),
        );
        let extensions = projection.metadata().legacy_extensions.clone();
        object.remove("legacy_extensions");
        for (key, value) in extensions {
            object.entry(key).or_insert(value);
        }
        let mut auxiliary = serde_json::Map::new();
        for (_, payload) in projection.auxiliary() {
            match payload {
                store::AuxiliaryRecordedPayload::Legacy { value, .. } => {
                    if let Some(fields) = value.as_object() {
                        auxiliary.extend(fields.clone());
                    }
                }
                store::AuxiliaryRecordedPayload::EnvSnapshot(snapshot) => {
                    auxiliary
                        .entry("env_snapshots")
                        .or_insert(serde_json::json!([]))
                        .as_array_mut()
                        .context("invalid environment history")?
                        .push(serde_json::to_value(snapshot)?);
                }
                store::AuxiliaryRecordedPayload::MemoryInjection(injection) => {
                    auxiliary
                        .entry("memory_injections")
                        .or_insert(serde_json::json!([]))
                        .as_array_mut()
                        .context("invalid memory history")?
                        .push(serde_json::to_value(injection)?);
                }
                store::AuxiliaryRecordedPayload::ReplayEvent { value, .. } => {
                    auxiliary
                        .entry("replay_events")
                        .or_insert(serde_json::json!([]))
                        .as_array_mut()
                        .context("invalid replay history")?
                        .push(value.clone());
                }
            }
        }
        for key in [
            "env_snapshots",
            "memory_injections",
            "replay_events",
            "pruned_tool_results",
            "stream_rule_ledger",
            "compaction",
            "is_debug",
        ] {
            if let Some(value) = auxiliary.get(key) {
                object.insert(key.into(), value.clone());
            }
        }
        // Historical process identity is inert; only this host's live registry owns execution.
        object.insert("status".into(), serde_json::json!(super::SessionStatus::Closed));
        object.insert("last_pid".into(), serde_json::Value::Null);
        object.insert("last_active_at".into(), serde_json::Value::Null);
        let mut session: Self = serde_json::from_value(value)?;
        if let Some((_, payload)) = projection.compaction() {
            session.restore_event_compaction(
                payload,
                (
                    projection.messages().len() - projection.active_messages().len(),
                    projection.messages().len(),
                ),
            );
        }
        session.restore_local_runtime(&paths, &id)?;
        session.event_state = EventState {
            source_metadata: Some(projection.metadata().clone()),
            local_binding: Some(super::event_wire::LocalRuntime::capture(&session)),
            external_source,
            paths: Some(paths.clone()),
            bound_id: Some(id.as_str().to_owned()),
            head: Some(projection.head().clone()),
            source_hash: source_hash(&paths, &id)?,
            active_start: projection.messages().len() - projection.active_messages().len(),
            auxiliary: serde_json::json!({"compaction": session.compaction}),
            tools,
            explicit_head: super::event_store::explicit_selection(&paths, &id)?,
            compaction_executable: projection.compaction().is_some(),
            prefix_usage: Some(super::aggregate_usage(
                projection
                    .messages()
                    .iter()
                    .take(projection.messages().len() - projection.active_messages().len())
                    .map(|m| &m.message),
                crate::protocol::TokenUsageTotals {
                    cache_prompt_tokens: Some(0),
                    ..Default::default()
                },
            )),
            prefix_visible: projection
                .messages()
                .iter()
                .take(projection.messages().len() - projection.active_messages().len())
                .filter(|m| super::is_visible_conversation_message(&m.message))
                .count(),
            ..Default::default()
        };
        session.event_state.identity = Some(session.route_identity());
        session.refresh_agent_model_overrides()?;
        session.backfill_prompt_title();
        session.event_state.metadata = Some(session.event_metadata()?);
        session.reset_persist_state(true);
        session.reset_provider_messages_cache();
        session.mark_memory_profile_dirty();
        session.event_state.auxiliary_digest = Some(store::sha256_hex(
            store::canonical_json(&session.auxiliary_value())?.as_bytes(),
        ));
        if let Err(error) = super::event_cache::publish(&session) {
            crate::logging::warn(&format!("Session auxiliary cache not published: {error}"));
        }
        Ok(session)
    }
}
