/**
 * Wire types for the horon-agent harness API (protocol v1).
 *
 * Mirrors `crates/harness-api` exactly: request tags live under `req`,
 * event tags under `ev`, and every frame carries `v`. Keep this file in sync
 * with the Rust enums; `test/schema-parity.test.ts` fails the build if the
 * tag sets drift apart.
 */

export const API_VERSION_MAJOR = 1;
export const API_VERSION_MINOR = 18;

/**
 * Additive capabilities this protocol version defines, with the surfaces each
 * one gates. Mirrors `CAPABILITY_SURFACES` in `crates/harness-api/src/lib.rs`.
 *
 * `monitor_v1` gates no method on purpose: a monitor starts and stops through
 * the existing tool path, so the string only tells a client how to read what
 * comes back. `structured_compaction_v1` sets the same precedent.
 */
export const CAPABILITY_SURFACES: ReadonlyArray<readonly [string, readonly string[]]> = [
  ["durable_runtime_v1", ["get_runtime_state", "apply_control", "get_control_receipt"]],
  ["monitor_v1", []],
  ["snapshot_marker_v1", ["get_snapshot", "snapshot_marker"]],
  ["ask_receipts_v1", ["ask_response.answer_id"]],
];

/** Every capability this protocol version defines, in advertisement order. */
export const ADVERTISED_CAPABILITIES: readonly string[] = CAPABILITY_SURFACES.map(([capability]) => capability);

export type RuntimeCommand =
  | "skills"
  | "context"
  | "info"
  | "memory"
  | "swarm"
  | "agents"
  | "subagent_model"
  | "autoreview"
  | "autojudge"
  | "transport"
  | "fast"
  | "overnight"
  | "productivity"
  | "initiatives"
  | "refresh_model_list"
  | "continue"
  | "resume_all"
  | "remote"
  | "server_reload"
  | "review"
  | "judge"
  | "subagent"
  | "auth"
  | "logout"
  | "plan";

export type PermissionDecision = "allow" | "allow_always" | "deny";

/** One selectable option in an `ask_requested` question. */
export interface AskOption {
  label: string;
  description?: string;
}

/** One question the `ask` tool wants a person to settle. */
export interface AskQuestion {
  /** Stable id the answer echoes; derived from the position when absent. */
  id?: string;
  question: string;
  header?: string;
  options: AskOption[];
  /** Index into `options` the model recommends. */
  recommended?: number;
  multi?: boolean;
}

/** One question's chosen option labels, as sent in `ask_response`. */
export interface AskSelection {
  question_id: string;
  selected: string[];
}

export type ErrorCode =
  | "unsupported_version"
  | "unknown_request"
  | "unknown_session"
  | "invalid_request"
  | "internal"
  | "unsupported";

/** Cumulative built-in file-tool changes, not net worktree diff. */
export interface SessionEditStats {
  added: number;
  removed: number;
  approximate: boolean;
}

export interface SessionInfo {
  edit_stats?: SessionEditStats;
  session_id: string;
  /** Swarm owner, never the transcript's ordinary fork parent. */
  parent_session_id?: string;
  /** Stable task/role label, separate from the canonical display title. */
  agent_label?: string;
  /** Last persisted swarm lifecycle status, not connection status. */
  swarm_status?: string;
  working_dir?: string;
  title?: string;
  status: string;
  /** Whether a user client is attached. Omitted by older servers, meaning false. */
  user_connected?: boolean;
  /** Approximate size of the stored transcript, in bytes. */
  transcript_bytes?: number;
  archived?: boolean;
  archived_at_ms?: number;
}

/** Tracked turns with a persisted response, separate from historical picker selections. */
export interface ModelUsage {
  count: number;
  last_used_unix_secs?: number | null;
  tracking_started_unix_secs?: number | null;
  selection_count: number;
  last_selected_unix_secs?: number | null;
}

/** Best-first usage ordering. Apply search relevance first and stable identity last. */
export function compareModelUsage(a?: ModelUsage | null, b?: ModelUsage | null): number {
  if (!a || !b) return a ? -1 : b ? 1 : 0;
  return (
    b.count - a.count ||
    (b.last_used_unix_secs ?? -1) - (a.last_used_unix_secs ?? -1) ||
    b.selection_count - a.selection_count ||
    (b.last_selected_unix_secs ?? -1) - (a.last_selected_unix_secs ?? -1)
  );
}

