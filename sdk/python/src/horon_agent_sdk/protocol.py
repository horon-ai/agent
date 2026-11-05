"""Wire types for the horon-agent harness API (protocol v1).

Mirrors ``crates/harness-api`` exactly: request tags live under ``req``, event
tags under ``ev``, and every frame carries ``v``. Keep this module in sync with
the Rust enums; the parity test in ``tests/test_protocol_parity.py`` fails when
the tag sets drift apart from ``sdk/typescript/src/protocol.ts``, and the Rust
suite checks the TypeScript mirror against the enums.

Frames are plain dictionaries on the wire. The ``TypedDict`` classes below
document their shapes for readers and editors; no runtime coercion happens, so
a payload the server adds to stays readable rather than being dropped by a
stricter parser.

``TypedDict`` with ``total=False`` is used for shapes whose fields are optional
in the protocol. Required fields are the ones a reply always carries; they are
noted per class.
"""

from __future__ import annotations

from typing import Any, Literal, TypeAlias, TypedDict

API_VERSION_MAJOR = 1
API_VERSION_MINOR = 14

CAPABILITY_SURFACES: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("durable_runtime_v1", ("get_runtime_state", "apply_control", "get_control_receipt")),
    ("monitor_v1", ()),
    ("snapshot_marker_v1", ("get_snapshot", "snapshot_marker")),
    ("ask_receipts_v1", ("ask_response.answer_id",)),
)

"""Additive capabilities this protocol version defines, with the surfaces each
gates. Mirrors ``CAPABILITY_SURFACES`` in ``crates/harness-api/src/lib.rs``.

``monitor_v1`` gates no method on purpose: a monitor starts and stops through the
existing tool path, so the string only tells a client how to read what comes
back. ``structured_compaction_v1`` sets the same precedent.
"""

ADVERTISED_CAPABILITIES: tuple[str, ...] = tuple(capability for capability, _ in CAPABILITY_SURFACES)

RuntimeCommand: TypeAlias = Literal[
    "skills",
    "context",
    "info",
    "cache",
    "memory",
    "swarm",
    "agents",
    "subagent_model",
    "autoreview",
    "autojudge",
    "transport",
    "fast",
    "subscription",
    "overnight",
    "productivity",
    "initiatives",
    "premium",
    "provider_test_coverage",
    "refresh_model_list",
    "continue",
    "resume_all",
    "remote",
    "server_reload",
    "review",
    "judge",
    "subagent",
    "auth",
    "logout",
]

PermissionDecision: TypeAlias = Literal["allow", "allow_always", "deny"]


class AskOption(TypedDict, total=False):
    """One selectable option in an ``ask_requested`` question."""

    label: str
    description: str


class AskQuestion(TypedDict, total=False):
    """One question the ``ask`` tool wants a person to settle."""

    # Stable id the answer echoes; derived from the position when absent.
    id: str
    question: str
    header: str
    options: list[AskOption]
    # Index into ``options`` the model recommends.
    recommended: int
    multi: bool


class AskSelection(TypedDict):
    """One question's chosen option labels, as sent in ``ask_response``."""

    question_id: str
    selected: list[str]


ErrorCode: TypeAlias = Literal[
    "unsupported_version",
    "unknown_request",
    "unknown_session",
    "invalid_request",
    "internal",
    "unsupported",
]


class SessionEditStats(TypedDict):
    """Cumulative built-in file-tool changes, not net worktree diff."""

    added: int
    removed: int
    approximate: bool


class SessionInfo(TypedDict, total=False):
    session_id: str
    status: str
    edit_stats: SessionEditStats
    # Swarm owner, never the transcript's ordinary fork parent.
    parent_session_id: str
    # Stable task/role label, separate from the canonical display title.
    agent_label: str
    # Last persisted swarm lifecycle status, not connection status.
    swarm_status: str
    working_dir: str
    title: str
    # Whether a user client is attached. Omitted by older servers, meaning false.
    user_connected: bool
    # Approximate size of the stored transcript, in bytes.
    transcript_bytes: int
    archived: bool
    archived_at_ms: int


