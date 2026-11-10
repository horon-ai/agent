//! Server-to-client events: replies and streaming.

use crate::handoff::HandoffCompaction;
use horon_agent_tool_types::AskQuestion;
use serde::{Deserialize, Serialize};

/// Curated event surface. Internally-tagged on `"ev"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum ApiEvent {
    /// Handshake accepted. Sent in reply to `Hello`.
    HelloOk {
        version: u32,
        /// Server name and version, e.g. "horon-agent/0.55.1".
        server: String,
        /// Optional capability strings for additive feature discovery.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },

    /// Generic success acknowledgment for requests without a richer reply.
    Ok,

    /// Reply to a runtime command. Not a transcript message or model output.
    RuntimeCommandResult {
        session_id: String,
        command: crate::RuntimeCommand,
        data: serde_json::Value,
        message: String,
    },

    /// Request failed.
    Error {
        code: ErrorCode,
        message: String,
        /// Finer cause for new clients. Old clients ignore the unknown field
        /// and keep decoding `code`, so the enum never grows for them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<crate::replication::StoreErrorDetail>,
    },

    /// Reply to `ListSessions`.
    Sessions { sessions: Vec<SessionInfo> },

    /// Reply to `CreateSession` / `AttachSession`.
    Attached { session: SessionInfo },

    /// Reply to `ForkSession`.
    SessionForked { session: SessionInfo },

    /// Reply to `GetHistory`.
    History {
        session_id: String,
        messages: Vec<HistoryMessage>,
        /// Images anchored to user prompts or tool calls in this transcript.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<RenderedImage>,
    },

    /// Reply to `Ping`.
    Pong,

    // --- Streaming events (carry session_id, not tied to a request id) ---
    /// Assistant text delta.
    TextDelta {
        session_id: String,
        text: String,
        /// Stream correlation id, not a persisted history message id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },

    /// Assistant message text ended. Reasoning alone is not a boundary.
    TextDone {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },

    /// Replace previously streamed text for this message, including rollback.
    /// An empty replacement retracts the message, even after TextDone.
    TextReplace {
        session_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },

    /// Model reasoning delta (render dim/italic; safe to ignore).
    ReasoningDelta { session_id: String, text: String },

    /// Reasoning finished for the current step.
    ReasoningDone {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<f64>,
    },

    /// Effective tool inventory, in reply to `ListTools`.
    Tools {
        session_id: String,
        tools: Vec<crate::SessionToolDefinition>,
    },

    /// Custom tool execution requested from the owning client.
    ToolCall {
        session_id: String,
        call_id: String,
        name: String,
        input: serde_json::Value,
    },

    /// A tool name is known, even if no argument bytes have arrived yet.
    /// Parallel calls may start before earlier calls finish streaming input.
    ToolStart {
        session_id: String,
        call_id: String,
        name: String,
    },
    /// Incremental, potentially incomplete JSON. A nonempty `call_id` identifies
    /// the call independently of event interleaving. Empty IDs are legacy input
    /// for the most recently started call.
    ToolInputDelta {
        session_id: String,
        call_id: String,
        delta: String,
    },
    ToolExec {
        session_id: String,
        call_id: String,
        name: String,
    },
    ToolDone {
        session_id: String,
        call_id: String,
        name: String,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Images the model just received from a tool result or image generator.
    /// Clients should render these at their transcript anchor immediately.
    SidePaneImages {
        session_id: String,
        images: Vec<RenderedImage>,
    },

    /// A tool call's arguments were rewritten before the tool ran, because the
    /// model's JSON writer did not match the tool's schema. Sent just before the
    /// call's `ToolDone`. Additive: a client that does not know this event
    /// ignores it, and the call finishes either way.
    ToolArgsRepaired {
        session_id: String,
        call_id: String,
        name: String,
        /// The arguments exactly as the model sent them.
        original_input: serde_json::Value,
        /// One line per repair applied, in the order they were applied.
        changes: Vec<String>,
    },

    /// Usage for the latest provider call, not cumulative session or turn totals.
    /// Input/cache accounting is provider-specific: Anthropic reports cache
    /// reads and writes separately, while OpenAI includes cache reads in input.
    TokenUsage {
        session_id: String,
        input: u64,
        output: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_input: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_creation_input: Option<u64>,
    },

    /// The provider request that just completed missed the KV (prompt) cache:
    /// a meaningful part of the previously cached prefix had to be resent.
    /// Follows the request's `TokenUsage`. `harness_caused` misses (the
    /// harness mutated the prefix) should be rendered prominently; switches
    /// and expiry are informational.
    KvCacheMiss {
        session_id: String,
        /// Stable snake_case id, e.g. `prefix_changed`, `expired`.
        reason: String,
        harness_caused: bool,
        missed_tokens: u64,
        expected_tokens: u64,
        read_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        documented_cause: Option<String>,
        /// Ready-to-display one-line summary.
        message: String,
    },

    /// An abnormal stop, never emitted for natural completion. This precedes
    /// TurnDone (and Error for failures). Render as status, not assistant text.
    /// Transport loss alone does not establish a Crash.
    TurnStopped {
        session_id: String,
        reason: crate::TurnStopReason,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_stop_reason: Option<String>,
    },

    /// The turn finished; the agent is idle.
    TurnDone {
        session_id: String,
        /// Soft interrupts still queued and not seen by this turn. Present only
        /// when nonzero and the bridge advertises `soft_interrupt_ids`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_soft_interrupts: Option<u32>,
    },

    /// Queued soft interrupts were delivered to the model. Everything the
    /// model writes after this event is written having seen them, and any
    /// interrupt still queued has not been seen yet.
    ///
    /// Emitted in stream order: after the last `tool_done` of the batch and
    /// before the first output of the response that sees the messages.
    SoftInterruptInjected {
        session_id: String,
        /// Ids from `soft_interrupt.client_id`, in queue order. Several queued
        /// messages are delivered as one user message. Empty when none of the
        /// delivered messages carried an id.
        #[serde(default)]
        client_ids: Vec<String>,
        /// `B` (turn would have ended), `C` (urgent, between tools), `D`
        /// (after all tools), or `turn_start` (sent while idle, so it started
        /// a new turn).
        point: String,
        /// Tools skipped by an urgent interrupt at point C.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_skipped: Option<u32>,
        /// `system` or `background_task` for daemon-originated injections.
        /// Absent for user messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_role: Option<String>,
    },

    /// Reply to `CancelSoftInterrupts` with `client_ids`.
    SoftInterruptsCancelled {
        /// Ids removed before delivery.
        cancelled: Vec<String>,
        /// Ids that were not queued: already delivered, or never queued.
        not_queued: Vec<String>,
    },

    /// Reply to `BackgroundTool`.
    BackgroundToolResult {
        /// Whether a running tool call was moved to the background.
        moved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
    },

    /// The daemon requests that its external operator decide when to run the
    /// session. Emitted only when external wake ownership is configured.
    WakeRequested {
        session_id: String,
        reason: String,
        notification: String,
    },

    /// A background task the agent is waiting on reported progress, or
    /// finished.
    ///
    /// The daemon already tracks percent/counts for backgrounded work (a long
    /// build, a test sweep, a swarm plan) and pushes it to its own UI, which
    /// draws a bar. Forwarding it as a typed event means any API client can
    /// draw the same bar instead of leaving the user with a spinner that says
    /// only "still working".
    BackgroundProgress {
        session_id: String,
        /// The `bg` task id, so a client can key one bar per task.
        task_id: String,
        /// Human label for the work, e.g. `bash` or `Model list refresh`.
        label: String,
        /// Completion fraction 0..=100, when the task reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        percent: Option<f32>,
        /// One-line status, e.g. `42% · Running tests`.
        summary: String,
        /// The task ended: clients should retire its bar.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        done: bool,
    },

    /// The agent accepted a user message: it is in the session's queue and
    /// will be processed. Sent once per `SendMessage` that the daemon acks.
    ///
    /// Distinct from the request-level `Ok`: `Ok` only says the bridge parsed
    /// the frame, while this says the agent itself has the message. A client
    /// that shows "sent" versus "acknowledged" needs the second fact, and
    /// without it the only proof a message landed is the reply, which can be
    /// minutes away.
    MessageAccepted { session_id: String },

    /// The harness needs a permission decision from the user.
    PermissionRequest {
        session_id: String,
        request_id: String,
        tool_name: String,
        description: String,
    },

    /// An approval request is parked, waiting for a decision. Unlike
    /// `PermissionRequest` this also covers clientless runs, where the decision
    /// arrives through a file or channel reply rather than this connection.
    ApprovalPending {
        session_id: String,
        request_id: String,
        /// Tool name or action the approval covers.
        action: String,
        /// RFC 3339 UTC deadline after which the timeout default applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<String>,
    },

    /// An approval request reached a final decision. Clients acknowledge with
    /// `ApprovalAck` so the daemon can drop its durable copy; a missing ack
    /// means the daemon re-emits it after a restart, deduped by `request_id`.
    ApprovalResolved {
        session_id: String,
        request_id: String,
        approved: bool,
        /// How the decision was reached (`file`, `inline`, `timeout_default`, `superseded`, ...).
        via: String,
        /// `tool` for the waiting tool call, `runner` for the ambient runner.
        resolved_by: String,
    },

    /// The run switched provider routes after a failover prompt. `chain_id`
    /// scopes the attempt budget, so an older chain's late ack cannot clear it.
    FailoverSwitched {
        session_id: String,
        chain_id: String,
        /// Route left behind.
        from: String,
        /// Route now in use.
        to: String,
        reason: String,
        /// 1-based attempt number within this chain.
        attempt: u32,
        /// Attempt cap for this chain.
        max: u32,
    },

    /// The run stopped and needs a human decision before it can continue.
    ///
    /// `request_id` is `None` when the stop is not tied to a permission
    /// request, for example when the automatic-failover budget ran out. A
    /// consumer must not try to resolve such a stop as an approval request.
    RunNeedsHumanResume {
        session_id: String,
        request_id: Option<String>,
        reason: String,
        attempts: u32,
    },

    /// Client acknowledgement of `ApprovalResolved`, carrying the id the client
    /// has consumed. The bridge mirrors the daemon's event vocabulary one to
    /// one, so the acknowledgement lives here too; a server never originates
    /// it.
    ApprovalAck { request_id: String },

    /// The `ask` tool needs a person to settle a decision, and the attached
    /// client said it can render questions. Answer with `ask_response`, echoing
    /// `ask_id`. The tool stops waiting on its own deadline and resolves the
    /// questions itself, so a client that never answers costs the turn a
    /// bounded wait rather than a hang.
    AskRequested {
        session_id: String,
        ask_id: String,
        questions: Vec<AskQuestion>,
    },

    /// Client acknowledgement of `FailoverSwitched`, scoped to one chain so a
    /// late ack from an older chain cannot clear the current one.
    FailoverAck {
        chain_id: String,
        from: String,
        to: String,
        attempt: u32,
    },

    /// Recovery intent from attachment history, emitted at most once per attach.
    /// May precede `Attached`. Subscribe to events before attaching. The client
    /// decides whether to send the continuation; the bridge never sends it.
    /// Ordinary history refreshes, empty histories, and active turns do not emit it.
    SessionRecovery {
        session_id: String,
        continuation_message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reconnect_notice: Option<String>,
    },

    /// Session-level status change (idle, generating, tool_running, ...).
    SessionStatus {
        session_id: String,
        status: String,
        /// Soft interrupts queued and not yet delivered, on the snapshot
        /// statuses sent after attach or reconnect. Absent when unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_soft_interrupts: Option<u32>,
    },

    /// Provider request lifecycle. The value uses the daemon's stable display
    /// vocabulary, for example `connecting`, `sending request`, `waiting for
    /// response`, `streaming`, or `retrying (2/4)`.
    ///
    /// This is separate from `SessionStatus`: a session can be `generating`
    /// throughout all of these phases, while clients need the finer progress to
    /// avoid looking stuck before the model emits its first token.
    ConnectionPhase { session_id: String, phase: String },

    /// The provider and model serving the attached session.
    ///
    /// Sent unsolicited after attach, and again whenever the model changes, so
    /// a client can show which model it is talking to without polling.
    ModelInfo {
        session_id: String,
        /// Provider name, e.g. `anthropic`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Model id, e.g. `claude-sonnet-4-20250514`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Reasoning effort, e.g. `high`, for providers that expose it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        /// Credential the session bills against, as resolved by the daemon:
        /// `oauth` or `api_key`. `None` when the provider has no such split
        /// or the daemon did not report it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_method: Option<String>,
    },

    /// Reply to `ListModels`: the models this session can switch to.
    Models {
        session_id: String,
        /// Model ids, in the daemon's preferred order.
        models: Vec<String>,
        /// The model currently serving the session, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<String>,
    },

    /// Provider/runtime identity and every route the daemon currently exposes.
    RuntimeInfo {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Reasoning effort, e.g. `high`, for providers that expose it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        /// Credential the session bills against (`oauth` or `api_key`), as
        /// resolved by the daemon. Clients must prefer this over guessing
        /// from `routes`, where one model can have both an OAuth and an API
        /// key route.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_method: Option<String>,
        routes: Vec<ModelRouteInfo>,
    },

    /// An API-key credential was persisted or removed.
    CredentialUpdated { provider: String, configured: bool },

    /// Reply to `ReadFile`.
    FileContent {
        session_id: String,
        path: String,
        content: String,
        size: u64,
        truncated: bool,
    },

    /// Reply to `FindFiles`.
    Files { session_id: String, paths: Vec<String> },

    /// Reply to `SearchText`.
    TextMatches {
        session_id: String,
        matches: Vec<TextMatch>,
    },

    /// Reply to `FileStatus`.
    FileStatus {
        session_id: String,
        path: String,
        exists: bool,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modified_ms: Option<u64>,
    },

    /// Reply to `GetTodoState`.
    TodoState {
        session_id: String,
        state: horon_agent_todo_policy::TodoSnapshot,
    },

    /// Reply to `SearchSessions`: transcript hits across every stored session.
    /// `query` is echoed so a client that fired several searches can match the
    /// answer to the one still on screen.
    SessionSearchResults {
        query: String,
        results: Vec<SessionSearchHit>,
    },

    /// Reply to `ListArtifacts`: the conversation's images and generated files.
    /// Metadata only. `path` is relative to the session's working directory, so
    /// `ReadFile` can serve it; entries are never returned for files that no
    /// longer exist.
    Artifacts {
        session_id: String,
        artifacts: Vec<ArtifactEntry>,
    },

    /// Reply to `UsageReport`.
    ///
    /// Every count is a durable per-request total read from a stored session
    /// record, or the daemon's own live total for the attached session. A
    /// metric that no request recorded is `null`, never zero: a provider that
    /// omitted cache accounting did not report zero cache tokens, and a usage
    /// screen that prints `0` there states something nobody measured.
    ///
    /// `pricing` is always `"unknown"` and `estimated_cost` is always `null`.
    /// This repository carries no price table, so there is nothing to multiply
    /// tokens by; inventing rates would put a made-up number in front of the
    /// user. Both fields are always present so a client can branch on them.
    UsageReport {
        /// The session the report was narrowed to; absent for a full report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// One row per session in scope, ordered by session id.
        sessions: Vec<UsageSession>,
        /// Sums across `sessions`. A metric no session recorded stays absent.
        totals: UsageTotals,
        /// Per-model aggregates across `sessions`, ordered by model id.
        models: Vec<UsageModel>,
        /// Context window of the active route in tokens, when the daemon has
        /// reported it for the attached session. Absent means unknown, not
        /// the default window of some other model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_window: Option<u64>,
        /// Subscription limits the daemon published for this account. Empty
        /// when no login, no subscription, or a daemon that does not report.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        limits: Vec<UsageLimitInfo>,
        /// Always `"unknown"`: this build has no price table.
        pricing: String,
        /// Always `null`: no price table means no cost estimate.
        estimated_cost: Option<f64>,
    },

    /// Reply to `Compact`: compaction was scheduled.
    ///
    /// Compaction is not synchronous. The daemon summarizes at the next safe
    /// point rather than interrupting a turn mid-flight, so this confirms the
    /// request was accepted, not that the transcript has already shrunk. A
    /// client that wants the result should re-read the history afterwards.
    ///
    /// A `method: "handoff"` request is the exception: it is answered after the
    /// boundary is committed, with `handoff` naming what happened.
    Compacted {
        session_id: String,
        /// Human-readable status, e.g. why compaction was refused.
        message: String,
        /// Present only for a handoff request, where the reply is the result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handoff: Option<HandoffCompaction>,
    },

    /// A session's title changed, whether set by a client or generated.
    SessionRenamed {
        session_id: String,
        /// The explicit title, absent when it was cleared.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// What a client should display, generated when no title is set.
        display_title: String,
    },

    /// Reply to `ListSessionHeads` and `SelectSessionHead`.
    SessionHeads {
        session_id: String,
        heads: Vec<String>,
        /// Absent when several heads need an explicit host-local selection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_head: Option<String>,
    },

    /// Reply to `GetHistoryPage`.
    SessionHistoryPage {
        session_id: String,
        /// The fixed head this page was computed on.
        head: String,
        /// Oldest first.
        messages: Vec<HistoryMessage>,
        /// Opaque continuation, passed back as `before_event_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
    },

    /// Reply to `ExportSessionMarkdown`.
    SessionMarkdown {
        session_id: String,
        /// Full Markdown transcript: secrets redacted, long tool results
        /// truncated with a marker.
        markdown: String,
    },

    /// Reply to `ExportSessionEvents`.
    SessionEventPage {
        session_id: String,
        heads: Vec<String>,
        events: Vec<crate::replication::CanonicalEventRecord>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blob_refs: Vec<crate::replication::PayloadRef>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        record_refs: Vec<crate::replication::CanonicalRecordRef>,
    },

    /// Reply to `ImportSessionEvents`.
    SessionImportResult {
        session_id: String,
        accepted_ids: Vec<String>,
        heads: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        conflict_heads: Vec<String>,
    },

    /// Reply to `ExportSessionBlob`.
    SessionBlobChunk {
        session_id: String,
        sha256: String,
        offset: u64,
        data_base64: String,
        next_offset: u64,
        total_size: u64,
        complete: bool,
    },

    /// Reply to `ImportSessionBlob` and `GetSessionBlobStatus`.
    SessionBlobStatus {
        session_id: String,
        sha256: String,
        received: u64,
        total_size: u64,
        complete: bool,
    },

    /// Reply to `ImportSessionRecordBlob` and `GetSessionRecordBlobStatus`.
    SessionRecordBlobStatus {
        session_id: String,
        reference: crate::replication::CanonicalRecordRef,
        received: u64,
        total_size: u64,
        complete: bool,
    },

    /// Reply to `GetRuntimeState`: the run, queue, receipt, and monitor
    /// summary of this session's durable runtime journal, after a restart as
    /// well as before one.
    ///
    /// A summary and nothing more. Accepted input content, command arguments,
    /// and command environment variables stay in the journal: a status panel
    /// needs counts, phases, and causes, and none of those require the text of
    /// what was queued. Requires `durable_runtime_v1`, and is sent only to a
    /// connection that negotiated it.
    RuntimeState {
        session_id: String,
        state: RuntimeStateSummary,
    },

    /// Reply to `ApplyControl` and `GetControlReceipt`.
    ///
    /// A retry of the same operation id reports the recorded receipt instead
    /// of applying the operation twice. Requires `durable_runtime_v1`.
    ControlReceipt {
        session_id: String,
        receipt: RuntimeControlReceipt,
    },

    /// Sent immediately before the `Snapshot` it bounds, on the same
    /// connection, so a client can classify later live events as strictly
    /// newer than that snapshot instead of inferring order from arrival.
    ///
    /// A stream event: it carries no `reply_to`. Requires
    /// `snapshot_marker_v1`, and is sent only to a connection that negotiated
    /// it.
    SnapshotMarker {
        session_id: String,
        marker: SnapshotMarkerInfo,
    },

    /// Reply to `GetSnapshot`: one consistent state view plus a bounded
    /// history tail, at the marker announced by the `SnapshotMarker` sent just
    /// before it. Requires `snapshot_marker_v1`.
    Snapshot {
        session_id: String,
        snapshot: SessionSnapshot,
    },

    /// Forward-compatibility catch-all: clients must skip this silently.
    #[serde(other)]
    Unknown,
}

impl ApiEvent {
    /// The capability a connection must have negotiated before this event may
    /// be delivered to it, or `None` for an event every connection may see.
    ///
    /// A new event is opt-in: a connection that never asked for the feature
    /// must not receive it, because a client that did not negotiate has no
    /// branch for it and would either drop it silently or render it wrong. The
    /// bridge filters every outbound frame through this table, so an event
    /// added here cannot leak to a legacy connection even if the producer
    /// forgets to check.
    pub fn required_capability(&self) -> Option<&'static str> {
        match self {
            Self::RuntimeState { .. } | Self::ControlReceipt { .. } => Some(crate::CAPABILITY_DURABLE_RUNTIME_V1),
            Self::Snapshot { .. } | Self::SnapshotMarker { .. } => Some(crate::CAPABILITY_SNAPSHOT_MARKER_V1),
            _ => None,
        }
    }
}

/// One transcript hit from `SearchSessions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSearchHit {
    /// Stored session the hit belongs to.
    pub session_id: String,
    /// Display title of that session, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `user`, `assistant`, or `metadata` for a title/session-level hit.
    pub role: String,
    /// Plain-text excerpt around the match, with `...` around a trimmed run.
    pub preview: String,
    /// RFC3339 timestamp of the matched message, or of the session for a
    /// metadata hit.
    pub at: String,
}