export interface ModelRouteInfo {
  model: string;
  provider: string;
  api_method: string;
  available: boolean;
  detail: string;
  usage?: ModelUsage;
}

export interface TextMatch {
  path: string;
  line: number;
  column: number;
  preview: string;
}

/** A session's persisted todo state, as read by the follow-up policy.
 * Todos, plan, goals, and observations use the runtime's todo schema. */
export interface TodoSnapshot {
  todos: Array<Record<string, unknown>>;
  plan: Record<string, unknown>;
  goals: Array<Record<string, unknown>>;
  gate_observations: Array<Record<string, unknown>>;
  long_session_review_due: boolean;
}

/** Persistent effects of a follow-up decision. */
export interface TodoEffects {
  clear_gate_observations?: boolean;
  mark_long_session_review_delivered?: boolean;
}

/** Durable raw provider counts summed over all assistant rounds in one user turn.
 * Missing metrics are unknown, not zero. Cache accounting differs by provider.
 * Restored duration is currently unavailable. */
export interface ResponseStats {
  duration_secs?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_read_tokens?: number;
  cache_creation_tokens?: number;
}

/** One transcript hit from `search_sessions`. */
export interface SessionSearchHit {
  session_id: string;
  /** Absent when the session has no title. */
  title?: string;
  /** "user", "assistant", or "metadata" for a title/session-level hit. */
  role: string;
  /** Plain-text excerpt around the match, `...`-trimmed. */
  preview: string;
  /** RFC3339 time of the matched message, or of the session for a metadata hit. */
  at: string;
}

/** What an artifact is: an HTML page or an image. */
export type ArtifactKind = "html" | "image";

/** One entry from `list_artifacts`. Metadata only; `read_file` serves the bytes. */
export interface ArtifactEntry {
  /** Path relative to the session's working directory, `/`-separated. */
  path: string;
  kind: ArtifactKind;
  bytes: number;
  mtime_ms: number;
}

export interface HistoryMessage {
  /** Only on the final assistant row. Preview/old-server history may omit it. */
  response_stats?: ResponseStats;
  /** "user" | "assistant" | "tool" */
  role: string;
  content: string;
}

/** Durable token usage for one session. A metric no request recorded is `null`. */
export interface UsageSession {
  session_id: string;
  model?: string;
  input: number | null;
  output: number | null;
  cache_read_input: number | null;
  cache_creation_input: number | null;
  /** Provider requests that recorded usage, not user turns. */
  requests: number;
}

/** Sums across every session in a usage report. */
export interface UsageTotals {
  input: number | null;
  output: number | null;
  cache_read_input: number | null;
  cache_creation_input: number | null;
  requests: number;
  sessions: number;
}

/** Usage attributed to one model; `model` is `unknown` when metadata named none. */
export interface UsageModel {
  model: string;
  input: number | null;
  output: number | null;
  cache_read_input: number | null;
  cache_creation_input: number | null;
  requests: number;
  sessions: number;
}

/** One subscription limit the daemon published for the account. */
export interface UsageLimitInfo {
  name: string;
  used?: number | null;
  limit?: number | null;
  unit: string;
  resets_at?: string | null;
}

export type RenderedImageSource =
  { kind: "user_input" } | { kind: "tool_result"; tool_name: string } | { kind: "other"; role: string };

export type RenderedImageAnchor = { kind: "tool_call"; id: string } | { kind: "user_prompt"; ordinal: number };

export interface RenderedImage {
  media_type: string;
  data: string;
  label?: string;
  source: RenderedImageSource;
  anchor?: RenderedImageAnchor;
  /** Insert before this History.messages index (including hidden rows). Length means append. */
  history_message_index?: number;
}

/** One loss a provider request suffered to fit its schema dialect. */
export type DegradationKind = "schema_keyword_dropped" | "strict_disabled" | "duplicate_tool_dropped";

export interface DegradationReceipt {
  kind: DegradationKind;
  /** The tool definition the loss applies to. */
  tool: string;
  /** Dropped keyword, `format:<value>`, `strict`, or the duplicated tool name. */
  target: string;
}

/** Base64 image attachment: [mediaType, base64Data]. */
export type ImageAttachment = [string, string];

/** A tool definition exposed to the model. */
export interface SessionToolDefinition {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
}

/** How the daemon delivers messages that arrive while it is busy. */
export interface MessageModes {
  /** Queued messages the first delivery point of a turn carries. */
  follow_up_mode: string;
  /** Queued messages each later delivery point of the same turn carries. */
  steering_mode: string;
  /** Whether a queued message waits for the running tool call or stops it. */
  interrupt_mode: string;
}

