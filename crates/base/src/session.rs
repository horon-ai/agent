use crate::id::{extract_session_name, new_id, new_memorable_session_id_avoiding};
use crate::message::{ContentBlock, Message, Role};
pub use crate::storage::{
    SessionCounts, SessionPresence, active_session_ids, find_active_session_id_by_pid, mark_streaming, session_counts,
    session_presence, streaming_session_ids, unmark_streaming, user_session_counts, user_session_presence,
};
use crate::storage::{active_pids_dir, register_active_pid, unregister_active_pid};

/// RAII guard that marks a session as actively streaming for its lifetime.
///
/// Wraps the on-disk streaming marker from `horon-agent-storage` (cleared on every
/// exit path so presence UIs never show a phantom streaming session) and
/// additionally holds a macOS power assertion so the system does not
/// idle-sleep in the middle of a streaming model response.
pub struct StreamingGuard {
    _marker: crate::storage::StreamingGuard,
    #[allow(dead_code)]
    sleep_assertion: horon_agent_platform::PowerAssertion,
}

impl StreamingGuard {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            _marker: crate::storage::StreamingGuard::new(session_id),
            sleep_assertion: horon_agent_platform::PowerAssertion::prevent_user_idle_system_sleep(
                "HoronAgent streaming model response",
            ),
        }
    }

    /// Whether this guard currently holds a sleep-prevention assertion.
    ///
    /// Test-only: lets a test assert the in-process state directly instead of
    /// inferring it from the machine-global `pmset` table.
    #[cfg(test)]
    pub fn assertion_is_active(&self) -> bool {
        self.sleep_assertion.is_active()
    }

    /// Release the sleep-prevention assertion through the same path `Drop`
    /// uses, so a test can observe the release on a live value (a dropped
    /// value cannot be queried).
    #[cfg(test)]
    pub fn release_sleep_assertion(&mut self) {
        self.sleep_assertion.release();
    }
}
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

mod agent_models;
mod crash;
pub use agent_models::AgentModelOverrides;
mod event_actor;
mod event_archive;
mod event_cache;
mod event_commit;
mod event_compaction;
mod event_load;
mod event_origin;
mod event_pool;
mod event_replication;
pub use event_replication::{SessionEventJob, SessionGeneration, event_store_api};
mod event_save;
mod event_store;
mod event_tools;
mod event_views;
mod event_wire;
mod journal;
mod load_telemetry;
mod maintenance;
mod memory_profile;
mod model;
mod persistence;
mod render;
mod resume_marker;
mod storage_paths;
pub use crash::{
    CrashedSessionsInfo, detect_crashed_sessions, find_recent_crashed_sessions, find_session_by_name_or_id,
    recover_crashed_sessions, recover_crashed_sessions_by_ids,
};
pub use horon_agent_session_types::{
    EnvSnapshot, GitState, SessionImproveMode, SessionStatus, StoredCompactionState, StoredDisplayRole,
    StoredMemoryInjection, StoredMessage, StoredNativeRoute, StoredRecoveryBlocked, StoredRestorationEntry,
    StoredRestorationManifest, StoredSummaryMetadata, StoredTokenUsage,
};
use journal::{PersistVectorMode, SessionJournalMeta, SessionPersistState};
pub use maintenance::prune_old_session_backups;
pub use memory_profile::SessionMemoryProfileSnapshot;
use memory_profile::{
    ContentBlockMemoryStats, RESIDENT_TAIL_BUDGET_BYTES, SessionMemoryProfileCache, summarize_blocks,
    summarize_message_content,
};
use model::SESSION_CONTEXT_PREFIX;
pub use model::{StoredReplayEvent, StoredReplayEventKind};
pub use render::{
    RenderedCompactedHistoryInfo, RenderedImage, RenderedImageAnchor, RenderedImageSource, RenderedMessage,
    has_rendered_images, is_attached_image_label_text, is_compacted_history_notice, reasoning_line_content,
    reasoning_line_markup, render_images, render_messages, render_messages_and_images,
    render_messages_and_images_with_compacted_history, summarize_tool_calls,
};
pub use storage_paths::session_journal_path_from_snapshot;
#[cfg(test)]
pub(crate) use storage_paths::session_path_in_dir;
pub use storage_paths::{discard_session_original, session_exists, session_journal_path, session_path, store_paths};
use storage_paths::{estimate_json_bytes, persist_vector_mode_label};
pub use {horon_agent_session_types::prompt_title, persistence::drain_saves_for_shutdown};

fn stored_messages_to_messages(messages: &[StoredMessage]) -> Vec<Message> {
    messages.iter().map(StoredMessage::to_message).collect()
}

fn message_has_tool_image(content: &[ContentBlock]) -> bool {
    content.iter().any(|block| matches!(block, ContentBlock::Image { .. }))
        && content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

fn is_internal_system_reminder_message(message: &StoredMessage) -> bool {
    message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.trim_start()),
            _ => None,
        })
        .is_some_and(|text| text.starts_with("<system-reminder>"))
}

fn is_visible_conversation_message(message: &StoredMessage) -> bool {
    message.display_role.is_none()
        && !is_internal_system_reminder_message(message)
        && !is_scheduled_task_message(message)
}

/// Recognize scheduler prompts persisted before they received an explicit
/// system display role. This keeps old sessions from treating them as user
/// prompts after resume.
pub fn is_scheduled_task_message(message: &StoredMessage) -> bool {
    message.role == Role::User
        && message.content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text, .. } if text.trim_start().starts_with("[Scheduled task]\n"))
        })
}

/// Identity of the inputs the cached provider projection was built from.
///
/// The cache is a derived copy of `messages`, so it is only reusable while
/// every input that shapes the projection still matches: the transcript
/// revision (any append, rewrite, truncate or in-place revision), the message
/// count, the route the request would be sent on, the image handling flags, and
/// the compaction generation. A mismatch means the cached bytes are stale and
/// the next read must rebuild.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProviderMessagesCacheKey {
    revision: u64,
    message_len: usize,
    route_digest: u64,
    compaction_generation: u64,
    strip_images: bool,
    image_budget: Option<usize>,
}

impl ProviderMessagesCacheKey {
    /// Stable 64-bit fingerprint of every key component.
    fn digest(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut feed = |value: u64| {
            hash ^= value;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        feed(self.revision);
        feed(self.message_len as u64);
        feed(self.route_digest);
        feed(self.compaction_generation);
        feed(u64::from(self.strip_images));
        feed(self.image_budget.map_or(u64::MAX, |budget| budget as u64));
        hash
    }
}

/// Fingerprint of the provider route a request would be sent on.
fn provider_route_digest(model: Option<&str>, route_api_method: Option<&str>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut feed = |value: u64| {
        hash ^= value;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for text in [model, route_api_method] {
        let bytes = text.unwrap_or("").as_bytes();
        feed(bytes.len() as u64);
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            feed(u64::from_le_bytes(word));
        }
    }
    hash
}

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    messages: Vec<StoredMessage>,
    #[serde(skip)]
    event_state: event_store::EventState,
    #[serde(default, flatten)]
    legacy_extensions: BTreeMap<String, serde_json::Value>,
    /// Full assembled system prompt replacement, including an intentionally empty prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Durable logical input turn identity for per-route usage deduplication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_usage_turn_id: Option<String>,
    /// Persisted compacted-view state so reload/resume can continue using the
    /// active summary + recent tail instead of re-sending the full transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<StoredCompactionState>,
    /// Cache lineage root shared by one fork family.
    ///
    /// A root session gets a fresh UUID; a fork inherits its parent value. A session
    /// saved before this field existed derives it once from its verified parent chain,
    /// or from its own id when no parent can be read, and keeps it on the next save.
    /// Deliberately separate from `provider_session_id`, which names an upstream
    /// resume handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_root_id: Option<String>,
    /// Provider-specific session ID (e.g., Claude Code CLI session for resume)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// Stable provider/profile key for session-source filtering (e.g. "openai",
    /// "opencode", "opencode-go").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,