/// One file a conversation produced.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Html,
    Image,
}

/// One entry from `ListArtifacts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactEntry {
    /// Path relative to the session's working directory, `/`-separated.
    pub path: String,
    pub kind: ArtifactKind,
    /// Size on disk at the time of the reply.
    pub bytes: u64,
    /// Last modification time, epoch milliseconds.
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    UnknownRequest,
    UnknownSession,
    InvalidRequest,
    Internal,
    /// The request needs a capability this connection did not negotiate, or
    /// this build does not serve yet. `message` names the missing capability.
    /// Never sent for a request an older client could send, so an older client
    /// never has to decode it.
    Unsupported,
}

/// Summary of one session's durable runtime journal.
///
/// A summary and nothing more. Accepted input content, command arguments, and
/// command environment variables stay in the journal: a status panel needs
/// counts, phases, and causes, and none of those require the text of what was
/// queued.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStateSummary {
    pub journal: RuntimeJournalSummary,
    /// Runs still open, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RuntimeRunSummary>,
    pub queue: RuntimeQueueSummary,
    /// Terminal input receipts, most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<RuntimeReceiptSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<RuntimeMonitorSummary>,
    /// Wakes produced but not yet acknowledged by a delivered input.
    pub pending_wakes: u32,
    /// Controls recorded but not yet terminal.
    pub pending_controls: u32,
    /// The most recent stop across all runs, so a client can say why the last
    /// turn ended without reading the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stop: Option<RuntimeStopSummary>,
}

