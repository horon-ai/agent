//! Pure JSON-to-JSON translation between the harness API and the legacy
//! internal protocol. Kept side-effect free so it is trivially unit-testable.

mod compact_handoff;
mod credentials;
mod search_artifacts;
mod usage_report;

use crate::background_progress::parse_background_notification;
use horon_agent_harness_api::{
    ApiEvent, ErrorCode, HistoryMessage, ModelRouteInfo, ServerFrame, SessionInfo, TextMatch,
};
use horon_agent_session_store as store;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::sync::{
    Mutex, MutexGuard, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
pub(crate) fn horon_agent_home_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Default number of messages a `peek_session` returns. A preview is a glance,
/// so this is a tail rather than a transcript: enough to recognise which
/// conversation it is, few enough that peeking a dozen sessions stays cheap.
const PEEK_LIMIT: u64 = 12;

/// Requests the daemon only accepts on an attached (subscribed) connection.
///
/// `peek_session` and `list_sessions` are deliberately absent: they are served
/// from stored records precisely so a client can look around before attaching.
const REQUIRES_ATTACH: &[&str] = &[
    "runtime_command",
    "detach_session",
    "configure_tools",
    "list_tools",
    "tool_result",
    "send_message",
    "cancel",
    "soft_interrupt",
    "cancel_soft_interrupts",
    "background_tool",
    "clear",
    "rewind",
    "rewind_undo",
    "get_history",
    "list_models",
    "set_model",
    "set_reasoning_effort",
    "compact",
    "rename_session",
    "set_session_saved",
    "get_runtime_info",
    "fork_session",
    "transfer_session",
    "read_file",
    "find_files",
    "search_text",
    "file_status",
    "approval_ack",
    "failover_ack",
    "permission_response",
    "ask_response",
];

const DEFAULT_FILE_BYTES: u64 = 1024 * 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_FIND_LIMIT: usize = 200;
const MAX_FIND_LIMIT: usize = 2_000;
const MAX_WALK_FILES: usize = 20_000;
const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_SEARCH_BYTES: u64 = 64 * 1024 * 1024;

/// Flatten a stored message's `content` to plain text.
///
/// The daemon writes content either as a bare string or as an array of typed
/// blocks, so both shapes are accepted; anything without text (a tool call, an
/// image) contributes nothing rather than a placeholder.
pub(crate) fn flatten_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    blocks
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}
use serde_json::{Value, json};

/// Where a translated client request should go.
#[derive(Debug)]
// Short-lived per-request value; boxing ServerFrame would touch every reply site.
#[allow(clippy::large_enum_variant)]
pub enum Outbound {
    /// Forward to the legacy daemon connection.
    Legacy(Value),
    /// Answer the API client directly (no daemon round trip needed).
    Reply(ServerFrame),
    /// Answer with a frame whose wire bytes were already serialized, measured
    /// and bounded, so a page near the cap is not serialized a second time.
    /// The frame stays beside its bytes because it is the correlated reply.
    Serialized { frame: ServerFrame, bytes: Vec<u8> },
}

type SessionFileStatus = (bool, String, Option<u64>, Option<u64>);
type SessionFileStatusResult = Result<SessionFileStatus, (ErrorCode, String)>;

// Legacy daemon replies are broadcast to every connected client. Request ids
// must therefore be unique across bridge connections, not merely within one
// `BridgeState`, or two panels issuing the same numbered request can each
// mistake the other's reply for their own.
static NEXT_LEGACY_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_TEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Per-connection translation state.
#[derive(Debug, Default)]
pub struct BridgeState {
    /// Refreshed from an authoritative source-daemon Ping before each list.
    pub user_connected_sessions: std::collections::HashSet<String>,
    pub runtime_commands_supported: bool,
    /// Set only after a safe legacy ping capability probe succeeds.
    pub session_tools_supported: bool,
    /// The daemon accepts soft interrupt ids and per-id cancellation, and
    /// replies to `background_tool` with a result.
    pub soft_interrupt_ids_supported: bool,
    /// The daemon answers the stable API's durable runtime requests
    /// (`get_runtime_state`, `apply_control`, `get_control_receipt`). An older
    /// daemon answers them as unknown requests with an uncorrelated error
    /// frame, so forwarding without this check would park the client until its
    /// own timeout.
    pub durable_runtime_api_supported: bool,
    /// Leftover queue size reported just before the current turn's `done`.
    pending_soft_interrupts_at_done: Option<u32>,
    /// Whether an unannounced transport loss should crash the attached session.
    pub crash_on_disconnect: bool,
    /// Client-side inline response renderer, negotiated in Hello.
    pub supports_user_response: bool,
    /// Client can render `ask_requested` and answer it, negotiated in Hello.
    pub supports_ask_requests: bool,
    /// Session id assigned by the daemon for this connection.
    pub session_id: Option<String>,
    /// Legacy id of the in-flight `message` request, so `done` maps to
    /// `turn_done`.
    pending_message_id: Option<u64>,
    /// Current text message and all text in the current provider attempt.
    /// Completed messages remain retractable until the response commits.
    text_message_id: Option<String>,
    // Tuple fields: correlation id, text, whether TextDone was emitted.
    text_attempt: Vec<(String, String, bool)>,
    text_response_ended: bool,
    /// A turn can belong to another attachment or a server-initiated wake.
    observed_turn_active: bool,
    /// Deduplicate specific limit/cancel notices from the subsequent failure.
    turn_stop_reported: bool,
    activity_version: u64,
    pending_activity_snapshots: std::collections::HashMap<u64, u64>,
    /// Control requests also emit done, sometimes after their richer reply.
    /// Retain those ids until done so they never terminate an observed turn.
    pending_control_done_ids: std::collections::HashSet<u64>,
    /// Soft interrupts normally join an active turn, but the daemon promotes
    /// one received while idle into a new turn. Retain their ids so a matching
    /// `done` can become `turn_done` without confusing queued interrupts with
    /// the active ordinary message.
    pending_soft_interrupt_ids: Vec<u64>,
    /// Legacy and API ids for a context-only message. Its daemon completion
    /// event is a request reply, not a model turn boundary.
    pending_no_reply_message_id: Option<(u64, u64)>,
    /// Legacy id of an in-flight `create/attach` subscribe.
    pending_attach_id: Option<(u64, u64, Option<String>)>,
    /// Subscribe errors can arrive before the correlated state response.
    pending_attach_subscribe_id: Option<u64>,
    /// Subscribe history may arrive before or after the correlated state reply.
    /// Keep its correlation independently, and consume even non-recovery history
    /// so duplicates cannot later inject a continuation into a completed turn.
    pending_recovery_history: Option<(u64, Option<String>)>,
    /// Legacy and API ids for an in-flight session fork.
    pending_fork_id: Option<(u64, u64)>,
    /// Legacy id of the unsolicited model-catalog probe sent after attach. Its
    /// reply becomes a `model_info` event rather than a request reply, so it is
    /// tracked apart from `pending_simple`.
    pending_model_probe: Option<u64>,
    /// Legacy id -> API id for simple acked requests (ping, clear, ...).
    pending_simple: Vec<(u64, u64, SimpleKind)>,
    /// Every session the daemon has told us about, newest snapshot wins.
    ///
    /// The legacy protocol has no session-list request, but it volunteers the
    /// full set on every `state` event, so the bridge remembers it rather than
    /// answering `list_sessions` with only the one session this connection
    /// happens to be attached to.
    known_sessions: Vec<String>,
    /// Working directory per session, as far as it is known.
    session_dirs: std::collections::BTreeMap<String, String>,
    /// Directory sent with an in-flight `create_session` subscribe. A fresh
    /// session has no persisted metadata until its first prompt, so the
    /// attach reply falls back to this instead of reporting no directory.
    pending_create_dir: Option<String>,
    /// Models the daemon last reported for this session.
    ///
    /// The daemon volunteers the catalog on attach and again whenever it
    /// changes, so `list_models` is answered from here rather than by asking
    /// again: a picker that opens instantly is the difference between a usable
    /// model switcher and a spinner.
    available_models: Vec<String>,
    /// An empty catalog is a valid snapshot, not an uninitialized cache.
    model_catalog_loaded: bool,
    /// Model currently serving the session, tracked alongside the catalog so
    /// a picker can mark the active entry.
    current_model: Option<String>,
    current_provider: Option<String>,
    /// Credential the daemon resolved for the session (`oauth`/`api_key`).
    current_credential: Option<String>,
    /// Reasoning effort last reported by the daemon, so identity events can
    /// carry it without a round trip.
    current_effort: Option<String>,
    available_routes: Vec<ModelRouteInfo>,
    /// Deltas can precede the first catalog or a delayed snapshot reply.
    model_usage_updates: BTreeMap<(String, String, String), horon_agent_harness_api::ModelUsage>,
    /// Live usage figures the daemon pushed on this connection.
    usage: usage_report::Cache,
    /// Bytes the client actually sent for the request being translated, set by
    /// the connection loop before each translation.
    ///
    /// A replication request's size cap must bound what was read, and
    /// re-serializing the parsed value to measure it cost a second copy of up
    /// to 16 MiB per request. `None` in tests and any direct caller: the cap
    /// then measures the serialized request, which is what it always did.
    pub request_envelope_bytes: Option<usize>,
}

impl BridgeState {
    pub fn with_crash_on_disconnect(crash_on_disconnect: bool) -> Self {
        Self {
            crash_on_disconnect,
            ..Self::default()
        }
    }
}

#[derive(Debug, Default)]
struct ArchiveState {
    sessions: BTreeMap<String, u64>,
    restored: BTreeSet<String>,
    archive_after_days: Option<u32>,
}

impl ArchiveState {
    fn is_archived(&self, id: &str, canonical: bool) -> bool {
        self.sessions.contains_key(id) || (canonical && !self.restored.contains(id))
    }
}

/// The small, canonical subset of a persisted `Session` needed by list and
/// attach responses. Serde skips the heavyweight transcript fields without
/// materializing them.
#[derive(Debug, Default, Deserialize)]
struct PersistedSessionMetadata {
    #[serde(default)]
    working_dir: Option<String>,
    /// Generated or imported title.
    #[serde(default)]
    title: Option<String>,
    /// User-provided rename, which is what `Session::display_title` prefers.
    #[serde(default)]
    custom_title: Option<String>,
    #[serde(default)]
    todo_title: Option<String>,
    #[serde(default)]
    saved: bool,
    #[serde(default)]
    save_label: Option<String>,
    #[serde(skip)]
    updated_at_ms: Option<i64>,
    #[serde(skip)]
    last_active_at_ms: Option<i64>,
}

impl PersistedSessionMetadata {
    fn display_title(&self) -> Option<String> {
        self.custom_title
            .as_deref()
            .and_then(Self::normalized_title)
            // Bookmarks labelled before labels doubled as titles.
            .or_else(|| self.save_label())
            .or_else(|| self.todo_title.as_deref().and_then(Self::normalized_title))
            .or_else(|| self.title.as_deref().and_then(Self::normalized_title))
    }

    fn save_label(&self) -> Option<String> {
        self.saved
            .then(|| self.save_label.as_deref().and_then(Self::normalized_title))
            .flatten()
    }

    fn normalized_title(title: &str) -> Option<String> {
        let title = title.trim();
        (!title.is_empty()).then(|| title.to_string())
    }
}

#[derive(Clone, Debug)]
struct RecentSessionIndexEntry {
    session_id: String,
    working_dir: Option<String>,
    generated_title: Option<String>,
    custom_title: Option<String>,
    todo_title: Option<String>,
    saved: bool,
    save_label: Option<String>,
    updated_at_ms: i64,
    last_active_at_ms: Option<i64>,
    archived: bool,
}