/** Wire-level session tool policy. Callback functions never cross the wire. */
export interface ToolConfiguration {
  enabled?: string[] | null;
  disabled?: string[];
  custom?: SessionToolDefinition[];
}

/** Version of the replication exchange carried inside protocol v1. */
export const REPLICATION_PROTOCOL_VERSION = 1;
/** Maximum inline plus referenced records in one page. */
export const MAX_REPLICATION_PAGE_EVENTS = 256;
/** Maximum serialized page envelope, including JSON escaping. */
export const MAX_REPLICATION_WIRE_BYTES = 16 * 1024 * 1024;
/** Maximum history messages a page may request. */
export const MAX_HISTORY_PAGE = 200;
/** History messages returned when the request omits `limit`. */
export const DEFAULT_HISTORY_PAGE = 50;

/**
 * One original event record, exactly as stored.
 *
 * `canonical_json` is opaque UTF-8 canonical event text. Never parse it into a
 * number model and re-serialize it: u64 values above 2^53, `1.0`, and `-0.0`
 * survive only as bytes.
 */
export interface CanonicalEventRecord {
  event_id: string;
  /** The event's own hash, which excludes its `content_hash` field. */
  content_hash: string;
  /** The complete canonical event JSON, including `content_hash`. */
  canonical_json: string;
}

/** A whole canonical record that did not fit the inline envelope. */
export interface CanonicalRecordRef {
  /** Zero-based logical position among all inline and referenced records. */
  position: number;
  event_id: string;
  content_hash: string;
  /** SHA-256 of the complete canonical record bytes. */
  sha256: string;
  size: number;
}

/** Reference to an original payload blob stored outside the event log. */
export interface PayloadRef {
  sha256: string;
  size: number;
  /** Always `canonical_json` on this wire. */
  encoding: string;
}

/** One contiguous 1 MiB chunk of an original blob. */
export interface SessionBlobChunk {
  sha256: string;
  offset: number;
  /** RFC 4648 standard base64, with padding. */
  data_base64: string;
  next_offset: number;
  total_size: number;
  complete: boolean;
}

/** Reply to `ListSessionHeads` and `SelectSessionHead`. */
export interface SessionHeads {
  session_id: string;
  heads: string[];
  /** Absent when several heads need an explicit host-local selection. */
  selected_head?: string;
}

/** Reply to `GetHistoryPage`. */
export interface SessionHistoryPage {
  session_id: string;
  /** The fixed head this page was computed on. */
  head: string;
  /** Oldest first. */
  messages: HistoryMessage[];
  /** Opaque continuation, passed back as `before_event_id`. */
  next_cursor?: string;
}

/** Reply to `ExportSessionEvents`. */
export interface SessionEventPage {
  session_id: string;
  heads: string[];
  events: CanonicalEventRecord[];
  next_cursor?: string;
  blob_refs?: PayloadRef[];
  record_refs?: CanonicalRecordRef[];
}

/** Reply to `ImportSessionEvents`. */
export interface SessionImportResult {
  session_id: string;
  accepted_ids: string[];
  heads: string[];
  retry_cursor?: string;
  conflict_heads?: string[];
}

/** Reply to `GetSessionBlobStatus` and `ImportSessionBlob`. */
export interface SessionBlobStatus {
  session_id: string;
  sha256: string;
  received: number;
  total_size: number;
  complete: boolean;
}

/** Reply to `GetSessionRecordBlobStatus` and `ImportSessionRecordBlob`. */
export interface SessionRecordBlobStatus {
  session_id: string;
  reference: CanonicalRecordRef;
  received: number;
  total_size: number;
  complete: boolean;
}

/** One session's durable runtime journal, summarised. Never the queued text. */
export interface RuntimeStateSummary {
  journal: RuntimeJournalSummary;
  /** Runs still open, oldest first. */
  runs?: RuntimeRunSummary[];
  queue: RuntimeQueueSummary;
  /** Terminal input receipts, most recent first. */
  receipts?: RuntimeReceiptSummary[];
  monitors?: RuntimeMonitorSummary[];
  /** Wakes produced but not yet acknowledged by a delivered input. */
  pending_wakes: number;
  /** Controls recorded but not yet terminal. */
  pending_controls: number;
  /** Why the last turn ended, without reading the transcript. */
  last_stop?: RuntimeStopSummary;
}