/// The journal file behind one session's runtime state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeJournalSummary {
    /// A journal exists on disk. `false` with zero counts means no runtime
    /// state has been recorded yet, which is not an error.
    pub exists: bool,
    /// Restart-scoped generation on disk; 0 before the first row.
    pub generation: u64,
    /// Last recorded sequence number.
    pub last_seq: u64,
    /// Bytes the journal occupies.
    pub bytes: u64,
    /// Complete rows recorded.
    pub records: u64,
    /// The journal cannot take another row and needs operator attention.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

/// One open run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeRunSummary {
    pub run_id: String,
    /// RFC 3339 UTC.
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<String>,
}

/// How much work is queued or in flight. Content is deliberately absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeQueueSummary {
    /// Inputs still holding work, whatever their phase.
    pub live_inputs: u32,
    /// Inputs whose outcome is not settled: started, or sealed in doubt.
    pub undetermined_inputs: u32,
    /// Bytes of accepted content still retained, without the content itself.
    pub live_input_bytes: u64,
}

/// One terminal input receipt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeReceiptSummary {
    pub input_id: String,
    /// `committed` or `cancelled`.
    pub phase: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// One monitor and the one-line text of its last checkpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeMonitorSummary {
    pub monitor_id: String,
    /// Monitor kind, for example `file` or `command`.
    pub kind: String,
    pub stopped: bool,
    /// The monitor's own checkpoint summary, never its spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_summary: Option<String>,
}