impl From<&RecentSessionIndexEntry> for PersistedSessionMetadata {
    fn from(entry: &RecentSessionIndexEntry) -> Self {
        Self {
            working_dir: entry.working_dir.clone(),
            title: entry.generated_title.clone(),
            custom_title: entry.custom_title.clone(),
            todo_title: entry.todo_title.clone(),
            saved: entry.saved,
            save_label: entry.save_label.clone(),
            updated_at_ms: Some(entry.updated_at_ms),
            last_active_at_ms: entry.last_active_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum SimpleKind {
    RuntimeCommand,
    Detach,
    Tools,
    ToolControl,
    Ping,
    History,
    Ok,
    /// Awaiting `model_changed`, which carries its own error field.
    Model,
    /// Awaiting `reasoning_effort_changed`.
    ReasoningEffort,
    /// Awaiting `compacted_history`.
    Compact,
    /// Awaiting the catalog reply that answers `list_models`.
    Models,
    RuntimeInfo,
    /// Awaiting `background_tool_result`.
    BackgroundTool,
    /// Awaiting `soft_interrupts_cancelled`.
    SoftInterruptCancel,
    /// Awaiting `runtime_state`.
    RuntimeState,
    /// Awaiting `control_receipt`.
    ControlReceipt,
    /// Awaiting `snapshot_result`.
    Snapshot,
    Credential {
        provider: String,
        configured: bool,
    },
}

/// Whether translating this daemon event may perform synchronous file I/O.
///
/// Keep in sync with the arms of [`BridgeState::legacy_event_to_api`] that
/// read persisted session records or side-panel files. Everything else is
/// pure in-memory translation and must not pay for `block_in_place`.
pub fn legacy_event_may_block(event: &Value) -> bool {
    matches!(event["type"].as_str(), Some("state" | "split_response" | "history"))
}

impl BridgeState {
    fn legacy_id(&mut self) -> u64 {
        NEXT_LEGACY_ID.fetch_add(1, Ordering::Relaxed)
    }

    /// Translate one API request (raw JSON) into outbound actions.
    pub fn api_request_to_legacy(&mut self, request: &Value) -> Vec<Outbound> {
        let outbound = self.translate_request(request);
        for action in &outbound {
            if let Outbound::Legacy(value) = action
                && matches!(value["type"].as_str(), Some("state" | "get_history"))
                && let Some(id) = value["id"].as_u64()
            {
                self.pending_activity_snapshots.insert(id, self.activity_version);
            }
            if let Outbound::Legacy(value) = action
                && matches!(
                    value["type"].as_str(),
                    Some(
                        "subscribe"
                            | "clear"
                            | "prepare_disconnect"
                            | "notify_auth_changed"
                            | "invalidate_openai_usage"
                            | "invalidate_anthropic_usage"
                    )
                )
                && let Some(id) = value["id"].as_u64()
            {
                self.pending_control_done_ids.insert(id);
            }
        }
        self.observe_pending_requests();
        outbound
    }

    /// Requests awaiting a daemon reply: every queue and option that a reply
    /// drains. Recorded after each request translation and each daemon event.
    fn pending_request_count(&self) -> u64 {
        let counters = [
            self.pending_simple.len() as u64,
            self.pending_attach_id.is_some() as u64,
            self.pending_attach_subscribe_id.is_some() as u64,
            self.pending_recovery_history.is_some() as u64,
            self.pending_fork_id.is_some() as u64,
            self.pending_model_probe.is_some() as u64,
            self.pending_no_reply_message_id.is_some() as u64,
            self.pending_control_done_ids.len() as u64,
            self.pending_activity_snapshots.len() as u64,
        ];
        counters.into_iter().sum()
    }

    fn observe_pending_requests(&self) {
        horon_agent_perf_counters::observe_pending_requests(self.pending_request_count());
    }

    fn translate_request(&mut self, request: &Value) -> Vec<Outbound> {
        let api_id = request["id"].as_u64().unwrap_or(0);
        let req = request["req"].as_str().unwrap_or("");

        // Stateful requests only mean something once this connection is
        // attached. Forwarding one before then is not merely useless: the
        // daemon answers "Client must Subscribe with a working_dir before
        // sending stateful requests" and *closes the connection*, so a client
        // that mistypes a session id loses every other session it was
        // streaming, and the SDK reports a bare EPIPE. Answer locally instead,
        // with the code that actually says what went wrong.
        // `pending_attach_id` means a subscribe is already on the wire, so the
        // daemon will have a session by the time this arrives: a client that
        // pipelines `create_session` and `send_message` without awaiting must
        // still work. Only a connection that never asked to attach is refused.
        if self.session_id.is_none() && self.pending_attach_id.is_none() && REQUIRES_ATTACH.contains(&req) {
            let requested = request["session_id"].as_str().unwrap_or("");
            return vec![Outbound::Reply(ServerFrame::reply(
                api_id,
                ApiEvent::Error {
                    code: ErrorCode::UnknownSession,
                    message: if requested.is_empty() {
                        format!("`{req}` needs an attached session; call create_session or attach_session first")
                    } else {
                        format!(
                            "not attached to session `{requested}`; call attach_session first (it is not attached, or does not exist)"
                        )
                    },
                    details: None,
                },
            ))];
        }

        // A request naming a session other than the attached one would be
        // silently applied to the attached session, because the legacy
        // protocol has no session field: `clear` on a typo'd id would wipe the
        // wrong transcript. Refuse rather than destroy the wrong thing.
        if let Some(attached) = self.session_id.as_deref()
            && REQUIRES_ATTACH.contains(&req)
            && let Some(requested) = request["session_id"].as_str()
            && !requested.is_empty()
            && requested != attached
        {
            return vec![Outbound::Reply(ServerFrame::reply(
                api_id,
                ApiEvent::Error {
                    code: ErrorCode::UnknownSession,
                    message: format!(
                        "this connection is attached to `{attached}`, not `{requested}`; attach to it first or use another connection"
                    ),
                    details: None,
                },
            ))];
        }

        if req == "runtime_command" {
            if !self.runtime_commands_supported {
                return Self::error_reply(
                    api_id,
                    ErrorCode::InvalidRequest,
                    "This daemon does not support runtime commands. Update and restart the daemon.",
                );
            }
            let parsed = serde_json::from_value::<horon_agent_harness_api::ApiRequest>(request.clone());
            let Ok(horon_agent_harness_api::ApiRequest::RuntimeCommand {
                command,
                args,
                session_id,
            }) = parsed
            else {
                return Self::error_reply(api_id, ErrorCode::InvalidRequest, "Invalid runtime command request");
            };
            if session_id.is_empty() {
                return Self::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
            }
            let id = self.legacy_id();
            self.pending_simple.push((id, api_id, SimpleKind::RuntimeCommand));
            return vec![Outbound::Legacy(
                json!({"type": "runtime_command", "id": id, "command": command, "args": args}),
            )];
        }
        if matches!(req, "configure_tools" | "list_tools" | "tool_result") {
            // Validate the typed surface even though this bridge accepts raw JSON.
            // Never coerce a malformed result/configuration into a valid mutation.
            if let Err(error) = serde_json::from_value::<horon_agent_harness_api::ApiRequest>(request.clone()) {
                return Self::error_reply(api_id, ErrorCode::InvalidRequest, &error.to_string());
            }
            if request["session_id"].as_str().is_none_or(str::is_empty) {
                return Self::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
            }
            if let Some((_, _, Some(target))) = &self.pending_attach_id
                && self.session_id.is_none()
                && request["session_id"].as_str() != Some(target.as_str())
            {
                return Self::error_reply(
                    api_id,
                    ErrorCode::UnknownSession,
                    "request does not match the pending attachment",
                );
            }
            if !self.session_tools_supported {
                return Self::error_reply(
                    api_id,
                    ErrorCode::UnknownRequest,
                    "daemon does not advertise the session_tools capability",
                );
            }
            let id = self.legacy_id();
            let mut legacy = json!({"type": req, "id": id});
            match req {
                "configure_tools" => legacy["tools"] = request["tools"].clone(),
                "tool_result" => {
                    legacy["call_id"] = request["call_id"].clone();
                    legacy["output"] = request["output"].clone();
                    if let Some(error) = request.get("error") {
                        legacy["error"] = error.clone();
                    }
                }
                _ => {}
            }
            self.pending_simple.push((
                id,
                api_id,
                if req == "list_tools" {
                    SimpleKind::Tools
                } else {
                    SimpleKind::ToolControl
                },
            ));
            return vec![Outbound::Legacy(legacy)];
        }

        match req {
            "archive_session" | "restore_session" => {
                let archived = req == "archive_session";
                let session_id = request["session_id"].as_str().unwrap_or_default();
                let Some(path) = Self::session_original_path(session_id) else {
                    return Self::error_reply(api_id, ErrorCode::UnknownSession, "session does not exist");
                };
                if !path.is_file() {
                    return Self::error_reply(api_id, ErrorCode::UnknownSession, "session does not exist");
                }
                let reason = if archived { "api archive" } else { "api restore" };
                // An attached session's writer belongs to the daemon, so the record
                // is appended there, exactly like rename and bookmark.
                if self.session_id.as_deref() == Some(session_id) && Self::canonical_layout(session_id) {
                    let id = self.legacy_id();
                    self.pending_simple.push((id, api_id, SimpleKind::Ok));
                    return vec![Outbound::Legacy(json!({
                        "type": "archive_session",
                        "id": id,
                        "archived": archived,
                        "reason": reason,
                    }))];
                }
                if let Err(reply) = Self::record_archive_event(session_id, archived, reason, api_id) {
                    return reply;
                }
                let _write_guard = Self::state_write_guard();
                let mut archive = Self::load_archive_state();
                if archived {
                    archive.sessions.insert(session_id.to_string(), Self::now_ms());
                    archive.restored.remove(session_id);
                } else {
                    archive.sessions.remove(session_id);
                    archive.restored.insert(session_id.to_string());
                }
                match Self::save_archive_state(&archive) {
                    Ok(()) => vec![Outbound::Reply(ServerFrame::reply(api_id, ApiEvent::Ok))],
                    Err(message) => Self::error_reply(api_id, ErrorCode::Internal, &message),
                }
            }
            "set_retention_policy" => {
                let days = match request.get("archive_after_days") {
                    None | Some(Value::Null) => None,
                    Some(value) => match value.as_u64() {
                        Some(days @ 1..=36_500) => Some(days as u32),
                        _ => {
                            return Self::error_reply(
                                api_id,
                                ErrorCode::InvalidRequest,
                                "archive_after_days must be 1..=36500, or null to disable",
                            );
                        }
                    },
                };
                let _write_guard = Self::state_write_guard();
                let mut archive = Self::load_archive_state();
                archive.archive_after_days = days;
                match Self::save_archive_state(&archive) {
                    Ok(()) => vec![Outbound::Reply(ServerFrame::reply(api_id, ApiEvent::Ok))],
                    Err(message) => Self::error_reply(api_id, ErrorCode::Internal, &message),
                }
            }
            "create_session" | "attach_session" => {
                let id = self.legacy_id();
                let state_id = self.legacy_id();
                let catalog_id = self.legacy_id();
                let requested_session = (req == "attach_session")
                    .then(|| request["session_id"].as_str().map(str::to_string))
                    .flatten();
                self.pending_recovery_history = Some((id, requested_session.clone()));
                self.pending_attach_id = Some((state_id, api_id, requested_session));
                self.pending_attach_subscribe_id = Some(id);
                self.pending_model_probe = Some(catalog_id);
                self.pending_create_dir = None;
                let mut subscribe = json!({
                    "type": "subscribe",
                    "id": id,
                    "supports_user_response": self.supports_user_response,
                    "supports_ask_requests": self.supports_ask_requests,
                });
                if self.crash_on_disconnect {
                    subscribe["crash_on_disconnect"] = json!(true);
                }
                if req == "create_session" {
                    if let Some(prompt) = request["system_prompt"].as_str() {
                        subscribe["system_prompt"] = json!(prompt);
                    }
                    let working_dir = request["working_dir"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string()));
                    subscribe["working_dir"] = json!(working_dir);
                    self.pending_create_dir = working_dir.clone();
                } else if let Some(target) = request["session_id"].as_str() {
                    // The daemon owns both live and persisted session roots.
                    // Empty panels have no file yet, and a cached disk cwd may
                    // be older than the live Agent. Never override either with
                    // the bridge's process cwd just to bootstrap attachment.
                    subscribe["target_session_id"] = json!(target);
                }
                // The daemon assigns the session during subscribe but reports
                // the id via `state`, so chase the subscribe with get_state.
                // The model identity arrives the same way, via the catalog
                // reply, so ask for it now rather than making the client poll.
                vec![
                    Outbound::Legacy(subscribe),
                    Outbound::Legacy(json!({"type": "state", "id": state_id})),
                    Outbound::Legacy(
                        json!({"type": "get_model_catalog", "id": catalog_id, "subscribe_usage_updates": true}),
                    ),
                ]
            }
            "send_message" => {
                let id = self.legacy_id();
                let no_reply = request["no_reply"].as_bool().unwrap_or(false);
                if no_reply {
                    self.pending_no_reply_message_id = Some((id, api_id));
                } else {
                    self.pending_message_id = Some(id);
                }
                let mut message = json!({
                    "type": "message",
                    "id": id,
                    "content": request["content"].as_str().unwrap_or(""),
                });
                if no_reply {
                    message["no_reply"] = json!(true);
                }
                if let Some(reminder) = request["system_reminder"].as_str() {
                    message["system_reminder"] = json!(reminder);
                }
                if let Some(images) = request["images"].as_array()
                    && !images.is_empty()
                {
                    message["images"] = json!(images);
                }
                vec![Outbound::Legacy(message)]
            }
            "fork_session" => {
                let id = self.legacy_id();
                self.pending_fork_id = Some((id, api_id));
                vec![Outbound::Legacy(json!({"type": "split", "id": id}))]
            }
            "transfer_session" => {
                let id = self.legacy_id();
                self.pending_fork_id = Some((id, api_id));
                vec![Outbound::Legacy(json!({"type": "transfer", "id": id}))]
            }
            "cancel" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({"type": "cancel", "id": id}))]
            }
            "soft_interrupt" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                self.pending_soft_interrupt_ids.push(id);
                let mut interrupt = json!({
                    "type": "soft_interrupt",
                    "id": id,
                    "content": request["content"].as_str().unwrap_or(""),
                    "urgent": request["urgent"].as_bool().unwrap_or(false),
                });
                if let Some(images) = request["images"].as_array()
                    && !images.is_empty()
                {
                    interrupt["images"] = json!(images);
                }
                if let Some(client_id) = request["client_id"].as_str() {
                    // Older daemons ignore the field, which is harmless here.
                    interrupt["client_id"] = json!(client_id);
                }
                vec![Outbound::Legacy(interrupt)]
            }
            "clear" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({"type": "clear", "id": id}))]
            }
            "rewind" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({
                    "type": "rewind",
                    "id": id,
                    "message_index": request["message_index"].as_u64().unwrap_or(1),
                }))]
            }
            "get_history" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::History));
                vec![Outbound::Legacy(json!({"type": "get_history", "id": id}))]
            }
            "list_session_heads"
            | "select_session_head"
            | "get_history_page"
            | "export_session_events"
            | "import_session_events"
            | "export_session_blob"
            | "import_session_blob"
            | "get_session_blob_status"
            | "export_session_record_blob"
            | "import_session_record_blob"
            | "get_session_record_blob_status"
            | "export_session_markdown" => crate::replication::handle(request, self.request_envelope_bytes),
            // Answered from the stored record rather than the daemon: the legacy
            // protocol can only speak about the attached session, and attaching
            // merely to read it would disturb the very thing being previewed.
            "peek_session" => {
                let session_id = request["session_id"].as_str().unwrap_or_default();
                let limit = request["limit"].as_u64().unwrap_or(PEEK_LIMIT) as usize;
                vec![Outbound::Reply(ServerFrame::reply(
                    api_id,
                    ApiEvent::History {
                        session_id: session_id.to_string(),
                        messages: Self::stored_tail(session_id, limit),
                        images: Vec::new(),
                    },
                ))]
            }
            // Answered locally before attach. The daemon treats `ping` as a
            // "lightweight control" request: when it arrives as the first
            // frame on a connection it is answered and the connection is then
            // closed, which would tear down the client's whole session. A
            // liveness probe must never cost the caller its connection, and
            // reaching the bridge already proves the socket is alive.
            "ping" if self.session_id.is_none() => {
                vec![Outbound::Reply(ServerFrame::reply(api_id, ApiEvent::Pong))]
            }
            "ping" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ping));
                vec![Outbound::Legacy(json!({"type": "ping", "id": id}))]
            }
            "list_sessions" => {
                let list_started = std::time::Instant::now();
                // A fresh API connection has not received a daemon `state`
                // snapshot. Start with every persisted record, then merge the
                // live snapshot so unattached dashboards and global event
                // subscribers discover complete session state.
                let limit = request["limit"].as_u64().map(|limit| limit as usize);
                let indexed_metadata: BTreeMap<_, _> = match Self::recent_session_index_entries() {
                    Ok(entries) => entries
                        .into_iter()
                        .map(|entry| (entry.session_id.clone(), entry))
                        .collect(),
                    Err(error) => return Self::store_error_reply(api_id, &error),
                };
                let mut ids: BTreeSet<String> = Self::stored_session_ids(&indexed_metadata).into_iter().collect();
                let ids_loaded = list_started.elapsed();
                ids.extend(self.known_sessions.iter().cloned());
                if let Some(attached) = self.session_id.clone() {
                    ids.insert(attached);
                }
                if let Some(limit) = limit {
                    let mut recent: Vec<_> = ids.into_iter().collect();
                    recent.sort_unstable_by_key(|id| {
                        std::cmp::Reverse(
                            indexed_metadata
                                .get(id)
                                .map(|entry| entry.last_active_at_ms.unwrap_or(entry.updated_at_ms))
                                .or_else(|| Self::session_modified_ms(id).and_then(|at| i64::try_from(at).ok())),
                        )
                    });
                    recent.truncate(limit);
                    ids = recent.into_iter().collect();
                }
                ids.extend(
                    indexed_metadata
                        .values()
                        .filter(|entry| entry.saved)
                        .map(|entry| entry.session_id.clone()),
                );
                // Connected user clients must remain discoverable even when
                // their idle session is older than the persisted recency page.
                ids.extend(self.user_connected_sessions.iter().cloned());
                // Titles are deliberately not cached. A rename is persisted
                // before `SessionRenamed` is broadcast, and every list call
                // should reflect that newest canonical value even on another
                // API connection.
                let metadata: BTreeMap<String, PersistedSessionMetadata> = ids
                    .iter()
                    .filter_map(|id| {
                        indexed_metadata
                            .get(id)
                            .map(PersistedSessionMetadata::from)
                            .or_else(|| Self::resolve_legacy_session_metadata(id))
                            .map(|metadata| (id.clone(), metadata))
                    })
                    .collect();
                let mut metadata = metadata;
                Self::backfill_prompt_titles(&ids, &indexed_metadata, &mut metadata);
                let metadata_loaded = list_started.elapsed();
                for id in &ids {
                    if !self.session_dirs.contains_key(id)
                        && let Some(dir) = metadata.get(id).and_then(|metadata| metadata.working_dir.clone())
                    {
                        self.session_dirs.insert(id.clone(), dir);
                    }
                }
                let _write_guard = Self::state_write_guard();
                let mut archive = Self::load_archive_state();
                if let Some(days) = archive.archive_after_days {
                    let cutoff = Self::now_ms().saturating_sub(u64::from(days) * 86_400_000);
                    let mut changed = false;
                    for id in &ids {
                        if self.session_id.as_ref() == Some(id)
                            || self.user_connected_sessions.contains(id)
                            || archive.sessions.contains_key(id)
                        {
                            continue;
                        }
                        if Self::session_modified_ms(id).is_some_and(|modified| modified < cutoff) {
                            archive.sessions.insert(id.clone(), Self::now_ms());
                            archive.restored.remove(id);
                            changed = true;
                        }
                    }
                    if changed && let Err(message) = Self::save_archive_state(&archive) {
                        return Self::error_reply(api_id, ErrorCode::Internal, &message);
                    }
                }
                let include_archived = request["include_archived"].as_bool().unwrap_or(false);
                let mut sessions: Vec<_> = ids
                    .into_iter()
                    .filter(|session_id| {
                        include_archived
                            || !archive.is_archived(
                                session_id,
                                indexed_metadata.get(session_id).is_some_and(|row| row.archived),
                            )
                    })
                    .map(|session_id| SessionInfo {
                        user_connected: self.user_connected_sessions.contains(&session_id),
                        edit_stats: None,
                        parent_session_id: None,
                        agent_label: None,
                        swarm_status: None,
                        working_dir: self.session_dirs.get(&session_id).cloned(),
                        title: metadata
                            .get(&session_id)
                            .and_then(PersistedSessionMetadata::display_title),
                        status: if self.session_id.as_ref() == Some(&session_id) {
                            "attached".into()
                        } else {
                            "idle".into()
                        },
                        transcript_bytes: Self::transcript_bytes(&session_id),
                        saved: metadata.get(&session_id).is_some_and(|value| value.saved),
                        save_label: metadata.get(&session_id).and_then(PersistedSessionMetadata::save_label),
                        updated_at_ms: metadata
                            .get(&session_id)
                            .and_then(|value| value.updated_at_ms)
                            .or_else(|| Self::session_modified_ms(&session_id).map(|value| value as i64)),
                        last_active_at_ms: metadata.get(&session_id).and_then(|value| value.last_active_at_ms),
                        archived: archive.is_archived(
                            &session_id,
                            indexed_metadata.get(&session_id).is_some_and(|row| row.archived),
                        ),
                        archived_at_ms: archive.sessions.get(&session_id).copied(),
                        session_id,
                    })
                    .collect();
                horon_agent_harness_api::enrich_sessions_from_local_swarm_state(&mut sessions);
                horon_agent_harness_api::enrich_sessions_from_local_edit_stats(&mut sessions);
                let completed = list_started.elapsed();
                eprintln!(
                    "harness API bridge: list_sessions ids={:.1}ms metadata={:.1}ms total={:.1}ms count={}",
                    ids_loaded.as_secs_f64() * 1_000.0,
                    metadata_loaded.as_secs_f64() * 1_000.0,
                    completed.as_secs_f64() * 1_000.0,
                    sessions.len()
                );
                vec![Outbound::Reply(ServerFrame::reply(
                    api_id,
                    ApiEvent::Sessions { sessions },
                ))]
            }
            // Answered from the cached catalog. The daemon pushes it on attach
            // and on every change, so asking again would add a round trip to
            // an interaction (opening a picker) that must feel instant.
            "list_models" => {
                // Attach pushes the catalog, but a client that asks in the
                // same breath as attaching can beat it. Returning an empty
                // list would look like "no models exist", so ask the daemon
                // and answer when the catalog lands.
                if !self.model_catalog_loaded {
                    let id = self.legacy_id();
                    self.pending_simple.push((id, api_id, SimpleKind::Models));
                    return vec![Outbound::Legacy(
                        json!({"type": "get_model_catalog", "id": id, "subscribe_usage_updates": true}),
                    )];
                }
                vec![Outbound::Reply(ServerFrame::reply(
                    api_id,
                    ApiEvent::Models {
                        session_id: self.session_id.clone().unwrap_or_default(),
                        models: self.available_models.clone(),
                        current: self.current_model.clone(),
                    },
                ))]
            }
            "get_runtime_info" => {
                if !self.model_catalog_loaded {
                    let id = self.legacy_id();
                    self.pending_simple.push((id, api_id, SimpleKind::RuntimeInfo));
                    return vec![Outbound::Legacy(
                        json!({"type": "get_model_catalog", "id": id, "subscribe_usage_updates": true}),
                    )];
                }
                vec![Outbound::Reply(ServerFrame::reply(api_id, self.runtime_info()))]
            }
            "usage_report" => usage_report::reply(self, request, api_id),
            "notify_auth_changed" => {
                let provider = request["provider"].as_str().unwrap_or_default();
                if provider.is_empty()
                    || provider.len() > 64
                    || !provider
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "invalid auth provider identifier");
                }
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({
                    "type": "notify_auth_changed", "id": id, "provider": provider
                }))]
            }
            "invalidate_usage" => {
                let legacy_type = match request["provider"].as_str().unwrap_or_default() {
                    "claude" | "anthropic" => "invalidate_anthropic_usage",
                    "openai" => "invalidate_openai_usage",
                    _ => {
                        return Self::error_reply(
                            api_id,
                            ErrorCode::InvalidRequest,
                            "unsupported usage provider; supported: claude, openai",
                        );
                    }
                };
                let account_label = request["account_label"].as_str();
                if account_label
                    .is_some_and(|label| label.is_empty() || label.len() > 128 || label.chars().any(char::is_control))
                {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "invalid account label");
                }
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({
                    "type": legacy_type, "id": id, "account_label": account_label
                }))]
            }
            "set_api_key" | "clear_api_key" => {
                let provider = request["provider"].as_str().unwrap_or_default();
                let Some((provider, env_keys, file_name)) = Self::credential_binding(provider) else {
                    return Self::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        "unsupported API-key provider; supported: anthropic-api, openai-api, openrouter, gemini-api",
                    );
                };
                let configured = req == "set_api_key";
                let key = request["api_key"].as_str().unwrap_or_default();
                if configured && (key.trim().is_empty() || key.trim() != key || key.contains(['\n', '\r', '\0'])) {
                    return Self::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        "api_key must be a non-empty, trimmed, non-NUL single line",
                    );
                }
                if let Err(message) = Self::write_credential(file_name, env_keys, configured.then_some(key)) {
                    return Self::error_reply(api_id, ErrorCode::Internal, &message);
                }
                let id = self.legacy_id();
                self.pending_simple.push((
                    id,
                    api_id,
                    SimpleKind::Credential {
                        provider: provider.to_string(),
                        configured,
                    },
                ));
                vec![Outbound::Legacy(json!({
                    "type": "notify_auth_changed",
                    "id": id,
                    "provider": provider,
                    "auth": {
                        "provider": provider,
                        "credential_source": "api_key_file",
                        "auth_method": "remote_tui_paste_api_key"
                    }
                }))]
            }
            "read_file" => {
                let session_id = request["session_id"].as_str().unwrap_or_default();
                let relative = request["path"].as_str().unwrap_or_default();
                let max = request["max_bytes"]
                    .as_u64()
                    .unwrap_or(DEFAULT_FILE_BYTES)
                    .min(MAX_FILE_BYTES);
                match Self::read_session_file(session_id, relative, max) {
                    Ok((content, size, truncated)) => vec![Outbound::Reply(ServerFrame::reply(
                        api_id,
                        ApiEvent::FileContent {
                            session_id: session_id.to_string(),
                            path: relative.to_string(),
                            content,
                            size,
                            truncated,
                        },
                    ))],
                    Err((code, message)) => Self::error_reply(api_id, code, &message),
                }
            }
            "find_files" => {
                let session_id = request["session_id"].as_str().unwrap_or_default();
                let query = request["query"].as_str().unwrap_or_default();
                let limit = request["limit"]
                    .as_u64()
                    .unwrap_or(DEFAULT_FIND_LIMIT as u64)
                    .min(MAX_FIND_LIMIT as u64) as usize;
                match Self::find_session_files(session_id, query, limit) {
                    Ok(paths) => vec![Outbound::Reply(ServerFrame::reply(
                        api_id,
                        ApiEvent::Files {
                            session_id: session_id.to_string(),
                            paths,
                        },
                    ))],
                    Err((code, message)) => Self::error_reply(api_id, code, &message),
                }
            }
            "search_text" => {
                let session_id = request["session_id"].as_str().unwrap_or_default();
                let query = request["query"].as_str().unwrap_or_default();
                let under = request["path"].as_str();
                let limit = request["limit"]
                    .as_u64()
                    .unwrap_or(DEFAULT_FIND_LIMIT as u64)
                    .min(MAX_FIND_LIMIT as u64) as usize;
                match Self::search_session_text(session_id, query, under, limit) {
                    Ok(matches) => vec![Outbound::Reply(ServerFrame::reply(
                        api_id,
                        ApiEvent::TextMatches {
                            session_id: session_id.to_string(),
                            matches,
                        },
                    ))],
                    Err((code, message)) => Self::error_reply(api_id, code, &message),
                }
            }
            "get_todo_state" => {
                let session_id = request["session_id"].as_str().unwrap_or_default();
                match Self::todo_home(session_id) {
                    Ok(home) => vec![Outbound::Reply(ServerFrame::reply(
                        api_id,
                        ApiEvent::TodoState {
                            session_id: session_id.to_string(),
                            state: horon_agent_todo_policy::store::load_snapshot(&home, session_id),
                        },
                    ))],
                    Err((code, message)) => Self::error_reply(api_id, code, &message),
                }
            }
            "ack_todo_follow_up" => {
                let session_id = request["session_id"].as_str().unwrap_or_default();
                let effects: horon_agent_todo_policy::TodoEffects =
                    match serde_json::from_value(request["effects"].clone()) {
                        Ok(effects) => effects,
                        Err(error) => {
                            return Self::error_reply(
                                api_id,
                                ErrorCode::InvalidRequest,
                                &format!("invalid effects: {error}"),
                            );
                        }
                    };
                let result = Self::todo_home(session_id).and_then(|home| {
                    horon_agent_todo_policy::store::apply_effects(&home, session_id, effects)
                        .map_err(|error| (ErrorCode::Internal, error.to_string()))
                });
                match result {
                    Ok(()) => vec![Outbound::Reply(ServerFrame::reply(api_id, ApiEvent::Ok))],
                    Err((code, message)) => Self::error_reply(api_id, code, &message),
                }
            }
            "file_status" => {
                let session_id = request["session_id"].as_str().unwrap_or_default();
                let relative = request["path"].as_str().unwrap_or_default();
                match Self::session_file_status(session_id, relative) {
                    Ok((exists, kind, size, modified_ms)) => {
                        vec![Outbound::Reply(ServerFrame::reply(
                            api_id,
                            ApiEvent::FileStatus {
                                session_id: session_id.to_string(),
                                path: relative.to_string(),
                                exists,
                                kind,
                                size,
                                modified_ms,
                            },
                        ))]
                    }
                    Err((code, message)) => Self::error_reply(api_id, code, &message),
                }
            }
            "search_sessions" | "list_artifacts" => search_artifacts::handle(req, api_id, request),
            "set_model" => {
                let model = request["model"].as_str().unwrap_or("");
                if model.is_empty() {
                    return vec![Outbound::Reply(ServerFrame::reply(
                        api_id,
                        ApiEvent::Error {
                            code: ErrorCode::InvalidRequest,
                            message: "set_model needs a non-empty `model`".into(),
                            details: None,
                        },
                    ))];
                }
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Model));
                vec![Outbound::Legacy(json!({
                    "type": "set_model",
                    "id": id,
                    "model": model,
                }))]
            }
            "set_reasoning_effort" => {
                let effort = request["effort"].as_str().unwrap_or("");
                if effort.is_empty() {
                    return vec![Outbound::Reply(ServerFrame::reply(
                        api_id,
                        ApiEvent::Error {
                            code: ErrorCode::InvalidRequest,
                            message: "set_reasoning_effort needs a non-empty `effort`".into(),
                            details: None,
                        },
                    ))];
                }
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::ReasoningEffort));
                vec![Outbound::Legacy(json!({
                    "type": "set_reasoning_effort",
                    "id": id,
                    "effort": effort,
                }))]
            }
            "compact" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Compact));
                vec![Outbound::Legacy(compact_handoff::request(id, request))]
            }
            "rename_session" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                let mut rename = json!({"type": "rename_session", "id": id});
                // An absent title clears it and restores the generated one, so
                // null and "" must stay distinguishable on the wire.
                if let Some(title) = request["title"].as_str() {
                    rename["title"] = json!(title);
                }
                vec![Outbound::Legacy(rename)]
            }
            "set_session_saved" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                let mut save = json!({
                    "type": "set_session_saved",
                    "id": id,
                    "saved": request["saved"].as_bool().unwrap_or(true),
                });
                if let Some(label) = request["label"].as_str() {
                    save["label"] = json!(label);
                }
                vec![Outbound::Legacy(save)]
            }
            "rewind_undo" => {
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({"type": "rewind_undo", "id": id}))]
            }
            "cancel_soft_interrupts" => {
                let client_ids = match request.get("client_ids") {
                    None | Some(Value::Null) => None,
                    Some(Value::Array(ids)) if ids.iter().all(Value::is_string) => Some(
                        ids.iter()
                            .filter_map(|id| id.as_str().map(str::to_string))
                            .collect::<Vec<_>>(),
                    ),
                    Some(_) => {
                        return Self::error_reply(
                            api_id,
                            ErrorCode::InvalidRequest,
                            "`client_ids` must be an array of strings",
                        );
                    }
                };
                let id = self.legacy_id();
                match client_ids {
                    Some(client_ids) => {
                        // An older daemon ignores the filter and would clear
                        // the whole queue: refuse instead of over-cancelling.
                        if !self.soft_interrupt_ids_supported {
                            return Self::error_reply(
                                api_id,
                                ErrorCode::UnknownRequest,
                                "daemon does not advertise the soft_interrupt_ids capability",
                            );
                        }
                        self.pending_simple.push((id, api_id, SimpleKind::SoftInterruptCancel));
                        vec![Outbound::Legacy(json!({
                            "type": "cancel_soft_interrupts",
                            "id": id,
                            "client_ids": client_ids,
                        }))]
                    }
                    None => {
                        self.pending_simple.push((id, api_id, SimpleKind::Ok));
                        vec![Outbound::Legacy(json!({"type": "cancel_soft_interrupts", "id": id}))]
                    }
                }
            }
            "background_tool" => {
                let id = self.legacy_id();
                let kind = if self.soft_interrupt_ids_supported {
                    SimpleKind::BackgroundTool
                } else {
                    SimpleKind::Ok
                };
                self.pending_simple.push((id, api_id, kind));
                vec![Outbound::Legacy(json!({"type": "background_tool", "id": id}))]
            }
            "detach_session" => {
                let id = self.legacy_id();
                // Done is the release barrier. The daemon's initial Ack is
                // sent before it releases SDK callback ownership.
                self.pending_simple.push((id, api_id, SimpleKind::Detach));
                vec![Outbound::Legacy(json!({"type": "prepare_disconnect", "id": id}))]
            }
            "permission_response" => {
                // The legacy protocol carries the decision as
                // `permission_decision`; the daemon records it in the same
                // durable store channel replies and the IMAP poller write to,
                // so the waiting tool sees it on its next poll and emits
                // `approval_resolved`.
                let Some(request_id) = request["request_id"].as_str().filter(|value| !value.is_empty()) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "request_id must not be empty");
                };
                let Some(decision) = request["decision"].as_str() else {
                    return Self::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        "decision must be one of allow, allow_always or deny",
                    );
                };
                if !matches!(decision, "allow" | "allow_always" | "deny") {
                    return Self::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        "decision must be one of allow, allow_always or deny",
                    );
                }
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({
                    "type": "permission_decision",
                    "id": id,
                    "request_id": request_id,
                    "decision": decision,
                }))]
            }
            "ask_response" => {
                // An answer for a question set that is no longer pending is
                // refused by the daemon, which is what keeps a late reply from
                // resolving a newer question or a turn that already continued.
                let Some(ask_id) = request["ask_id"].as_str().filter(|value| !value.is_empty()) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "ask_id must not be empty");
                };
                let Some(answers) = request["answers"].as_array().filter(|answers| !answers.is_empty()) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "answers must be a non-empty array");
                };
                // `answer_id` is the `ask_receipts_v1` field: forwarded so the
                // daemon's answer ledger can replay a retry instead of
                // resolving the question twice. Absent is the legacy request,
                // which must reach the daemon byte-for-byte as it always did,
                // so the key is omitted rather than sent as null.
                let answer_id = request["answer_id"].as_str().filter(|value| !value.is_empty());
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                let mut frame = json!({
                    "type": "ask_response",
                    "id": id,
                    "request_id": ask_id,
                    "answers": answers,
                });
                if let Some(answer_id) = answer_id {
                    frame["answer_id"] = json!(answer_id);
                }
                vec![Outbound::Legacy(frame)]
            }
            "approval_ack" => {
                // The daemon clears the record's durable `pending_event` on this
                // frame, so an ack with no identifier would clear nothing while
                // reporting success. Refuse it instead.
                let Some(request_id) = request["request_id"].as_str().filter(|value| !value.is_empty()) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "request_id must not be empty");
                };
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({
                    "type": "approval_ack",
                    "id": id,
                    "request_id": request_id,
                }))]
            }
            "failover_ack" => {
                // The whole announcement key must travel: a partial key would
                // either clear nothing or, worse, read as a different switch.
                let (Some(chain_id), Some(from), Some(to)) = (
                    request["chain_id"].as_str().filter(|value| !value.is_empty()),
                    request["from"].as_str().filter(|value| !value.is_empty()),
                    request["to"].as_str().filter(|value| !value.is_empty()),
                ) else {
                    return Self::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        "chain_id, from and to must be non-empty strings",
                    );
                };
                let Some(attempt) = request["attempt"].as_u64().and_then(|value| u32::try_from(value).ok()) else {
                    return Self::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        "attempt must be a non-negative integer",
                    );
                };
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Ok));
                vec![Outbound::Legacy(json!({
                    "type": "failover_ack",
                    "id": id,
                    "chain_id": chain_id,
                    "from": from,
                    "to": to,
                    "attempt": attempt,
                }))]
            }
            // The three `durable_runtime_v1` requests are answered by the
            // daemon, which owns the session, its cancel signals, and the
            // control ledger. The bridge only translates: a request it served
            // from its own memory would be a different operation from the one
            // the session actually runs.
            //
            // A daemon that does not advertise the surface is refused with the
            // structured `unsupported` reply rather than forwarded, because an
            // older daemon answers an unknown request with an error frame
            // carrying no id, and the client would wait out its own timeout.
            "get_runtime_state" | "apply_control" | "get_control_receipt" | "get_snapshot"
                if !self.durable_runtime_api_supported =>
            {
                let capability = horon_agent_harness_api::CAPABILITY_DURABLE_RUNTIME_V1;
                vec![Outbound::Reply(horon_agent_harness_api::unsupported_reply(
                    api_id, capability, req,
                ))]
            }
            "get_runtime_state" => {
                let Some(session_id) = Self::request_session(request, api_id) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
                };
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::RuntimeState));
                vec![Outbound::Legacy(json!({
                    "type": "get_runtime_state",
                    "id": id,
                    "session_id": session_id,
                }))]
            }
            "apply_control" => {
                let Some(session_id) = Self::request_session(request, api_id) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
                };
                // The operation id is the whole safety of a retry: without it a
                // resend would apply the operation twice, so a request that
                // carries none is refused instead of guessed at.
                let Some(operation_id) = request["operation_id"].as_str().filter(|value| !value.is_empty()) else {
                    return Self::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        "operation_id must not be empty; it is what makes a retry apply once",
                    );
                };
                let Some(action) = request["action"].as_str().filter(|value| !value.is_empty()) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "action must not be empty");
                };
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::ControlReceipt));
                let mut frame = json!({
                    "type": "apply_control",
                    "id": id,
                    "session_id": session_id,
                    "operation_id": operation_id,
                    "action": action,
                });
                if let Some(args) = request.get("args").filter(|args| !args.is_null()) {
                    frame["args"] = args.clone();
                }
                vec![Outbound::Legacy(frame)]
            }
            "get_control_receipt" => {
                let Some(session_id) = Self::request_session(request, api_id) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
                };
                let Some(operation_id) = request["operation_id"].as_str().filter(|value| !value.is_empty()) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "operation_id must not be empty");
                };
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::ControlReceipt));
                vec![Outbound::Legacy(json!({
                    "type": "get_control_receipt",
                    "id": id,
                    "session_id": session_id,
                    "operation_id": operation_id,
                }))]
            }
            "get_snapshot" => {
                let Some(session_id) = Self::request_session(request, api_id) else {
                    return Self::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
                };
                let id = self.legacy_id();
                self.pending_simple.push((id, api_id, SimpleKind::Snapshot));
                let mut frame = json!({
                    "type": "get_snapshot",
                    "id": id,
                    "session_id": session_id,
                });
                // Optional fields travel only when the client sent them, so a
                // bare request asks for the daemon's own defaults.
                for field in ["cursor", "tail", "max_bytes"] {
                    if let Some(value) = request.get(field).filter(|value| !value.is_null()) {
                        frame[field] = value.clone();
                    }
                }
                vec![Outbound::Legacy(frame)]
            }
            other => vec![Outbound::Reply(ServerFrame::reply(
                api_id,
                ApiEvent::Error {
                    code: ErrorCode::UnknownRequest,
                    message: format!("unknown request: {other}"),
                    details: None,
                },
            ))],
        }
    }

    fn attachment_recovery(event: &Value) -> Option<ServerFrame> {
        if event["messages"].as_array().is_none_or(Vec::is_empty)
            || event["activity"]["is_processing"].as_bool() == Some(true)
        {
            return None;
        }
        let directive = &event["reload_recovery"];
        let (continuation_message, reconnect_notice) = if let Some(message) = directive["continuation_message"].as_str()
        {
            (
                message.to_string(),
                directive["reconnect_notice"].as_str().map(str::to_string),
            )
        } else if event["was_interrupted"].as_bool() == Some(true) {
            // Compatibility with daemons predating reload_recovery. Keep in
            // sync with ReloadContext::interrupted_session_continuation_message.
            ("Your session was interrupted by a server reload while a tool was running. The tool was aborted and results may be incomplete. Continue exactly where you left off and do not ask the user what to do next.".to_string(), None)
        } else {
            return None;
        };
        if continuation_message.trim().is_empty() {
            return None;
        }
        Some(ServerFrame::event(ApiEvent::SessionRecovery {
            session_id: event["session_id"].as_str()?.to_string(),
            continuation_message,
            reconnect_notice,
        }))
    }

    fn text_message_id(&mut self) -> String {
        if let Some(id) = &self.text_message_id {
            return id.clone();
        }
        let id = format!("text-{}", NEXT_TEXT_ID.fetch_add(1, Ordering::Relaxed));
        self.text_message_id = Some(id.clone());
        self.text_attempt.push((id.clone(), String::new(), false));
        id
    }

    fn finish_text(&mut self) -> Vec<ServerFrame> {
        let Some(id) = self.text_message_id.take() else {
            return vec![];
        };
        let Some((_, text, done)) = self.text_attempt.iter_mut().find(|(key, _, _)| key == &id) else {
            return vec![];
        };
        if text.is_empty() || *done {
            return vec![];
        }
        *done = true;
        vec![ServerFrame::event(ApiEvent::TextDone {
            session_id: self.session_id.clone().unwrap_or_default(),
            message_id: Some(id),
        })]
    }

    fn replace_text_suffix(&mut self, old_text: &str, new_text: &str) -> Vec<ServerFrame> {
        if old_text.is_empty() {
            return vec![];
        }
        // Citation annotations arrive at output_item.done, immediately before
        // TextDone. Correct only the active message, never redistribute text
        // across earlier messages using their pre-correction byte lengths.
        let Some(id) = self.text_message_id.as_ref() else {
            return vec![];
        };
        let Some((_, text, _)) = self.text_attempt.iter_mut().find(|(key, _, _)| key == id) else {
            return vec![];
        };
        let Some(prefix) = text.strip_suffix(old_text) else {
            return vec![];
        };
        if old_text == new_text {
            return vec![];
        }
        let prefix_len = prefix.len();
        text.truncate(prefix_len);
        text.push_str(new_text);
        vec![ServerFrame::event(ApiEvent::TextReplace {
            session_id: self.session_id.clone().unwrap_or_default(),
            message_id: Some(id.clone()),
            text: text.clone(),
        })]
    }

    fn replace_text(&mut self, replacement: &str) -> Vec<ServerFrame> {
        // Legacy replacements cover the entire provider response, including
        // text messages already closed by output_item.done. Distribute the
        // replacement over those boundaries, retracting truncated messages.
        if self.text_attempt.is_empty() && !replacement.is_empty() {
            self.text_message_id();
        }
        let count = self.text_attempt.len();
        let mut remaining = replacement;
        let mut frames = Vec::new();
        for (index, (id, text, done)) in self.text_attempt.iter_mut().enumerate() {
            let mut len = if index + 1 == count {
                remaining.len()
            } else {
                text.len().min(remaining.len())
            };
            while !remaining.is_char_boundary(len) {
                len -= 1;
            }
            let next = &remaining[..len];
            remaining = &remaining[len..];
            if text != next {
                *text = next.to_string();
                frames.push(ServerFrame::event(ApiEvent::TextReplace {
                    session_id: self.session_id.clone().unwrap_or_default(),
                    message_id: Some(id.clone()),
                    text: text.clone(),
                }));
            }
            // Wrapped-tool recovery can restore a suffix after MessageEnd,
            // including text that was empty (and therefore never framed) at
            // its original boundary. Complete it without duplicating markers
            // for previously completed messages corrected in place.
            if !text.is_empty() && !*done && (self.text_response_ended || self.text_message_id.as_ref() != Some(id)) {
                *done = true;
                frames.push(ServerFrame::event(ApiEvent::TextDone {
                    session_id: self.session_id.clone().unwrap_or_default(),
                    message_id: Some(id.clone()),
                }));
            }
        }
        if self.text_response_ended {
            self.text_message_id = None;
        }
        frames
    }

    /// Translate one legacy server event (raw JSON) into API frames.
    pub fn legacy_event_to_api(&mut self, event: &Value) -> Vec<ServerFrame> {
        let frames = self.translate_legacy_event(event);
        self.observe_pending_requests();
        frames
    }

    fn translate_legacy_event(&mut self, event: &Value) -> Vec<ServerFrame> {
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "runtime_command_result" {
            let legacy_id = event["id"].as_u64().unwrap_or(0);
            let Some(api_id) = self.take_simple(legacy_id, SimpleKind::RuntimeCommand) else {
                return vec![];
            };
            let Ok(command) = serde_json::from_value(event["command"].clone()) else {
                return vec![ServerFrame::reply(
                    api_id,
                    ApiEvent::Error {
                        code: ErrorCode::Internal,
                        message: "Invalid runtime command response".into(),
                        details: None,
                    },
                )];
            };
            return vec![ServerFrame::reply(
                api_id,
                ApiEvent::RuntimeCommandResult {
                    session_id: self.session_id.clone().unwrap_or_default(),
                    command,
                    data: event["data"].clone(),
                    message: event["message"].as_str().unwrap_or("").into(),
                },
            )];
        }
        // Filter stale owner callbacks before updating any turn/text state.
        if kind == "tool_call"
            && (self.session_id.is_none() || event["session_id"].as_str() != self.session_id.as_deref())
        {
            return vec![];
        }
        let session = |state: &Self| state.session_id.clone().unwrap_or_default();
        if self.text_response_ended
            && matches!(
                kind,
                "text_delta" | "reasoning_delta" | "tool_start" | "tool_exec" | "tool_call"
            )
        {
            // A new response or execution commits the previous provider
            // attempt. Do not retract it if the next response retries before
            // producing any text (reasoning/tool output is replay-visible too).
            self.text_attempt.clear();
            self.text_response_ended = false;
        }
        if matches!(
            kind,
            "text_delta" | "reasoning_delta" | "tool_start" | "tool_exec" | "tool_call"
        ) || (kind == "connection_phase"
            && event["phase"].as_str().is_some_and(|phase| {
                matches!(
                    phase,
                    "authenticating" | "connecting" | "sending request" | "waiting for response" | "streaming"
                ) || phase.starts_with("retrying (")
            }))
        {
            self.observed_turn_active = true;
            self.activity_version += 1;
        }
        let mut frames = match kind {
            "tools" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let Some(api_id) = self.take_simple(id, SimpleKind::Tools) else {
                    return vec![];
                };
                let reply = match serde_json::from_value(event["tools"].clone()) {
                    Ok(tools) => ApiEvent::Tools {
                        session_id: session(self),
                        tools,
                    },
                    Err(error) => ApiEvent::Error {
                        code: ErrorCode::Internal,
                        message: format!("invalid daemon tools response: {error}"),
                        details: None,
                    },
                };
                vec![ServerFrame::reply(api_id, reply)]
            }
            "tool_call" => {
                // A custom call is an execution request, not a tool-start update.
                // Do not manufacture empty identifiers from malformed events.
                let (Some(call_id), Some(name), Some(input), Some(session_id)) = (
                    event["call_id"].as_str(),
                    event["name"].as_str(),
                    event.get("input"),
                    event["session_id"].as_str(),
                ) else {
                    return vec![];
                };
                // The callback owner may have switched attachments since this
                // event was queued. Never run another session's input using
                // the newly attached session's handler.
                if self.session_id.as_deref() != Some(session_id) {
                    return vec![];
                }
                vec![ServerFrame::event(ApiEvent::ToolCall {
                    session_id: session_id.to_string(),
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    input: input.clone(),
                })]
            }

            "session" => {
                let session_id = event["session_id"].as_str().unwrap_or("").to_string();
                // `session` is a broadcast lifecycle notification. The daemon
                // sends it for other live sessions too, so it cannot identify
                // which session this connection is attached to. Attachment
                // identity comes from the correlated `state` reply below.
                // Treating this broadcast as connection state made a panel for
                // session A start rejecting its own commands after session B
                // announced itself.
                vec![ServerFrame::event(ApiEvent::SessionStatus {
                    session_id,
                    status: "attached".into(),
                    pending_soft_interrupts: None,
                })]
            }
            "state" => {
                let session_id = event["session_id"].as_str().unwrap_or("").to_string();
                let id = event["id"].as_u64().unwrap_or(0);
                if let Some((state_id, api_id, requested_session)) = &self.pending_attach_id
                    && *state_id == id
                    && requested_session
                        .as_deref()
                        .is_none_or(|requested| requested == session_id)
                {
                    let api_id = *api_id;
                    self.pending_attach_id = None;
                    self.pending_attach_subscribe_id = None;
                    // `state` snapshots can also be broadcast for other live
                    // sessions. Only the reply correlated with this connection's
                    // attach request establishes its identity. Otherwise opening
                    // a second panel can retarget the first panel's bridge and
                    // make its next command fail with a wrong-session error.
                    if !session_id.is_empty() {
                        if self.session_id.as_ref().is_some_and(|old| old != &session_id) {
                            self.available_models.clear();
                            self.available_routes.clear();
                            self.current_model = None;
                            self.current_provider = None;
                            self.current_effort = None;
                            self.current_credential = None;
                            self.model_catalog_loaded = false;
                            self.usage.reset_identity();
                        }
                        if self.session_id.as_deref() != Some(&session_id) {
                            self.text_message_id = None;
                            self.text_attempt.clear();
                        }
                        self.session_id = Some(session_id.clone());
                    }
                    let fresh_activity = self.pending_activity_snapshots.remove(&id) == Some(self.activity_version);
                    if fresh_activity {
                        self.observed_turn_active = event["is_processing"].as_bool().unwrap_or(false);
                    }
                    self.usage.note_state(&session_id, event);
                    let metadata = Self::resolve_session_metadata(&session_id);
                    // New sessions persist nothing until the first prompt.
                    let working_dir = metadata
                        .as_ref()
                        .and_then(|metadata| metadata.working_dir.clone())
                        .or_else(|| self.pending_create_dir.take())
                        .or_else(|| self.session_dirs.get(&session_id).cloned());
                    self.pending_create_dir = None;
                    if let Some(dir) = &working_dir
                        && !session_id.is_empty()
                    {
                        self.session_dirs
                            .entry(session_id.clone())
                            .or_insert_with(|| dir.clone());
                    }
                    let mut frames = vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::Attached {
                            session: SessionInfo {
                                user_connected: self.user_connected_sessions.contains(&session_id),
                                edit_stats: None,
                                parent_session_id: None,
                                agent_label: None,
                                swarm_status: None,
                                transcript_bytes: Self::transcript_bytes(&session_id),
                                saved: metadata.as_ref().is_some_and(|value| value.saved),
                                updated_at_ms: metadata
                                    .as_ref()
                                    .and_then(|value| value.updated_at_ms)
                                    .or_else(|| Self::session_modified_ms(&session_id).map(|value| value as i64)),
                                last_active_at_ms: metadata.as_ref().and_then(|value| value.last_active_at_ms),
                                session_id: session_id.clone(),
                                working_dir,
                                title: metadata.as_ref().and_then(PersistedSessionMetadata::display_title),
                                status: if event["is_processing"].as_bool().unwrap_or(false) {
                                    "processing".into()
                                } else {
                                    "idle".into()
                                },
                                archived: false,
                                archived_at_ms: None,
                                save_label: metadata.as_ref().and_then(PersistedSessionMetadata::save_label),
                            },
                        },
                    )];
                    if fresh_activity {
                        frames.push(ServerFrame::event(ApiEvent::SessionStatus {
                            session_id,
                            status: if self.observed_turn_active { "running" } else { "idle" }.into(),
                            pending_soft_interrupts: event["pending_soft_interrupts"]
                                .as_u64()
                                .map(|count| count as u32),
                        }));
                    }
                    frames
                } else {
                    vec![]
                }
            }
            "text_delta" => {
                let text = event["text"].as_str().unwrap_or("").to_string();
                let message_id = if text.is_empty() {
                    self.text_message_id.clone()
                } else {
                    let id = self.text_message_id();
                    self.text_attempt.last_mut().unwrap().1.push_str(&text);
                    Some(id)
                };
                vec![ServerFrame::event(ApiEvent::TextDelta {
                    session_id: session(self),
                    text,
                    message_id,
                })]
            }
            "kv_cache_request" => {
                // Sent unconditionally immediately before complete_split, so
                // this is an actual provider-request boundary, not a status
                // heuristic. Late post-processing replacements precede it.
                let frames = self.finish_text();
                self.text_attempt.clear();
                self.text_response_ended = false;
                frames
            }
            "text_done" => self.finish_text(),
            "message_end" => {
                let frames = self.finish_text();
                // Post-processing can still replace wrapped-tool text after
                // MessageEnd. Retain it until the next response's first delta.
                self.text_response_ended = true;
                frames
            }
            "text_replace" => self.replace_text(event["text"].as_str().unwrap_or("")),
            "text_suffix_replace" => match (event["old_text"].as_str(), event["new_text"].as_str()) {
                (Some(old_text), Some(new_text)) => self.replace_text_suffix(old_text, new_text),
                _ => vec![],
            },
            "retry_rollback" => {
                let frames = self.replace_text("");
                self.text_message_id = None;
                self.text_attempt.clear();
                frames
            }
            "reasoning_delta" => vec![ServerFrame::event(ApiEvent::ReasoningDelta {
                session_id: session(self),
                text: event["text"].as_str().unwrap_or("").to_string(),
            })],
            "reasoning_done" => vec![ServerFrame::event(ApiEvent::ReasoningDone {
                session_id: session(self),
                duration_secs: event["duration_secs"].as_f64(),
            })],
            "connection_phase" => vec![ServerFrame::event(ApiEvent::ConnectionPhase {
                session_id: session(self),
                phase: event["phase"].as_str().unwrap_or("connecting").to_string(),
            })],
            // Argument streaming is not an assistant-message boundary.
            "tool_start" => vec![ServerFrame::event(ApiEvent::ToolStart {
                session_id: session(self),
                call_id: event["id"].as_str().unwrap_or("").to_string(),
                name: event["name"].as_str().unwrap_or("").to_string(),
            })],
            "tool_input" => vec![ServerFrame::event(ApiEvent::ToolInputDelta {
                session_id: session(self),
                call_id: event["id"].as_str().unwrap_or("").to_string(),
                delta: event["delta"].as_str().unwrap_or("").to_string(),
            })],
            "tool_exec" => {
                let mut frames = self.finish_text();
                frames.push(ServerFrame::event(ApiEvent::ToolExec {
                    session_id: session(self),
                    call_id: event["id"].as_str().unwrap_or("").to_string(),
                    name: event["name"].as_str().unwrap_or("").to_string(),
                }));
                frames
            }
            "tool_done" => vec![ServerFrame::event(ApiEvent::ToolDone {
                session_id: session(self),
                call_id: event["id"].as_str().unwrap_or("").to_string(),
                name: event["name"].as_str().unwrap_or("").to_string(),
                output: event["output"].as_str().unwrap_or("").to_string(),
                error: event["error"].as_str().map(str::to_string),
            })],
            "side_pane_images" => vec![ServerFrame::event(ApiEvent::SidePaneImages {
                session_id: event["session_id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| session(self)),
                images: serde_json::from_value(event["images"].clone()).unwrap_or_default(),
            })],
            "tokens" => {
                let session_id = session(self);
                vec![ServerFrame::event(usage_report::token_usage_event(
                    &mut self.usage,
                    &session_id,
                    event,
                ))]
            }
            "kv_cache_miss" => vec![ServerFrame::event(ApiEvent::KvCacheMiss {
                session_id: session(self),
                reason: event["reason"].as_str().unwrap_or("unknown").to_string(),
                harness_caused: event["harness_caused"].as_bool().unwrap_or(false),
                missed_tokens: event["missed_tokens"].as_u64().unwrap_or(0),
                expected_tokens: event["expected_tokens"].as_u64().unwrap_or(0),
                read_tokens: event["read_tokens"].as_u64().unwrap_or(0),
                documented_cause: event["documented_cause"].as_str().map(str::to_string),
                message: event["message"].as_str().unwrap_or("KV cache miss").to_string(),
            })],
            "done" => {
                let id = event["id"].as_u64().unwrap_or(0);
                if let Some(api_id) = self.take_simple(id, SimpleKind::Detach) {
                    self.pending_control_done_ids.remove(&id);
                    self.session_id = None;
                    self.turn_stop_reported = false;
                    self.observed_turn_active = false;
                    self.text_message_id = None;
                    self.text_attempt.clear();
                    return vec![ServerFrame::reply(api_id, ApiEvent::Ok)];
                }
                // Subscribe and controls also emit `done`. Exclude their ids,
                // but retain session-scoped completions fanned out by another
                // attachment or a server-initiated turn (whose id is zero).
                let completed_message = self.pending_message_id == Some(id);
                let completed_idle_interrupt = self.pending_soft_interrupt_ids.contains(&id);
                let control_done = self.pending_control_done_ids.remove(&id);
                let observed_done = self.observed_turn_active && !control_done && self.session_id.is_some();
                if completed_message || completed_idle_interrupt || observed_done {
                    self.observed_turn_active = false;
                    self.turn_stop_reported = false;
                    self.activity_version += 1;
                    self.pending_message_id = None;
                    // Any other retained soft interrupts were queued into the
                    // turn which just ended. Their ids will not emit `done`.
                    self.pending_soft_interrupt_ids.clear();
                    let mut frames = self.finish_text();
                    self.text_attempt.clear();
                    frames.push(ServerFrame::event(ApiEvent::TurnDone {
                        session_id: session(self),
                        pending_soft_interrupts: self.pending_soft_interrupts_at_done.take(),
                    }));
                    frames
                } else {
                    vec![]
                }
            }
            "turn_stopped" | "provider_guardrail" => {
                if self.session_id.is_none() || self.turn_stop_reported {
                    return vec![];
                }
                self.turn_stop_reported = true;
                self.observed_turn_active = true;
                let reason = if kind == "provider_guardrail" {
                    horon_agent_harness_api::TurnStopReason::ProviderGuardrail
                } else {
                    serde_json::from_value(event["reason"].clone())
                        .unwrap_or(horon_agent_harness_api::TurnStopReason::Unknown)
                };
                vec![ServerFrame::event(ApiEvent::TurnStopped {
                    session_id: session(self),
                    reason,
                    message: event["message"]
                        .as_str()
                        .unwrap_or("The turn stopped unexpectedly.")
                        .to_string(),
                    provider_stop_reason: event["provider_stop_reason"]
                        .as_str()
                        .or_else(|| event["stop_reason"].as_str())
                        .map(str::to_string),
                })]
            }
            "interrupted" => {
                self.activity_version += 1;
                let mut frames = vec![];
                // Older runtimes may only emit Interrupted. An idle cancel is
                // not an abnormal turn, and a late notice after Done is not a
                // second stop. Keep observed activity until Done for observers.
                if !self.turn_stop_reported && (self.observed_turn_active || self.pending_message_id.is_some()) {
                    self.turn_stop_reported = true;
                    frames.push(ServerFrame::event(ApiEvent::TurnStopped {
                        session_id: session(self),
                        reason: horon_agent_harness_api::TurnStopReason::Interrupted,
                        message: "The turn was interrupted by a cancellation request.".into(),
                        provider_stop_reason: None,
                    }));
                }
                frames.push(ServerFrame::event(ApiEvent::SessionStatus {
                    session_id: session(self),
                    status: "cancelled".into(),
                    pending_soft_interrupts: None,
                }));
                frames
            }
            "wake_requested" => vec![ServerFrame::event(ApiEvent::WakeRequested {
                session_id: event["session_id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| session(self)),
                reason: event["reason"].as_str().unwrap_or("").to_string(),
                notification: event["notification"].as_str().unwrap_or("").to_string(),
            })],
            "context_message_added" => {
                let id = event["id"].as_u64().unwrap_or(0);
                if self
                    .pending_no_reply_message_id
                    .is_some_and(|(legacy_id, _)| legacy_id == id)
                {
                    let (_, api_id) = self.pending_no_reply_message_id.take().unwrap();
                    vec![ServerFrame::reply(api_id, ApiEvent::Ok)]
                } else {
                    vec![]
                }
            }
            "split_response" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let Some((_, api_id)) = self.pending_fork_id.take().filter(|(legacy_id, _)| *legacy_id == id) else {
                    return vec![];
                };
                let session_id = event["new_session_id"].as_str().unwrap_or_default().to_string();
                let metadata = Self::resolve_session_metadata(&session_id);
                vec![ServerFrame::reply(
                    api_id,
                    ApiEvent::SessionForked {
                        session: SessionInfo {
                            user_connected: self.user_connected_sessions.contains(&session_id),
                            edit_stats: None,
                            parent_session_id: None,
                            agent_label: None,
                            swarm_status: None,
                            transcript_bytes: Self::transcript_bytes(&session_id),
                            saved: metadata.as_ref().is_some_and(|value| value.saved),
                            updated_at_ms: Self::session_modified_ms(&session_id).map(|value| value as i64),
                            last_active_at_ms: metadata.as_ref().and_then(|value| value.last_active_at_ms),
                            session_id,
                            working_dir: metadata.as_ref().and_then(|value| value.working_dir.clone()),
                            title: event["new_session_name"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| metadata.as_ref().and_then(PersistedSessionMetadata::display_title)),
                            status: "idle".into(),
                            archived: false,
                            archived_at_ms: None,
                            save_label: metadata.as_ref().and_then(PersistedSessionMetadata::save_label),
                        },
                    },
                )]
            }
            "pong" => self
                .take_simple(event["id"].as_u64().unwrap_or(0), SimpleKind::Ping)
                .map(|api_id| vec![ServerFrame::reply(api_id, ApiEvent::Pong)])
                .unwrap_or_default(),
            "history" => {
                let id = event["id"].as_u64().unwrap_or(0);
                // The daemon volunteers the full session set on `history`,
                // which is the only place it appears: remember it so
                // `list_sessions` can answer with more than this connection.
                self.note_sessions(event);
                if self
                    .pending_recovery_history
                    .as_ref()
                    .is_some_and(|(subscribe_id, target)| {
                        *subscribe_id == id
                            && event["session_id"].as_str().is_some_and(|sid| {
                                !sid.is_empty() && target.as_deref().is_none_or(|target| target == sid)
                            })
                    })
                {
                    self.pending_recovery_history = None;
                    let frames: Vec<_> = Self::attachment_recovery(event).into_iter().collect();
                    return frames;
                }
                // The catalog probe rides the same `history` reply shape but
                // carries no messages: it is model identity, not transcript.
                if self.pending_model_probe == Some(id) {
                    self.pending_model_probe = None;
                    self.note_models(event);
                    return vec![
                        ServerFrame::event(self.model_info(session(self), event)),
                        ServerFrame::event(self.runtime_info()),
                    ];
                }
                // A `list_models` that arrived before the catalog did: the
                // client asked too early, so answer it now that it is here.
                if let Some(api_id) = self.take_simple(id, SimpleKind::Models) {
                    self.note_models(event);
                    return vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::Models {
                            session_id: session(self),
                            models: self.available_models.clone(),
                            current: self.current_model.clone(),
                        },
                    )];
                }
                if let Some(api_id) = self.take_simple(id, SimpleKind::RuntimeInfo) {
                    self.note_models(event);
                    return vec![ServerFrame::reply(api_id, self.runtime_info())];
                }
                let Some(api_id) = self.take_simple(id, SimpleKind::History) else {
                    return vec![];
                };
                let mut messages: Vec<HistoryMessage> = event["messages"]
                    .as_array()
                    .map(|messages| {
                        messages
                            .iter()
                            .map(|m| HistoryMessage {
                                response_stats: serde_json::from_value(m["response_stats"].clone()).unwrap_or(None),
                                role: m["role"].as_str().unwrap_or("").to_string(),
                                content: m["content"].as_str().unwrap_or("").to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // A stored terminal-looking row can still belong to a running
                // turn (e.g. an automatic continuation). Do not show a footer yet.
                if event["activity"]["is_processing"].as_bool() == Some(true) {
                    let start = messages.iter().rposition(|m| m.role == "user").unwrap_or(0);
                    for message in &mut messages[start..] {
                        message.response_stats = None;
                    }
                }
                let images = serde_json::from_value(event["images"].clone()).unwrap_or_default();
                let mut frames = vec![ServerFrame::reply(
                    api_id,
                    ApiEvent::History {
                        session_id: session(self),
                        messages,
                        images,
                    },
                )];
                // A snapshot requested before newer stream/terminal events must
                // not resurrect a completed turn or stop a newly started one.
                let fresh_activity = self.pending_activity_snapshots.remove(&id) == Some(self.activity_version);
                if fresh_activity && let Some(active) = event["activity"]["is_processing"].as_bool() {
                    self.observed_turn_active = active;
                    frames.push(ServerFrame::event(ApiEvent::SessionStatus {
                        session_id: session(self),
                        status: if active { "running" } else { "idle" }.into(),
                        pending_soft_interrupts: None,
                    }));
                }
                frames
            }
            // The model can change mid-session (`/model`, a cycle, or an auth
            // change re-resolving the route), so both pushes are forwarded.
            "model_changed" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let reply_to = self.take_simple(id, SimpleKind::Model);
                // The daemon reports a rejected switch in-band, as an `error`
                // field on the success event rather than as an error frame.
                // Relayed as a real error so a client sees a failed request
                // instead of a silent no-op that leaves the picker wrong.
                if let Some(error) = event["error"].as_str() {
                    let frame = ApiEvent::Error {
                        code: ErrorCode::InvalidRequest,
                        message: error.to_string(),
                        details: None,
                    };
                    return match reply_to {
                        Some(api_id) => vec![ServerFrame::reply(api_id, frame)],
                        None => vec![],
                    };
                }
                if let Some(model) = event["model"].as_str() {
                    self.current_model = Some(model.to_string());
                }
                if let Some(provider) = event["provider_name"].as_str() {
                    self.note_provider(provider);
                }
                self.note_credential(event);
                self.usage.note_context_window(event);
                // Newer daemons report the effort the switched-to model runs
                // with (`null` when the switch cleared it). Older ones omit
                // the key, so the cached value is kept as before.
                if event.get("reasoning_effort").is_some() {
                    self.current_effort = event["reasoning_effort"].as_str().map(str::to_string);
                }
                let info = ApiEvent::ModelInfo {
                    session_id: session(self),
                    provider: self.current_provider.clone(),
                    model: self.current_model.clone(),
                    reasoning_effort: self.current_effort.clone(),
                    auth_method: self.current_credential.clone(),
                };
                // Both a reply and a broadcast: the caller needs its request
                // resolved, and every other client watching the session needs
                // to know the model moved under them.
                match reply_to {
                    Some(api_id) => vec![ServerFrame::reply(api_id, ApiEvent::Ok), ServerFrame::event(info)],
                    None => vec![ServerFrame::event(info)],
                }
            }
            "reasoning_effort_changed" => {
                let id = event["id"].as_u64().unwrap_or(0);
                // Remember the new effort even when the change was requested by
                // another client, so later identity events stay truthful.
                // A success without `effort` means the provider now runs with
                // none (e.g. `none` normalised away), which is also a change.
                let changed = event["error"].as_str().is_none() && {
                    let effort = event["effort"].as_str().map(str::to_string);
                    let moved = self.current_effort != effort;
                    self.current_effort = effort;
                    moved
                };
                // A successful change is also broadcast as identity, mirroring
                // model_changed: every attached client needs to know the
                // effort moved under it, not only the one that asked.
                let info = changed.then(|| {
                    ServerFrame::event(ApiEvent::ModelInfo {
                        session_id: session(self),
                        provider: self.current_provider.clone(),
                        model: self.current_model.clone(),
                        reasoning_effort: self.current_effort.clone(),
                        auth_method: self.current_credential.clone(),
                    })
                });
                let Some(api_id) = self.take_simple(id, SimpleKind::ReasoningEffort) else {
                    return info.into_iter().collect();
                };
                match event["error"].as_str() {
                    Some(error) => vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::Error {
                            code: ErrorCode::InvalidRequest,
                            message: error.to_string(),
                            details: None,
                        },
                    )],
                    None => std::iter::once(ServerFrame::reply(api_id, ApiEvent::Ok))
                        .chain(info)
                        .collect(),
                }
            }
            // Compaction is scheduled, not performed inline, and the daemon
            // reports refusal via `success: false` with a reason rather than
            // an error frame. Relayed as a real error so a client is not told
            // "done" about work that never happened.
            "compact_result" => compact_handoff::result_reply(self, event),
            "session_renamed" => {
                let session_id = event["session_id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| session(self));
                vec![ServerFrame::event(ApiEvent::SessionRenamed {
                    session_id,
                    title: event["title"].as_str().map(str::to_string),
                    display_title: event["display_title"].as_str().unwrap_or("").to_string(),
                })]
            }
            "model_usage_updated" => {
                let route = &event["route"];
                let (Some(model), Some(provider), Some(api_method), Ok(usage)) = (
                    route["model"].as_str(),
                    route["provider"].as_str(),
                    route["api_method"].as_str(),
                    serde_json::from_value::<horon_agent_harness_api::ModelUsage>(route["usage"].clone()),
                ) else {
                    return vec![];
                };
                let observed = self
                    .model_usage_updates
                    .entry((model.into(), provider.into(), api_method.into()))
                    .or_default();
                observed.merge_observation(&usage);
                for cached in &mut self.available_routes {
                    if model == cached.model && provider == cached.provider && api_method == cached.api_method {
                        cached
                            .usage
                            .get_or_insert_with(Default::default)
                            .merge_observation(observed);
                    }
                }
                vec![ServerFrame::event(self.runtime_info())]
            }
            "available_models_updated" => {
                self.note_models(event);
                vec![
                    ServerFrame::event(self.model_info(session(self), event)),
                    ServerFrame::event(self.runtime_info()),
                ]
            }
            // Background-task traffic reaches clients as a notification whose
            // body is the markdown the TUI renders. The API refuses to make
            // clients re-derive a progress bar from prose, so the two shapes
            // that matter (a progress tick and a task finishing) are parsed
            // into one typed event and everything else is dropped.
            "notification" => {
                let message = event["message"].as_str().unwrap_or("");
                match parse_background_notification(message) {
                    Some(mut progress) => {
                        progress.session_id = session(self);
                        vec![ServerFrame::event(progress.into_event())]
                    }
                    None => vec![],
                }
            }
            "runtime_state" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let Some(api_id) = self.take_simple(id, SimpleKind::RuntimeState) else {
                    return vec![];
                };
                // The daemon serializes its own summary type, so a field it adds
                // later reaches an older bridge as an ignored key rather than a
                // parse failure. A reply this bridge cannot read is reported as
                // an error instead of being passed on half-decoded.
                match serde_json::from_value::<horon_agent_harness_api::RuntimeStateSummary>(event["state"].clone()) {
                    Ok(state) => vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::RuntimeState {
                            session_id: session(self),
                            state,
                        },
                    )],
                    Err(error) => vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::Error {
                            code: ErrorCode::Internal,
                            message: format!("the daemon's runtime state could not be read: {error}"),
                            details: None,
                        },
                    )],
                }
            }
            "control_receipt" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let Some(api_id) = self.take_simple(id, SimpleKind::ControlReceipt) else {
                    return vec![];
                };
                let outcome = match event["outcome"].as_str().unwrap_or_default() {
                    "terminal" => horon_agent_harness_api::RuntimeControlOutcome::Terminal,
                    "replay" => horon_agent_harness_api::RuntimeControlOutcome::Replay,
                    "conflict" => horon_agent_harness_api::RuntimeControlOutcome::Conflict,
                    "indeterminate" => horon_agent_harness_api::RuntimeControlOutcome::Indeterminate,
                    "applying" => horon_agent_harness_api::RuntimeControlOutcome::Applying,
                    _ => horon_agent_harness_api::RuntimeControlOutcome::Unknown,
                };
                vec![ServerFrame::reply(
                    api_id,
                    ApiEvent::ControlReceipt {
                        session_id: session(self),
                        receipt: horon_agent_harness_api::RuntimeControlReceipt {
                            operation_id: event["operation_id"].as_str().unwrap_or_default().to_string(),
                            action: event["action"].as_str().unwrap_or_default().to_string(),
                            outcome,
                            result: event.get("result").filter(|value| !value.is_null()).cloned(),
                            message: event["message"].as_str().unwrap_or_default().to_string(),
                        },
                    },
                )]
            }
            "snapshot_result" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let Some(api_id) = self.take_simple(id, SimpleKind::Snapshot) else {
                    return vec![];
                };
                // The daemon captured the marker and the view in one critical
                // section and sent them as one frame, so the two frames built
                // here are exactly one marker for one snapshot. A view this
                // bridge cannot read is reported as an error rather than
                // delivered half-decoded.
                let snapshot =
                    match serde_json::from_value::<horon_agent_harness_api::SessionSnapshot>(event["snapshot"].clone())
                    {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            return vec![ServerFrame::reply(
                                api_id,
                                ApiEvent::Error {
                                    code: ErrorCode::Internal,
                                    message: format!("the daemon's snapshot could not be read: {error}"),
                                    details: None,
                                },
                            )];
                        }
                    };
                let session_id = session(self);
                let marker = horon_agent_harness_api::SnapshotMarkerInfo {
                    generation: snapshot.generation.clone(),
                    seq: snapshot.seq,
                    marker_id: snapshot.marker_id.clone(),
                };
                vec![
                    // The marker carries no `reply_to`: it is a stream event, so
                    // it travels the ordinary queue ahead of its reply and a
                    // client classifies it as a boundary rather than an answer.
                    ServerFrame::event(ApiEvent::SnapshotMarker {
                        session_id: session_id.clone(),
                        marker,
                    }),
                    ServerFrame::reply(api_id, ApiEvent::Snapshot { session_id, snapshot }),
                ]
            }
            "ack" => {
                let id = event["id"].as_u64().unwrap_or(0);
                // The daemon acking the in-flight `message` is the proof the
                // agent has the text: report it as its own event so a client
                // can move a message from "sent" to "acknowledged" without
                // waiting for the first token of the reply.
                if self.pending_message_id == Some(id) {
                    // Acceptance starts a turn before any provider output. An
                    // older idle snapshot must not stop this pre-stream turn.
                    self.observed_turn_active = true;
                    self.activity_version += 1;
                    return vec![ServerFrame::event(ApiEvent::MessageAccepted {
                        session_id: session(self),
                    })];
                }
                let Some(index) = self
                    .pending_simple
                    .iter()
                    .position(|(legacy_id, _, _)| *legacy_id == id)
                else {
                    return vec![];
                };
                let (_, api_id, kind) = self.pending_simple.remove(index);
                match kind {
                    SimpleKind::Ok | SimpleKind::ToolControl => {
                        vec![ServerFrame::reply(api_id, ApiEvent::Ok)]
                    }
                    SimpleKind::Credential { provider, configured } => vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::CredentialUpdated { provider, configured },
                    )],
                    // Acks are not terminal replies for richer request kinds.
                    other => {
                        self.pending_simple.push((id, api_id, other));
                        vec![]
                    }
                }
            }
            "error" => {
                // A rejected tool configuration/result is not a failed model
                // turn. Preserve streaming state and correlate only its request.
                let id = event["id"].as_u64().unwrap_or(0);
                if let Some(index) = self.pending_simple.iter().position(|(pending_id, _, kind)| {
                    *pending_id == id && matches!(kind, SimpleKind::Tools | SimpleKind::ToolControl)
                }) {
                    let (_, api_id, _) = self.pending_simple.remove(index);
                    return vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::Error {
                            code: ErrorCode::Internal,
                            message: event["message"].as_str().unwrap_or_default().to_string(),
                            details: None,
                        },
                    )];
                }
                self.activity_version += 1;
                self.observed_turn_active = false;
                let id = event["id"].as_u64().unwrap_or(0);
                self.pending_control_done_ids.remove(&id);
                self.pending_activity_snapshots.remove(&id);
                if let Some((state_id, api_id, target)) = self.pending_attach_id.as_ref()
                    && (self.pending_attach_subscribe_id == Some(id) || *state_id == id)
                {
                    let api_id = *api_id;
                    let code = if target.is_some() {
                        ErrorCode::UnknownSession
                    } else {
                        ErrorCode::Internal
                    };
                    self.pending_attach_id = None;
                    self.pending_attach_subscribe_id = None;
                    self.pending_model_probe = None;
                    self.pending_recovery_history = None;
                    return vec![ServerFrame::reply(
                        api_id,
                        ApiEvent::Error {
                            code,
                            message: event["message"].as_str().unwrap_or_default().to_string(),
                            details: None,
                        },
                    )];
                }
                self.pending_soft_interrupt_ids.retain(|pending_id| *pending_id != id);
                let message = event["message"].as_str().unwrap_or("").to_string();
                // A turn that fails ends with `error` *instead of* `done`, so
                // the turn is over: forget the pending message, or a later
                // unrelated `done` carrying the same id would be reported as
                // this turn finishing.
                if self.pending_message_id == Some(id) {
                    self.pending_message_id = None;
                }
                let no_reply_api_id = self
                    .pending_no_reply_message_id
                    .filter(|(legacy_id, _)| *legacy_id == id)
                    .map(|(_, api_id)| api_id);
                if no_reply_api_id.is_some() {
                    self.pending_no_reply_message_id = None;
                }
                let fork_api_id = self
                    .pending_fork_id
                    .filter(|(legacy_id, _)| *legacy_id == id)
                    .map(|(_, api_id)| api_id);
                if fork_api_id.is_some() {
                    self.pending_fork_id = None;
                }
                // Route to a pending request when possible, else stream it.
                let reply_to = no_reply_api_id.or(fork_api_id).or_else(|| {
                    self.pending_simple
                        .iter()
                        .position(|(legacy_id, _, _)| *legacy_id == id)
                        .map(|index| self.pending_simple.remove(index).1)
                });
                let frame_event = ApiEvent::Error {
                    code: ErrorCode::Internal,
                    message,
                    details: None,
                };
                let mut frames = vec![match reply_to {
                    Some(api_id) => ServerFrame::reply(api_id, frame_event),
                    None => ServerFrame::event(frame_event),
                }];
                if reply_to.is_none() && self.turn_stop_reported {
                    self.turn_stop_reported = false;
                    self.pending_message_id = None;
                    self.pending_soft_interrupt_ids.clear();
                    frames.extend(self.finish_text());
                    self.text_attempt.clear();
                    frames.push(ServerFrame::event(ApiEvent::TurnDone {
                        session_id: session(self),
                        pending_soft_interrupts: self.pending_soft_interrupts_at_done.take(),
                    }));
                }
                frames
            }
            "soft_interrupt_injected" => {
                // Daemon-originated injections (background task results,
                // system notes) are also turn boundaries an embedder may care
                // about, so forward every injection, not only id-bearing ones.
                let client_ids = event["client_ids"]
                    .as_array()
                    .map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                // Injection precedes the response that sees it: whatever text
                // was open belongs to the previous response.
                let mut frames = self.finish_text();
                self.observed_turn_active = true;
                self.activity_version += 1;
                frames.push(ServerFrame::event(ApiEvent::SoftInterruptInjected {
                    session_id: session(self),
                    client_ids,
                    point: event["point"].as_str().unwrap_or("").to_string(),
                    tools_skipped: event["tools_skipped"].as_u64().map(|n| n as u32),
                    display_role: event["display_role"].as_str().map(str::to_string),
                }));
                frames
            }
            "soft_interrupt_queue" => {
                let pending = event["pending"].as_u64().unwrap_or(0) as u32;
                self.pending_soft_interrupts_at_done = (pending > 0).then_some(pending);
                vec![]
            }
            "soft_interrupts_cancelled" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let Some(api_id) = self.take_simple(id, SimpleKind::SoftInterruptCancel) else {
                    return vec![];
                };
                let strings = |key: &str| -> Vec<String> {
                    event[key]
                        .as_array()
                        .map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect())
                        .unwrap_or_default()
                };
                vec![ServerFrame::reply(
                    api_id,
                    ApiEvent::SoftInterruptsCancelled {
                        cancelled: strings("cancelled"),
                        not_queued: strings("not_queued"),
                    },
                )]
            }
            "background_tool_result" => {
                let id = event["id"].as_u64().unwrap_or(0);
                let Some(api_id) = self.take_simple(id, SimpleKind::BackgroundTool) else {
                    return vec![];
                };
                vec![ServerFrame::reply(
                    api_id,
                    ApiEvent::BackgroundToolResult {
                        moved: event["moved"].as_bool().unwrap_or(false),
                        tool_call_id: event["tool_call_id"].as_str().map(str::to_string),
                        tool_name: event["tool_name"].as_str().map(str::to_string),
                    },
                )]
            }
            // Approval, ask and failover state. These are stream events like
            // `turn_stopped`, not replies: the id they carry is the id the
            // client echoes in the matching response, not a request id.
            "ask_requested" => {
                let Some(ask_id) = event["request_id"].as_str() else {
                    return vec![];
                };
                // A question set this bridge cannot decode is dropped like any
                // other malformed event: the tool's own deadline then resolves
                // it on the recommended option instead of the client showing a
                // card it cannot fill.
                let Ok(questions) = serde_json::from_value(event["questions"].clone()) else {
                    return vec![];
                };
                vec![ServerFrame::event(ApiEvent::AskRequested {
                    session_id: session(self),
                    ask_id: ask_id.to_string(),
                    questions,
                })]
            }
            "approval_pending" => {
                let Some(request_id) = event["request_id"].as_str() else {
                    return vec![];
                };
                vec![ServerFrame::event(ApiEvent::ApprovalPending {
                    session_id: session(self),
                    request_id: request_id.to_string(),
                    action: event["action"].as_str().unwrap_or_default().to_string(),
                    expires_at: event["expires_at"].as_str().map(str::to_string),
                })]
            }
            "approval_resolved" => {
                let Some(request_id) = event["request_id"].as_str() else {
                    return vec![];
                };
                vec![ServerFrame::event(ApiEvent::ApprovalResolved {
                    session_id: session(self),
                    request_id: request_id.to_string(),
                    approved: event["approved"].as_bool().unwrap_or(false),
                    via: event["via"].as_str().unwrap_or_default().to_string(),
                    resolved_by: event["resolved_by"].as_str().unwrap_or_default().to_string(),
                })]
            }
            "failover_switched" => {
                let Some(chain_id) = event["chain_id"].as_str() else {
                    return vec![];
                };
                vec![ServerFrame::event(ApiEvent::FailoverSwitched {
                    session_id: session(self),
                    chain_id: chain_id.to_string(),
                    from: event["from"].as_str().unwrap_or_default().to_string(),
                    to: event["to"].as_str().unwrap_or_default().to_string(),
                    reason: event["reason"].as_str().unwrap_or_default().to_string(),
                    attempt: event["attempt"].as_u64().map(|n| n as u32).unwrap_or(0),
                    max: event["max"].as_u64().map(|n| n as u32).unwrap_or(0),
                })]
            }
            "run_needs_human_resume" => {
                vec![ServerFrame::event(ApiEvent::RunNeedsHumanResume {
                    session_id: event["session_id"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| session(self)),
                    // Absent means the stop names no permission request (an
                    // exhausted failover budget, for example); it must survive
                    // as `None` rather than dropping the event.
                    request_id: event["request_id"].as_str().map(str::to_string),
                    reason: event["reason"].as_str().unwrap_or_default().to_string(),
                    attempts: event["attempts"].as_u64().map(|n| n as u32).unwrap_or(0),
                })]
            }
            "approval_ack" => {
                let Some(request_id) = event["request_id"].as_str() else {
                    return vec![];
                };
                vec![ServerFrame::event(ApiEvent::ApprovalAck {
                    request_id: request_id.to_string(),
                })]
            }
            "failover_ack" => {
                let Some(chain_id) = event["chain_id"].as_str() else {
                    return vec![];
                };
                vec![ServerFrame::event(ApiEvent::FailoverAck {
                    chain_id: chain_id.to_string(),
                    from: event["from"].as_str().unwrap_or_default().to_string(),
                    to: event["to"].as_str().unwrap_or_default().to_string(),
                    attempt: event["attempt"].as_u64().map(|n| n as u32).unwrap_or(0),
                })]
            }
            // Everything else on the legacy stream is not part of the stable
            // API surface yet; drop it.
            _ => vec![],
        };
        for frame in &mut frames {
            if let ApiEvent::Attached { session } | ApiEvent::SessionForked { session } = &mut frame.event {
                horon_agent_harness_api::enrich_sessions_from_local_edit_stats(std::slice::from_mut(session));
                horon_agent_harness_api::enrich_sessions_from_local_swarm_state(std::slice::from_mut(session));
            }
        }
        frames
    }

    /// Read provider/model identity out of any legacy event that carries the
    /// `provider_name`/`provider_model` pair (the catalog reply and the
    /// available-models push both do).
    /// Remember the catalog carried by any legacy event that has one.
    ///
    /// The daemon reports models on attach and on every change; caching here
    /// is what lets `list_models` answer without a round trip.
    fn note_models(&mut self, event: &Value) {
        self.model_catalog_loaded = true;
        self.usage.note_context_window(event);
        if let Some(models) = event["available_models"].as_array() {
            let names: Vec<String> = models
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            self.available_models = names;
        }
        if let Some(model) = event["provider_model"].as_str() {
            self.current_model = Some(model.to_string());
        }
        if let Some(provider) = event["provider_name"].as_str() {
            self.note_provider(provider);
        }
        self.note_credential(event);
        if event.get("reasoning_effort").is_some() {
            self.current_effort = event["reasoning_effort"].as_str().map(str::to_string);
        }
        if let Some(routes) = event["available_model_routes"].as_array() {
            self.available_routes = routes
                .iter()
                .filter_map(|route| {
                    Some(ModelRouteInfo {
                        model: route["model"].as_str()?.to_string(),
                        provider: route["provider"].as_str()?.to_string(),
                        api_method: route["api_method"].as_str()?.to_string(),
                        available: route["available"].as_bool().unwrap_or(false),
                        detail: route["detail"].as_str().unwrap_or_default().to_string(),
                        usage: serde_json::from_value(route["usage"].clone()).unwrap_or(None),
                    })
                })
                .collect();
            for route in &mut self.available_routes {
                if let Some(usage) = self.model_usage_updates.get(&(
                    route.model.clone(),
                    route.provider.clone(),
                    route.api_method.clone(),
                )) {
                    route
                        .usage
                        .get_or_insert_with(Default::default)
                        .merge_observation(usage);
                }
            }
        }
    }

    fn note_provider(&mut self, provider: &str) {
        if self.current_provider.as_deref() != Some(provider) {
            // Effort is provider-specific. ModelChanged and auth pushes can
            // omit it, so never carry the previous provider's setting across.
            self.current_effort = None;
        }
        self.current_provider = Some(provider.to_string());
    }

    /// Remember the credential the daemon resolved, when the event says.
    /// Older daemons omit the key, which keeps the last known value.
    fn note_credential(&mut self, event: &Value) {
        if let Some(credential) = event.get("resolved_credential") {
            self.current_credential = credential.as_str().map(str::to_string);
        }
    }

    fn runtime_info(&self) -> ApiEvent {
        ApiEvent::RuntimeInfo {
            session_id: self.session_id.clone().unwrap_or_default(),
            provider: self.current_provider.clone(),
            model: self.current_model.clone(),
            reasoning_effort: self.current_effort.clone(),
            auth_method: self.current_credential.clone(),
            routes: self.available_routes.clone(),
        }
    }

    fn model_info(&self, session_id: String, event: &Value) -> ApiEvent {
        ApiEvent::ModelInfo {
            session_id,
            provider: event["provider_name"].as_str().map(str::to_string),
            model: event["provider_model"].as_str().map(str::to_string),
            reasoning_effort: event["reasoning_effort"]
                .as_str()
                .map(str::to_string)
                .or_else(|| self.current_effort.clone()),
            auth_method: self.current_credential.clone(),
        }
    }

    /// Path of a session's persisted record, or `None` if the id is not a
    /// plain session id.
    ///
    /// One funnel for three reasons. It honours `HORON_HOME`, without which a
    /// launched instance reads the *user's* sessions: `peek_session` served
    /// the real transcripts of the horon-agent the user runs interactively, which
    /// defeats the isolation an embedded instance exists to provide. It
    /// rejects ids that are not bare session ids, since the id comes straight
    /// off the wire and is interpolated into a path, so `../../.ssh/id_rsa`
    /// would otherwise be read and returned. And it keeps the three callers
    /// from drifting apart, which is how the first two problems survived
    /// being fixed anywhere else.
    fn session_record_path(session_id: &str) -> Option<std::path::PathBuf> {
        // Session ids are `session_<name>_<millis>_<hex>`; anything with a
        // separator or a parent reference is not one.
        if session_id.is_empty()
            || session_id.len() > 128
            || !session_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return None;
        }
        let home = match std::env::var_os("HORON_HOME") {
            Some(home) => std::path::PathBuf::from(home),
            None => std::path::Path::new(&std::env::var_os("HOME")?).join(".horon"),
        };
        Some(home.join("sessions").join(format!("{session_id}.json")))
    }

    /// Metadata of a session, read from its persisted record.
    ///
    /// The legacy `history` event lists session *ids* only, but the strip
    /// groups by directory, so the bridge resolves them from the same files
    /// the daemon persists. Best-effort by design: an unreadable or missing
    /// record simply leaves the session ungrouped rather than failing the
    /// list. Session records can be hundreds of megabytes, while the fields we
    /// need are written in the small header and trailer. Reading bounded windows
    /// avoids parsing every transcript message just to populate the sidebar.
    fn resolve_session_metadata(session_id: &str) -> Option<PersistedSessionMetadata> {
        if let Some(entry) = Self::recent_session_index_entries()
            .ok()?
            .iter()
            .find(|entry| entry.session_id == session_id)
        {
            return Some(PersistedSessionMetadata::from(entry));
        }
        Self::resolve_legacy_session_metadata(session_id)
    }

    fn resolve_legacy_session_metadata(session_id: &str) -> Option<PersistedSessionMetadata> {
        let path = Self::session_record_path(session_id)?;
        // A missing or malformed record is expected (a session may predate the
        // fields, or be mid-write), and the only cost is missing metadata, so
        // this degrades rather than failing the whole session list or attach.
        const WINDOW: usize = 64 * 1024;
        let mut file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        let head_len = usize::try_from(len.min(WINDOW as u64)).ok()?;
        let mut head = vec![0; head_len];
        file.read_exact(&mut head).ok()?;
        let tail = if len > WINDOW as u64 {
            file.seek(SeekFrom::End(-(WINDOW as i64))).ok()?;
            let mut tail = vec![0; WINDOW];
            file.read_exact(&mut tail).ok()?;
            tail
        } else {
            Vec::new()
        };
        Some(PersistedSessionMetadata {
            title: Self::metadata_string(&head, "title", false),
            custom_title: Self::metadata_string(&tail, "custom_title", true)
                .or_else(|| Self::metadata_string(&head, "custom_title", true)),
            todo_title: None,
            saved: Self::metadata_bool(&tail, "saved", true)
                .or_else(|| Self::metadata_bool(&head, "saved", false))
                .unwrap_or(false),
            save_label: Self::metadata_string(&tail, "save_label", true)
                .or_else(|| Self::metadata_string(&head, "save_label", true)),
            updated_at_ms: None,
            last_active_at_ms: None,
            working_dir: Self::metadata_string(&tail, "working_dir", true)
                .or_else(|| Self::metadata_string(&head, "working_dir", true)),
        })
    }

    fn metadata_string(bytes: &[u8], field: &str, last: bool) -> Option<String> {
        let needle = format!("\"{field}\":");
        let mut starts = bytes
            .windows(needle.len())
            .enumerate()
            .filter_map(|(at, window)| (window == needle.as_bytes()).then_some(at + needle.len()));
        let start = if last { starts.next_back()? } else { starts.next()? };
        Option::<String>::deserialize(&mut serde_json::Deserializer::from_slice(&bytes[start..]))
            .ok()
            .flatten()
    }

    fn metadata_bool(bytes: &[u8], field: &str, last: bool) -> Option<bool> {
        let needle = format!("\"{field}\":");
        let mut starts = bytes
            .windows(needle.len())
            .enumerate()
            .filter_map(|(at, window)| (window == needle.as_bytes()).then_some(at + needle.len()));
        let start = if last { starts.next_back()? } else { starts.next()? };
        bool::deserialize(&mut serde_json::Deserializer::from_slice(&bytes[start..])).ok()
    }

    fn resolve_working_dir(session_id: &str) -> Option<String> {
        Self::resolve_session_metadata(session_id)?.working_dir
    }

    /// Size of a session's stored record, in bytes.
    ///
    /// A stat rather than a parse: this runs for every session on every list
    /// request, and deserializing a dozen multi-megabyte transcripts to count
    /// their characters would make the cheap call expensive. The file is
    /// almost entirely message content, so its size tracks the conversation
    /// closely enough for a client to size or sort by.
    fn transcript_bytes(session_id: &str) -> Option<u64> {
        let path = Self::session_original_path(session_id)?;
        std::fs::metadata(path).ok().map(|meta| meta.len())
    }

    fn session_modified_ms(session_id: &str) -> Option<u64> {
        Self::session_original_path(session_id)?
            .metadata()
            .ok()?
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis() as u64)
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// HoronAgent home for a todo-state request, rejecting ids unsafe for a path.
    fn todo_home(session_id: &str) -> Result<std::path::PathBuf, (ErrorCode, String)> {
        if !horon_agent_todo_policy::store::is_safe_session_id(session_id) {
            return Err((ErrorCode::InvalidRequest, "invalid session_id".to_string()));
        }
        Self::horon_agent_home().ok_or_else(|| (ErrorCode::Internal, "no horon-agent home".to_string()))
    }

    pub(crate) fn horon_agent_home() -> Option<std::path::PathBuf> {
        std::env::var_os("HORON_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".horon")))
    }

    /// Name sessions recorded before first-prompt titles existed. Each
    /// session is derived once and cached in the shared index (an empty
    /// value marks "no usable prompt"). The work is time-boxed per request,
    /// newest first, so a large backlog is spread across list calls.
    fn backfill_prompt_titles(
        ids: &BTreeSet<String>,
        indexed: &BTreeMap<String, RecentSessionIndexEntry>,
        metadata: &mut BTreeMap<String, PersistedSessionMetadata>,
    ) {
        const BUDGET: std::time::Duration = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let mut pending: Vec<_> = ids
            .iter()
            .filter(|id| !indexed.contains_key(*id))
            .filter(|id| metadata.get(*id).is_some_and(|value| value.title.is_none()))
            .collect();
        pending.sort_by_key(|id| std::cmp::Reverse(Self::session_modified_ms(id)));
        for id in pending {
            if started.elapsed() > BUDGET {
                break;
            }
            if let Some(title) = Self::first_prompt_title(id)
                && let Some(value) = metadata.get_mut(id)
            {
                value.title = Some(title);
            }
        }
    }

    /// Title derived from a session's first visible user prompt, matching the
    /// runtime's own fallback. Streams the record so only that prompt's text
    /// is materialized, then falls back to the append journal.
    fn first_prompt_title(session_id: &str) -> Option<String> {
        use serde::de::{IgnoredAny, SeqAccess, Visitor};

        #[derive(Deserialize)]
        struct LiteBlock {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            text: Option<String>,
        }
        #[derive(Deserialize)]
        struct LiteMessage {
            role: String,
            #[serde(default)]
            display_role: Option<IgnoredAny>,
            #[serde(default)]
            content: Vec<LiteBlock>,
        }
        impl LiteMessage {
            fn title(&self) -> Option<String> {
                if self.role != "user" || self.display_role.is_some() {
                    return None;
                }
                let mut texts = self
                    .content
                    .iter()
                    .filter(|block| block.kind == "text")
                    .filter_map(|block| block.text.as_deref());
                let first = texts.next()?;
                if first.trim_start().starts_with("<system-reminder>") {
                    return None;
                }
                std::iter::once(first)
                    .chain(texts)
                    .find_map(horon_agent_session_types::prompt_title)
            }
        }
        #[derive(Default)]
        struct FirstPrompt(Option<String>);
        impl<'de> Deserialize<'de> for FirstPrompt {
            fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
                struct Scan;
                impl<'de> Visitor<'de> for Scan {
                    type Value = FirstPrompt;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a message list")
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FirstPrompt, A::Error> {
                        let mut found = None;
                        while found.is_none() {
                            match seq.next_element::<LiteMessage>()? {
                                Some(message) => found = message.title(),
                                None => return Ok(FirstPrompt(None)),
                            }
                        }
                        while seq.next_element::<IgnoredAny>()?.is_some() {}
                        Ok(FirstPrompt(found))
                    }
                }
                de.deserialize_seq(Scan)
            }
        }
        #[derive(Deserialize)]
        struct Record {
            #[serde(default)]
            messages: FirstPrompt,
        }
        #[derive(Deserialize)]
        struct JournalEntry {
            #[serde(default)]
            append_messages: FirstPrompt,
        }

        let path = Self::session_record_path(session_id)?;
        let file = std::fs::File::open(&path).ok()?;
        let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::new(file));
        if let Some(title) = Record::deserialize(&mut de).ok().and_then(|r| r.messages.0) {
            return Some(title);
        }
        let journal = path.with_file_name(format!("{session_id}.journal.jsonl"));
        let reader = std::io::BufReader::new(std::fs::File::open(journal).ok()?);
        std::io::BufRead::lines(reader)
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<JournalEntry>(&line).ok())
            .find_map(|entry| entry.append_messages.0)
    }

    fn recent_session_index_entries() -> Result<Vec<RecentSessionIndexEntry>, store::StoreError> {
        let Some(home) = Self::horon_agent_home() else {
            return Ok(Vec::new());
        };
        let home = if home.exists() { home.canonicalize()? } else { home };
        let paths = store::SessionPaths::new(home.join("sessions"));
        store::legacy::require_index_migration(&paths)?;
        if !paths.root().exists() {
            return Ok(Vec::new());
        }
        // Legacy-only directories predate the private event layout. Keep their
        // bounded compatibility reader; only event directories enter the catalog.
        let has_events = std::fs::read_dir(paths.root())?
            .any(|entry| entry.is_ok_and(|entry| entry.path().join("events.jsonl").exists()));
        if !has_events {
            return Ok(Vec::new());
        }
        let rows = store::catalog::list(&paths, store::ReadProbe::default())?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let id = row.session_id.as_str();
                let live_todo = ["", "-plan"]
                    .iter()
                    .any(|suffix| home.join(format!("todos/{id}{suffix}.json")).exists());
                let todo_title = if live_todo {
                    let snapshot = horon_agent_todo_policy::store::load_snapshot(&home, id);
                    horon_agent_todo_policy::derive_session_title(&snapshot.todos, &snapshot.plan)
                } else {
                    row.todo_title
                };
                RecentSessionIndexEntry {
                    session_id: id.to_owned(),
                    working_dir: row.working_dir,
                    generated_title: row.generated_title,
                    custom_title: row.custom_title,
                    todo_title,
                    saved: row.saved,
                    save_label: row.save_label,
                    updated_at_ms: row.metadata_updated_at.timestamp_millis(),
                    last_active_at_ms: row.last_active_at_ms,
                    archived: row.archived,
                }
            })
            .collect())
    }

    fn stored_session_ids(indexed: &BTreeMap<String, RecentSessionIndexEntry>) -> Vec<String> {
        let mut ids: BTreeSet<String> = indexed.keys().cloned().collect();
        if let Some(home) = Self::horon_agent_home()
            && let Ok(entries) = std::fs::read_dir(home.join("sessions"))
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_file())
                    && path.extension().is_some_and(|extension| extension == "json")
                    && let Some(id) = path.file_stem().and_then(|name| name.to_str())
                    && Self::session_record_path(id).is_some()
                {
                    ids.insert(id.to_owned());
                }
            }
        }
        ids.into_iter().collect()
    }

    /// Whether this session's original is the canonical event log.
    fn canonical_layout(session_id: &str) -> bool {
        let Some(home) = Self::horon_agent_home() else {
            return false;
        };
        let home = if home.exists() {
            home.canonicalize().unwrap_or(home)
        } else {
            home
        };
        let paths = store::SessionPaths::new(home.join("sessions"));
        let Ok(id) = store::SessionId::parse(session_id) else {
            return false;
        };
        matches!(
            store::reader::layout(&paths, &id),
            Ok(Some(store::reader::SourceLayout::Canonical))
        )
    }

    /// Record an explicit archive or restore on the canonical event log.
    ///
    /// A session still in the preserved legacy layout has no event log, so it
    /// keeps the host-local hide that the SDK always wrote; nothing is deleted
    /// in either case. A live writer, diverged heads or a failed append become
    /// the API error instead of a silent local-only success.
    fn record_archive_event(session_id: &str, archived: bool, reason: &str, api_id: u64) -> Result<(), Vec<Outbound>> {
        let Some(home) = Self::horon_agent_home() else {
            return Ok(());
        };
        let home = if home.exists() {
            home.canonicalize().unwrap_or(home)
        } else {
            home
        };
        let paths = store::SessionPaths::new(home.join("sessions"));
        let Ok(id) = store::SessionId::parse(session_id) else {
            return Err(Self::error_reply(
                api_id,
                ErrorCode::InvalidRequest,
                "invalid session id",
            ));
        };
        match store::reader::layout(&paths, &id) {
            Ok(Some(store::reader::SourceLayout::Canonical)) => {}
            Ok(_) => return Ok(()),
            Err(error) => {
                return Err(Self::error_reply(api_id, ErrorCode::Internal, &error.message));
            }
        }
        match store::archive::record(&paths, &id, archived, reason) {
            Ok(outcome) => {
                if let store::archive::ArchiveOutcome::Recorded {
                    cache_warning: Some(warning),
                    ..
                } = outcome
                {
                    eprintln!("harness API bridge: archive recorded, cache needs repair: {warning}");
                }
                Ok(())
            }
            Err(error) => {
                if error.code == store::StoreErrorCode::Busy {
                    // Another process owns this session's writer, so no durable record can
                    // be written here. Reporting success would hide the session on this host
                    // while the canonical log still says otherwise, and a later restore would
                    // then be a no-op against a record that was never made. The API error is
                    // the honest answer; the caller retries once the owner is idle.
                    return Err(Self::error_reply(
                        api_id,
                        ErrorCode::Internal,
                        "session is busy in another process; archive was not recorded",
                    ));
                }
                let code = match error.code {
                    store::StoreErrorCode::UnknownSession => ErrorCode::UnknownSession,
                    store::StoreErrorCode::ConflictHeads => ErrorCode::InvalidRequest,
                    _ => ErrorCode::Internal,
                };
                Err(Self::error_reply(api_id, code, &error.message))
            }
        }
    }

    fn session_original_path(session_id: &str) -> Option<std::path::PathBuf> {
        let home = Self::horon_agent_home()?;
        let home = if home.exists() { home.canonicalize().ok()? } else { home };
        let id = store::SessionId::parse(session_id).ok()?;
        let paths = store::SessionPaths::new(home.join("sessions"));
        let candidate = paths.root().join(id.as_str()).join("events.jsonl");
        if candidate.symlink_metadata().is_ok() {
            let event = paths.events_log(&id).ok()?;
            event.symlink_metadata().ok()?.is_file().then_some(event)
        } else {
            Self::session_record_path(session_id)
        }
    }

    fn archive_state_path() -> Option<std::path::PathBuf> {
        Some(Self::horon_agent_home()?.join("sdk-archive.json"))
    }

    fn state_write_guard() -> MutexGuard<'static, ()> {
        static STATE_WRITES: OnceLock<Mutex<()>> = OnceLock::new();
        STATE_WRITES
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn load_archive_state() -> ArchiveState {
        let Some(path) = Self::archive_state_path() else {
            return ArchiveState::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return ArchiveState::default();
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return ArchiveState::default();
        };
        let sessions = value["sessions"]
            .as_object()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter_map(|(id, at)| at.as_u64().map(|at| (id.clone(), at)))
                    .collect()
            })
            .unwrap_or_default();
        ArchiveState {
            sessions,
            restored: value["restored"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect(),
            archive_after_days: value["archive_after_days"].as_u64().map(|days| days as u32),
        }
    }

    fn save_archive_state(state: &ArchiveState) -> Result<(), String> {
        let path = Self::archive_state_path().ok_or_else(|| "could not resolve horon-agent home".to_string())?;
        let parent = path.parent().ok_or_else(|| "invalid archive path".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let value = json!({
            "sessions": state.sessions,
            "archive_after_days": state.archive_after_days,
            "restored": state.restored,
        });
        Self::atomic_owner_write(
            &path,
            serde_json::to_string_pretty(&value)
                .map_err(|error| error.to_string())?
                .as_bytes(),
        )
    }

    fn app_config_dir() -> Option<std::path::PathBuf> {
        if let Some(home) = std::env::var_os("HORON_HOME") {
            return Some(std::path::Path::new(&home).join("config/horon-agent"));
        }
        horon_agent_platform::paths::user_config_dir("horon-agent")
    }

    fn session_root(session_id: &str) -> Result<std::path::PathBuf, (ErrorCode, String)> {
        let root = Self::resolve_working_dir(session_id)
            .ok_or((ErrorCode::UnknownSession, "session has no working directory".into()))?;
        std::fs::canonicalize(root).map_err(|error| {
            (
                ErrorCode::InvalidRequest,
                format!("working directory is unavailable: {error}"),
            )
        })
    }

    fn safe_session_path(session_id: &str, relative: &str) -> Result<std::path::PathBuf, (ErrorCode, String)> {
        if relative.is_empty() || std::path::Path::new(relative).is_absolute() {
            return Err((
                ErrorCode::InvalidRequest,
                "path must be a non-empty relative path".into(),
            ));
        }
        if std::path::Path::new(relative)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err((ErrorCode::InvalidRequest, "path may not contain `..`".into()));
        }
        let root = Self::session_root(session_id)?;
        let candidate = root.join(relative);
        if candidate.exists() {
            let canonical =
                std::fs::canonicalize(&candidate).map_err(|error| (ErrorCode::InvalidRequest, error.to_string()))?;
            if !canonical.starts_with(&root) {
                return Err((
                    ErrorCode::InvalidRequest,
                    "path escapes the session working directory".into(),
                ));
            }
            return Ok(canonical);
        }
        // A missing final path can still be safely statted when its nearest
        // existing ancestor remains under the canonical root.
        let mut ancestor = candidate.as_path();
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or((ErrorCode::InvalidRequest, "invalid path".into()))?;
        }
        let canonical =
            std::fs::canonicalize(ancestor).map_err(|error| (ErrorCode::InvalidRequest, error.to_string()))?;
        if !canonical.starts_with(&root) {
            return Err((
                ErrorCode::InvalidRequest,
                "path escapes the session working directory".into(),
            ));
        }
        Ok(candidate)
    }

    fn read_session_file(
        session_id: &str,
        relative: &str,
        max: u64,
    ) -> Result<(String, u64, bool), (ErrorCode, String)> {
        let path = Self::safe_session_path(session_id, relative)?;
        let metadata = path
            .metadata()
            .map_err(|error| (ErrorCode::InvalidRequest, error.to_string()))?;
        if !metadata.is_file() {
            return Err((ErrorCode::InvalidRequest, "path is not a regular file".into()));
        }
        let size = metadata.len();
        let file = std::fs::File::open(path).map_err(|error| (ErrorCode::InvalidRequest, error.to_string()))?;
        let mut bytes = Vec::new();
        file.take(max.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| (ErrorCode::Internal, error.to_string()))?;
        let truncated = bytes.len() as u64 > max;
        bytes.truncate(max as usize);
        let content = match std::str::from_utf8(&bytes) {
            Ok(content) => content.to_string(),
            Err(error) if truncated && error.error_len().is_none() => {
                String::from_utf8(bytes[..error.valid_up_to()].to_vec()).expect("valid UTF-8 prefix")
            }
            Err(_) => {
                return Err((ErrorCode::InvalidRequest, "file is not UTF-8 text".into()));
            }
        };
        Ok((content, size, truncated))
    }

    fn walk_files(root: &std::path::Path, limit: usize) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        let mut scanned_entries = 0usize;
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                scanned_entries += 1;
                if scanned_entries > MAX_WALK_ENTRIES {
                    return files;
                }
                let name = entry.file_name();
                if matches!(name.to_str(), Some(".git" | "node_modules" | "target" | ".next")) {
                    continue;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    pending.push(entry.path());
                }
                if kind.is_file() {
                    files.push(entry.path());
                    if files.len() >= limit {
                        return files;
                    }
                }
            }
        }
        files
    }

    fn find_session_files(session_id: &str, query: &str, limit: usize) -> Result<Vec<String>, (ErrorCode, String)> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let root = Self::session_root(session_id)?;
        let needle = query.to_ascii_lowercase();
        // Scan a bounded project-wide set before applying the name filter.
        // Stopping at `limit * 10` made a query incorrectly return no results
        // whenever the matching directory happened to sort later in a large
        // repository.
        Ok(Self::walk_files(&root, MAX_WALK_FILES)
            .into_iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
                relative.to_ascii_lowercase().contains(&needle).then_some(relative)
            })
            .take(limit)
            .collect())
    }

    fn search_session_text(
        session_id: &str,
        query: &str,
        under: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TextMatch>, (ErrorCode, String)> {
        if query.is_empty() {
            return Err((ErrorCode::InvalidRequest, "query must not be empty".into()));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let root = Self::session_root(session_id)?;
        let search_root = match under {
            Some(relative) => Self::safe_session_path(session_id, relative)?,
            None => root.clone(),
        };
        let files = if search_root.is_file() {
            vec![search_root]
        } else {
            Self::walk_files(&search_root, MAX_WALK_FILES)
        };
        let mut matches = Vec::new();
        let mut remaining_bytes = MAX_SEARCH_BYTES;
        for path in files {
            let Some(size) = path.metadata().ok().map(|meta| meta.len()) else {
                continue;
            };
            if size > DEFAULT_FILE_BYTES || size > remaining_bytes {
                continue;
            }
            remaining_bytes -= size;
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (line_index, line) in text.lines().enumerate() {
                for (byte_column, _) in line.match_indices(query) {
                    matches.push(TextMatch {
                        path: path
                            .strip_prefix(&root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .replace('\\', "/"),
                        line: line_index as u32 + 1,
                        column: line[..byte_column].chars().count() as u32 + 1,
                        preview: line.chars().take(500).collect(),
                    });
                    if matches.len() >= limit {
                        return Ok(matches);
                    }
                }
            }
        }
        Ok(matches)
    }

    fn session_file_status(session_id: &str, relative: &str) -> SessionFileStatusResult {
        let path = Self::safe_session_path(session_id, relative)?;
        let Ok(meta) = path.metadata() else {
            return Ok((false, "missing".into(), None, None));
        };
        let kind = if meta.is_file() {
            "file"
        } else if meta.is_dir() {
            "directory"
        } else {
            "other"
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        Ok((true, kind.into(), meta.is_file().then_some(meta.len()), modified))
    }

    fn error_reply(api_id: u64, code: ErrorCode, message: &str) -> Vec<Outbound> {
        vec![Outbound::Reply(ServerFrame::reply(
            api_id,
            ApiEvent::Error {
                code,
                message: message.to_string(),
                details: None,
            },
        ))]
    }

    /// Store errors keep their typed family and cause instead of collapsing
    /// into one internal string that an old client cannot branch on.
    fn store_error_reply(api_id: u64, error: &store::StoreError) -> Vec<Outbound> {
        let code = match error.category {
            store::StoreErrorCategory::InvalidRequest => ErrorCode::InvalidRequest,
            store::StoreErrorCategory::UnknownSession => ErrorCode::UnknownSession,
            store::StoreErrorCategory::Internal => ErrorCode::Internal,
        };
        vec![Outbound::Reply(ServerFrame::reply(
            api_id,
            ApiEvent::Error {
                code,
                message: format!("{} ({})", error.message, error.code_str()),
                details: horon_agent_harness_api::StoreErrorDetail::from_wire_detail(&error.wire_detail()),
            },
        ))]
    }

    /// The last `limit` messages of a session, read from its stored record.
    ///
    /// Content blocks are flattened to their text, which is what a preview
    /// wants: a reader glancing at another session needs the words, not the
    /// tool-call structure around them.
    fn stored_tail(session_id: &str, limit: usize) -> Vec<HistoryMessage> {
        let Some(path) = Self::session_record_path(session_id) else {
            return vec![];
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return vec![];
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return vec![];
        };
        let Some(messages) = value["messages"].as_array() else {
            return vec![];
        };
        messages
            .iter()
            .rev()
            .filter_map(|message| {
                let role = message["role"].as_str()?;
                // Only the conversation: a preview of tool traffic would be
                // noise where the point is to recognise which conversation
                // this is.
                if role != "user" && role != "assistant" {
                    return None;
                }
                let content = flatten_content(&message["content"]);
                (!content.trim().is_empty()).then(|| HistoryMessage {
                    response_stats: None,
                    role: role.to_string(),
                    content,
                })
            })
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// Record the session set the daemon reported, plus any working
    /// directory it mentioned. Kept separate so both the attach probe and an
    /// explicit history request feed the same list.
    fn note_sessions(&mut self, event: &Value) {
        if let Some(all) = event["all_sessions"].as_array() {
            let listed: Vec<String> = all.iter().filter_map(|v| v.as_str()).map(str::to_string).collect();
            if !listed.is_empty() {
                self.known_sessions = listed;
            }
        }
        if let Some(dir) = event["working_dir"].as_str()
            && let Some(session_id) = event["session_id"].as_str()
        {
            self.session_dirs.insert(session_id.to_string(), dir.to_string());
        }
    }

    /// The non-empty `session_id` a request names, or `None` when it carries none.
    fn request_session(request: &Value, _api_id: u64) -> Option<String> {
        request["session_id"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    fn take_simple(&mut self, legacy_id: u64, kind: SimpleKind) -> Option<u64> {
        let index = self
            .pending_simple
            .iter()
            .position(|(id, _, k)| *id == legacy_id && *k == kind)?;
        Some(self.pending_simple.remove(index).1)
    }
}

#[cfg(test)]
#[path = "translate_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "translate/usage_report_tests.rs"]
mod usage_report_tests;

#[cfg(test)]
#[path = "translate_tests/credential_profile_tests.rs"]
mod credential_profile_tests;

#[cfg(test)]
#[path = "translate_handoff_tests.rs"]
mod translate_handoff_tests;

#[cfg(test)]
#[path = "user_response_capability_tests.rs"]
mod user_response_capability_tests;

#[cfg(test)]
mod bridge_wiring_tests;

#[cfg(test)]
#[path = "permission_ask_tests.rs"]
mod permission_ask_tests;

/// Translation coverage for the approval and failover events. Kept next to the
/// arms it exercises; the shared bridge fixtures live in translate_tests.rs.
#[cfg(test)]
mod approval_failover_translate_tests {
    use super::*;

    fn state_with_session() -> BridgeState {
        BridgeState {
            session_id: Some("s1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn approval_and_failover_events_translate_to_api_events() {
        let mut state = state_with_session();

        let frames = state.legacy_event_to_api(&json!({
            "type": "approval_pending",
            "request_id": "req-1",
            "action": "bash",
            "expires_at": "2026-10-17T12:00:00Z"
        }));
        assert_eq!(
            frames[0].event,
            ApiEvent::ApprovalPending {
                session_id: "s1".into(),
                request_id: "req-1".into(),
                action: "bash".into(),
                expires_at: Some("2026-10-17T12:00:00Z".into()),
            }
        );

        let frames = state.legacy_event_to_api(&json!({
            "type": "approval_resolved",
            "request_id": "req-1",
            "approved": true,
            "via": "file",
            "resolved_by": "runner"
        }));
        assert_eq!(
            frames[0].event,
            ApiEvent::ApprovalResolved {
                session_id: "s1".into(),
                request_id: "req-1".into(),
                approved: true,
                via: "file".into(),
                resolved_by: "runner".into(),
            }
        );

        let frames = state.legacy_event_to_api(&json!({
            "type": "failover_switched",
            "chain_id": "chain-1",
            "from": "openai",
            "to": "anthropic",
            "reason": "rate_limit",
            "attempt": 2,
            "max": 3
        }));
        assert_eq!(
            frames[0].event,
            ApiEvent::FailoverSwitched {
                session_id: "s1".into(),
                chain_id: "chain-1".into(),
                from: "openai".into(),
                to: "anthropic".into(),
                reason: "rate_limit".into(),
                attempt: 2,
                max: 3,
            }
        );

        let frames = state.legacy_event_to_api(&json!({
            "type": "run_needs_human_resume",
            "request_id": "req-1",
            "session_id": "s1",
            "reason": "unverifiable",
            "attempts": 2
        }));
        assert_eq!(
            frames[0].event,
            ApiEvent::RunNeedsHumanResume {
                session_id: "s1".into(),
                request_id: Some("req-1".into()),
                reason: "unverifiable".into(),
                attempts: 2,
            }
        );

        let frames = state.legacy_event_to_api(&json!({
            "type": "run_needs_human_resume",
            "session_id": "s1",
            "reason": "failover_budget_exhausted",
            "attempts": 3
        }));
        assert_eq!(
            frames[0].event,
            ApiEvent::RunNeedsHumanResume {
                session_id: "s1".into(),
                request_id: None,
                reason: "failover_budget_exhausted".into(),
                attempts: 3,
            }
        );

        let frames = state.legacy_event_to_api(&json!({"type": "approval_ack", "request_id": "req-1"}));
        assert_eq!(
            frames[0].event,
            ApiEvent::ApprovalAck {
                request_id: "req-1".into()
            }
        );

        let frames = state.legacy_event_to_api(&json!({
            "type": "failover_ack",
            "chain_id": "chain-1",
            "from": "openai",
            "to": "anthropic",
            "attempt": 1
        }));
        assert_eq!(
            frames[0].event,
            ApiEvent::FailoverAck {
                chain_id: "chain-1".into(),
                from: "openai".into(),
                to: "anthropic".into(),
                attempt: 1,
            }
        );
    }

    #[test]
    fn approval_events_tolerate_missing_optional_fields_and_unknown_ones() {
        let mut state = state_with_session();

        // An older daemon sends no expiry, and a newer one may add fields:
        // neither may break translation or the connection.
        let frames = state.legacy_event_to_api(&json!({
            "type": "approval_pending",
            "request_id": "req-2",
            "action": "write",
            "future_field": {"nested": 1}
        }));
        assert_eq!(
            frames[0].event,
            ApiEvent::ApprovalPending {
                session_id: "s1".into(),
                request_id: "req-2".into(),
                action: "write".into(),
                expires_at: None,
            }
        );

        // A malformed event with no request id is dropped, not turned into an
        // event with an empty identifier.
        let frames = state.legacy_event_to_api(&json!({"type": "approval_pending", "action": "bash"}));
        assert!(frames.is_empty());
    }

    #[test]
    fn an_unknown_legacy_event_kind_is_dropped_without_a_frame() {
        let mut state = state_with_session();
        let frames = state.legacy_event_to_api(&json!({"type": "some_future_event", "payload": 1}));
        assert!(frames.is_empty(), "an unknown kind must be ignored, not fatal");
    }

    /// The inbound direction: a client's ack has to reach the daemon as the
    /// legacy frame that clears the durable copy, and the API reply waits for
    /// the daemon's own ack rather than claiming success up front.
    #[test]
    fn acks_translate_to_legacy_frames_and_wait_for_the_daemon_ack() {
        let mut state = state_with_session();

        let outbound = state.api_request_to_legacy(&json!({
            "v": 1,
            "id": 41,
            "req": "approval_ack",
            "session_id": "s1",
            "request_id": "req-1"
        }));
        let [Outbound::Legacy(legacy)] = &outbound[..] else {
            panic!("an approval ack is forwarded, got {outbound:?}");
        };
        assert_eq!(legacy["type"], "approval_ack");
        assert_eq!(legacy["request_id"], "req-1");
        let legacy_id = legacy["id"].as_u64().expect("legacy id");
        assert!(
            state
                .pending_simple
                .iter()
                .any(|(id, api_id, kind)| *id == legacy_id && *api_id == 41 && *kind == SimpleKind::Ok),
            "the reply waits for the daemon ack"
        );

        let outbound = state.api_request_to_legacy(&json!({
            "v": 1,
            "id": 42,
            "req": "failover_ack",
            "session_id": "s1",
            "chain_id": "chain-1",
            "from": "openai",
            "to": "anthropic",
            "attempt": 2
        }));
        let [Outbound::Legacy(legacy)] = &outbound[..] else {
            panic!("a failover ack is forwarded, got {outbound:?}");
        };
        assert_eq!(legacy["type"], "failover_ack");
        assert_eq!(legacy["chain_id"], "chain-1");
        assert_eq!(legacy["from"], "openai");
        assert_eq!(legacy["to"], "anthropic");
        assert_eq!(legacy["attempt"], 2);
        assert_eq!(
            legacy.as_object().map(|fields| fields.len()),
            Some(6),
            "the frame carries exactly the type, id and the four switch-key fields"
        );

        // An ack without its key would report success while clearing nothing.
        let outbound = state.api_request_to_legacy(&json!({
            "v": 1,
            "id": 43,
            "req": "approval_ack",
            "session_id": "s1",
            "request_id": ""
        }));
        assert!(
            matches!(
                &outbound[..],
                [Outbound::Reply(ServerFrame {
                    event: ApiEvent::Error { .. },
                    ..
                })]
            ),
            "an empty request id is refused, got {outbound:?}"
        );

        let outbound = state.api_request_to_legacy(&json!({
            "v": 1,
            "id": 44,
            "req": "failover_ack",
            "session_id": "s1",
            "chain_id": "chain-1",
            "from": "openai",
            "to": "anthropic"
        }));
        assert!(
            matches!(
                &outbound[..],
                [Outbound::Reply(ServerFrame {
                    event: ApiEvent::Error { .. },
                    ..
                })]
            ),
            "a missing attempt is refused, got {outbound:?}"
        );
    }
}

#[cfg(test)]
#[path = "replication_tests.rs"]
mod replication_tests;

#[cfg(test)]
#[path = "history_page_perf_tests.rs"]
mod history_page_perf_tests;