/** The journal file behind one session's runtime state. */
export interface RuntimeJournalSummary {
  /** A journal exists on disk; `false` with zero counts is not an error. */
  exists: boolean;
  /** Restart-scoped generation on disk; 0 before the first row. */
  generation: number;
  last_seq: number;
  bytes: number;
  records: number;
  /** The journal cannot take another row and needs operator attention. */
  read_only?: boolean;
}

export interface RuntimeRunSummary {
  run_id: string;
  /** RFC 3339 UTC. */
  started_at: string;
  input_id?: string;
}

/** How much work is queued or in flight. Content is deliberately absent. */
export interface RuntimeQueueSummary {
  live_inputs: number;
  /** Started, or sealed in doubt. */
  undetermined_inputs: number;
  live_input_bytes: number;
}

export interface RuntimeReceiptSummary {
  input_id: string;
  /** `committed` or `cancelled`. */
  phase: string;
  seq: number;
  run_id?: string;
}

export interface RuntimeMonitorSummary {
  monitor_id: string;
  /** Monitor kind, for example `file` or `command`. */
  kind: string;
  stopped: boolean;
  /** The monitor's own checkpoint summary, never its spec. */
  last_summary?: string;
}

export interface RuntimeStopSummary {
  run_id: string;
  /** `completed`, `user_cancelled`, `shutdown`, `provider_error`, `timeout`,
   * `loop_guard`, `limit`, or `interrupted_unknown`. */
  cause: string;
  /** RFC 3339 UTC. */
  at: string;
}

/** What is known about one control operation. */
export type RuntimeControlOutcome =
  | "applying"
  | "terminal"
  | "replay"
  | "conflict"
  | "unknown"
  | "indeterminate";

export interface RuntimeControlReceipt {
  operation_id: string;
  action: string;
  outcome: RuntimeControlOutcome;
  /** The action's own result once terminal. Never command output. */
  result?: unknown;
  /** Ready-to-display one-line status. */
  message: string;
}

/** The boundary a snapshot was taken at. */
export interface SnapshotMarkerInfo {
  /** Restart-scoped; a restart changes it. */
  generation: string;
  seq: number;
  /** The id the paired `snapshot` reply carries. */
  marker_id: string;
}

/** One consistent view of runtime state plus a bounded history tail. */
export interface SessionSnapshot {
  generation: string;
  seq: number;
  marker_id: string;
  state: RuntimeStateSummary;
  /** Oldest first, like `history`. */
  messages?: HistoryMessage[];
  next_history_cursor?: string;
  /** The tail or byte budget cut the history short. */
  truncated?: boolean;
}

/** Finer cause on an error frame. Old servers omit it; old clients ignore it. */
export interface StoreErrorDetail {
  store_code: string;
  retryable: boolean;
  accepted_ids?: string[];
  retry_cursor?: string;
  conflict_heads?: string[];
}