    /// Legacy native plugin backend id.
    ///
    /// Sessions and journals written before the plugin runtime was removed
    /// still carry this field. It is read for deserialization compatibility and
    /// intentionally ignored: the direct OpenAI-compatible provider owns the
    /// model path now. Kept separate from `provider_key`, which keeps its
    /// provider/profile classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Model identifier for this session (e.g., "gpt-5.2-codex")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// API method/runtime route used to select this model (e.g. "openrouter",
    /// "openai-compatible:nvidia-nim", "openai-api").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_api_method: Option<String>,
    /// Provider reasoning/thinking effort for this session (e.g., OpenAI low|medium|high|xhigh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Optional fixed model to use for subagents launched from this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_model: Option<String>,
    /// Session worker routing. Missing keys follow saved global defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_model_overrides: AgentModelOverrides,
    /// Last requested `/improve` mode for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub improve_mode: Option<SessionImproveMode>,
    /// Whether automatic end-of-turn review is enabled for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoreview_enabled: Option<bool>,
    /// Whether automatic end-of-turn judging is enabled for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autojudge_enabled: Option<bool>,
    /// Working directory (for self-dev detection)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Reminder waiting for the next prompt, set when the client's additional
    /// directories changed after the session context was captured. Host-local:
    /// a reload re-derives it from the list the client sends with the new
    /// attachment.
    #[serde(skip)]
    pending_additional_dirs_reminder: Option<String>,
    /// Memorable short name (e.g., "fox", "oak")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    /// Session exit status - why it ended (if not active)
    #[serde(default)]
    pub status: SessionStatus,
    /// PID of the process that last owned this session (for crash detection)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pid: Option<u32>,
    /// Last time the session was marked active
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<DateTime<Utc>>,
    /// Whether this is a debug/test session (created via debug socket)
    #[serde(default)]
    pub is_debug: bool,
    /// Whether this session has been saved/bookmarked by the user
    #[serde(default)]
    pub saved: bool,
    /// Optional user-provided label for saved sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_label: Option<String>,
    /// Environment snapshots for post-mortem debugging
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_snapshots: Vec<EnvSnapshot>,
    /// Memory injection events (for replay visualization)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_injections: Vec<StoredMemoryInjection>,
    /// Non-conversation UI/state events persisted for higher-fidelity replay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replay_events: Vec<StoredReplayEvent>,
    /// Tool results whose provider-facing text was replaced by a pruning marker.
    ///
    /// Keyed by tool-use id. The transcript keeps the original output, so the
    /// ledger only narrows what a request sends; clearing it restores the full
    /// text on the next request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pruned_tool_results: BTreeMap<String, horon_agent_compaction_core::pruning::PruneKind>,
    /// Last user-turn index on which each `[[stream_rules]]` rule fired, for
    /// the `once` and `after-gap` repeat policies.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stream_rule_ledger: BTreeMap<String, u32>,
    /// Migration epoch of the machine move that delivered this copy
    /// (`horon-agent cloud move` / `return`). Zero for sessions that never moved.
    #[serde(default)]
    pub migration_epoch: u64,
    #[serde(skip)]
    persist_state: SessionPersistState,
    #[serde(skip)]
    provider_messages_cache: Vec<Message>,
    #[serde(skip)]
    provider_message_prefix_hashes_cache: Vec<u64>,
    #[serde(skip)]
    provider_messages_cache_len: usize,
    #[serde(skip)]
    provider_messages_cache_mode: PersistVectorMode,
    /// Inputs the cached provider projection was built from.
    #[serde(skip)]
    provider_messages_cache_key: ProviderMessagesCacheKey,
    /// Bumped by every transcript mutation that can change the projection.
    #[serde(skip)]
    provider_messages_revision: u64,
    /// Compaction generation the current transcript state was produced under.
    #[serde(skip)]
    provider_messages_compaction_generation: u64,
    /// How many times the provider projection had to be built from history.
    /// Monotonic for the lifetime of the session; never reset by cache release.
    #[serde(skip)]
    provider_context_materializations: u64,
    /// How many times a caller took an owned copy of the provider projection.
    #[serde(skip)]
    provider_context_clones: u64,
    #[serde(skip)]
    memory_profile_cache: SessionMemoryProfileCache,
    #[serde(skip)]
    memory_profile_dirty: bool,
}

/// Max number of environment snapshots to retain per session
const MAX_ENV_SNAPSHOTS: usize = 8;

/// Session extension key holding the additional directories the attached client
/// advertised. It stays an extension so a build that predates it reads and
/// preserves the key instead of refusing the session.
const ADDITIONAL_DIRS_EXTENSION: &str = "additional_dirs";

fn current_working_dir_string() -> Option<String> {
    std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string())
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let trimmed = v.trim();
            !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn default_is_test_session() -> bool {
    env_flag_enabled("HORON_AGENT_TEST_SESSION")
}

pub fn derive_session_provider_key(provider_name: &str) -> Option<String> {
    let normalized_name = provider_name.trim().to_ascii_lowercase();
    if normalized_name == "horon-agent" {
        return Some("horon-agent".to_string());
    }
    if let Ok(runtime_provider) = std::env::var("HORON_AGENT_RUNTIME_PROVIDER") {
        let runtime_provider = runtime_provider.trim().to_ascii_lowercase();
        if !runtime_provider.is_empty() && runtime_provider != "openai-compatible" {
            return Some(runtime_provider);
        }
    }

    if let Ok(namespace) = std::env::var("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE") {
        let namespace = namespace.trim().to_ascii_lowercase();
        if !namespace.is_empty() {
            return Some(namespace);
        }
    }

    if let Ok(active) = std::env::var("HORON_AGENT_ACTIVE_PROVIDER") {
        let active = active.trim().to_ascii_lowercase();
        if !active.is_empty() {
            return Some(active);
        }
    }

    let fallback = match normalized_name.as_str() {
        "anthropic" | "claude" | "claude cli" => "claude",
        "openai" => "openai",
        "github copilot" | "copilot" => "copilot",
        "openrouter" => "openrouter",
        "cursor" => "cursor",
        "gemini" => "gemini",
        "antigravity" => "antigravity",
        "" => return None,
        other => other,
    };

    Some(fallback.to_string())
}

impl Session {
    pub fn debug_memory_profile(&self) -> serde_json::Value {
        let message_stats = summarize_message_content(self.messages.iter().map(|message| &message.content));

        let session_message_json_bytes: usize = self.messages.iter().map(estimate_json_bytes).sum();
        let provider_cache_stats =
            summarize_message_content(self.provider_messages_cache.iter().map(|message| &message.content));
        let provider_messages_cache_json_bytes: usize =
            self.provider_messages_cache.iter().map(estimate_json_bytes).sum();
        let env_snapshots_json_bytes: usize = self.env_snapshots.iter().map(estimate_json_bytes).sum();
        let memory_injections_json_bytes: usize = self.memory_injections.iter().map(estimate_json_bytes).sum();
        let replay_events_json_bytes: usize = self.replay_events.iter().map(estimate_json_bytes).sum();
        let compaction_json_bytes = self.compaction.as_ref().map(estimate_json_bytes).unwrap_or(0);
        let compaction_summary_bytes = self.compaction.as_ref().map(|c| c.summary_text.len()).unwrap_or(0);
        let compaction_encrypted_bytes = self
            .compaction
            .as_ref()
            .and_then(|c| c.openai_encrypted_content.as_ref())
            .map(|text| text.len())
            .unwrap_or(0);

        serde_json::json!({
            "session_id": self.id,
            "messages": {
                "count": self.messages.len(),
                "json_bytes": session_message_json_bytes,
                "released_prefix_count": self.event_state.active_start,
                "resident_tail_budget_bytes": RESIDENT_TAIL_BUDGET_BYTES,
                "memory": message_stats.to_json(),
            },
            "compaction": {
                "present": self.compaction.is_some(),
                "covers_up_to_turn": self
                    .compaction
                    .as_ref()
                    .map(|c| c.covers_up_to_turn)
                    .unwrap_or(0),
                "original_turn_count": self
                    .compaction
                    .as_ref()
                    .map(|c| c.original_turn_count)
                    .unwrap_or(0),
                "compacted_count": self
                    .compaction
                    .as_ref()
                    .map(|c| c.compacted_count)
                    .unwrap_or(0),
                "json_bytes": compaction_json_bytes,
                "summary_text_bytes": compaction_summary_bytes,
                "encrypted_content_bytes": compaction_encrypted_bytes,
            },
            "env_snapshots": {
                "count": self.env_snapshots.len(),
                "json_bytes": env_snapshots_json_bytes,
            },
            "memory_injections": {
                "count": self.memory_injections.len(),
                "json_bytes": memory_injections_json_bytes,
            },
            "replay_events": {
                "count": self.replay_events.len(),
                "json_bytes": replay_events_json_bytes,
            },
            "provider_messages_cache": {
                "count": self.provider_messages_cache.len(),
                "source_len": self.provider_messages_cache_len,
                "mode": persist_vector_mode_label(self.provider_messages_cache_mode),
                "key": self.provider_context_cache_key(),
                "materializations": self.provider_context_materializations,
                "clones": self.provider_context_clones,
                "json_bytes": provider_messages_cache_json_bytes,
                "memory": provider_cache_stats.to_json(),
            },
            "totals": {
                "payload_text_bytes": message_stats.payload_text_bytes(),
                "json_bytes": session_message_json_bytes
                    + provider_messages_cache_json_bytes
                    + env_snapshots_json_bytes
                    + memory_injections_json_bytes
                    + replay_events_json_bytes
                    + compaction_json_bytes,
                "canonical_transcript_json_bytes": session_message_json_bytes,
                "provider_cache_json_bytes": provider_messages_cache_json_bytes,
                "canonical_tool_result_bytes": message_stats.tool_result_bytes,
                "provider_cache_tool_result_bytes": provider_cache_stats.tool_result_bytes,
                "canonical_large_blob_bytes": message_stats.large_block_bytes,
                "provider_cache_large_blob_bytes": provider_cache_stats.large_block_bytes,
            }
        })
    }

