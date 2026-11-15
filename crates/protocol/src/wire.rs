use super::*;

/// Serde default for boolean fields that should default to `true` when absent,
/// so older clients that omit the field keep their previous (unconditional)
/// behavior.
fn default_true() -> bool {
    true
}

/// Wire spec for a task-DAG node submitted by an agent (seed/expand/inject).
/// Mirrors `horon_agent_plan::dag::NodeSpec` but kept as an explicit wire type so the
/// protocol stays self-describing and serde-stable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskGraphNodeSpec {
    pub id: String,
    pub content: String,
    /// "explore" | "implement" | "verify" | "fix" | "synthesize". Defaults to
    /// "explore" when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub priority: u8,
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// SDK-owned session tool declaration. Parameters is a JSON schema object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Replaces the session SDK overlay. None inherits normal selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionToolConfig {
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
    #[serde(default)]
    pub disabled: Vec<String>,
    #[serde(default)]
    pub custom: Vec<SessionToolDefinition>,
}

/// Client capabilities an ACP connection offers the daemon for one session.
///
/// Each flag says the client can serve the matching ACP v1 method back to the
/// agent (`fs/read_text_file`, `fs/write_text_file`, `terminal/*`). A daemon that
/// never sees this block treats the session as one with no client-side
/// capabilities, which is the behaviour every non-ACP client already gets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcpClientCapabilities {
    #[serde(default)]
    pub fs_read_text_file: bool,
    #[serde(default)]
    pub fs_write_text_file: bool,
    #[serde(default)]
    pub terminal: bool,
}

/// ACP options attached to a [`Request::Subscribe`].
///
/// Absent means "not an ACP connection", so it stays off the wire entirely and
/// older clients are byte-for-byte unaffected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcpSubscribeOptions {
    #[serde(default)]
    pub client_capabilities: AcpClientCapabilities,
    /// The connection can answer `tool_permission_requested` prompts.
    #[serde(default)]
    pub permission_requests: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_dirs: Vec<String>,
    /// ACP protocol version this connection negotiated. Absent means the
    /// connection did not ask for a version, which the daemon treats as v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
}

/// An error answered by a client for an agent-issued `client_request`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientRpcError {
    pub code: i64,
    pub message: String,
}

/// One MCP server an ACP client asks the daemon to add to a session.
///
/// `transport` is `"stdio"` (needs `command`), `"http"` or `"sse"` (both need
/// `url`); the daemon rejects anything else. Servers added this way are never
/// shared with other sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMcpServer {
    pub name: String,
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