export type ApiRequest =
  | { req: "hello"; min_version: number; max_version: number; client: string; supports_user_response?: boolean; supports_ask_requests?: boolean }
  | { req: "list_sessions"; include_archived?: boolean; limit?: number }
  | { req: "archive_session"; session_id: string }
  | { req: "restore_session"; session_id: string }
  | { req: "set_retention_policy"; archive_after_days?: number }
  | { req: "create_session"; working_dir?: string; system_prompt?: string }
  | { req: "attach_session"; session_id: string }
  | { req: "fork_session"; session_id: string }
  | { req: "transfer_session"; session_id: string }
  | { req: "detach_session"; session_id: string }
  | { req: "configure_tools"; session_id: string; tools: ToolConfiguration }
  | { req: "list_tools"; session_id: string }
  | { req: "get_message_modes"; session_id: string }
  | { req: "tool_result"; session_id: string; call_id: string; output: string; error?: string }
  | {
      req: "send_message";
      session_id: string;
      content: string;
      images?: ImageAttachment[];
      no_reply?: boolean;
      system_reminder?: string;
    }
  | { req: "cancel"; session_id: string }
  | {
      req: "soft_interrupt";
      session_id: string;
      content: string;
      images?: ImageAttachment[];
      urgent?: boolean;
      /** Opaque id echoed in `soft_interrupt_injected` and accepted by a filtered cancel. */
      client_id?: string;
    }
  | { req: "get_history"; session_id: string }
  | { req: "peek_session"; session_id: string; limit?: number }
  | { req: "clear"; session_id: string }
  | { req: "rewind"; session_id: string; message_index: number; summarize?: boolean }
  | {
      req: "permission_response";
      session_id: string;
      request_id: string;
      decision: PermissionDecision;
    }
  /** Retires the daemon's durable copy of an `approval_resolved` event. */
  | { req: "approval_ack"; session_id: string; request_id: string }
  /**
   * Answers an `ask_requested` event. A reply for an `ask_id` that is no longer
   * pending is refused, so a late answer cannot resolve a newer question.
   */
  | {
      req: "ask_response";
      session_id: string;
      ask_id: string;
      answers: AskSelection[];
      /**
       * Retry-safe identity for this answer. Requires `ask_receipts_v1`; the
       * same id with the same answers is a replay, a different id for an
       * answered question is a conflict.
       */
      answer_id?: string;
    }
  /** Retires the daemon's durable copy of a `failover_switched` event. */
  | {
      req: "failover_ack";
      session_id: string;
      chain_id: string;
      from: string;
      to: string;
      attempt: number;
    }
  | { req: "list_models"; session_id: string }
  | { req: "get_runtime_info"; session_id: string }
  /** Token usage, context window, per-model aggregates and subscription limits. */
  | { req: "usage_report"; session_id?: string }
  | { req: "runtime_command"; session_id: string; command: RuntimeCommand; args?: string[] }
  | { req: "set_api_key"; provider: string; api_key: string }
  | { req: "notify_auth_changed"; provider: string }
  | { req: "invalidate_usage"; provider: string; account_label?: string }
  | { req: "clear_api_key"; provider: string }
  | { req: "read_file"; session_id: string; path: string; max_bytes?: number }
  | { req: "find_files"; session_id: string; query: string; limit?: number }
  | { req: "search_text"; session_id: string; query: string; path?: string; limit?: number }
  | { req: "file_status"; session_id: string; path: string }
  | { req: "search_sessions"; query: string; limit?: number }
  | { req: "list_artifacts"; session_id: string }
  | { req: "get_todo_state"; session_id: string }
  | { req: "ack_todo_follow_up"; session_id: string; effects: TodoEffects }
  | { req: "set_model"; session_id: string; model: string }
  | { req: "set_reasoning_effort"; session_id: string; effort: string }
  | { req: "compact"; session_id: string; method?: "handoff" }
  | { req: "rename_session"; session_id: string; title?: string }
  | { req: "set_session_saved"; session_id: string; saved: boolean; label?: string }
  | { req: "rewind_undo"; session_id: string }
  /** Without `client_ids`, clears the queue (replies `ok`). With them, replies `soft_interrupts_cancelled`. */
  | { req: "cancel_soft_interrupts"; session_id: string; client_ids?: string[] }
  /** Replies `background_tool_result` when the `background_tool_result` capability is advertised, else `ok`. */
  | { req: "background_tool"; session_id: string }
  | { req: "list_session_heads"; session_id: string }
  | { req: "select_session_head"; session_id: string; head_event_id: string }
  | {
      req: "get_history_page";
      session_id: string;
      head_event_id?: string;
      before_event_id?: string;
      limit?: number;
    }
  | {
      req: "export_session_events";
      session_id: string;
      protocol_version?: number;
      heads?: string[];
      known_ids?: string[];
      cursor?: string;
    }
  | { req: "export_session_markdown"; session_id: string }
  | {
      req: "import_session_events";
      session_id: string;
      protocol_version?: number;
      heads?: string[];
      events: CanonicalEventRecord[];
      next_cursor?: string;
      blob_refs?: PayloadRef[];
      record_refs?: CanonicalRecordRef[];
      retry_cursor?: string;
    }
  | {
      req: "export_session_blob";
      session_id: string;
      sha256: string;
      total_size: number;
      offset: number;
    }
  | { req: "import_session_blob"; session_id: string; chunk: SessionBlobChunk }
  | { req: "get_session_blob_status"; session_id: string; sha256: string; total_size: number }
  | {
      req: "export_session_record_blob";
      session_id: string;
      reference: CanonicalRecordRef;
      offset: number;
    }
  | {
      req: "import_session_record_blob";
      session_id: string;
      reference: CanonicalRecordRef;
      chunk: SessionBlobChunk;
    }
  | { req: "get_session_record_blob_status"; session_id: string; reference: CanonicalRecordRef }
  /** Requires `durable_runtime_v1`. Replies `runtime_state`. */
  | { req: "get_runtime_state"; session_id: string }
  /**
   * Apply one control operation under a caller-chosen id. Requires
   * `durable_runtime_v1`. Replies `control_receipt`.
   */
  | {
      req: "apply_control";
      session_id: string;
      operation_id: string;
      action: string;
      args?: unknown;
    }
  /** Requires `durable_runtime_v1`. Replies `control_receipt`. */
  | { req: "get_control_receipt"; session_id: string; operation_id: string }
  /**
   * Requires `snapshot_marker_v1`. Replies `snapshot`, preceded on the same
   * connection by the matching `snapshot_marker`.
   */
  | { req: "get_snapshot"; session_id: string; tail?: number; max_bytes?: number }
  | { req: "ping" };