class ModelUsage(TypedDict, total=False):
    """Tracked turns with a persisted response, separate from historical picker selections."""

    count: int
    last_used_unix_secs: int | None
    tracking_started_unix_secs: int | None
    selection_count: int
    last_selected_unix_secs: int | None


def compare_model_usage(a: ModelUsage | None, b: ModelUsage | None) -> int:
    """Best-first usage ordering.

    Apply search relevance first and stable identity last.
    """
    if not a or not b:
        return -1 if a else (1 if b else 0)

    def _or(value: int | None, fallback: int) -> int:
        return fallback if value is None else value

    return (
        (b.get("count", 0) - a.get("count", 0))
        or (_or(b.get("last_used_unix_secs"), -1) - _or(a.get("last_used_unix_secs"), -1))
        or (b.get("selection_count", 0) - a.get("selection_count", 0))
        or (_or(b.get("last_selected_unix_secs"), -1) - _or(a.get("last_selected_unix_secs"), -1))
    )


class ModelRouteInfo(TypedDict, total=False):
    model: str
    provider: str
    api_method: str
    available: bool
    detail: str
    usage: ModelUsage


class TextMatch(TypedDict):
    path: str
    line: int
    column: int
    preview: str


class TodoSnapshot(TypedDict, total=False):
    """A session's persisted todo state, as read by the follow-up policy."""

    todos: list[dict[str, Any]]
    plan: dict[str, Any]
    goals: list[dict[str, Any]]
    gate_observations: list[dict[str, Any]]
    long_session_review_due: bool


class TodoEffects(TypedDict, total=False):
    """Persistent effects of a follow-up decision."""

    clear_gate_observations: bool
    mark_long_session_review_delivered: bool


class ResponseStats(TypedDict, total=False):
    """Durable raw provider counts summed over all assistant rounds in one user turn."""

    duration_secs: float
    input_tokens: int
    output_tokens: int
    cache_read_tokens: int
    cache_creation_tokens: int


class SessionSearchHit(TypedDict, total=False):
    """One transcript hit from ``search_sessions``."""

    session_id: str
    title: str
    # "user", "assistant", or "metadata" for a title/session-level hit.
    role: str
    preview: str
    at: str


ArtifactKind: TypeAlias = Literal["html", "image"]


class ArtifactEntry(TypedDict):
    """One entry from ``list_artifacts``. Metadata only; ``read_file`` serves the bytes."""

    path: str
    kind: ArtifactKind
    bytes: int
    mtime_ms: int


class HistoryMessage(TypedDict, total=False):
    # Only on the final assistant row. Preview/old-server history may omit it.
    response_stats: ResponseStats
    role: str
    content: str


class UsageSession(TypedDict, total=False):
    """Durable token usage for one session. A metric no request recorded is ``None``."""

    session_id: str
    model: str
    input: int | None
    output: int | None
    cache_read_input: int | None
    cache_creation_input: int | None
    requests: int


class UsageTotals(TypedDict, total=False):
    """Sums across every session in a usage report."""

    input: int | None
    output: int | None
    cache_read_input: int | None
    cache_creation_input: int | None
    requests: int
    sessions: int


class UsageModel(TypedDict, total=False):
    """Usage attributed to one model; ``model`` is ``unknown`` when metadata named none."""

    model: str
    input: int | None
    output: int | None
    cache_read_input: int | None
    cache_creation_input: int | None
    requests: int
    sessions: int


class UsageLimitInfo(TypedDict, total=False):
    """One subscription limit the daemon published for the account."""

    name: str
    used: float | None
    limit: float | None
    unit: str
    resets_at: str | None


class RenderedImageSource(TypedDict, total=False):
    kind: str
    tool_name: str
    role: str


class RenderedImageAnchor(TypedDict, total=False):
    kind: str
    id: str
    ordinal: int


class RenderedImage(TypedDict, total=False):
    media_type: str
    data: str
    label: str
    source: RenderedImageSource
    anchor: RenderedImageAnchor
    # Insert before this History.messages index (including hidden rows). Length means append.
    history_message_index: int


#: Base64 image attachment: ``(media_type, base64_data)``.
ImageAttachment: TypeAlias = tuple[str, str]