    fn journal_meta(&self) -> SessionJournalMeta {
        SessionJournalMeta {
            parent_id: self.parent_id.clone(),
            title: self.title.clone(),
            custom_title: self.custom_title.clone(),
            system_prompt: self.system_prompt.clone(),
            updated_at: self.updated_at,
            compaction: self.compaction.clone(),
            provider_session_id: self.provider_session_id.clone(),
            cache_root_id: self.cache_root_id.clone(),
            model_usage_turn_id: self.model_usage_turn_id.clone(),
            provider_key: self.provider_key.clone(),
            plugin_id: self.plugin_id.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            subagent_model: self.subagent_model.clone(),
            agent_model_overrides: self.agent_model_overrides.clone(),
            improve_mode: self.improve_mode,
            autoreview_enabled: self.autoreview_enabled,
            autojudge_enabled: self.autojudge_enabled,
            working_dir: self.working_dir.clone(),
            short_name: self.short_name.clone(),
            status: self.status.clone(),
            last_pid: self.last_pid,
            last_active_at: self.last_active_at,
            is_debug: self.is_debug,
            saved: self.saved,
            save_label: self.save_label.clone(),
        }
    }

    fn reset_persist_state(&mut self, snapshot_exists: bool) {
        self.persist_state = SessionPersistState {
            snapshot_exists,
            messages_len: self.messages.len(),
            messages_mode: PersistVectorMode::Clean,
            env_snapshots_mode: PersistVectorMode::Clean,
            memory_injections_mode: PersistVectorMode::Clean,
            replay_events_mode: PersistVectorMode::Clean,
            transcript_stripped: self.persist_state.transcript_stripped,
        };
    }

    fn reset_provider_messages_cache(&mut self) {
        self.provider_messages_cache.clear();
        self.provider_message_prefix_hashes_cache.clear();
        self.provider_messages_cache_len = 0;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
        self.memory_profile_cache.provider_cache_count = 0;
        self.memory_profile_cache.provider_cache_json_bytes = 0;
        self.memory_profile_cache.provider_cache_stats = ContentBlockMemoryStats::default();
    }

    /// Drop the derived provider-facing transcript once the current request has
    /// copied the messages it needs. Event originals remain on disk and the
    /// resident active context can rebuild this cache on the next provider call.
    ///
    /// Long-running server sessions otherwise keep two fully owned transcript
    /// copies while waiting on the network. Tool results and reasoning payloads
    /// can make that duplicate tens of MiB per active session.
    pub fn release_provider_messages_cache(&mut self) {
        self.provider_messages_cache = Vec::new();
        self.provider_message_prefix_hashes_cache = Vec::new();
        self.provider_messages_cache_len = 0;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
        self.memory_profile_cache.provider_cache_count = 0;
        self.memory_profile_cache.provider_cache_json_bytes = 0;
        self.memory_profile_cache.provider_cache_stats = ContentBlockMemoryStats::default();
    }

    fn push_provider_message_cache_entry(&mut self, message: Message) {
        let message_hash = crate::message::stable_message_hash(&message);
        let prefix_hash = self
            .provider_message_prefix_hashes_cache
            .last()
            .copied()
            .map(|prev| crate::message::extend_stable_hash(prev, message_hash))
            .unwrap_or(message_hash);
        self.memory_profile_cache.provider_cache_count += 1;
        self.memory_profile_cache.provider_cache_json_bytes += estimate_json_bytes(&message);
        self.memory_profile_cache
            .provider_cache_stats
            .merge_from(&summarize_blocks(&message.content));
        self.provider_messages_cache.push(message);
        self.provider_message_prefix_hashes_cache.push(prefix_hash);
    }

    fn mark_memory_profile_dirty(&mut self) {
        self.memory_profile_dirty = true;
    }

    fn rebuild_memory_profile_cache(&mut self) {
        let message_stats = summarize_message_content(self.messages.iter().map(|message| &message.content));
        let provider_cache_stats =
            summarize_message_content(self.provider_messages_cache.iter().map(|message| &message.content));

        self.memory_profile_cache = SessionMemoryProfileCache {
            messages_count: self.messages.len(),
            messages_json_bytes: self.messages.iter().map(estimate_json_bytes).sum(),
            message_stats,
            env_snapshots_count: self.env_snapshots.len(),
            env_snapshots_json_bytes: self.env_snapshots.iter().map(estimate_json_bytes).sum(),
            memory_injections_count: self.memory_injections.len(),
            memory_injections_json_bytes: self.memory_injections.iter().map(estimate_json_bytes).sum(),
            replay_events_count: self.replay_events.len(),
            replay_events_json_bytes: self.replay_events.iter().map(estimate_json_bytes).sum(),
            provider_cache_count: self.provider_messages_cache.len(),
            provider_cache_json_bytes: self.provider_messages_cache.iter().map(estimate_json_bytes).sum(),
            provider_cache_stats,
        };
        self.memory_profile_dirty = false;
    }

    fn ensure_memory_profile_cache(&mut self) {
        if self.memory_profile_dirty {
            self.rebuild_memory_profile_cache();
        }
    }

    pub fn memory_profile_snapshot(&mut self) -> SessionMemoryProfileSnapshot {
        self.ensure_memory_profile_cache();
        let compaction_json_bytes = self.compaction.as_ref().map(estimate_json_bytes).unwrap_or(0);

        SessionMemoryProfileSnapshot {
            message_count: self.memory_profile_cache.messages_count,
            released_prefix_message_count: self.event_state.active_start,
            resident_tail_budget_bytes: RESIDENT_TAIL_BUDGET_BYTES,
            provider_cache_message_count: self.memory_profile_cache.provider_cache_count,
            env_snapshot_count: self.memory_profile_cache.env_snapshots_count,
            memory_injection_count: self.memory_profile_cache.memory_injections_count,
            replay_event_count: self.memory_profile_cache.replay_events_count,
            payload_text_bytes: self.memory_profile_cache.message_stats.payload_text_bytes(),
            total_json_bytes: self.memory_profile_cache.messages_json_bytes
                + self.memory_profile_cache.provider_cache_json_bytes
                + self.memory_profile_cache.env_snapshots_json_bytes
                + self.memory_profile_cache.memory_injections_json_bytes
                + self.memory_profile_cache.replay_events_json_bytes
                + compaction_json_bytes,
            provider_cache_json_bytes: self.memory_profile_cache.provider_cache_json_bytes,
            canonical_tool_result_bytes: self.memory_profile_cache.message_stats.tool_result_bytes,
            provider_cache_tool_result_bytes: self.memory_profile_cache.provider_cache_stats.tool_result_bytes,
            canonical_large_blob_bytes: self.memory_profile_cache.message_stats.large_block_bytes,
            provider_cache_large_blob_bytes: self.memory_profile_cache.provider_cache_stats.large_block_bytes,
        }
    }