/// The most recent run stop.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStopSummary {
    pub run_id: String,
    /// `completed`, `user_cancelled`, `shutdown`, `provider_error`,
    /// `timeout`, `loop_guard`, `limit`, or `interrupted_unknown`.
    pub cause: String,
    /// RFC 3339 UTC.
    pub at: String,
}

/// What is known about one control operation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlOutcome {
    /// Recorded and still in flight.
    #[default]
    Applying,
    /// Reached a terminal record; `result` holds what it produced.
    Terminal,
    /// The same operation id had already settled; nothing ran twice.
    Replay,
    /// The id is settled with a different request; nothing changed.
    Conflict,
    /// No such operation was recorded.
    Unknown,
    /// It started but its terminal record could not be written, so its effect
    /// is not knowable. Never retried automatically.
    Indeterminate,
}

/// The recorded state of one control operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RuntimeControlReceipt {
    pub operation_id: String,
    pub action: String,
    pub outcome: RuntimeControlOutcome,
    /// The action's own result once terminal. Never command output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Ready-to-display one-line status.
    pub message: String,
}

/// The boundary a snapshot was taken at.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMarkerInfo {
    /// Restart-scoped identity of the observed state; a restart changes it.
    pub generation: String,
    /// Monotonic sequence within that generation.
    pub seq: u64,
    /// The id the paired `snapshot` reply carries.
    pub marker_id: String,
}

