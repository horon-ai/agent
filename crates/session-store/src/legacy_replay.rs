//! In-memory legacy replay. Deliberately never calls legacy healing writers.
use crate::legacy::{LegacyDiagnostic, LegacyRead, LegacySource, corrupt};
use crate::legacy_bundle::Bundle;
use crate::legacy_parse::{AUX, Entry, entry, message, metadata, salvage, snapshot};
use crate::*;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

pub(crate) struct Replay {
    pub metadata: SessionMetadata,
    pub messages: Vec<Value>,
    pub auxiliary: Map<String, Value>,
    pub compaction: Option<Value>,
    pub archived: bool,
    pub diagnostics: Vec<LegacyDiagnostic>,
}

pub(crate) fn replay(source: &LegacySource, bundle: &Bundle) -> Result<Replay, StoreError> {
    let id = source.id.as_str();
    let primary = bundle.files.get(&format!("sessions/{id}.json"));
    let mut diagnostics = Vec::new();
    let initial = match primary {
        Some(bytes) => snapshot(bytes, &source.id),
        None if bundle.has_index() && cfg!(feature = "legacy-session-index-import") => {
            // Offline index-only rows represent metadata, never an invented transcript.
            Ok(crate::legacy_parse::Snapshot {
                id: id.to_owned(),
                messages: Vec::new(),
                meta: Map::new(),
            })
        }
        None => {
            return Err(StoreError::unknown_session("primary legacy snapshot absent"));
        }
    };
    let mut state = match initial {
        Ok(state) => state,
        Err(primary_error) => match bundle.files.get(&format!("sessions/{id}.bak")) {
            Some(raw) => {
                let state = snapshot(raw, &source.id)?;
                diagnostics.push(LegacyDiagnostic::BackupRecovered);
                state
            }
            None => return Err(primary_error),
        },
    };
    bundle.guard_snapshot(source, &Value::Object(state.meta.clone()))?;
    let snapshot_time = metadata(&state.meta)?.updated_at;
    let mut seen = BTreeSet::new();
    let mut unique = Vec::new();
    let mut duplicates = 0;
    for raw in state.messages {
        if seen.insert(message(&raw)?.id) {
            unique.push(raw);
        } else {
            duplicates += 1;
        }
    }
    state.messages = unique;
    let mut stale_prefix = true;
    let mut auxiliary_history = Vec::new();
    auxiliary_history.push(Value::Object(
        state
            .meta
            .iter()
            .filter(|(key, _)| AUX.contains(&key.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    ));
    if let Some(raw) = bundle.files.get(&format!("sessions/{id}.journal.jsonl")) {
        let text = std::str::from_utf8(raw).map_err(|e| corrupt(e.to_string()))?;
        for (line, text) in text.lines().enumerate().filter(|(_, s)| !s.trim().is_empty()) {
            let entries = match serde_json::from_str::<Entry>(text.trim())
                .map_err(|e| corrupt(e.to_string()))
                .and_then(entry)
            {
                Ok(e) => vec![e],
                Err(_) => {
                    let recovered = salvage(text.trim());
                    diagnostics.push(LegacyDiagnostic::JournalDamage {
                        line: line + 1,
                        salvaged: recovered.len(),
                    });
                    recovered
                }
            };
            for e in entries {
                let updated: DateTime<Utc> =
                    serde_json::from_value(e.meta["updated_at"].clone()).map_err(|error| corrupt(error.to_string()))?;
                let ids = e
                    .append_messages
                    .iter()
                    .map(message)
                    .map(|r| r.map(|m| m.id))
                    .collect::<Result<Vec<_>, _>>()?;
                let stale = stale_prefix
                    && if ids.is_empty() {
                        updated <= snapshot_time
                    } else {
                        ids.iter().all(|id| seen.contains(id))
                    };
                if stale {
                    diagnostics.push(LegacyDiagnostic::StaleJournalEntry { line: line + 1 });
                    continue;
                }
                stale_prefix = false;
                auxiliary_history.push(Value::Object(
                    e.meta
                        .iter()
                        .filter(|(k, _)| AUX.contains(&k.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ));
                // Optional fields absent from a full journal meta deserialize to defaults.
                for key in [
                    "parent_id",
                    "title",
                    "custom_title",
                    "system_prompt",
                    "compaction",
                    "provider_session_id",
                    "model_usage_turn_id",
                    "provider_key",
                    "model",
                    "reasoning_effort",
                    "subagent_model",
                    "improve_mode",
                    "autoreview_enabled",
                    "autojudge_enabled",
                    "testing_build",
                    "working_dir",
                    "short_name",
                    "last_pid",
                    "last_active_at",
                    "save_label",
                ] {
                    state.meta.insert(key.into(), Value::Null);
                }
                for key in ["is_canary", "is_debug", "saved"] {
                    state.meta.insert(key.into(), json!(false));
                }
                state.meta.insert("agent_model_overrides".into(), json!({}));
                state.meta.extend(e.meta);
                for (raw, id) in e.append_messages.into_iter().zip(ids) {
                    if seen.insert(id) {
                        state.messages.push(raw);
                    } else {
                        duplicates += 1;
                    }
                }
                for (key, values) in [
                    ("env_snapshots", e.append_env_snapshots),
                    ("memory_injections", e.append_memory_injections),
                    ("replay_events", e.append_replay_events),
                ] {
                    let items = state
                        .meta
                        .entry(key)
                        .or_insert(json!([]))
                        .as_array_mut()
                        .ok_or_else(|| corrupt("legacy vector is not an array"))?;
                    items.extend(values);
                }
                if !e.extra.is_empty() {
                    auxiliary_history.push(json!({"journal_extensions":e.extra}));
                }
            }
        }
    }
    if duplicates > 0 {
        diagnostics.push(LegacyDiagnostic::DuplicateMessages { count: duplicates });
    }
    let mut auxiliary: Map<String, Value> = state
        .meta
        .iter()
        .filter(|(k, _)| AUX.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    auxiliary.insert("historical_auxiliary".into(), json!(auxiliary_history));
    let mut archived = false;
    for (name, bytes) in &bundle.files {
        if name == "sdk-archive.json" || name.starts_with("todos/") || name.ends_with(&format!("edit-stats/{id}.json"))
        {
            let value: Value = serde_json::from_slice(bytes).map_err(|e| corrupt(format!("{name}: {e}")))?;
            if name == "sdk-archive.json" {
                archived = value["sessions"][id].as_u64().is_some();
            }
            auxiliary.insert(name.clone(), value);
        } else if name.contains(".pre-wipe-") || name.ends_with(".corrupt.jsonl") {
            diagnostics.push(LegacyDiagnostic::DiagnosticFile { name: name.clone() });
        }
    }
    bundle.guard_snapshot(source, &Value::Object(state.meta.clone()))?;
    Ok(Replay {
        metadata: metadata(&state.meta)?,
        messages: state.messages,
        compaction: state.meta.get("compaction").filter(|v| !v.is_null()).cloned(),
        auxiliary,
        archived,
        diagnostics,
    })
}

impl Replay {
    pub fn into_read(self) -> Result<LegacyRead, StoreError> {
        Ok(LegacyRead {
            metadata: self.metadata,
            messages: self.messages.iter().map(message).collect::<Result<_, _>>()?,
            auxiliary: Value::Object(self.auxiliary),
            diagnostics: self.diagnostics,
        })
    }
}