/** What a `compactHandoff` request did. */
export type CompactHandoffOutcome =
  | { outcome: "committed"; covered: number }
  | { outcome: "nothing_to_cover" }
  | { outcome: "provider_error"; detail: string }
  | { outcome: "cancelled" };

export type ApiEvent =
  | { ev: "hello_ok"; version: number; server: string; capabilities?: string[] }
  | { ev: "ok" }
  | { ev: "runtime_command_result"; session_id: string; command: RuntimeCommand; data: unknown; message: string }
  | { ev: "error"; code: ErrorCode; message: string; details?: StoreErrorDetail }
  | { ev: "sessions"; sessions: SessionInfo[] }
  | { ev: "attached"; session: SessionInfo }
  | { ev: "session_forked"; session: SessionInfo }
  | { ev: "history"; session_id: string; messages: HistoryMessage[]; images?: RenderedImage[] }
  | { ev: "pong" }
  | { ev: "text_delta"; session_id: string; text: string; message_id?: string }
  | { ev: "text_done"; session_id: string; message_id?: string }
  | { ev: "text_replace"; session_id: string; message_id?: string; text: string }
  | { ev: "reasoning_delta"; session_id: string; text: string }
  | { ev: "reasoning_done"; session_id: string; duration_secs?: number }
  | { ev: "tool_start"; session_id: string; call_id: string; name: string }
  | { ev: "tool_input_delta"; session_id: string; call_id: string; delta: string }
  | { ev: "tool_exec"; session_id: string; call_id: string; name: string }
  | { ev: "tools"; session_id: string; tools: SessionToolDefinition[] }
  | { ev: "message_modes"; session_id: string; modes: MessageModes }
  | { ev: "tool_call"; session_id: string; call_id: string; name: string; input: unknown }
  | {
      ev: "tool_done";
      session_id: string;
      call_id: string;
      name: string;
      output: string;
      error?: string;
    }
  | { ev: "side_pane_images"; session_id: string; images: RenderedImage[] }
  | {
      ev: "degradation_receipts";
      session_id: string;
      message_id: string;
      receipts: DegradationReceipt[];
    }
  | {
      ev: "tool_args_repaired";
      session_id: string;
      call_id: string;
      name: string;
      original_input: unknown;
      changes: string[];
    }
  | {
      ev: "token_usage";
      session_id: string;
      input: number;
      output: number;
      cache_read_input?: number;
      cache_creation_input?: number;
    }
  | {
      ev: "kv_cache_miss";
      session_id: string;
      reason: string;
      harness_caused: boolean;
      missed_tokens: number;
      expected_tokens: number;
      read_tokens: number;
      documented_cause?: string;
      message: string;
    }
  | { ev: "turn_stopped"; session_id: string; reason: TurnStopReason; message: string; provider_stop_reason?: string }
  /** `pending_soft_interrupts`: queued messages this turn never saw (only when nonzero). */
  | { ev: "turn_done"; session_id: string; pending_soft_interrupts?: number }
  /**
   * Queued soft interrupts reached the model. Output after this event was written
   * having seen them. Several queued messages arrive as one user message, so
   * `client_ids` lists every id delivered, in queue order. `point` is B, C, D, or
   * `turn_start` (sent while idle, so it started a turn).
   */
  | {
      ev: "soft_interrupt_injected";
      session_id: string;
      client_ids: string[];
      point: string;
      tools_skipped?: number;
      display_role?: string;
    }
  /** Reply to a filtered `cancel_soft_interrupts`. Decided atomically against delivery. */
  | { ev: "soft_interrupts_cancelled"; cancelled: string[]; not_queued: string[] }
  /** Reply to `background_tool`. */
  | { ev: "background_tool_result"; moved: boolean; tool_call_id?: string; tool_name?: string }
  | {
      ev: "wake_requested";
      session_id: string;
      reason: string;
      notification: string;
    }
  | {
      ev: "background_progress";
      session_id: string;
      task_id: string;
      label: string;
      percent?: number;
      summary: string;
      done?: boolean;
    }
  | { ev: "message_accepted"; session_id: string }
  | {
      ev: "permission_request";
      session_id: string;
      request_id: string;
      tool_name: string;
      description: string;
    }
  /** An approval is parked. Also used for clientless runs, where the decision
   *  arrives out of band; ack with `approval_ack` once `approval_resolved` lands. */
  | {
      ev: "approval_pending";
      session_id: string;
      request_id: string;
      action: string;
      expires_at?: string;
    }
  | {
      ev: "approval_resolved";
      session_id: string;
      request_id: string;
      approved: boolean;
      via: string;
      resolved_by: string;
    }
  /**
   * The `ask` tool needs a person to settle a decision. Answer with
   * `ask_response` echoing `ask_id`; the tool resolves the questions itself on
   * its own deadline, so an unanswered question costs a bounded wait.
   */
  | {
      ev: "ask_requested";
      session_id: string;
      ask_id: string;
      questions: AskQuestion[];
    }
  /** `chain_id` scopes the attempt budget: a late ack from an older chain must not clear it. */
  | {
      ev: "failover_switched";
      session_id: string;
      chain_id: string;
      from: string;
      to: string;
      reason: string;
      attempt: number;
      max: number;
    }
  /** The run stopped and needs a human. `request_id` is null when the stop is
   *  not tied to a permission request (an exhausted failover budget, for
   *  example); do not try to resolve it as an approval id. */
  | {
      ev: "run_needs_human_resume";
      session_id: string;
      request_id: string | null;
      reason: string;
      attempts: number;
    }
  | { ev: "approval_ack"; request_id: string }
  | { ev: "failover_ack"; chain_id: string; from: string; to: string; attempt: number }
  /** Attachment recovery intent. Can precede attached. Never auto-sent by the bridge. */
  | { ev: "session_recovery"; session_id: string; continuation_message: string; reconnect_notice?: string }
  | { ev: "session_status"; session_id: string; status: string; pending_soft_interrupts?: number }
  | { ev: "connection_phase"; session_id: string; phase: string }
  | {
      ev: "model_info";
      session_id: string;
      provider?: string;
      model?: string;
      reasoning_effort?: string;
      auth_method?: string;
    }
  | { ev: "models"; session_id: string; models: string[]; current?: string }
  | {
      ev: "runtime_info";
      session_id: string;
      provider?: string;
      model?: string;
      reasoning_effort?: string;
      auth_method?: string;
      fallback_models?: string[];
      serving_model?: string;
      routes: ModelRouteInfo[];
    }
  | { ev: "credential_updated"; provider: string; configured: boolean }
  | {
      ev: "file_content";
      session_id: string;
      path: string;
      content: string;
      size: number;
      truncated: boolean;
    }
  | { ev: "files"; session_id: string; paths: string[] }
  | { ev: "text_matches"; session_id: string; matches: TextMatch[] }
  | {
      ev: "file_status";
      session_id: string;
      path: string;
      exists: boolean;
      kind: string;
      size?: number;
      modified_ms?: number;
    }
  | { ev: "todo_state"; session_id: string; state: TodoSnapshot }
  | { ev: "session_search_results"; query: string; results: SessionSearchHit[] }
  | { ev: "artifacts"; session_id: string; artifacts: ArtifactEntry[] }
  /**
   * Reply to `usage_report`. Every count is a durable per-request total or the
   * daemon's own live total; a metric no request recorded is `null`, never
   * zero. `pricing` is always `"unknown"` and `estimated_cost` always `null`:
   * this harness carries no price table.
   */
  | {
      ev: "usage_report";
      session_id?: string;
      sessions: UsageSession[];
      totals: UsageTotals;
      models: UsageModel[];
      context_window?: number;
      limits?: UsageLimitInfo[];
      pricing: string;
      estimated_cost: number | null;
    }
  | {
      ev: "compacted";
      session_id: string;
      message: string;
      handoff?: CompactHandoffOutcome;
    }
  | {
      ev: "session_renamed";
      session_id: string;
      title?: string;
      display_title: string;
    }
  | { ev: "session_heads"; session_id: string; heads: string[]; selected_head?: string }
  | { ev: "session_markdown"; session_id: string; markdown: string }
  | {
      ev: "session_history_page";
      session_id: string;
      head: string;
      messages: HistoryMessage[];
      next_cursor?: string;
    }
  | {
      ev: "session_event_page";
      session_id: string;
      heads: string[];
      events: CanonicalEventRecord[];
      next_cursor?: string;
      blob_refs?: PayloadRef[];
      record_refs?: CanonicalRecordRef[];
    }
  | {
      ev: "session_import_result";
      session_id: string;
      accepted_ids: string[];
      heads: string[];
      retry_cursor?: string;
      conflict_heads?: string[];
    }
  | {
      ev: "session_blob_chunk";
      session_id: string;
      sha256: string;
      offset: number;
      data_base64: string;
      next_offset: number;
      total_size: number;
      complete: boolean;
    }
  | { ev: "session_blob_status"; session_id: string; sha256: string; received: number; total_size: number; complete: boolean }
  | {
      ev: "session_record_blob_status";
      session_id: string;
      reference: CanonicalRecordRef;
      received: number;
      total_size: number;
      complete: boolean;
    }
  /** Reply to `get_runtime_state`. Requires `durable_runtime_v1`. */
  | { ev: "runtime_state"; session_id: string; state: RuntimeStateSummary }
  /** Reply to `apply_control` and `get_control_receipt`. */
  | { ev: "control_receipt"; session_id: string; receipt: RuntimeControlReceipt }
  /**
   * Sent immediately before the `snapshot` it bounds, so later live events are
   * provably newer than that snapshot. Requires `snapshot_marker_v1`.
   */
  | { ev: "snapshot_marker"; session_id: string; marker: SnapshotMarkerInfo }
  /** Reply to `get_snapshot`. Requires `snapshot_marker_v1`. */
  | { ev: "snapshot"; session_id: string; snapshot: SessionSnapshot };

