//! Explicit full-history serde export and small event metadata.
use super::Session;
use anyhow::{Context, Result};
use horon_agent_session_store::SessionMetadata;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

/// Host-local only. Replica export never carries this execution ownership.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct LocalRuntime {
    pub status: super::SessionStatus,
    pub last_pid: Option<u32>,
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub migration_epoch: Option<u64>,
    #[serde(default)]
    pub provider_session_id: Option<String>,
}

impl LocalRuntime {
    pub(super) fn capture(session: &Session) -> Self {
        Self {
            status: session.status.clone(),
            last_pid: session.last_pid,
            last_active_at: session.last_active_at,
            working_dir: session.working_dir.clone(),
            migration_epoch: Some(session.migration_epoch),
            provider_session_id: session.provider_session_id.clone(),
        }
    }

    pub(super) fn publish(
        &self,
        paths: &horon_agent_session_store::SessionPaths,
        id: &horon_agent_session_store::SessionId,
    ) -> Result<()> {
        super::event_store::publish_local(&paths.session_dir(id)?.join("local-runtime.json"), self)?;
        horon_agent_platform::sync_dir(&paths.session_dir(id)?, horon_agent_platform::SyncMode::Full)?;
        Ok(())
    }
}

impl Session {
    pub(super) fn restore_local_runtime(
        &mut self,
        paths: &horon_agent_session_store::SessionPaths,
        id: &horon_agent_session_store::SessionId,
    ) -> Result<()> {
        match std::fs::read(paths.session_dir(id)?.join("local-runtime.json")) {
            Ok(bytes) => {
                let runtime: LocalRuntime = serde_json::from_slice(&bytes)?;
                self.status = runtime.status;
                self.last_pid = runtime.last_pid;
                self.last_active_at = runtime.last_active_at;
                if let Some(epoch) = runtime.migration_epoch {
                    self.working_dir = runtime.working_dir;
                    self.migration_epoch = epoch;
                    self.provider_session_id = runtime.provider_session_id;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // An offline local migration receipt identifies local provenance.
                // Raw replica events, unlike that receipt, cannot select execution cwd/epoch.
                if !paths
                    .session_dir(id)?
                    .join("legacy-source-manifest.json")
                    .try_exists()?
                {
                    self.working_dir = None;
                    self.migration_epoch = 0;
                    self.provider_session_id = None;
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub(super) fn event_metadata(&self) -> Result<SessionMetadata> {
        let mut value = serde_json::to_value(self.journal_meta())?;
        let map = value.as_object_mut().context("metadata is not an object")?;
        for key in ["compaction", "status", "last_pid", "last_active_at", "is_debug"] {
            map.remove(key);
        }
        map.insert("created_at".into(), json!(self.created_at));
        map.insert("route_api_method".into(), json!(self.route_api_method));
        map.insert("migration_epoch".into(), json!(self.migration_epoch));
        map.insert("legacy_extensions".into(), json!(self.legacy_extensions));
        let mut metadata: SessionMetadata = serde_json::from_value(value)?;
        // Local execution bindings are not metadata edits. Preserve a remote
        // explicit NULL (and all other canonical provenance) until the caller
        // actually changes the corresponding local value.
        if let (Some(source), Some(binding)) = (&self.event_state.source_metadata, &self.event_state.local_binding) {
            if self.working_dir == binding.working_dir {
                metadata.working_dir = source.working_dir.clone();
            }
            if binding.migration_epoch == Some(self.migration_epoch) {
                metadata.migration_epoch = source.migration_epoch;
            }
            if self.provider_session_id == binding.provider_session_id {
                metadata.provider_session_id = source.provider_session_id.clone();
            }
        }
        Ok(metadata)
    }

    pub(super) fn auxiliary_value(&self) -> Value {
        let mut value = serde_json::Map::new();
        if let Some(source) = &self.event_state.fork_source {
            value.insert("fork_provenance".into(), source.clone());
        }
        if !self.persist_state.transcript_stripped {
            if !self.event_state.auxiliary_stripped {
                value.insert("env_snapshots".into(), json!(self.env_snapshots));
                value.insert("memory_injections".into(), json!(self.memory_injections));
                value.insert("replay_events".into(), json!(self.replay_events));
                value.insert("stream_rule_ledger".into(), json!(self.stream_rule_ledger));
            }
            value.insert("compaction".into(), json!(self.compaction));
        }
        if !self.persist_state.transcript_stripped
            && !self.event_state.auxiliary_stripped
            && !self.pruned_tool_results.is_empty()
        {
            value.insert("pruned_tool_results".into(), json!(self.pruned_tool_results));
        }
        value.insert("is_debug".into(), json!(self.is_debug));
        value.insert(
            "runtime_history".into(),
            json!({
                "status":self.status, "last_pid":self.last_pid, "last_active_at":self.last_active_at
            }),
        );
        Value::Object(value)
    }
}

impl Serialize for Session {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let mut value = serde_json::to_value(self.journal_meta()).map_err(S::Error::custom)?;
        let map = value
            .as_object_mut()
            .ok_or_else(|| S::Error::custom("invalid session metadata"))?;
        map.extend(self.legacy_extensions.clone());
        map.insert("id".into(), json!(self.id));
        map.insert("created_at".into(), json!(self.created_at));
        map.insert("route_api_method".into(), json!(self.route_api_method));
        map.insert("migration_epoch".into(), json!(self.migration_epoch));
        map.insert(
            "messages".into(),
            json!(self.history_messages().map_err(S::Error::custom)?),
        );
        map.insert("env_snapshots".into(), json!(self.env_snapshots));
        map.insert("memory_injections".into(), json!(self.memory_injections));
        map.insert("replay_events".into(), json!(self.replay_events));
        value.serialize(serializer)
    }
}
