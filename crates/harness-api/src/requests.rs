//! Client-to-server requests: the curated stable surface.

use serde::{Deserialize, Serialize};

/// A session-local tool executed by the client, or an effective tool description.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the input. Must be a JSON object.
    pub parameters: serde_json::Map<String, serde_json::Value>,
}

/// Replacement tool configuration for a live session, not a patch.
/// Reconfigure after daemon restart or loading a persisted session. Custom
/// tools execute on the configuring client's connection.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolConfiguration {
    /// Omitted/null inherits defaults. Empty disables all built-in/MCP tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
    /// Additive custom tools, overriding a built-in/MCP tool with the same name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<SessionToolDefinition>,
}

/// What a `Compact` request should write as the new boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactMethod {
    /// A generated handoff document covering everything before the verbatim
    /// recent-token tail. The reply waits for the boundary commit.
    Handoff,
}

/// Curated request surface. Internally-tagged on `"req"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum ApiRequest {
    /// Version negotiation. Must be the first frame on a connection.
    Hello {
        min_version: u32,
        max_version: u32,
        /// Client name and version, e.g. "external-client/0.1.0".
        client: String,
        /// Opt in only when this client renders user_response tool calls.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        supports_user_response: bool,
        /// Opt in only when this client can render `ask_requested` questions
        /// and answer them with `ask_response`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        supports_ask_requests: bool,
    },

    /// List sessions visible to this client.
    ListSessions {
        /// Include sessions the user archived through this API.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        include_archived: bool,
        /// Return at most this many most-recently modified persisted sessions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },

    /// Reversibly hide a session from the default list. Its transcript remains
    /// on disk and can be restored at any time.
    ArchiveSession { session_id: String },

    /// Put an archived session back in the default list.
    RestoreSession { session_id: String },

    /// Configure automatic archival of inactive sessions. `None` disables it.
    SetRetentionPolicy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive_after_days: Option<u32>,
    },

    /// Create a new session (optionally in a working directory) and attach.
    CreateSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        /// Replace the complete assembled system prompt for this session.
        /// An empty string is an explicit empty override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
    },

    /// Attach to an existing session and subscribe to its event stream.
    AttachSession { session_id: String },

    /// Clone an attached session's transcript into a new, idle session.
    ForkSession { session_id: String },

    /// Create a fresh session containing a runtime-compacted handoff.
    /// The parent remains unchanged. Replies with `SessionForked`.
    TransferSession { session_id: String },

    /// Detach from the currently attached session.
    DetachSession { session_id: String },

    /// Send a user message to the attached session.
    SendMessage {
        session_id: String,
        content: String,
        /// Hidden recovery/context instruction, not a user transcript message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_reminder: Option<String>,
        /// (media_type, base64_data) pairs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<(String, String)>,
        /// Persist the message as context without starting a model turn.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        no_reply: bool,
    },

    /// Replace the attached session's tool configuration.
    ConfigureTools {
        session_id: String,
        tools: ToolConfiguration,
    },

    /// List the effective tools available to the attached session.
    ListTools { session_id: String },

    /// Read the daemon's `[messages]` delivery modes. Answered with
    /// `MessageModes`; the values are configuration, not session state, so the
    /// reply does not depend on the session existing.
    GetMessageModes { session_id: String },

    /// Complete a client-executed custom tool call. Acknowledged with `Ok`.
    ToolResult {
        session_id: String,
        call_id: String,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Cancel the in-flight generation.
    Cancel { session_id: String },

    /// Inject a message at the next safe point without cancelling.
    SoftInterrupt {
        session_id: String,
        content: String,
        /// (media_type, base64_data) pairs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<(String, String)>,
        #[serde(default)]
        urgent: bool,
        /// Opaque caller id. Echoed in `soft_interrupt_injected` and accepted
        /// by `cancel_soft_interrupts { client_ids }`. Requires the
        /// `soft_interrupt_ids` capability; older bridges ignore it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },

    /// Fetch conversation history.
    GetHistory { session_id: String },

    /// Fetch the tail of *any* session's conversation, attached or not.
    ///
    /// `GetHistory` can only answer for the session this connection is
    /// attached to, because it is routed through the daemon's attachment.
    /// A client showing several sessions at once (a switcher, an overview, a
    /// dashboard) needs a glance at the others without attaching to each in
    /// turn, which would disturb the very sessions it is trying to preview.
    /// Served from the stored record and capped to the last `limit` messages.
    PeekSession {
        session_id: String,
        /// Messages to return from the end. Defaults to a small tail.
        #[serde(default)]
        limit: Option<u32>,
    },

    /// Clear conversation history.
    Clear { session_id: String },

    /// Rewind history to the given 1-based message index.
    ///
    /// `summarize` asks for a report on the branch the rewind discards; the
    /// daemon also needs `[compaction] rewind_summary` on. Omit it for the
    /// pre-existing behavior.
    Rewind {
        session_id: String,
        message_index: usize,
        summarize: Option<bool>,
    },

    /// Reply to a `PermissionRequest` event.
    PermissionResponse {
        session_id: String,
        request_id: String,
        decision: PermissionDecision,
    },

    /// Acknowledge an `ApprovalResolved` event so the daemon can drop its
    /// durable copy. Until it arrives the event is re-delivered on every
    /// attach and restart. A record that no longer exists is not an error.
    ApprovalAck { session_id: String, request_id: String },

    /// Export the conversation as a Markdown transcript.
    ///
    /// Returns the full markdown text for the given session. An unknown
    /// session returns the existing unknown-session error. The existing
    /// redaction mechanism redacts secrets; tool outputs longer than 4 KiB
    /// are truncated with a marker.
    ExportSessionMarkdown { session_id: String },

    /// Answer an `AskRequested` event.
    ///
    /// `ask_id` echoes the event. A reply for an id that is no longer pending
    /// (answered, timed out, or whose client disconnected) is refused, so a
    /// late answer can never resolve a newer question or change a turn that
    /// already continued.
    ///
    /// `answer_id` is an optional caller-chosen identity for this answer.
    /// Repeating the same `question_id`+`answer_id` with the same digest is a
    /// replay of one answer, not a second resolution, which is what makes a
    /// retry after a lost reply safe. A different `answer_id` for an already
    /// answered question is a conflict. Requires `ask_receipts_v1`; a client
    /// that does not negotiate it omits the field and keeps the legacy
    /// behaviour exactly.
    AskResponse {
        session_id: String,
        ask_id: String,
        answers: Vec<AskSelection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer_id: Option<String>,
    },

    /// Acknowledge a `FailoverSwitched` event. The full switch key travels so
    /// a late ack for an older chain cannot clear a newer announcement.
    FailoverAck {
        session_id: String,
        chain_id: String,
        from: String,
        to: String,
        attempt: u32,
    },

    /// List the models this session can switch to.
    ///
    /// A client that cannot enumerate models cannot offer a model picker, so
    /// it is stuck on whatever the daemon defaulted to. Served from the
    /// catalog the daemon already reports on attach.
    ListModels { session_id: String },

    /// Provider routes and active runtime identity for the attached session.
    GetRuntimeInfo { session_id: String },

    /// Token usage, context window, per-model aggregates and subscription
    /// limits. Requires `usage_report`.
    ///
    /// `session_id` narrows the report to one session; omitting it reports
    /// every stored session plus the attached one. A session id that names no
    /// session answers `unknown_session` rather than an empty report, because
    /// "this session spent nothing" and "there is no such session" are
    /// different facts and a usage screen must not confuse them.
    UsageReport {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    /// Execute a runtime-backed command without a model turn. Requires
    /// `runtime_commands`. Invalid or unavailable actions return an error.
    RuntimeCommand {
        session_id: String,
        command: crate::RuntimeCommand,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },

    /// Persist an API-key credential in horon-agent's owner-only provider store and
    /// notify the daemon to reload it. OAuth tokens are intentionally excluded.
    SetApiKey { provider: String, api_key: String },

    /// Remove a previously persisted API-key credential.
    ClearApiKey { provider: String },

    /// Reload provider credentials already saved outside the harness (e.g. OAuth).
    /// No tokens or callback input travel in this request.
    NotifyAuthChanged { provider: String },

    /// Drop the daemon's cached quota and quota cooldown for one subscription
    /// login after the client redeemed a banked usage reset out of band.
    /// `provider` is `claude` or `openai`. `account_label: None` is the default
    /// login. This never redeems a reset and carries no credentials.
    InvalidateUsage {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_label: Option<String>,
    },

    /// Read one UTF-8 file under the session working directory.
    ReadFile {
        session_id: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u64>,
    },

    /// Find files by case-insensitive path substring under the session root.
    FindFiles {
        session_id: String,
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },

    /// Search UTF-8 files for a literal text string.
    SearchText {
        session_id: String,
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },

    /// Read safe filesystem metadata for a path under the session root.
    FileStatus { session_id: String, path: String },

    /// Search every stored session's transcript, not just the attached one.
    ///
    /// A switcher that can only filter the session it is attached to cannot
    /// answer "which conversation was that in". Results come from the same
    /// index the agent's own session search uses. A blank query is rejected
    /// with `invalid_request`; nothing else about the string is interpreted as
    /// a path, glob, or SQL fragment.
    SearchSessions {
        query: String,
        /// Maximum hits, defaulting to 20 and capped at 50.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },

    /// List the files a conversation produced: images it carries and HTML or
    /// image files in the session's working directory written since it began.
    /// Metadata only; use `ReadFile` for contents. Requires `artifacts`.
    ListArtifacts { session_id: String },

    /// Read the session's todo state (todos, plan, goals, deferred gate
    /// observations, long-session review clock) for client-side auto-poke and
    /// quality gates. Replies with `TodoState`. Requires `todo_state`.
    GetTodoState { session_id: String },

    /// Apply the persistent effects of a follow-up decision made with
    /// `horon_agent_todo_policy::FollowUpPolicy` (consume the gate observation log,
    /// mark the long-session review delivered). Replies with `Ok`.
    AckTodoFollowUp {
        session_id: String,
        effects: horon_agent_todo_policy::TodoEffects,
    },

    /// Switch the session to a different model.
    ///
    /// `model` is an id from `ListModels`, e.g. `claude-opus-5`. A route
    /// suffix like `claude-opus-4-6[1m]` selects a specific context variant.
    SetModel { session_id: String, model: String },

    /// Set how much the model deliberates before answering.
    ///
    /// The cost/quality dial: `minimal`, `low`, `medium`, `high`, `xhigh`, or
    /// `max`, depending on what the provider supports. Providers that do not
    /// support it answer with an error rather than silently ignoring it.
    SetReasoningEffort { session_id: String, effort: String },

    /// Summarize the transcript so far, freeing context.
    ///
    /// Without this a long-lived client eventually hits the context limit and
    /// has no recourse but to clear the conversation and lose everything.
    ///
    /// `method: "handoff"` writes a generated handoff document as the boundary
    /// instead of a model summary, and the reply waits for that write, so it
    /// reports the outcome. Absent keeps the scheduled summary that answers
    /// before any work starts.
    Compact {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<CompactMethod>,
    },

    /// Set a session's title, or clear it to restore the generated one.
    RenameSession {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },

    /// Bookmark (`saved: true`) or unbookmark a session. A non-empty `label`
    /// also becomes the session's title, announced with `SessionRenamed`.
    SetSessionSaved {
        session_id: String,
        saved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Restore the history that the last `Rewind` removed.
    ///
    /// `Rewind` is destructive, so without an undo a client cannot offer it
    /// safely: a mis-click costs the user their conversation.
    RewindUndo { session_id: String },

    /// Drop soft interrupts that have been queued but not yet delivered.
    ///
    /// The counterpart to `SoftInterrupt`: a client that lets a user queue a
    /// follow-up must also let them take it back before it lands.
    ///
    /// Without `client_ids`, clears the whole queue and replies `Ok`. With
    /// `client_ids`, removes only those and replies `SoftInterruptsCancelled`,
    /// decided atomically against injection.
    CancelSoftInterrupts {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_ids: Option<Vec<String>>,
    },

    /// Move the currently running tool call to the background so the turn can
    /// continue without waiting for it. The TUI's Alt+B. Replies
    /// `BackgroundToolResult` when the bridge advertises
    /// `background_tool_result`, otherwise `Ok` whether or not a tool ran.
    BackgroundTool { session_id: String },

    /// Durable heads of one stored session. Requires `session_event_store_v1`.
    ListSessionHeads { session_id: String },

    /// Select the host-local head to execute. Requires `session_event_store_v1`.
    /// The head must exist and replay; a live owner answers `busy`.
    SelectSessionHead { session_id: String, head_event_id: String },

    /// One page of visible history on a fixed head. Requires
    /// `session_history_cursor_v1`. Replies `SessionHistoryPage`.
    GetHistoryPage {
        session_id: String,
        /// Defaults to the selected head; an explicit head keeps the cursor
        /// stable when the selection changes later.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_event_id: Option<String>,
        /// Continuation from a previous page, passed back verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_event_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },

    /// Export one bounded parent-first page of original events. Requires
    /// `session_replication_v1`. Replies `SessionEventPage`.
    ExportSessionEvents {
        session_id: String,
        #[serde(default = "crate::replication::default_protocol_version")]
        protocol_version: u32,
        /// Fixed by the first page. Empty selects all current heads there.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        heads: Vec<String>,
        /// Events the receiver already has; at most 256.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        known_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },

    /// Import one validated page, keeping any synchronized prefix. Requires
    /// `session_replication_v1`. Replies `SessionImportResult` or a typed error.
    ImportSessionEvents {
        session_id: String,
        #[serde(default = "crate::replication::default_protocol_version")]
        protocol_version: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        heads: Vec<String>,
        events: Vec<crate::replication::CanonicalEventRecord>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blob_refs: Vec<crate::replication::PayloadRef>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        record_refs: Vec<crate::replication::CanonicalRecordRef>,
        /// Continuation from a failed import, passed back verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_cursor: Option<String>,
    },

    /// Read one 1 MiB chunk of an original payload blob. Requires
    /// `session_replication_v1`. Replies `SessionBlobChunk`.
    ExportSessionBlob {
        session_id: String,
        sha256: String,
        total_size: u64,
        offset: u64,
    },

    /// Stage one chunk of an original payload blob. Replies `SessionBlobStatus`.
    ImportSessionBlob {
        session_id: String,
        chunk: crate::replication::SessionBlobChunk,
    },

    /// Resume an interrupted payload transfer. Replies `SessionBlobStatus`.
    GetSessionBlobStatus {
        session_id: String,
        sha256: String,
        total_size: u64,
    },

    /// Read one 1 MiB chunk of a whole canonical record carried by reference.
    /// Replies `SessionBlobChunk` with the record's hash and size.
    ExportSessionRecordBlob {
        session_id: String,
        reference: crate::replication::CanonicalRecordRef,
        offset: u64,
    },

    /// Stage one chunk of a whole canonical record. Replies
    /// `SessionRecordBlobStatus`.
    ImportSessionRecordBlob {
        session_id: String,
        reference: crate::replication::CanonicalRecordRef,
        chunk: crate::replication::SessionBlobChunk,
    },

    /// Resume an interrupted whole-record transfer. Replies
    /// `SessionRecordBlobStatus`.
    GetSessionRecordBlobStatus {
        session_id: String,
        reference: crate::replication::CanonicalRecordRef,
    },

    /// Liveness check.
    Ping,

    /// Run, queue, receipt, and monitor summary read from this session's
    /// durable runtime journal. Requires `durable_runtime_v1`. Replies
    /// `RuntimeState`.
    ///
    /// A summary, never a dump: input content, command arguments, and command
    /// environment variables stay in the journal. A client showing "what is
    /// this session doing" needs counts, causes, and identities, not the text
    /// of what was queued.
    GetRuntimeState { session_id: String },

    /// Apply one control operation (a cancel, an abort, a retry decision) with
    /// a caller-chosen id, so a retry after a lost reply cannot apply it twice.
    /// Requires `durable_runtime_v1`. Replies `ControlReceipt`.
    ApplyControl {
        session_id: String,
        /// Stable identity of this operation, chosen by the caller.
        operation_id: String,
        /// Stable action name, for example `cancel_input`.
        action: String,
        /// Action-specific arguments. Absent when the action takes none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<serde_json::Value>,
    },

    /// Read the terminal state of one control operation. Requires
    /// `durable_runtime_v1`. Replies `ControlReceipt`.
    GetControlReceipt { session_id: String, operation_id: String },

    /// One consistent view of runtime state plus a bounded history tail, with
    /// the marker that separates it from the live stream. Requires
    /// `snapshot_marker_v1`. Replies `Snapshot`, preceded on the same
    /// connection by the matching `SnapshotMarker`.
    GetSnapshot {
        session_id: String,
        /// Messages to return from the end of the transcript.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail: Option<u32>,
        /// Byte budget for the returned tail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u64>,
    },
    /// Forward-compatibility catch-all. Servers reply with an error frame.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    AllowAlways,
    Deny,
}

impl ApiRequest {
    /// The session this request acts on, when it names one.
    ///
    /// The SDK uses it to attach the session id to an error frame, so a
    /// "session not found" failure can say which id was not found without the
    /// caller re-deriving it from the request it just sent.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::ArchiveSession { session_id, .. } => Some(session_id.as_str()),
            Self::RestoreSession { session_id, .. } => Some(session_id.as_str()),
            Self::AttachSession { session_id, .. } => Some(session_id.as_str()),
            Self::ForkSession { session_id, .. } => Some(session_id.as_str()),
            Self::TransferSession { session_id, .. } => Some(session_id.as_str()),
            Self::DetachSession { session_id, .. } => Some(session_id.as_str()),
            Self::SendMessage { session_id, .. } => Some(session_id.as_str()),
            Self::ConfigureTools { session_id, .. } => Some(session_id.as_str()),
            Self::ListTools { session_id, .. } => Some(session_id.as_str()),
            Self::ToolResult { session_id, .. } => Some(session_id.as_str()),
            Self::Cancel { session_id, .. } => Some(session_id.as_str()),
            Self::SoftInterrupt { session_id, .. } => Some(session_id.as_str()),
            Self::GetHistory { session_id, .. } => Some(session_id.as_str()),
            Self::PeekSession { session_id, .. } => Some(session_id.as_str()),
            Self::Clear { session_id, .. } => Some(session_id.as_str()),
            Self::Rewind { session_id, .. } => Some(session_id.as_str()),
            Self::PermissionResponse { session_id, .. } => Some(session_id.as_str()),
            Self::ApprovalAck { session_id, .. } => Some(session_id.as_str()),
            Self::ExportSessionMarkdown { session_id, .. } => Some(session_id.as_str()),
            Self::AskResponse { session_id, .. } => Some(session_id.as_str()),
            Self::FailoverAck { session_id, .. } => Some(session_id.as_str()),
            Self::ListModels { session_id, .. } => Some(session_id.as_str()),
            Self::GetRuntimeInfo { session_id, .. } => Some(session_id.as_str()),
            Self::UsageReport { session_id, .. } => session_id.as_deref(),
            Self::RuntimeCommand { session_id, .. } => Some(session_id.as_str()),
            Self::ReadFile { session_id, .. } => Some(session_id.as_str()),
            Self::FindFiles { session_id, .. } => Some(session_id.as_str()),
            Self::SearchText { session_id, .. } => Some(session_id.as_str()),
            Self::FileStatus { session_id, .. } => Some(session_id.as_str()),
            Self::ListArtifacts { session_id, .. } => Some(session_id.as_str()),
            Self::GetTodoState { session_id, .. } => Some(session_id.as_str()),
            Self::AckTodoFollowUp { session_id, .. } => Some(session_id.as_str()),
            Self::SetModel { session_id, .. } => Some(session_id.as_str()),
            Self::SetReasoningEffort { session_id, .. } => Some(session_id.as_str()),
            Self::Compact { session_id, .. } => Some(session_id.as_str()),
            Self::RenameSession { session_id, .. } => Some(session_id.as_str()),
            Self::SetSessionSaved { session_id, .. } => Some(session_id.as_str()),
            Self::RewindUndo { session_id, .. } => Some(session_id.as_str()),
            Self::CancelSoftInterrupts { session_id, .. } => Some(session_id.as_str()),
            Self::BackgroundTool { session_id, .. } => Some(session_id.as_str()),
            Self::ListSessionHeads { session_id, .. } => Some(session_id.as_str()),
            Self::SelectSessionHead { session_id, .. } => Some(session_id.as_str()),
            Self::GetHistoryPage { session_id, .. } => Some(session_id.as_str()),
            Self::ExportSessionEvents { session_id, .. } => Some(session_id.as_str()),
            Self::ImportSessionEvents { session_id, .. } => Some(session_id.as_str()),
            Self::ExportSessionBlob { session_id, .. } => Some(session_id.as_str()),
            Self::ImportSessionBlob { session_id, .. } => Some(session_id.as_str()),
            Self::GetSessionBlobStatus { session_id, .. } => Some(session_id.as_str()),
            Self::ExportSessionRecordBlob { session_id, .. } => Some(session_id.as_str()),
            Self::ImportSessionRecordBlob { session_id, .. } => Some(session_id.as_str()),
            Self::GetSessionRecordBlobStatus { session_id, .. } => Some(session_id.as_str()),
            Self::GetRuntimeState { session_id, .. } => Some(session_id.as_str()),
            Self::ApplyControl { session_id, .. } => Some(session_id.as_str()),
            Self::GetControlReceipt { session_id, .. } => Some(session_id.as_str()),
            Self::GetSnapshot { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        }
    }

    /// The capability this request requires, or `None` when it is part of the
    /// surface every client has.
    ///
    /// One table, used by the bridge to refuse an unnegotiated request and by
    /// both SDKs to refuse it before it leaves the process, so the two sides
    /// cannot disagree about which method needs which capability.
    pub fn required_capability(&self) -> Option<&'static str> {
        match self {
            Self::GetRuntimeState { .. } | Self::ApplyControl { .. } | Self::GetControlReceipt { .. } => {
                Some(crate::CAPABILITY_DURABLE_RUNTIME_V1)
            }
            Self::GetSnapshot { .. } => Some(crate::CAPABILITY_SNAPSHOT_MARKER_V1),
            // Only the optional field is gated: an `ask_response` without an
            // `answer_id` is the legacy request and needs no negotiation.
            Self::AskResponse { answer_id, .. } if answer_id.is_some() => Some(crate::CAPABILITY_ASK_RECEIPTS_V1),
            _ => None,
        }
    }

    /// The capability this request needs that `advertised` does not contain.
    ///
    /// The bridge's whole gate: a request whose capability is missing must be
    /// answered with a structured `unsupported` error naming that capability,
    /// never served and never silently ignored. `None` means the request may
    /// proceed.
    pub fn missing_capability(&self, advertised: &[String]) -> Option<&'static str> {
        self.required_capability()
            .filter(|capability| !advertised.iter().any(|value| value == capability))
    }
}

/// One question's chosen option labels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskSelection {
    pub question_id: String,
    pub selected: Vec<String>,
}