class SessionToolDefinition(TypedDict):
    """A tool definition exposed to the model."""

    name: str
    description: str
    parameters: dict[str, Any]


class ToolConfiguration(TypedDict, total=False):
    """Wire-level session tool policy. Callback functions never cross the wire."""

    enabled: list[str] | None
    disabled: list[str]
    custom: list[SessionToolDefinition]


#: Version of the replication exchange carried inside protocol v1.
REPLICATION_PROTOCOL_VERSION = 1
#: Maximum inline plus referenced records in one page.
MAX_REPLICATION_PAGE_EVENTS = 256
#: Maximum serialized page envelope, including JSON escaping.
MAX_REPLICATION_WIRE_BYTES = 16 * 1024 * 1024
#: Maximum history messages a page may request.
MAX_HISTORY_PAGE = 200
#: History messages returned when the request omits ``limit``.
DEFAULT_HISTORY_PAGE = 50


class CanonicalEventRecord(TypedDict):
    """One original event record, exactly as stored.

    ``canonical_json`` is opaque UTF-8 canonical event text. Never parse it into a
    number model and re-serialize it: u64 values above 2**53, ``1.0``, and ``-0.0``
    survive only as bytes.
    """

    event_id: str
    # The event's own hash, which excludes its ``content_hash`` field.
    content_hash: str
    # The complete canonical event JSON, including ``content_hash``.
    canonical_json: str


class CanonicalRecordRef(TypedDict):
    """A whole canonical record that did not fit the inline envelope."""

    # Zero-based logical position among all inline and referenced records.
    position: int
    event_id: str
    content_hash: str
    # SHA-256 of the complete canonical record bytes.
    sha256: str
    size: int


class PayloadRef(TypedDict):
    """Reference to an original payload blob stored outside the event log."""

    sha256: str
    size: int
    # Always ``canonical_json`` on this wire.
    encoding: str


class SessionBlobChunk(TypedDict):
    """One contiguous 1 MiB chunk of an original blob."""

    sha256: str
    offset: int
    # RFC 4648 standard base64, with padding.
    data_base64: str
    next_offset: int
    total_size: int
    complete: bool


class SessionHeads(TypedDict, total=False):
    """Reply to ``list_session_heads`` and ``select_session_head``."""

    session_id: str
    heads: list[str]
    # Absent when several heads need an explicit host-local selection.
    selected_head: str


class SessionHistoryPage(TypedDict, total=False):
    """Reply to ``get_history_page``."""

    session_id: str
    # The fixed head this page was computed on.
    head: str
    # Oldest first.
    messages: list[HistoryMessage]
    # Opaque continuation, passed back as ``before_event_id``.
    next_cursor: str


class SessionEventPage(TypedDict, total=False):
    """Reply to ``export_session_events``."""

    session_id: str
    heads: list[str]
    events: list[CanonicalEventRecord]
    next_cursor: str
    blob_refs: list[PayloadRef]
    record_refs: list[CanonicalRecordRef]


class SessionImportResult(TypedDict, total=False):
    """Reply to ``import_session_events``."""

    session_id: str
    accepted_ids: list[str]
    heads: list[str]
    retry_cursor: str
    conflict_heads: list[str]


class SessionBlobStatus(TypedDict):
    """Reply to ``get_session_blob_status`` and ``import_session_blob``."""

    session_id: str
    sha256: str
    received: int
    total_size: int
    complete: bool


class SessionRecordBlobStatus(TypedDict):
    """Reply to ``get_session_record_blob_status`` and ``import_session_record_blob``."""

    session_id: str
    reference: CanonicalRecordRef
    received: int
    total_size: int
    complete: bool


class RuntimeJournalSummary(TypedDict, total=False):
    """The journal file behind one session's runtime state."""

    # A journal exists on disk; ``False`` with zero counts is not an error.
    exists: bool
    # Restart-scoped generation on disk; 0 before the first row.
    generation: int
    last_seq: int
    bytes: int
    records: int
    # The journal cannot take another row and needs operator attention.
    read_only: bool


class RuntimeRunSummary(TypedDict, total=False):
    run_id: str
    # RFC 3339 UTC.
    started_at: str
    input_id: str