    fn mark_messages_append_dirty(&mut self) {
        self.provider_messages_revision = self.provider_messages_revision.wrapping_add(1);
        if self.persist_state.messages_mode != PersistVectorMode::Full {
            self.persist_state.messages_mode = PersistVectorMode::Append;
        }
        if self.provider_messages_cache_mode != PersistVectorMode::Full {
            self.provider_messages_cache_mode = PersistVectorMode::Append;
        }
    }

    fn mark_messages_full_dirty(&mut self) {
        self.provider_messages_revision = self.provider_messages_revision.wrapping_add(1);
        self.persist_state.messages_mode = PersistVectorMode::Full;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
    }

    fn mark_env_snapshots_append_dirty(&mut self) {
        if self.persist_state.env_snapshots_mode != PersistVectorMode::Full {
            self.persist_state.env_snapshots_mode = PersistVectorMode::Append;
        }
    }

    fn mark_env_snapshots_full_dirty(&mut self) {
        self.persist_state.env_snapshots_mode = PersistVectorMode::Full;
    }

    fn mark_memory_injections_append_dirty(&mut self) {
        if self.persist_state.memory_injections_mode != PersistVectorMode::Full {
            self.persist_state.memory_injections_mode = PersistVectorMode::Append;
        }
    }

    fn mark_replay_events_append_dirty(&mut self) {
        if self.persist_state.replay_events_mode != PersistVectorMode::Full {
            self.persist_state.replay_events_mode = PersistVectorMode::Append;
        }
    }

    pub fn create_with_id(session_id: String, parent_id: Option<String>, title: Option<String>) -> Self {
        let now = Utc::now();
        let is_debug = default_is_test_session();
        // Try to extract short name from ID if it's a memorable ID
        let short_name = extract_session_name(&session_id).map(|s| s.to_string());
        let cache_root_id = root_cache_id(parent_id.is_none());
        let mut session = Self {
            id: session_id,
            parent_id,
            title,
            custom_title: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            event_state: event_store::EventState::default(),
            legacy_extensions: BTreeMap::new(),
            system_prompt: None,
            model_usage_turn_id: None,
            compaction: None,
            provider_session_id: None,
            cache_root_id,
            provider_key: None,
            plugin_id: None,
            model: None,
            route_api_method: None,
            reasoning_effort: None,
            subagent_model: None,
            agent_model_overrides: AgentModelOverrides::new(),
            improve_mode: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            working_dir: current_working_dir_string(),
            pending_additional_dirs_reminder: None,
            short_name,
            status: SessionStatus::Active,
            last_pid: Some(std::process::id()),
            last_active_at: Some(now),
            is_debug,
            saved: false,
            save_label: None,
            env_snapshots: Vec::new(),
            memory_injections: Vec::new(),
            replay_events: Vec::new(),
            pruned_tool_results: BTreeMap::new(),
            stream_rule_ledger: BTreeMap::new(),
            migration_epoch: 0,
            persist_state: SessionPersistState::default(),
            provider_messages_cache: Vec::new(),
            provider_message_prefix_hashes_cache: Vec::new(),
            provider_messages_cache_len: 0,
            provider_messages_cache_mode: PersistVectorMode::Full,
            provider_messages_cache_key: ProviderMessagesCacheKey::default(),
            provider_messages_revision: 0,
            provider_messages_compaction_generation: 0,
            provider_context_materializations: 0,
            provider_context_clones: 0,
            memory_profile_cache: SessionMemoryProfileCache::default(),
            memory_profile_dirty: false,
        };
        session.reset_persist_state(false);
        session
    }

    pub fn create(parent_id: Option<String>, title: Option<String>) -> Self {
        let now = Utc::now();
        // Keep memorable identities distinct across all currently active
        // sessions. This naturally covers swarm members and survives a server
        // reload because active PID markers retain their encoded short names.
        let used_names = active_session_ids()
            .into_iter()
            .filter_map(|session_id| extract_session_name(&session_id).map(str::to_string))
            .collect::<HashSet<_>>();
        let (id, short_name) = new_memorable_session_id_avoiding(&used_names);
        let is_debug = default_is_test_session();
        let cache_root_id = root_cache_id(parent_id.is_none());
        let mut session = Self {
            id,
            parent_id,
            title,
            custom_title: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            event_state: event_store::EventState::default(),
            legacy_extensions: BTreeMap::new(),
            system_prompt: None,
            model_usage_turn_id: None,
            compaction: None,
            provider_session_id: None,
            cache_root_id,
            provider_key: None,
            plugin_id: None,
            model: None,
            route_api_method: None,
            reasoning_effort: None,
            subagent_model: None,
            agent_model_overrides: AgentModelOverrides::new(),
            improve_mode: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            working_dir: current_working_dir_string(),
            pending_additional_dirs_reminder: None,
            short_name: Some(short_name),
            status: SessionStatus::Active,
            last_pid: Some(std::process::id()),
            last_active_at: Some(now),
            is_debug,
            saved: false,
            save_label: None,
            env_snapshots: Vec::new(),
            memory_injections: Vec::new(),
            replay_events: Vec::new(),
            pruned_tool_results: BTreeMap::new(),
            stream_rule_ledger: BTreeMap::new(),
            migration_epoch: 0,
            persist_state: SessionPersistState::default(),
            provider_messages_cache: Vec::new(),
            provider_message_prefix_hashes_cache: Vec::new(),
            provider_messages_cache_len: 0,
            provider_messages_cache_mode: PersistVectorMode::Full,
            provider_messages_cache_key: ProviderMessagesCacheKey::default(),
            provider_messages_revision: 0,
            provider_messages_compaction_generation: 0,
            provider_context_materializations: 0,
            provider_context_clones: 0,
            memory_profile_cache: SessionMemoryProfileCache::default(),
            memory_profile_dirty: false,
        };
        session.reset_persist_state(false);
        session
    }

    /// Mark this session as a debug/test session
    pub fn set_debug(&mut self, is_debug: bool) {
        self.is_debug = is_debug;
        // Debug status can change after activation (e.g. debug-socket created
        // sessions); keep presence UIs in sync when we are the active owner.
        if self.status == SessionStatus::Active {
            self.sync_internal_presence_flag();
        }
    }

    /// Save/bookmark this session with an optional label.
    ///
    /// A label is the name the user chose for the session, so it also becomes
    /// the session's display title everywhere sessions are listed.
    pub fn mark_saved(&mut self, label: Option<String>) {
        self.saved = true;
        let label = label.and_then(|label| {
            let label = label.trim();
            (!label.is_empty()).then(|| label.to_string())
        });
        if let Some(label) = label {
            self.custom_title = Some(label.clone());
            self.save_label = Some(label);
            self.updated_at = Utc::now();
        }
    }

    /// Remove the saved/bookmark status
    pub fn unmark_saved(&mut self) {
        self.saved = false;
        self.save_label = None;
    }

    /// Set or clear the user-provided display title.
    ///
    /// This intentionally does not change the immutable session id, memorable
    /// short name, generated title, provider session id, or saved/bookmark label.
    pub fn rename_title(&mut self, title: Option<String>) {
        self.custom_title = title.and_then(|title| {
            let title = title.trim();
            (!title.is_empty()).then(|| title.to_string())
        });
        self.updated_at = Utc::now();
    }

    /// Get the title users should see for this session: custom rename first,
    /// then the generated/imported title, if one exists.
    pub fn display_title(&self) -> Option<&str> {
        fn non_empty_trimmed(title: Option<&str>) -> Option<&str> {
            title.map(str::trim).filter(|title| !title.is_empty())
        }

        non_empty_trimmed(self.custom_title.as_deref())
            .or_else(|| {
                // Bookmarks labelled before labels doubled as titles.
                self.saved
                    .then(|| non_empty_trimmed(self.save_label.as_deref()))
                    .flatten()
            })
            .or_else(|| non_empty_trimmed(self.title.as_deref()))
    }