/**
 * An event kind this SDK does not know about.
 *
 * The harness may add events at any time within protocol v1, so one can arrive
 * at runtime. It is deliberately *not* part of `ApiEvent`: a member with
 * `ev: string` widens the discriminant, and TypeScript then refuses to narrow
 * `event.ev === "text_delta"` to the text-delta member, leaving every field
 * typed `unknown`. Forward compatibility is a runtime property, and paying for
 * it with the type safety of the ninety-nine percent case is a bad trade.
 *
 * Handle these with a `default` branch, or filter with `isKnownEvent`.
 */
export interface UnknownApiEvent {
  ev: string;
  [key: string]: unknown;
}

/** Any frame off the wire, known or not. Narrow with `isKnownEvent`. */
export type AnyApiEvent = ApiEvent | UnknownApiEvent;

export type ApiEventKind = ApiEvent["ev"];

export interface ClientFrame {
  v: number;
  id: number;
  [key: string]: unknown;
}

export type ServerFrame = { v: number; reply_to?: number } & UnknownApiEvent;

/** Every event tag the SDK knows about, for drift checks and routing. */
export const KNOWN_EVENT_KINDS = [
  "runtime_command_result",
  "hello_ok",
  "ok",
  "error",
  "sessions",
  "attached",
  "session_forked",
  "history",
  "side_pane_images",
  "pong",
  "text_delta",
  "text_done",
  "text_replace",
  "reasoning_delta",
  "reasoning_done",
  "tool_start",
  "tools",
  "message_modes",
  "tool_call",
  "tool_input_delta",
  "tool_exec",
  "tool_done",
  "tool_args_repaired",
  "degradation_receipts",
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
  "session_markdown",
  "session_event_page",
  "session_import_result",
  "session_blob_chunk",
  "session_blob_status",
  "session_record_blob_status",
  "runtime_state",
  "control_receipt",
  "snapshot_marker",
  "snapshot",
] as const;

/** Every request tag the SDK can send. */
export const KNOWN_REQUEST_KINDS = [
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
  "get_message_modes",
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
  "export_session_markdown",
] as const;

export function isKnownEvent(frame: AnyApiEvent): frame is ApiEvent {
  return (KNOWN_EVENT_KINDS as readonly string[]).includes(frame.ev);
}

/** Natural completion has no stop reason. Transport loss is not proof of a crash. */
export type TurnStopReason = "interrupted" | "failure" | "crash" | "provider_guardrail" | "limit_reached" | "unknown";