class RuntimeQueueSummary(TypedDict):
    """How much work is queued or in flight. Content is deliberately absent."""

    live_inputs: int
    # Started, or sealed in doubt.
    undetermined_inputs: int
    live_input_bytes: int


class RuntimeReceiptSummary(TypedDict, total=False):
    input_id: str
    # ``committed`` or ``cancelled``.
    phase: str
    seq: int
    run_id: str


class RuntimeMonitorSummary(TypedDict, total=False):
    monitor_id: str
    # Monitor kind, for example ``file`` or ``command``.
    kind: str
    stopped: bool
    # The monitor's own checkpoint summary, never its spec.
    last_summary: str


class RuntimeStopSummary(TypedDict, total=False):
    run_id: str
    # ``completed``, ``user_cancelled``, ``shutdown``, ``provider_error``,
    # ``timeout``, ``loop_guard``, ``limit``, or ``interrupted_unknown``.
    cause: str
    # RFC 3339 UTC.
    at: str


class RuntimeStateSummary(TypedDict, total=False):
    """One session's durable runtime journal, summarised. Never the queued text."""

    journal: RuntimeJournalSummary
    # Runs still open, oldest first.
    runs: list[RuntimeRunSummary]
    queue: RuntimeQueueSummary
    # Terminal input receipts, most recent first.
    receipts: list[RuntimeReceiptSummary]
    monitors: list[RuntimeMonitorSummary]
    # Wakes produced but not yet acknowledged by a delivered input.
    pending_wakes: int
    # Controls recorded but not yet terminal.
    pending_controls: int
    # Why the last turn ended, without reading the transcript.
    last_stop: RuntimeStopSummary


RuntimeControlOutcome: TypeAlias = Literal[
    "applying",
    "terminal",
    "replay",
    "conflict",
    "unknown",
    "indeterminate",
]


class RuntimeControlReceipt(TypedDict, total=False):
    """What is known about one control operation."""

    operation_id: str
    action: str
    outcome: RuntimeControlOutcome
    # The action's own result once terminal. Never command output.
    result: Any
    # Ready-to-display one-line status.
    message: str


class SnapshotMarkerInfo(TypedDict):
    """The boundary a snapshot was taken at."""

    # Restart-scoped; a restart changes it.
    generation: str
    seq: int
    # The id the paired ``snapshot`` reply carries.
    marker_id: str


class SessionSnapshot(TypedDict, total=False):
    """One consistent view of runtime state plus a bounded history tail."""

    generation: str
    seq: int
    marker_id: str
    state: RuntimeStateSummary
    # Oldest first, like ``history``.
    messages: list[HistoryMessage]
    next_history_cursor: str
    # The tail or byte budget cut the history short.
    truncated: bool


class StoreErrorDetail(TypedDict, total=False):
    """Finer cause on an error frame. Old servers omit it; old clients ignore it."""

    store_code: str
    retryable: bool
    accepted_ids: list[str]
    retry_cursor: str
    conflict_heads: list[str]


SidePanelPageFormat: TypeAlias = Literal["markdown", "pdf"]
SidePanelPageSource: TypeAlias = Literal["managed", "linked_file", "ephemeral"]


class SidePanelPage(TypedDict, total=False):
    """Markdown/PDF panel state, shared with the native runtime."""

    id: str
    title: str
    file_path: str
    format: SidePanelPageFormat
    source: SidePanelPageSource
    content: str
    # Base64 PDF bytes, separate from the human-readable Markdown fallback.
    pdf_data: str
    updated_at_ms: int


class AppletAction(TypedDict, total=False):
    """A user intent from an applet node (``horon-agent.applet/1``)."""

    action: str
    args: Any


class SidePanelSnapshot(TypedDict, total=False):
    # Monotonic explicit-focus intent. Absent on older servers.
    focus_revision: int
    focused_page_id: str | None
    # Omitted by the runtime when empty.
    pages: list[SidePanelPage]


TurnStopReason: TypeAlias = Literal[
    "interrupted",
    "failure",
    "crash",
    "provider_guardrail",
    "limit_reached",
    "unknown",
]
"""Natural completion has no stop reason. Transport loss is not proof of a crash."""