/// One consistent view of runtime state plus a bounded history tail.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionSnapshot {
    pub generation: String,
    pub seq: u64,
    pub marker_id: String,
    pub state: RuntimeStateSummary,
    /// Oldest first, like `History`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<HistoryMessage>,
    /// Opaque continuation for older pages, absent at the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_history_cursor: Option<String>,
    /// The tail or byte budget cut the history short.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    /// A non-headless user client is connected in the source daemon right now.
    /// Independent of lifecycle `status`, processing, and this API connection.
    /// False when unavailable from an older daemon.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_connected: bool,
    /// Cumulative built-in file-tool changes. Absent when unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_stats: Option<crate::SessionEditStats>,
    pub session_id: String,
    /// Swarm owner this agent reports to, not the transcript's fork parent.
    /// Absent for ordinary sessions and user-created forks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Stable task/role label assigned when spawning or assigning a swarm agent.
    /// Separate from `title`, which remains the user's canonical display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Last persisted swarm lifecycle status (for example `running`, `ready`,
    /// `completed`, or `failed`). Independent of this connection's `status`.
    /// Clients should tolerate new status strings and missing snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// The effective persisted display title. A custom rename takes precedence
    /// over the generated or imported title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: String,
    /// Size of the session's stored record, in bytes.
    ///
    /// A cheap, monotonic proxy for "how much conversation is in here": a
    /// client can size or sort by it without fetching every transcript, which
    /// is the difference between an instant overview and one that stalls on a
    /// dozen history requests. Approximate by design; `None` when the server
    /// could not determine it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_bytes: Option<u64>,
    /// Whether the user pinned/saved this session. Saved sessions sort before
    /// ordinary sessions in every first-party session picker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub saved: bool,
    /// Optional label given with `/save <label>`. Pickers display and search it
    /// alongside the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_label: Option<String>,
    /// Persisted transcript update time, used for newest-first ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
    /// Most recent active-process timestamp when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at_ms: Option<i64>,
    /// Archived sessions are hidden from the default list but never deleted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRouteInfo {
    pub model: String,
    pub provider: String,
    pub api_method: String,
    pub available: bool,
    pub detail: String,
    /// Tracked turns and prior picker selections, when supplied by the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::ModelUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextMatch {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