/// Client request to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    #[serde(rename = "configure_tools")]
    ConfigureTools { id: u64, tools: SessionToolConfig },
    #[serde(rename = "list_tools")]
    ListTools { id: u64 },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: u64,
        call_id: String,
        output: String,
        #[serde(default)]
        error: Option<String>,
    },
    /// Send a message to the agent
    #[serde(rename = "message")]
    Message {
        id: u64,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<(String, String)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_reminder: Option<String>,
        /// Skill selected by the client for this and subsequent turns. The
        /// daemon resolves the name against its own skill registry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_skill: Option<String>,
        /// Append the user message as context only. The daemon persists it and
        /// acknowledges it without starting a model turn.
        #[serde(default, skip_serializing_if = "is_false")]
        no_reply: bool,
    },

    /// Ask the daemon to drop a queued `message` request before the agent has
    /// stored it. `message_request_id` names the earlier `message` request,
    /// sent on this connection after it negotiated ACP v2.
    ///
    /// The daemon answers `Done { id }` when the message was withdrawn, which
    /// is possible only while its turn still waits for the agent. Once that turn
    /// holds the agent (and stores the message right after) the answer is
    /// `Error { id }` with the message `already inserted`; a request this
    /// connection has no record of gets an `Error` as well. Nothing else about
    /// the session changes: a turn already running for another surface keeps
    /// running.
    #[serde(rename = "withdraw_message")]
    WithdrawMessage { id: u64, message_request_id: u64 },

    /// Cancel current generation
    #[serde(rename = "cancel")]
    Cancel { id: u64 },

    /// Cancel one specific turn, named by the run id the daemon reported.
    ///
    /// The daemon answers `Ack` in both cases. When `run_id` is the run the
    /// session is executing right now, the cancel takes the same path a plain
    /// `cancel` does. When it names no run, or a run that already ended, the
    /// daemon stops nothing and answers `Ack` alone, so a cancel that lost the
    /// race with a finished turn cannot stop the one that replaced it. A
    /// connection that has not yet seen a `foreground_state` for its turn
    /// keeps using `cancel`, whose behaviour is unchanged.
    #[serde(rename = "cancel_turn")]
    CancelTurn { id: u64, run_id: String },

    /// Cancel one tool call of the turn running in this session.
    ///
    /// `call_id` is the tool call id the daemon reported in `tool_start`. The
    /// answer is [`ServerEvent::ToolCallCancelResult`]: `accepted: true` when a
    /// signal was delivered to the running call, `accepted: false` with
    /// `reason: "unknown"` when no call by that id is running here. The turn
    /// continues afterwards: the cancelled call reports a `tool_done` whose
    /// result text the model sees. A background task is not stopped by this
    /// request; it has its own [`Request::CancelBackgroundTerminal`].
    #[serde(rename = "cancel_tool_call")]
    CancelToolCall { id: u64, call_id: String },

    /// Cancel one background terminal this session started.
    ///
    /// Answered with [`ServerEvent::ToolCallCancelResult`]; the final
    /// `background_work` event is what confirms the stop.
    #[serde(rename = "cancel_background_terminal")]
    CancelBackgroundTerminal { id: u64, call_id: String },

    /// List the background work still running in this session.
    ///
    /// Answered with [`ServerEvent::BackgroundWorkList`].
    #[serde(rename = "list_background_work")]
    ListBackgroundWork { id: u64 },

    /// Move the currently executing tool to background
    #[serde(rename = "background_tool")]
    BackgroundTool { id: u64 },

    /// Soft interrupt: inject message at next safe point without cancelling
    #[serde(rename = "soft_interrupt")]
    SoftInterrupt {
        id: u64,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<(String, String)>,
        /// If true, can skip remaining tools at injection point C
        #[serde(default)]
        urgent: bool,
        /// Opaque caller id, echoed in `soft_interrupt_injected` and accepted
        /// by a filtered `cancel_soft_interrupts`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },

    /// Cancel pending soft interrupts (remove from server queue before injection).
    ///
    /// Without `client_ids` every queued interrupt is dropped and the request
    /// is acknowledged. With `client_ids` only the matching ones are dropped,
    /// and the daemon answers with `soft_interrupts_cancelled`.
    #[serde(rename = "cancel_soft_interrupts")]
    CancelSoftInterrupts {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_ids: Option<Vec<String>>,
    },

    /// Clear conversation history
    #[serde(rename = "clear")]
    Clear { id: u64 },

    /// Rewind conversation history to the given 1-based message index.
    ///
    /// `summarize` asks the daemon to append a report on the branch the rewind
    /// discards. It is off unless both this flag and `[compaction]
    /// rewind_summary` are set, and an absent flag is exactly the old request.
    #[serde(rename = "rewind")]
    Rewind {
        id: u64,
        message_index: usize,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        summarize: bool,
    },

    /// Undo the most recent rewind, if one is available.
    #[serde(rename = "rewind_undo")]
    RewindUndo { id: u64 },

    /// Health check
    #[serde(rename = "ping")]
    Ping { id: u64 },

    /// Get current state (debug)
    #[serde(rename = "state")]
    GetState { id: u64 },

    /// Execute a debug command (debug socket only)
    #[serde(rename = "debug_command")]
    DebugCommand {
        id: u64,
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    /// Subscribe to events (for TUI clients)
    #[serde(rename = "subscribe")]
    Subscribe {
        id: u64,
        /// Full system prompt override for a new session only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        /// Desktop can render user_response tool calls as inline answers.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        supports_user_response: bool,
        /// Desktop can render `ask_requested` questions and answer them.
        /// Only such a client makes the `ask` tool wait for a human.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        supports_ask_requests: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_instance_id: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        client_has_local_history: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        allow_session_takeover: bool,
        /// Legacy ownership hint, retained for wire compatibility. Disconnects
        /// only mark a session crashed when they interrupt unfinished processing,
        /// regardless of this flag. Idle/completed sessions close normally.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        crash_on_disconnect: bool,
        /// Keep an already-running turn alive when this transport disconnects.
        /// Opt-in for remote clients only. Idle sessions still close normally,
        /// and reattachment uses persisted history plus future live events, not
        /// replay of missed deltas. This does not survive daemon termination.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        continue_on_disconnect: bool,
        /// Terminal-identifying env vars (tmux/zellij/kitty/DISPLAY/...) captured
        /// from the connecting client so the server can route spawn/focus hooks
        /// to the client's terminal instead of its own stale startup env (#405).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terminal_env: Vec<(String, String)>,
        /// ACP bridge options. Absent for every client that predates ACP, so the
        /// field never appears on their wire and their behaviour is unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acp: Option<AcpSubscribeOptions>,
    },

    /// Declare that this client is intentionally detaching before its transport
    /// closes. Retained for compatibility with older servers that use
    /// `crash_on_disconnect` for idle sessions too.
    #[serde(rename = "prepare_disconnect")]
    PrepareDisconnect { id: u64 },

    /// Get full conversation history (for TUI sync on connect)
    #[serde(rename = "get_history")]
    GetHistory { id: u64 },

    /// Get only provider/model metadata and available models.
    #[serde(rename = "get_model_catalog")]
    GetModelCatalog {
        id: u64,
        /// Older clients cannot decode new event variants. Only clients that
        /// explicitly opt in receive incremental model_usage_updated events.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        subscribe_usage_updates: bool,
    },

    /// Trigger server hot reload (build new version, restart)
    #[serde(rename = "reload")]
    Reload {
        id: u64,
        /// When `true` (the default for backward compatibility), the server
        /// reloads unconditionally. When `false`, the server only reloads if it
        /// detects a strictly-newer reload candidate binary, so callers like
        /// `horon-agent server reload` can request a graceful upgrade without risking
        /// a downgrade (e.g. a newer self-dev daemon next to an older release).
        #[serde(default = "default_true")]
        force: bool,
    },

    /// Resume a specific session by ID
    #[serde(rename = "resume_session")]
    ResumeSession {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_instance_id: Option<String>,
        #[serde(default)]
        client_has_local_history: bool,
        #[serde(default)]
        allow_session_takeover: bool,
    },

    /// Deliver a scheduled task to a currently live session.
    #[serde(rename = "notify_session")]
    NotifySession {
        id: u64,
        session_id: String,
        message: String,
    },

    /// Set the active model by name.
    ///
    /// A legacy/desktop compatibility shape (`{"type":"set_route","model":...}`)
    /// is also accepted, but it is normalized into this variant inside
    /// [`crate::decode_request`] rather than via a serde `alias`. A serde alias
    /// would make this variant *also* answer to the `set_route` tag, and serde's
    /// internally-tagged enums pick the first matching variant by tag (not by
    /// fields), so the bare-model shape would shadow a structured route switch.
    #[serde(rename = "set_model")]
    SetModel { id: u64, model: String },

    /// Set reasoning effort for providers that expose it (OpenAI: none|minimal|low|medium|high|xhigh|max; Anthropic: none|low|medium|high|xhigh|max; DeepSeek: none|low|medium|high|max)
    #[serde(rename = "set_reasoning_effort")]
    SetReasoningEffort {
        id: u64,
        effort: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session_id: Option<String>,
    },

    /// Non-presentation command executed against the attached live runtime.
    #[serde(rename = "runtime_command")]
    RuntimeCommand {
        id: u64,
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },

    /// Set or clear the active session's custom display title.
    #[serde(rename = "rename_session")]
    RenameSession {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },

    /// Bookmark (`saved: true`, optional label) or unbookmark the active
    /// session. Routed through the daemon so its in-memory session, which
    /// owns later writes, does not overwrite the flag.
    #[serde(rename = "set_session_saved")]
    SetSessionSaved {
        id: u64,
        saved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Hide or restore the attached session by appending an ArchiveChanged
    /// record. Routed through the daemon because its in-memory session owns the
    /// writer lock and later writes; nothing is deleted either way.
    #[serde(rename = "archive_session")]
    ArchiveSession {
        id: u64,
        archived: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Permanently remove one stored session and every per-session file keyed
    /// by its id.
    ///
    /// Unlike `ArchiveSession` this destroys the transcript, so it requires an
    /// explicit `confirm` from the caller and is refused while a turn is
    /// running for the session, another process is streaming it, or a client is
    /// attached to it. An idle session held between turns is closed first, not
    /// refused. It is a lightweight control request: the purge needs no
    /// attached session, only the store.
    #[serde(rename = "delete_session")]
    DeleteSession { id: u64, session_id: String, confirm: bool },

    /// Split the current session — clone conversation into a new session
    #[serde(rename = "split")]
    Split { id: u64 },

    /// Transfer the current session into a compacted handoff session
    #[serde(rename = "transfer")]
    Transfer { id: u64 },

    /// Trigger manual context compaction
    #[serde(rename = "compact")]
    Compact {
        id: u64,
        /// Which kind of compaction to run. Absent is the summary path older
        /// clients have always asked for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<CompactMethod>,
    },

    /// Notify server that auth credentials changed (e.g., after login)
    #[serde(rename = "notify_auth_changed")]
    NotifyAuthChanged {
        id: u64,
        /// Optional runtime provider identity whose credentials changed. Older
        /// clients omit this and get the legacy generic refresh behavior.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Typed auth lifecycle event for new clients. The legacy `provider`
        /// string is retained for old clients, while this payload gives the
        /// server enough context to activate the intended runtime/catalog
        /// profile deterministically.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<AuthChanged>,
        /// First-run onboarding may ask the server to choose the strongest
        /// available route across all authenticated providers. Normal re-auth,
        /// account switching, and older clients leave this false.
        #[serde(default, skip_serializing_if = "is_false")]
        prefer_strongest: bool,
    },

    /// Switch active OpenAI account label on the server session.
    /// Invalidate daemon-local usage and quota cooldown state after a banked reset.
    /// This never redeems a reset or switches accounts. `None` pins the default
    /// account scope, not whichever account is active when the request arrives.
    #[serde(rename = "invalidate_openai_usage")]
    InvalidateOpenAiUsage { id: u64, account_label: Option<String> },

    /// Client acknowledgement of [`ServerEvent::ApprovalResolved`].
    ///
    /// The daemon clears the record's durable `pending_event` only after it
    /// sees this, so a restart re-delivers an approval a client never saw and
    /// stops re-delivering one it did. A record that no longer exists is not
    /// an error: the ack may arrive after the runner already cleaned up.
    #[serde(rename = "approval_ack")]
    ApprovalAck {
        id: u64,
        /// Matches the `request_id` of the `ApprovalResolved` being acknowledged.
        request_id: String,
    },

    /// A human decision on an `approval_pending` request.
    ///
    /// The decision is recorded in the same durable store external approvers
    /// (channel replies, the IMAP poller) write to, so the tool that is waiting
    /// on the request sees it on its next poll and emits `approval_resolved`.
    /// `allow_always` records an approval: this daemon has no persistent
    /// always-allow store to extend, so it must not claim to have created one.
    #[serde(rename = "permission_decision")]
    PermissionDecision {
        id: u64,
        /// Matches the `request_id` of the `approval_pending` being answered.
        request_id: String,
        decision: PermissionDecision,
    },

    /// Client answer to a [`ServerEvent::AskRequested`].
    ///
    /// Routed to the `ask` tool call waiting on that request id. An answer for
    /// an id that is no longer pending is refused, so a late reply can never
    /// resolve a newer question.
    ///
    /// `answer_id` is the caller-chosen identity of this answer. When it is
    /// present the answer also passes through the session's answer ledger, so
    /// repeating the same `request_id`+`answer_id` with the same answers
    /// replays one resolution instead of resolving twice, and a different
    /// answer under the same id is a conflict. Absent is the legacy request and
    /// behaves exactly as before.
    #[serde(rename = "ask_response")]
    AskResponse {
        id: u64,
        /// Matches the `request_id` of the `AskRequested` being answered.
        request_id: String,
        answers: Vec<AskSelection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer_id: Option<String>,
    },

    /// Client acknowledgement of [`ServerEvent::FailoverSwitched`].
    ///
    /// Carries the whole announcement key so a late ack for an older chain
    /// cannot clear a newer switch's durable copy.
    #[serde(rename = "failover_ack")]
    FailoverAck {
        id: u64,
        chain_id: String,
        from: String,
        to: String,
        attempt: u32,
    },

    // === ACP client bridge ===
    /// ACP client's answer to a [`ServerEvent::ClientRequest`].
    ///
    /// `request_id` echoes the request the agent asked the client to serve, so a
    /// late answer can only settle its own request. `result` and `error` are
    /// mutually exclusive on the wire; whichever the client sent is forwarded
    /// unchanged to the bridge call that is waiting on it.
    #[serde(rename = "client_response")]
    ClientResponse {
        id: u64,
        request_id: String,
        #[serde(default)]
        result: Option<serde_json::Value>,
        #[serde(default)]
        error: Option<ClientRpcError>,
    },

    /// Add MCP servers to the connection's current session.
    ///
    /// Sent by an ACP client's `session/new`, `session/load` or
    /// `session/resume` before the first prompt. The servers are owned by this
    /// session only and are never written to shared configuration.
    #[serde(rename = "configure_session_mcp")]
    ConfigureSessionMcp { id: u64, servers: Vec<SessionMcpServer> },

    /// Set or clear the approval mode for the connection's current session.
    ///
    /// `mode` is `"off"` (auto-approve everything) or `"write"` (auto-approve
    /// plain file mutation, ask for process and network calls); any other value
    /// is rejected by the daemon. `None` follows the daemon configuration.
    #[serde(rename = "set_session_approval_mode")]
    SetSessionApprovalMode {
        id: u64,
        #[serde(default)]
        mode: Option<String>,
    },

    /// Send a message to other agents
    #[serde(rename = "comm_message")]
    CommMessage {
        id: u64,
        from_session: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<CommDeliveryMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake: Option<bool>,
        /// Sender-provided one-line summary. Receiving UIs render long
        /// message bodies collapsed to this with an expand control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tldr: Option<String>,
        /// Cross-swarm target: a swarm label or swarm id other than the
        /// sender's own. When set, the message is a cross-swarm DM delivered
        /// to `to_session` inside that swarm, or to its coordinator when
        /// `to_session` is omitted. Broadcasts never cross swarms.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_swarm: Option<String>,
    },

    /// List every live swarm (id, label, coordinator, member count) so agents
    /// can discover cross-swarm DM targets.
    #[serde(rename = "comm_list_swarms")]
    CommListSwarms { id: u64, session_id: String },

    /// Set (or clear, with an empty label) the human-readable label of the
    /// caller's swarm. Labels are unique across swarms.
    #[serde(rename = "comm_set_swarm_label")]
    CommSetSwarmLabel { id: u64, session_id: String, label: String },

    /// List agents and their activity
    #[serde(rename = "comm_list")]
    CommList { id: u64, session_id: String },

    /// Propose a swarm plan update
    #[serde(rename = "comm_propose_plan")]
    CommProposePlan {
        id: u64,
        session_id: String,
        items: Vec<PlanItem>,
    },

    /// Approve a plan proposal (coordinator only)
    #[serde(rename = "comm_approve_plan")]
    CommApprovePlan {
        id: u64,
        session_id: String,
        proposer_session: String,
    },

    /// Reject a plan proposal (coordinator only)
    #[serde(rename = "comm_reject_plan")]
    CommRejectPlan {
        id: u64,
        session_id: String,
        proposer_session: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Seed the swarm task DAG in one call (the first agent's draft). Replaces or
    /// initializes the shared plan with a validated graph of nodes + edges.
    #[serde(rename = "comm_seed_graph")]
    CommSeedGraph {
        id: u64,
        session_id: String,
        /// "deep" (comprehensive, gated) or "light" (fan-out).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        nodes: Vec<TaskGraphNodeSpec>,
    },

    /// Decompose a node the caller owns into a child sub-DAG (composite path). In
    /// deep mode a critique/verify gate is auto-inserted.
    #[serde(rename = "comm_expand_node")]
    CommExpandNode {
        id: u64,
        session_id: String,
        node_id: String,
        children: Vec<TaskGraphNodeSpec>,
    },

    /// Complete a node the caller owns with a typed handoff artifact. In deep mode
    /// the artifact is validated for thinness.
    #[serde(rename = "comm_complete_node")]
    CommCompleteNode {
        id: u64,
        session_id: String,
        node_id: String,
        /// Handoff artifact as a JSON object string.
        artifact_json: String,
    },

    /// Inject gap/fix nodes from a gate that found a problem, re-blocking the gate
    /// (and its composite parent) until the new nodes drain.
    #[serde(rename = "comm_inject_gap")]
    CommInjectGap {
        id: u64,
        session_id: String,
        gate_id: String,
        nodes: Vec<TaskGraphNodeSpec>,
    },

    /// Spawn a new agent session (coordinator only)
    #[serde(rename = "comm_spawn")]
    CommSpawn {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_nonce: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn_mode: Option<String>,
        /// Optional per-spawn model override. Takes precedence over
        /// `agents.swarm_model` config. Supports explicit auth-route prefixes
        /// (e.g. `openai-api:gpt-5.5`) and the `inherit`/`coordinator`
        /// sentinels to force coordinator inheritance past a config pin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Optional reasoning effort for the spawned agent (e.g. `none`,
        /// `low`, `medium`, `high`, `xhigh`, `max`). Unset = provider default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        /// Optional short human-readable label for the spawned agent shown in
        /// swarm UI (gallery chips, member lists). Overrides the task label
        /// otherwise derived from the first line of `initial_message`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Where the child runs: `none` (default) inherits the parent's working
        /// tree; `worktree` gives it a private git worktree checked out from the
        /// parent's HEAD; `clone` gives it a copy-on-write clone of the whole
        /// repository, including ignored build caches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        isolation: Option<String>,
        /// For `isolation: worktree` or `clone`, what happens to the child's work
        /// when its turn finishes: `apply` (default) merges its branch into the
        /// parent worktree, `keep` leaves it for inspection, `drop` removes it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        iso_apply: Option<String>,
        /// Optional JSON Schema for the child's result. When present the child
        /// gets a `yield` tool that validates its structured result against
        /// this schema, and the published child result carries those fields.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_schema: Option<serde_json::Value>,
        /// Optional tool allow list for the child session, resolved from a named
        /// agent definition. `None` leaves the child's tool surface as the
        /// configured profile decides; `Some(list)` narrows it to `list`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_tools: Option<Vec<String>>,
        /// Optional session to fork a consult child from. The child starts with
        /// a copy of that transcript and can only read; the source session is
        /// never written to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_session: Option<String>,
    },

    /// List models/routes available for spawning swarm agents
    #[serde(rename = "comm_list_models")]
    CommListModels { id: u64, session_id: String },

    /// Stop/destroy an agent session (coordinator only)
    #[serde(rename = "comm_stop")]
    CommStop {
        id: u64,
        session_id: String,
        target_session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },

    /// Assign a role to an agent (coordinator only)
    #[serde(rename = "comm_assign_role")]
    CommAssignRole {
        id: u64,
        session_id: String,
        target_session: String,
        role: String,
    },

    /// Get a summary of an agent's recent tool calls
    #[serde(rename = "comm_summary")]
    CommSummary {
        id: u64,
        session_id: String,
        target_session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },

    /// Get a lightweight status snapshot for an agent, even while it is busy
    #[serde(rename = "comm_status")]
    CommStatus {
        id: u64,
        session_id: String,
        target_session: String,
    },

    /// Submit a structured swarm completion/progress report for this session
    #[serde(rename = "comm_report")]
    CommReport {
        id: u64,
        session_id: String,
        /// Completion status to record for this member. Defaults to ready.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        /// Main report body.
        message: String,
        /// Optional validation/testing summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        validation: Option<String>,
        /// Optional blockers/follow-up summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_up: Option<String>,
        /// Reporter-provided one-line summary. Receiving UIs render long
        /// report bodies collapsed to this with an expand control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tldr: Option<String>,
    },

    /// Read another agent's full conversation context
    #[serde(rename = "comm_read_context")]
    CommReadContext {
        id: u64,
        session_id: String,
        target_session: String,
    },

    /// Attach/resync this session with the swarm plan
    #[serde(rename = "comm_resync_plan")]
    CommResyncPlan { id: u64, session_id: String },

    /// Get a lightweight summary of the current swarm plan graph
    #[serde(rename = "comm_plan_status")]
    CommPlanStatus { id: u64, session_id: String },

    /// Assign a task from the plan to a specific agent (coordinator only)
    #[serde(rename = "comm_assign_task")]
    CommAssignTask {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Assign the next runnable unassigned task from the plan (coordinator only)
    #[serde(rename = "comm_assign_next")]
    CommAssignNext {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefer_spawn: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn_if_needed: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Optional model override for workers spawned by this assignment
        /// (same semantics as CommSpawn::model).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Optional reasoning effort for workers spawned by this assignment
        /// (same semantics as CommSpawn::effort).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },

    /// Control an existing assigned task lifecycle (coordinator only)
    #[serde(rename = "comm_task_control")]
    CommTaskControl {
        id: u64,
        session_id: String,
        action: String,
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Wait until specified (or all) swarm members reach a target status
    #[serde(rename = "comm_await_members")]
    CommAwaitMembers {
        id: u64,
        session_id: String,
        /// Statuses that count as "done" (e.g. ["completed", "stopped"])
        target_status: Vec<String>,
        /// Specific session IDs to watch. If empty, watches all non-self members.
        #[serde(default)]
        session_ids: Vec<String>,
        /// Whether to wait for all matching members or wake when any member matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        /// Timeout in seconds (default 3600 = 1 hour)
        #[serde(default)]
        timeout_secs: Option<u64>,
        /// Run the wait as a detached background watcher instead of blocking the
        /// requesting turn. Defaults to true so the agent stays responsive.
        #[serde(default = "default_true")]
        background: bool,
        /// When backgrounded, surface a notification card on completion.
        #[serde(default = "default_true")]
        notify: bool,
        /// When backgrounded, wake an idle requesting agent with the result (or
        /// soft-interrupt it if busy). Defaults to true.
        #[serde(default = "default_true")]
        wake: bool,
    },

    /// Read this session's durable runtime journal as a summary.
    ///
    /// Added for the stable harness API's `get_runtime_state`, which the bridge
    /// serves. The bridge is a separate process from this daemon, so it cannot
    /// read the daemon's in-memory state; the journal is the one owner both
    /// processes share. The reply is [`ServerEvent::RuntimeState`], and it is a
    /// summary: queued input content, command arguments, and command
    /// environment variables never travel.
    #[serde(rename = "get_runtime_state")]
    GetRuntimeState { id: u64 },

    /// Apply one control operation under its durable receipt.
    ///
    /// `operation_id` is the caller's identity for the request: the same id with
    /// the same action replays the recorded answer instead of applying it twice.
    /// The reply is [`ServerEvent::ControlReceipt`].
    #[serde(rename = "apply_control")]
    ApplyControl {
        id: u64,
        operation_id: String,
        /// `abort_run` or `cancel_inputs`.
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<serde_json::Value>,
    },

    /// Read the terminal state of one control operation.
    ///
    /// The reply is [`ServerEvent::ControlReceipt`].
    #[serde(rename = "get_control_receipt")]
    GetControlReceipt { id: u64, operation_id: String },

    /// One consistent observation view plus the boundary that separates it from
    /// the live stream.
    ///
    /// Added for the stable harness API's `get_snapshot`. The daemon owns the
    /// generation and the sequence, so the bridge cannot answer this itself.
    /// The reply is [`ServerEvent::SnapshotResult`], which carries the marker and
    /// the view together so the pairing cannot be lost between them.
    #[serde(rename = "get_snapshot")]
    GetSnapshot {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u64>,
    },
}

pub mod acp_v2_events;
pub mod event_types;
mod events;
mod model_identity;
pub use acp_v2_events::{BackgroundWorkEvent, BackgroundWorkItem};
pub use event_types::{FileDiffWire, PlanEntryWire};
pub use events::ServerEvent;
pub use model_identity::RuntimeModelIdentity;

/// One question's chosen option labels, as answered by a client.
///
/// The labels are echoed verbatim rather than indices: the answer the model
/// sees must be the same text the human clicked, even if the client reordered
/// the options while rendering them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskSelection {
    pub question_id: String,
    pub selected: Vec<String>,
}

/// A human decision on a pending approval request.
///
/// `AllowPath` and `AllowFolder` answer "yes, and stop asking for this place":
/// the first covers the one file the request named, the second covers its
/// folder and everything under it. Both are approved answers, so a client that
/// only knows the older three keeps working; the daemon remembers the scope
/// itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    AllowAlways,
    AllowPath,
    AllowFolder,
    Deny,
}

/// What a `compact` request should write as the new boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactMethod {
    /// A generated handoff document in place of the model's summary. The
    /// request answers after the boundary is committed, so its reply carries
    /// the outcome rather than "scheduled".
    Handoff,
}

/// What the handoff compaction asked for by a `compact` request did.
///
/// The reply is the result, not an acknowledgement: a failed generation leaves
/// the transcript unchanged and says so instead of reporting a summary that was
/// never written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HandoffCompactionReport {
    /// The document became the boundary, covering the first `covered` messages
    /// of the active context.
    Committed { covered: usize },
    /// The recent-token budget kept every message verbatim; nothing was written.
    NothingToCover,
    /// The completion failed, or its document was refused (empty, or without a
    /// `Goal` heading). The transcript is unchanged.
    ProviderError { detail: String },
    /// The caller cancelled before the completion answered. The transcript is
    /// unchanged.
    Cancelled,
}