#: Request payloads. Each ``req`` discriminator's shape is documented by the
#: matching ``HoronAgentClient`` method, which builds it field for field.
ApiRequest: TypeAlias = dict[str, Any]
#: Event payloads, known or not. Narrow on ``ev`` before reading fields.
ApiEvent: TypeAlias = dict[str, Any]
#: Any frame off the wire, known or not. Narrow with :func:`is_known_event`.
AnyApiEvent: TypeAlias = dict[str, Any]
#: An event kind this SDK does not know about, kept for forward compatibility.
UnknownApiEvent: TypeAlias = dict[str, Any]
ApiEventKind: TypeAlias = str
ClientFrame: TypeAlias = dict[str, Any]
ServerFrame: TypeAlias = dict[str, Any]


#: Every event tag the SDK knows about, for drift checks and routing.
KNOWN_EVENT_KINDS: tuple[str, ...] = (
    "runtime_command_result",
    "hello_ok",
    "ok",
    "error",
    "sessions",
    "attached",
    "session_forked",
    "history",
    "side_pane_images",
    "side_panel_state",
    "applet_state",
    "pong",
    "text_delta",
    "text_done",
    "text_replace",
    "reasoning_delta",
    "reasoning_done",
    "tool_start",
    "tools",
    "tool_call",
    "tool_input_delta",
    "tool_exec",
    "tool_done",
    "token_usage",
    "kv_cache_miss",
    "turn_done",
    "soft_interrupt_injected",
    "soft_interrupts_cancelled",
    "background_tool_result",
    "turn_stopped",
    "wake_requested",
    "background_progress",
    "message_accepted",
    "permission_request",
    "approval_pending",
    "approval_resolved",
    "failover_switched",
    "run_needs_human_resume",
    "approval_ack",
    "failover_ack",
    "ask_requested",
    "session_status",
    "session_recovery",
    "connection_phase",
    "model_info",
    "models",
    "runtime_info",
    "credential_updated",
    "file_content",
    "files",
    "text_matches",
    "file_status",
    "todo_state",
    "session_search_results",
    "artifacts",
    "usage_report",
    "compacted",
    "session_renamed",
    "session_heads",
    "session_history_page",
    "session_event_page",
    "session_import_result",
    "session_blob_chunk",
    "session_blob_status",
    "session_record_blob_status",
    "runtime_state",
    "control_receipt",
    "snapshot_marker",
    "snapshot",
)

#: Every request tag the SDK can send.
KNOWN_REQUEST_KINDS: tuple[str, ...] = (
    "runtime_command",
    "hello",
    "list_sessions",
    "archive_session",
    "restore_session",
    "set_retention_policy",
    "create_session",
    "attach_session",
    "fork_session",
    "transfer_session",
    "detach_session",
    "configure_tools",
    "list_tools",
    "tool_result",
    "send_message",
    "cancel",
    "soft_interrupt",
    "get_history",
    "peek_session",
    "clear",
    "rewind",
    "permission_response",
    "approval_ack",
    "ask_response",
    "failover_ack",
    "list_models",
    "get_runtime_info",
    "usage_report",
    "set_api_key",
    "clear_api_key",
    "notify_auth_changed",
    "invalidate_usage",
    "read_file",
    "find_files",
    "search_text",
    "file_status",
    "search_sessions",
    "list_artifacts",
    "get_todo_state",
    "ack_todo_follow_up",
    "set_model",
    "set_reasoning_effort",
    "compact",
    "rename_session",
    "set_session_saved",
    "applet_action",
    "close_applet",
    "rewind_undo",
    "cancel_soft_interrupts",
    "background_tool",
    "list_session_heads",
    "select_session_head",
    "get_history_page",
    "export_session_events",
    "import_session_events",
    "export_session_blob",
    "import_session_blob",
    "get_session_blob_status",
    "export_session_record_blob",
    "import_session_record_blob",
    "get_session_record_blob_status",
    "get_runtime_state",
    "apply_control",
    "get_control_receipt",
    "get_snapshot",
    "ping",
)


def is_known_event(frame: AnyApiEvent) -> bool:
    """Whether this SDK knows the event tag ``frame["ev"]``."""
    return frame.get("ev") in KNOWN_EVENT_KINDS