/// Durable token usage for one session.
///
/// Input and cache counts keep the provider's own accounting: a provider that
/// includes cache reads in input and one that reports them separately both
/// land here unchanged, exactly as the stored messages recorded them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageSession {
    pub session_id: String,
    /// The model the session recorded, when its metadata names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Summed input tokens; absent when no request recorded the metric.
    pub input: Option<u64>,
    /// Summed output tokens; absent when no request recorded the metric.
    pub output: Option<u64>,
    /// Summed cache-read input tokens; absent when no provider reported them.
    pub cache_read_input: Option<u64>,
    /// Summed cache-write input tokens; absent when no provider reported them.
    pub cache_creation_input: Option<u64>,
    /// Provider requests that recorded usage, not user turns: one turn can
    /// make several requests, and tool rounds count separately.
    pub requests: u64,
}

/// Sums across every session in a report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageTotals {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read_input: Option<u64>,
    pub cache_creation_input: Option<u64>,
    pub requests: u64,
    /// Sessions the sums cover, including sessions that recorded no usage.
    pub sessions: usize,
}

/// Usage attributed to one model across the sessions in a report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageModel {
    /// Model id, or `unknown` when the session metadata named none.
    pub model: String,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read_input: Option<u64>,
    pub cache_creation_input: Option<u64>,
    pub requests: u64,
    /// Sessions that used this model.
    pub sessions: usize,
}