    /// Get a visible label for title-oriented surfaces, falling back to the
    /// memorable session name when there is no generated or custom title.
    pub fn display_title_or_name(&self) -> &str {
        self.display_title().unwrap_or_else(|| self.display_name())
    }

    /// Record an environment snapshot for post-mortem debugging
    pub fn record_env_snapshot(&mut self, snapshot: EnvSnapshot) {
        self.memory_profile_cache.env_snapshots_count += 1;
        self.memory_profile_cache.env_snapshots_json_bytes += estimate_json_bytes(&snapshot);
        self.env_snapshots.push(snapshot);
        if self.env_snapshots.len() > MAX_ENV_SNAPSHOTS {
            let excess = self.env_snapshots.len() - MAX_ENV_SNAPSHOTS;
            self.env_snapshots.drain(0..excess);
            self.mark_memory_profile_dirty();
            self.mark_env_snapshots_full_dirty();
        } else {
            self.mark_env_snapshots_append_dirty();
        }
    }

    /// Extra directories the attached client advertised for this session.
    ///
    /// The list lives in the session's inert extension map instead of a field of
    /// its own: the session store seals the metadata keys it knows, so a new
    /// first-class key would make every older build refuse to load the session.
    /// The extension map is what older builds already carry through untouched.
    pub fn additional_dirs(&self) -> Vec<String> {
        self.legacy_extensions
            .get(ADDITIONAL_DIRS_EXTENSION)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    /// Record the additional directories the attached client advertised.
    ///
    /// Answers whether the list changed. The immutable session context can still
    /// be rebuilt until the first real message; after that, the only way to tell
    /// the model about a changed list is the next prompt's system reminder, so
    /// the session queues it here.
    pub fn set_additional_dirs(&mut self, directories: Vec<String>) -> bool {
        if self.additional_dirs() == directories {
            return false;
        }
        if directories.is_empty() {
            self.legacy_extensions.remove(ADDITIONAL_DIRS_EXTENSION);
        } else {
            self.legacy_extensions
                .insert(ADDITIONAL_DIRS_EXTENSION.to_string(), serde_json::json!(directories));
        }
        if self.has_session_context_message() && !self.refresh_initial_session_context_message() {
            self.pending_additional_dirs_reminder =
                Some(crate::prompt::additional_directories_line(&self.additional_dirs()));
        }
        true
    }

    /// Take the queued additional-directories reminder, if one is waiting.
    ///
    /// Taking is what makes it one-time: the next prompt carries it and no later
    /// prompt does.
    pub fn take_pending_additional_dirs_reminder(&mut self) -> Option<String> {
        self.pending_additional_dirs_reminder.take()
    }

    pub fn has_session_context_message(&self) -> bool {
        self.messages.iter().any(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::Text { text, .. } => text.starts_with(SESSION_CONTEXT_PREFIX),
                _ => false,
            })
        })
    }

    /// Persist an immutable session-context snapshot as the first provider-visible
    /// transcript item for new sessions. Existing non-empty sessions are left
    /// untouched so their historical context is never rewritten with newer state.
    pub fn ensure_initial_session_context_message(&mut self) -> bool {
        if !self.messages.is_empty() || self.has_session_context_message() {
            return false;
        }

        // Preserve an explicitly bound session directory. Shared-server clients
        // provide their cwd before this message is created, and replacing it with
        // the daemon process cwd would leak the directory that launched the server.
        if self.working_dir.is_none() {
            self.working_dir = current_working_dir_string();
        }

        let additional_dirs = self.additional_dirs();
        let context =
            crate::prompt::build_session_context(self.working_dir.as_deref().map(Path::new), &additional_dirs);
        let wrapped = format!("<system-reminder>\n{}\n</system-reminder>", context.trim());
        self.add_message_with_display_role(
            Role::User,
            vec![ContentBlock::Text {
                text: wrapped,
                cache_control: None,
            }],
            Some(StoredDisplayRole::System),
        );
        true
    }

    /// Refresh the initial immutable session-context message if the session has
    /// not started a real conversation yet. This covers remote/client-server
    /// startup where the server creates an Agent before the subscribing client
    /// sends the terminal working directory that tools will use.
    pub fn refresh_initial_session_context_message(&mut self) -> bool {
        if self.messages.iter().any(is_visible_conversation_message) {
            return false;
        }

        let additional_dirs = self.additional_dirs();
        let context =
            crate::prompt::build_session_context(self.working_dir.as_deref().map(Path::new), &additional_dirs);
        let wrapped = format!("<system-reminder>\n{}\n</system-reminder>", context.trim());
        let Some(message) = self.messages.iter_mut().find(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::Text { text, .. } => text.starts_with(SESSION_CONTEXT_PREFIX),
                _ => false,
            })
        }) else {
            return false;
        };

        for block in &mut message.content {
            if let ContentBlock::Text { text, .. } = block
                && text.starts_with(SESSION_CONTEXT_PREFIX)
            {
                if *text == wrapped {
                    return false;
                }
                *text = wrapped;
                self.mark_memory_profile_dirty();
                self.mark_messages_full_dirty();
                return true;
            }
        }

        false
    }

    /// Get the display name for this session (short memorable name if available)
    pub fn display_name(&self) -> &str {
        self.short_name
            .as_deref()
            .or_else(|| extract_session_name(&self.id))
            .unwrap_or(&self.id)
    }

    /// Append a model-visible notice telling the agent this session is a fork
    /// of `parent_session_id`'s conversation.
    ///
    /// Forking happens when the user splits a window mid-conversation (often
    /// while the parent agent is still streaming) and points the new window at
    /// a clone of the transcript. Without this notice the forked agent assumes
    /// it owns the in-flight request, duplicating the parent's work. The
    /// notice is wrapped in `<system-reminder>` so it stays out of the visible
    /// transcript while still reaching the model on the next turn.
    pub fn append_fork_notice(&mut self, parent_session_id: &str, parent_display_name: &str) {
        let text = format!(
            "<system-reminder>\nThis session was forked (split) from session {parent} ({parent_id}) by the user. \
The full conversation above is inherited from that session, but the original agent in {parent} \
is still active and will continue handling whatever request or work was in progress there. \
Do NOT continue or duplicate that in-flight work here. Treat the next user message as a fresh \
request in this new forked session, using the inherited conversation only as context.\n</system-reminder>",
            parent = parent_display_name,
            parent_id = parent_session_id,
        );
        self.add_message_with_display_role(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
            Some(StoredDisplayRole::System),
        );
    }

    /// Set the session status
    pub fn set_status(&mut self, status: SessionStatus) {
        self.status = status;
    }

    /// Mark session as closed normally
    pub fn mark_closed(&mut self) {
        self.status = SessionStatus::Closed;
        unregister_active_pid(&self.id);
    }

    /// Mark session as crashed
    pub fn mark_crashed(&mut self, message: Option<String>) {
        self.status = SessionStatus::Crashed { message };
        unregister_active_pid(&self.id);
    }

    /// Mark session as having an error
    pub fn mark_error(&mut self, message: String) {
        self.status = SessionStatus::Error { message };
    }

    /// Why this in-memory copy may not run turns or persist on this machine
    /// because the session migrated (see `horon_agent_storage::session_lease`).
    pub fn migration_lease_block(&self) -> Option<crate::storage::SessionLeaseBlock> {
        crate::storage::session_lease_block(&self.id, self.migration_epoch)
    }

    /// Mark session as active (e.g., when resuming)
    pub fn mark_active(&mut self) {
        self.status = SessionStatus::Active;
        let pid = std::process::id();
        self.last_pid = Some(pid);
        self.last_active_at = Some(Utc::now());
        register_active_pid(&self.id, pid);
        self.sync_internal_presence_flag();
    }

    /// Mark session as active for a specific PID
    pub fn mark_active_with_pid(&mut self, pid: u32) {
        self.status = SessionStatus::Active;
        self.last_pid = Some(pid);
        self.last_active_at = Some(Utc::now());
        register_active_pid(&self.id, pid);
        self.sync_internal_presence_flag();
    }

    /// Keep the on-disk internal-session flag in sync with this session's
    /// role. Debug/test sessions and spawned children (swarm workers,
    /// subagents) are internal: they stay tracked for lifecycle purposes but
    /// are hidden from user-facing presence UIs like the menu bar (issue
    /// #508).
    fn sync_internal_presence_flag(&self) {
        let internal = self.is_debug || self.parent_id.is_some();
        crate::storage::set_session_internal(&self.id, internal);
    }

    /// Detect if an active session likely crashed (process no longer running)
    /// Returns true if status was updated.
    pub fn detect_crash(&mut self) -> bool {
        if self.status != SessionStatus::Active {
            return false;
        }

        if let Some(pid) = self.last_pid {
            if !crash::is_pid_running(pid) {
                self.mark_crashed(Some(format!(
                    "Process {} exited unexpectedly (no shutdown signal captured)",
                    pid
                )));
                return true;
            }
        } else {
            // No PID info (older sessions): fall back to age heuristic
            let age = Utc::now().signed_duration_since(self.updated_at);
            if age.num_seconds() > 120 {
                self.mark_crashed(Some("Stale active session (possible abrupt termination)".to_string()));
                return true;
            }
        }

        false
    }

    pub fn redacted_for_export(&self) -> anyhow::Result<Self> {
        let mut redacted = self.history_snapshot()?;
        if let Some(title) = redacted.title.as_mut() {
            *title = crate::message::redact_secrets(title);
        }
        if let Some(title) = redacted.custom_title.as_mut() {
            *title = crate::message::redact_secrets(title);
        }
        if let Some(compaction) = redacted.compaction.as_mut() {
            compaction.summary_text = crate::message::redact_secrets(&compaction.summary_text);
        }
        for msg in &mut redacted.messages {
            for block in &mut msg.content {
                match block {
                    ContentBlock::Text { text, .. }
                    | ContentBlock::Reasoning { text }
                    | ContentBlock::ReasoningTrace { text } => {
                        *text = crate::message::redact_secrets(text);
                    }
                    ContentBlock::AnthropicThinking { thinking, .. } => {
                        *thinking = crate::message::redact_secrets(thinking);
                    }
                    ContentBlock::OpenAIReasoning { summary, .. } => {
                        for item in summary {
                            *item = crate::message::redact_secrets(item);
                        }
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        *content = crate::message::redact_secrets(content);
                    }
                    ContentBlock::ToolUse { input, .. } => redact_json_value(input),
                    // Export copy only: the stored item stays verbatim for
                    // replay, but queries can carry pasted credentials.
                    ContentBlock::ProviderNative { item, .. } => redact_json_value(item),
                    ContentBlock::Image { .. } => {}
                    ContentBlock::OpenAICompaction { .. } | ContentBlock::ToolReference { .. } => {}
                }
            }
        }
        for event in &mut redacted.replay_events {
            match &mut event.kind {
                StoredReplayEventKind::DisplayMessage { title, content, .. } => {
                    if let Some(title) = title.as_mut() {
                        *title = crate::message::redact_secrets(title);
                    }
                    *content = crate::message::redact_secrets(content);
                }
                StoredReplayEventKind::SwarmStatus { members } => {
                    for member in members {
                        if let Some(detail) = member.detail.as_mut() {
                            *detail = crate::message::redact_secrets(detail);
                        }
                    }
                }
                StoredReplayEventKind::SwarmPlan { items, reason, .. } => {
                    if let Some(reason) = reason.as_mut() {
                        *reason = crate::message::redact_secrets(reason);
                    }
                    for item in items {
                        item.content = crate::message::redact_secrets(&item.content);
                    }
                }
                StoredReplayEventKind::PlanMode { .. } => {}
            }
        }
        Ok(redacted)
    }

    pub fn token_usage_totals(&self) -> crate::protocol::TokenUsageTotals {
        aggregate_usage(
            self.messages.iter(),
            self.event_state
                .prefix_usage
                .unwrap_or(crate::protocol::TokenUsageTotals {
                    cache_prompt_tokens: Some(0),
                    ..Default::default()
                }),
        )
    }

    pub fn add_message(&mut self, role: Role, content: Vec<ContentBlock>) -> String {
        self.add_message_ext_with_display_role(role, content, None, None, None)
    }

    pub fn add_message_with_duration(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        tool_duration_ms: Option<u64>,
    ) -> String {
        self.add_message_ext_with_display_role(role, content, tool_duration_ms, None, None)
    }

    pub fn add_message_with_display_role(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        display_role: Option<StoredDisplayRole>,
    ) -> String {
        self.add_message_ext_with_display_role(role, content, None, None, display_role)
    }

    pub fn add_message_ext(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        tool_duration_ms: Option<u64>,
        token_usage: Option<StoredTokenUsage>,
    ) -> String {
        self.add_message_ext_with_display_role(role, content, tool_duration_ms, token_usage, None)
    }

    pub fn add_message_ext_with_display_role(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        tool_duration_ms: Option<u64>,
        token_usage: Option<StoredTokenUsage>,
        display_role: Option<StoredDisplayRole>,
    ) -> String {
        self.push_new_message(role, content, tool_duration_ms, token_usage, display_role, false)
    }

    /// The message a turn starts with, recorded as its start
    /// ([`StoredMessage::turn_start`]).
    pub fn add_turn_start_message(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        display_role: Option<StoredDisplayRole>,
    ) -> String {
        self.push_new_message(role, content, None, None, display_role, true)
    }

    fn push_new_message(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        tool_duration_ms: Option<u64>,
        token_usage: Option<StoredTokenUsage>,
        display_role: Option<StoredDisplayRole>,
        turn_start: bool,
    ) -> String {
        let id = new_id("message");
        self.append_stored_message(StoredMessage {
            turn_start,
            id: id.clone(),
            role,
            content,
            display_role,
            timestamp: Some(Utc::now()),
            tool_duration_ms,
            token_usage,
        });
        id
    }

    pub fn append_stored_message(&mut self, message: StoredMessage) {
        self.memory_profile_cache.messages_count += 1;
        self.memory_profile_cache.messages_json_bytes += estimate_json_bytes(&message);
        self.memory_profile_cache
            .message_stats
            .merge_from(&summarize_blocks(&message.content));
        self.adopt_prompt_title(&message);
        let adds_tool_image = message_has_tool_image(&message.content);
        self.messages.push(message);
        self.mark_messages_append_dirty();
        if adds_tool_image {
            self.decay_old_tool_images();
        }
    }

    /// Replace older screenshots only in the provider projection. The original
    /// images stay in event history; the newest screenshots reach the provider.
    /// Returns the number of images replaced.
    pub fn decay_old_tool_images(&mut self) -> usize {
        self.provider_messages();
        let mut contents: Vec<&mut Vec<ContentBlock>> = self
            .provider_messages_cache
            .iter_mut()
            .map(|m| &mut m.content)
            .collect();
        let stripped = horon_agent_compaction_core::decay_old_tool_images_in_contents(&mut contents);
        if stripped > 0 {
            self.mark_memory_profile_dirty();
        }
        stripped
    }

    /// Name an untitled session after its first real user prompt so lists show
    /// something recognizable instead of a generic placeholder. Renames,
    /// bookmark labels, and todo goals still take precedence at display time.
    fn adopt_prompt_title(&mut self, message: &StoredMessage) {
        if self.title.is_some() || message.role != Role::User || !is_visible_conversation_message(message) {
            return;
        }
        self.title = message.content.iter().find_map(|block| match block {
            ContentBlock::Text { text, .. } => prompt_title(text),
            _ => None,
        });
    }

    /// Give sessions recorded before prompt titles existed the same fallback.
    pub(crate) fn backfill_prompt_title(&mut self) {
        if self.title.is_some() {
            return;
        }
        let first_prompt = self
            .messages
            .iter()
            .find(|message| message.role == Role::User && is_visible_conversation_message(message))
            .cloned();
        if let Some(message) = first_prompt {
            self.adopt_prompt_title(&message);
        }
    }

    pub fn insert_message(&mut self, index: usize, message: StoredMessage) {
        self.note_event_mutation();
        self.messages.insert(index, message);
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
    }

    pub fn replace_messages(&mut self, messages: Vec<StoredMessage>) {
        self.note_event_mutation();
        self.messages = messages;
        self.event_state.active_start = 0;
        self.event_state.prefix_usage = None;
        self.event_state.prefix_visible = 0;
        self.compaction = None;
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
    }

    pub fn truncate_messages(&mut self, len: usize) {
        if len < self.messages.len() {
            self.note_event_mutation();
            self.messages.truncate(len);
            self.mark_memory_profile_dirty();
            self.mark_messages_full_dirty();
        }
    }

    /// Drop the resident active-context messages whose ids are in `ids`.
    ///
    /// Returns how many were removed. The event log stays append-only: like
    /// [`truncate_messages`](Self::truncate_messages), this marks the current
    /// generation stale so the next save writes a new snapshot instead of
    /// rewriting the events already recorded.
    pub fn remove_messages_by_id(&mut self, ids: &[String]) -> usize {
        if ids.is_empty() {
            return 0;
        }
        let before = self.messages.len();
        self.messages.retain(|message| !ids.iter().any(|id| id == &message.id));
        let removed = before - self.messages.len();
        if removed > 0 {
            self.note_event_mutation();
            self.mark_memory_profile_dirty();
            self.mark_messages_full_dirty();
        }
        removed
    }

    /// Drop oversized inline images from the stored transcript, oldest-first,
    /// until the total remaining base64 image payload fits within
    /// `target_total_chars`. Used to recover from provider HTTP 413
    /// "request too large" errors, which are driven by base64 image payload size
    /// rather than the token context window.
    ///
    /// Changes the provider projection without revising any original message.
    /// Returns the number of images omitted from that projection.
    pub fn strip_oversized_images(&mut self, target_total_chars: usize) -> usize {
        self.provider_messages();
        let mut contents: Vec<&mut Vec<ContentBlock>> = self
            .provider_messages_cache
            .iter_mut()
            .map(|m| &mut m.content)
            .collect();
        self.event_state.image_budget = Some(target_total_chars);
        let stripped = horon_agent_compaction_core::strip_large_images_in_contents(&mut contents, target_total_chars);
        if stripped > 0 {
            self.mark_memory_profile_dirty();
        }
        stripped
    }

    pub fn visible_conversation_message_count(&self) -> usize {
        self.event_state.prefix_visible
            + self
                .messages
                .iter()
                .filter(|message| is_visible_conversation_message(message))
                .count()
    }

    pub fn visible_conversation_messages(&self) -> Vec<&StoredMessage> {
        self.messages
            .iter()
            .filter(|message| is_visible_conversation_message(message))
            .collect()
    }

    pub fn stored_len_for_visible_conversation_message(&self, visible_index: usize) -> Option<usize> {
        if visible_index == 0 {
            return None;
        }

        let mut count = 0usize;
        for (stored_index, message) in self.messages.iter().enumerate() {
            if is_visible_conversation_message(message) {
                count += 1;
                if count == visible_index {
                    return Some(stored_index + 1);
                }
            }
        }
        None
    }

    /// Stored-message indices of the rewind targets shown in the TUI's
    /// numbered `/rewind` list, in display order.
    ///
    /// The TUI numbers user/assistant *transcript entries* (what the user
    /// actually sees), not raw stored messages. Stored tool-result messages
    /// and tool-call-only assistant messages render as tool cards or nothing,
    /// so counting raw stored messages diverges wildly from the on-screen
    /// numbering in tool-heavy sessions (issue #432). Deriving targets from
    /// the same rendering used for the transcript keeps `/rewind N` aligned
    /// with the numbers `/rewind` prints.
    ///
    /// A single stored message can produce multiple transcript entries (text
    /// split around a tool result); each entry keeps its own number and maps
    /// to the same stored index so numbering matches the visible list exactly.
    pub fn rewind_target_stored_indices(&self) -> Vec<usize> {
        render_messages(self)
            .into_iter()
            .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
            .filter_map(|message| message.stored_index)
            .collect()
    }

    /// Number of `/rewind` targets (see [`Self::rewind_target_stored_indices`]).
    pub fn rewind_target_count(&self) -> usize {
        self.rewind_target_stored_indices().len()
    }

    /// Record a memory injection event for replay visualization
    pub fn record_memory_injection(
        &mut self,
        summary: String,
        content: String,
        count: u32,
        age_ms: u64,
        memory_ids: Vec<String>,
    ) {
        let injection = StoredMemoryInjection {
            summary,
            content,
            count,
            memory_ids,
            age_ms: Some(age_ms),
            before_message: Some(self.messages.len()),
            timestamp: Utc::now(),
        };
        self.memory_profile_cache.memory_injections_count += 1;
        self.memory_profile_cache.memory_injections_json_bytes += estimate_json_bytes(&injection);
        self.memory_injections.push(injection);
        self.mark_memory_injections_append_dirty();
    }

    pub fn injected_memory_ids(&self) -> Vec<String> {
        let mut ids = HashSet::new();
        for injection in &self.memory_injections {
            ids.extend(injection.memory_ids.iter().cloned());
        }
        ids.into_iter().collect()
    }

    pub fn record_replay_display_message(
        &mut self,
        role: impl Into<String>,
        title: Option<String>,
        content: impl Into<String>,
    ) {
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind: StoredReplayEventKind::DisplayMessage {
                role: role.into(),
                title,
                content: content.into(),
            },
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    pub fn record_swarm_status_event(&mut self, members: Vec<crate::protocol::SwarmMemberStatus>) {
        let kind = StoredReplayEventKind::SwarmStatus { members };
        if self.replay_events.last().is_some_and(|last| last.kind == kind) {
            return;
        }
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind,
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    pub fn record_swarm_plan_event(
        &mut self,
        swarm_id: String,
        version: u64,
        items: Vec<crate::plan::PlanItem>,
        participants: Vec<String>,
        reason: Option<String>,
    ) {
        let kind = StoredReplayEventKind::SwarmPlan {
            swarm_id,
            version,
            items,
            participants,
            reason,
        };
        if self.replay_events.last().is_some_and(|last| last.kind == kind) {
            return;
        }
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind,
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    pub fn record_plan_mode_event(&mut self, mode: impl Into<String>) {
        let kind = StoredReplayEventKind::PlanMode { mode: mode.into() };
        if self.replay_events.last().is_some_and(|last| last.kind == kind) {
            return;
        }
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind,
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    pub fn provider_messages(&mut self) -> &[Message] {
        let key = self.current_provider_messages_cache_key();
        if self.provider_messages_cache_mode != PersistVectorMode::Clean || self.provider_messages_cache_key != key {
            self.release_provider_messages_cache();
            for message in self.messages_for_provider_uncached() {
                self.push_provider_message_cache_entry(message);
            }
            self.provider_messages_cache_len = self.messages.len();
            self.provider_messages_cache_key = key;
            self.provider_messages_cache_mode = PersistVectorMode::Clean;
            self.provider_context_materializations = self.provider_context_materializations.saturating_add(1);
        }
        &self.provider_messages_cache
    }

    /// Borrowed provider projection for callers that only read it.
    ///
    /// Same bytes as [`Self::messages_for_provider`] without the owned copy, so
    /// read-only call sites never duplicate the transcript.
    pub fn messages_for_provider_ref(&mut self) -> &[Message] {
        self.provider_messages()
    }

    /// Inputs the cached provider projection is valid for.
    fn current_provider_messages_cache_key(&self) -> ProviderMessagesCacheKey {
        ProviderMessagesCacheKey {
            revision: self.provider_messages_revision,
            message_len: self.messages.len(),
            route_digest: provider_route_digest(self.model.as_deref(), self.route_api_method.as_deref()),
            compaction_generation: self.provider_messages_compaction_generation,
            strip_images: self.event_state.strip_images,
            image_budget: self.event_state.image_budget,
        }
    }

    /// Fingerprint of the cache key currently in force.
    ///
    /// Equal fingerprints mean the cached projection is reusable; a change means
    /// the next read rebuilds instead of serving stale bytes.
    pub fn provider_context_cache_key(&self) -> u64 {
        self.current_provider_messages_cache_key().digest()
    }

    /// Record the compaction generation the transcript now reflects.
    ///
    /// A new generation invalidates the cached projection: a summary applied
    /// under it changes what the provider must receive.
    pub fn set_compaction_generation(&mut self, generation: u64) {
        if self.provider_messages_compaction_generation != generation {
            self.provider_messages_compaction_generation = generation;
            self.provider_messages_revision = self.provider_messages_revision.wrapping_add(1);
        }
    }

    /// Compaction generation the cached projection was built under.
    pub fn compaction_generation(&self) -> u64 {
        self.provider_messages_compaction_generation
    }

    /// `(materializations, clones)` for the provider projection.
    ///
    /// A materialization is a rebuild of the cached projection from history; a
    /// clone is a caller taking an owned copy. A turn that needs no compaction
    /// should not clone at all.
    pub fn provider_context_stats(&self) -> (u64, u64) {
        (self.provider_context_materializations, self.provider_context_clones)
    }

    pub fn provider_message_prefix_hashes(&mut self) -> &[u64] {
        let _ = self.provider_messages();
        let mut previous = None;
        self.provider_message_prefix_hashes_cache = self
            .provider_messages_cache
            .iter()
            .map(|message| {
                let message_hash = crate::message::stable_message_hash(message);
                let hash = previous
                    .map(|prior| crate::message::extend_stable_hash(prior, message_hash))
                    .unwrap_or(message_hash);
                previous = Some(hash);
                hash
            })
            .collect();
        &self.provider_message_prefix_hashes_cache
    }

    pub fn messages_for_provider_uncached(&self) -> Vec<Message> {
        let mut messages = stored_messages_to_messages(&self.messages);
        let mut contents = messages.iter_mut().map(|m| &mut m.content).collect::<Vec<_>>();
        horon_agent_compaction_core::decay_old_tool_images_in_contents(&mut contents);
        if self.event_state.strip_images {
            horon_agent_compaction_core::strip_all_images_in_contents(&mut contents);
        }
        if let Some(limit) = self.event_state.image_budget {
            horon_agent_compaction_core::strip_large_images_in_contents(&mut contents, limit);
        }
        messages
    }

    pub fn messages_for_provider(&mut self) -> Vec<Message> {
        self.provider_context_clones = self.provider_context_clones.saturating_add(1);
        let messages = self.provider_messages().to_vec();
        horon_agent_perf_counters::record_request_deep_copy(
            u64::try_from(self.memory_profile_cache.provider_cache_json_bytes).unwrap_or(u64::MAX),
            u64::try_from(messages.len()).unwrap_or(u64::MAX),
        );
        messages
    }

    /// Serialized JSON bytes of the cached provider projection.
    ///
    /// Maintained incrementally while the projection is rebuilt, so this is a
    /// field read rather than a re-serialization. Zero before the first
    /// materialization; a request copy duplicates exactly these bytes.
    pub fn provider_projection_json_bytes(&self) -> usize {
        self.memory_profile_cache.provider_cache_json_bytes
    }

    /// Drop heavyweight transcript vectors after remote startup has rendered the
    /// optimistic local history. The authoritative transcript comes from the
    /// server once the connection is established, so keeping another owned copy
    /// in the client only inflates memory during idle remote sessions.
    pub fn strip_transcript_for_remote_client(&mut self) {
        self.messages.clear();
        self.compaction = None;
        self.env_snapshots.clear();
        self.memory_injections.clear();
        self.replay_events.clear();
        self.rebuild_memory_profile_cache();
        self.reset_provider_messages_cache();
        self.reset_persist_state(true);
        self.persist_state.transcript_stripped = true;
    }

    /// Remove all ToolUse content blocks from a specific message.
    /// Used when tool calls are discarded (e.g. due to truncated output / max_tokens).
    pub fn remove_tool_use_blocks(&mut self, message_id: &str) {
        for msg in &mut self.messages {
            if msg.id == *message_id {
                msg.content
                    .retain(|block| !matches!(block, ContentBlock::ToolUse { .. }));
                self.mark_memory_profile_dirty();
                self.mark_messages_full_dirty();
                break;
            }
        }
    }
}

fn aggregate_usage<'a>(
    messages: impl IntoIterator<Item = &'a StoredMessage>,
    mut totals: crate::protocol::TokenUsageTotals,
) -> crate::protocol::TokenUsageTotals {
    for message in messages {
        let Some(usage) = message.token_usage.as_ref() else {
            continue;
        };
        totals.messages_with_token_usage = totals.messages_with_token_usage.saturating_add(1);
        totals.input_tokens = totals.input_tokens.saturating_add(usage.input_tokens);
        totals.output_tokens = totals.output_tokens.saturating_add(usage.output_tokens);
        if usage.cache_read_input_tokens.is_some() || usage.cache_creation_input_tokens.is_some() {
            totals.cache_prompt_tokens = totals
                .cache_prompt_tokens
                .zip(usage.prompt_tokens)
                .map(|(total, prompt)| total.saturating_add(prompt));
            totals.cache_reported_input_tokens = totals.cache_reported_input_tokens.saturating_add(usage.input_tokens);
        }
        totals.cache_read_input_tokens = totals
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens.unwrap_or(0));
        totals.cache_creation_input_tokens = totals
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens.unwrap_or(0));
    }
    totals
}