/// One subscription limit the daemon published for the account.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageLimitInfo {
    /// Stable limit id, for example `subscription_spend`.
    pub name: String,
    /// Amount consumed in the current window, when the daemon reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    /// Window cap, when the plan has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// Unit for `used` and `limit`, for example `usd`.
    pub unit: String,
    /// RFC 3339 reset time, when the daemon reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// Durable usage for one user turn, summed across its assistant/tool rounds.
/// Input is the raw provider-reported count, not normalized across providers.
/// Cache reads may be included in input (OpenAI) or separate (Anthropic).
/// Missing telemetry is unknown, not zero. Counts are absent if any assistant
/// round lacks that metric. This is not a session total or a billing estimate.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResponseStats {
    /// Whole-turn wall-clock seconds, including tools. Currently not persisted,
    /// so restored history leaves this absent. Never inferred from tool timings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryMessage {
    /// Present only on the final visible assistant row of a completed stored
    /// user turn. Tool-only intermediate rounds contribute to these totals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_stats: Option<ResponseStats>,
    /// "user" | "assistant" | "tool".
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderedImageSource {
    UserInput,
    ToolResult { tool_name: String },
    Other { role: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderedImageAnchor {
    ToolCall { id: String },
    UserPrompt { ordinal: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderedImage {
    pub media_type: String,
    pub data: String,
    pub label: Option<String>,
    pub source: RenderedImageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<RenderedImageAnchor>,
    /// Insert before this zero-based entry in the accompanying History.messages
    /// array (including hidden/system/tool rows). Its length means append.
    /// Set for restored tool images, whose tool-call row may not be exposed by
    /// a client. Absent on live events and older servers. Preserve vector order
    /// for multiple images at the same boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_message_index: Option<usize>,
}

#[cfg(test)]
mod image_history_tests {
    use super::*;

    #[test]
    fn image_history_boundary_is_optional_and_round_trips() {
        let legacy = serde_json::json!({"media_type": "image/png", "data": "bytes", "label": null,
            "source": {"kind": "tool_result", "tool_name": "read"}, "anchor": {"kind": "tool_call", "id": "read-1"}});
        let mut image: RenderedImage = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(image.history_message_index, None);
        assert_eq!(serde_json::to_value(&image).unwrap(), legacy);
        for boundary in [0, 3] {
            image.history_message_index = Some(boundary);
            let encoded = serde_json::to_value(&image).unwrap();
            assert_eq!(encoded["history_message_index"], boundary);
            assert_eq!(serde_json::from_value::<RenderedImage>(encoded).unwrap(), image);
        }
    }
}

#[cfg(test)]
mod response_stats_tests {
    use super::*;

    #[test]
    fn history_response_stats_are_backward_compatible_and_optional() {
        let old = serde_json::json!({"role":"assistant","content":"answer"});
        let message: HistoryMessage = serde_json::from_value(old.clone()).unwrap();
        assert!(message.response_stats.is_none());
        assert_eq!(serde_json::to_value(message).unwrap(), old);
        let new = serde_json::json!({"role":"assistant","content":"answer",
            "response_stats":{"input_tokens":0,"output_tokens":12,"cache_read_tokens":0}});
        let message: HistoryMessage = serde_json::from_value(new.clone()).unwrap();
        assert_eq!(message.response_stats.as_ref().unwrap().duration_secs, None);
        assert_eq!(serde_json::to_value(message).unwrap(), new);
    }
}