fn redact_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            *s = crate::message::redact_secrets(s);
        }
        serde_json::Value::Array(values) => {
            for entry in values {
                redact_json_value(entry);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if is_sensitive_json_key(key) {
                    *entry = serde_json::Value::String("[REDACTED_SECRET]".to_string());
                } else {
                    redact_json_value(entry);
                }
            }
        }
        _ => {}
    }
}

fn is_sensitive_json_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.contains("apikey")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.contains("password")
        || matches!(
            normalized.as_str(),
            "authorization" | "cookie" | "setcookie" | "privatekey" | "clientsecret"
        )
}

/// The lineage root a freshly created session starts with: a new UUID for a root, and
/// `None` for a child, whose root is copied from its parent or derived on first use.
fn root_cache_id(is_root: bool) -> Option<String> {
    is_root.then(|| uuid::Uuid::new_v4().to_string())
}

/// Namespace for deriving a stable root from a session id that predates the field.
const CACHE_ROOT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x6f0c_1a3e_5b2d_4c7a_9e11_2d3c_4b5a_6978);

impl Session {
    /// The lineage root this session belongs to, deriving and remembering it when the
    /// saved record predates the field.
    ///
    /// The parent chain is followed only through sessions that actually load, so a
    /// missing or unreadable parent yields a root of this session's own and never a
    /// shared one.
    pub fn cache_root(&mut self) -> String {
        if let Some(root) = &self.cache_root_id {
            return root.clone();
        }
        let root = self.derive_cache_root();
        self.cache_root_id = Some(root.clone());
        root
    }

    fn derive_cache_root(&self) -> String {
        let mut current = self.parent_id.clone();
        let mut ancestor = self.id.clone();
        let mut seen = std::collections::HashSet::new();
        while let Some(parent_id) = current {
            if !seen.insert(parent_id.clone()) {
                break;
            }
            match Session::load(&parent_id) {
                Ok(parent) => {
                    ancestor = parent.id.clone();
                    current = parent.parent_id.clone();
                }
                Err(_) => break,
            }
        }
        uuid::Uuid::new_v5(&CACHE_ROOT_NAMESPACE, ancestor.as_bytes()).to_string()
    }

    /// Copy a parent's lineage root onto this session.
    pub fn inherit_cache_root(&mut self, parent: &Session) {
        self.cache_root_id = parent.cache_root_id.clone();
    }
}

#[cfg(test)]
#[path = "session_tests/mod.rs"]
mod tests;

#[cfg(test)]
#[path = "session_tests/explicit_path.rs"]
mod explicit_path_tests;
#[cfg(test)]
#[path = "session_tests/event_store.rs"]
mod session_tests;
