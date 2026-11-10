/**
 * Harness client: handshake, request/reply correlation, and event streaming.
 */

import net from "node:net";
import { EventEmitter } from "node:events";
import { NdjsonDecoder, encodeFrame } from "./framing.js";
import { apiSocketPath, transportEndpoint } from "./sockets.js";
import { launchInstance, type LaunchOptions, type LaunchedInstance } from "./launch.js";
import { HarnessError } from "./errors.js";
import { ToolCallbacks, prepareTools, type SessionToolsOptions, type ToolResult } from "./tools.js";
import {
  assertRetryCount,
  buildStructuredCorrectionPrompt,
  buildStructuredPrompt,
  compileStructuredSchema,
  StructuredOutputError,
  validateStructuredText,
  type StructuredOutputAttempt,
  type StructuredOutputSchema,
} from "./structured.js";
import {
  API_VERSION_MAJOR,
  ADVERTISED_CAPABILITIES,
  MAX_REPLICATION_WIRE_BYTES,
  REPLICATION_PROTOCOL_VERSION,
  type ApiEvent,
  type ApiRequest,
  type AskSelection,
  type CanonicalRecordRef,
  type HistoryMessage,
  type ImageAttachment,
  type MessageModes,
  type ModelRouteInfo,
  type PayloadRef,
  type PermissionDecision,
  type RenderedImage,
  type RuntimeCommand,
  type RuntimeControlReceipt,
  type RuntimeStateSummary,
  type ServerFrame,
  type SessionBlobChunk,
  type SessionBlobStatus,
  type SessionEventPage,
  type SessionHeads,
  type SessionHistoryPage,
  type SessionImportResult,
  type SessionInfo,
  type SessionRecordBlobStatus,
  type SessionSnapshot,
  type SessionToolDefinition,
  type TextMatch,
  type TodoEffects,
  type TodoSnapshot,
  type TurnStopReason,
} from "./protocol.js";

export interface RuntimeInfo {
  server: string;
  protocolVersion: number;
  capabilities: string[];
  healthy: true;
  sessionId: string;
  provider?: string;
  model?: string;
  providers: string[];
  routes: ModelRouteInfo[];
}

export interface FileContent {
  path: string;
  content: string;
  size: number;
  truncated: boolean;
}

export interface FileStatus {
  path: string;
  exists: boolean;
  kind: string;
  size?: number;
  modifiedMs?: number;
}

/**
 * Narrow a frame to the event carrying this tag.
 *
 * A frame's `ev` is only known to be a string, so the tag has to be checked
 * before the frame can be read as the event it claims to be.
 */
function isEventOfKind<K extends ApiEvent["ev"]>(
  frame: ServerFrame,
  kind: K,
): frame is ServerFrame & Extract<ApiEvent, { ev: K }> {
  return frame.ev === kind;
}

/**
 * Narrow a frame to an event without promising a kind this SDK knows.
 *
 * The harness may add event kinds within protocol v1, so a frame whose `ev`
 * this SDK has never heard of still reaches consumers; read a field only after
 * `ev` identifies the kind.
 */
function isEventFrame(frame: ServerFrame): frame is ServerFrame & ApiEvent {
  return typeof frame.ev === "string";
}

/**
 * Event kinds a slow consumer must not lose.
 *
 * Approvals, asks, cancellations, turn terminals and tool outcomes are the
 * frames a caller acts on: a dropped `turn_done` hangs a turn and a dropped
 * `permission_request` hangs a tool. Narration and deltas are reproducible from
 * the transcript, so they are the only frames the ordinary budget may refuse.
 */
const CONTROL_EVENT_KINDS: ReadonlySet<string> = new Set([
  "error",
  "turn_done",
  "turn_stopped",
  "message_accepted",
  "permission_request",
  "approval_pending",
  "approval_resolved",
  "approval_ack",
  "ask_requested",
  "run_needs_human_resume",
  "soft_interrupt_injected",
  "soft_interrupts_cancelled",
  "tool_done",
  "background_tool_result",
]);

function isControlEvent(frame: ServerFrame): boolean {
  return typeof frame.ev === "string" && CONTROL_EVENT_KINDS.has(frame.ev);
}

/**
 * Estimate the serialized size of one frame in bytes, stopping once `ceiling`
 * is exceeded.
 *
 * Called once per queued frame, never on the way out: re-stringifying a large
 * payload on every `next()` would cost more than the buffer saves. UTF-8 is at
 * most three bytes per UTF-16 code unit (four for a surrogate pair, which is
 * two units), so this estimate never understates the wire size.
 */
function estimateEventBytes(value: unknown, ceiling: number): number {
  let total = 0;
  const stack: unknown[] = [value];
  while (stack.length > 0 && total <= ceiling) {
    const item = stack.pop();
    switch (typeof item) {
      case "string":
        total += item.length * 3;
        break;
      case "number":
        total += 8;
        break;
      case "boolean":
        total += 5;
        break;
      case "object": {
        if (item === null) {
          total += 4;
          break;
        }
        total += 2;
        if (Array.isArray(item)) {
          for (const entry of item) stack.push(entry);
          break;
        }
        for (const [key, entry] of Object.entries(item)) {
          total += key.length + 4;
          stack.push(entry);
        }
        break;
      }
      default:
        total += 4;
    }
  }
  return total;
}

/** Minimal duplex transport so tests and future WebSockets can plug in. */
export interface Transport {
  /**
   * Write one frame.
   *
   * Return `false` when the transport's buffer is full: the client then holds
   * further frames and resumes on `onDrain` instead of piling more bytes onto a
   * peer that is not keeping up. Returning nothing, or `true`, means the frame
   * was accepted. Existing transports that return nothing keep working: the
   * client treats them as always accepting.
   */
  write(data: string): void | boolean;
  onData(listener: (chunk: Buffer | string) => void): void;
  onClose(listener: (error?: Error) => void): void;
  close(): void;
  /**
   * Optional: call `listener` when the transport can accept writes again.
   *
   * Only consulted after `write` returned `false`. A transport that reports
   * backpressure without this signal is refused rather than written to in
   * violation of its own contract.
   */
  onDrain?(listener: () => void): void;
}

export function unixSocketTransport(socketPath: string): Promise<Transport> {
  return new Promise((resolve, reject) => {
    // On Windows the bridge listens on a named pipe derived from this path,
    // not on the path itself.
    const socket = net.createConnection(transportEndpoint(socketPath));
    socket.setNoDelay(true);
    // A bare `connect ENOENT /run/user/1000/horon-agent-api.sock` names the syscall
    // and hides the actual cause: the bridge is not running. The first thing
    // anyone does with this SDK is connect, so this is the error most likely
    // to be someone's first impression of it. Say what to do about it.
    const onConnectError = (cause: NodeJS.ErrnoException) => reject(connectError(socketPath, cause));
    socket.once("error", onConnectError);
    socket.once("connect", () => {
      socket.removeListener("error", onConnectError);
      // Keep a listener attached for the connection's lifetime: an unhandled
      // socket "error" (an EPIPE when the harness goes away mid-write, say)
      // is a fatal throw in Node, and callers should see a rejected request
      // instead of a dead process.
      socket.on("error", () => {});
      resolve({
        write: (data) => {
          if (socket.destroyed || socket.writableEnded) {
            throw new HarnessError("disconnected", "harness connection closed");
          }
          // A peer that vanishes between the destroyed check and the syscall
          // surfaces EPIPE, sometimes synchronously. Translate it into a
          // rejected request rather than an uncaught exception; the close
          // handler already fails everything else in flight.
          try {
            return socket.write(data, () => {});
          } catch (cause) {
            throw new HarnessError("disconnected", `write failed: ${String(cause)}`);
          }
        },
        onDrain: (listener) => socket.on("drain", listener),
        onData: (listener) => socket.on("data", listener),
        onClose: (listener) => {
          socket.on("close", () => listener());
          socket.on("error", (error) => listener(error));
        },
        close: () => socket.destroy(),
      });
    });
  });
}

/** Turn a dial failure into an error that names the likely cause and fix. */
function connectError(socketPath: string, cause: NodeJS.ErrnoException): HarnessError {
  const code = cause.code ?? "unknown";
  const hint =
    code === "ENOENT"
      ? `no harness API socket at ${socketPath}. Start the bridge with ` +
        "`horon-agent api-bridge`, or set HORON_AGENT_API_SOCKET to its path."
      : code === "ECONNREFUSED"
        ? `nothing is listening on ${socketPath}; a stale socket file is left over ` +
          "from a bridge that exited. Restart the bridge."
        : code === "EACCES"
          ? `permission denied on ${socketPath}: the socket belongs to another user.`
          : `could not connect to ${socketPath}: ${cause.message}`;
  const error = new HarnessError("connect_failed", hint);
  error.cause = cause;
  return error;
}

export interface ConnectOptions {
  /** Defaults to the resolved harness API socket path. */
  socketPath?: string;
  /** Client identity sent in the handshake, e.g. "my-app/1.0". */
  clientName?: string;
  /** Enable only when this client renders user_response tool calls inline. */
  supportsUserResponse?: boolean;
  /**
   * Enable only when this client renders `ask_requested` questions and answers
   * them with `respondToAsk`. Without it the server resolves every question
   * itself and never waits for this client.
   */
  supportsAskRequests?: boolean;
  /**
   * Maximum requests awaiting a reply at once. Defaults to 1024.
   *
   * Each outstanding request holds a pending entry until its reply or timeout,
   * so an unbounded request stream is unbounded memory. Past this many, a
   * request rejects immediately with `too_many_requests` rather than growing
   * the map: the caller learns the connection is saturated instead of watching
   * the process grow.
   */
  maxPendingRequests?: number;
  /** Supply a custom transport instead of dialing a Unix socket. */
  transport?: Transport;
  /** Milliseconds before a request without a reply rejects. 0 disables. */
  requestTimeoutMs?: number;
}

export interface CreateSessionOptions {
  workingDir?: string;
  /**
   * Replace the entire assembled system prompt, not just its base text.
   * Default instructions and assembled instruction/context additions are not appended.
   * Immutable after creation and persisted by the runtime for resume.
   * Omit for normal prompt assembly. An empty string explicitly overrides with no text.
   */
  systemPrompt?: string;
  /** Configured before createSession resolves, before any model turn starts. */
  tools?: SessionToolsOptions;
}

export interface EventsOptions {
  /**
   * Maximum ordinary events waiting for the consumer. Defaults to 10000.
   *
   * Control frames (approvals, asks, cancellations, turn terminals, tool
   * outcomes) get an extra quarter of this budget, so a delta flood cannot
   * evict the frame that ends a turn.
   */
  maxBufferedEvents?: number;
  /**
   * Maximum estimated bytes of ordinary events waiting for the consumer.
   * Defaults to 8 MiB, with a quarter of that reserved for control frames.
   */
  maxBufferedBytes?: number;
}

/** Events and estimated bytes a stream is holding for a slow consumer. */
export interface EventBufferStats {
  events: number;
  bytes: number;
}

/**
 * The iterator `events()` returns.
 *
 * `buffered` exposes the current queue size, so a consumer can see how far
 * behind it is instead of discovering the overflow only when it fails.
 */
export type EventStream = AsyncIterableIterator<ApiEvent> & { readonly buffered: EventBufferStats };

export interface GlobalEventsOptions {
  /** Stop discovery and close every per-session child connection. */
  signal?: AbortSignal;
  /** Milliseconds between checks for newly created sessions. Defaults to 1000. */
  discoveryIntervalMs?: number;
  /** Maximum events waiting for the consumer before the iterator fails loudly. Defaults to 10000. */
  maxBufferedEvents?: number;
}

export interface RunOptions {
  images?: ImageAttachment[];
  onEvent?: (event: ApiEvent) => void;
  /**
   * Auto-answer permission prompts.
   *
   * Only meaningful when the server advertises the `permissions` capability;
   * otherwise no prompt is ever issued and this is a no-op.
   */
  autoApprove?: boolean;
}

export interface SendMessageOptions {
  /** Attach images to the persisted user message. */
  images?: ImageAttachment[];
  /** Persist the message as context without starting a model turn. */
  noReply?: boolean;
  /** Wait for message_accepted when starting a normal model turn. */
  waitForAccept?: boolean;
  /** Maximum acknowledgement wait in milliseconds. */
  acceptTimeoutMs?: number;
}

export interface RunStructuredOptions<T = unknown> extends RunOptions {
  /** JSON Schema the assistant's JSON response must satisfy. */
  schema: StructuredOutputSchema<T>;
  /** Corrective retries after the first invalid response. Defaults to 2. */
  maxRetries?: number;
}

interface Pending {
  resolve: (frame: ServerFrame) => void;
  reject: (error: Error) => void;
  timer?: NodeJS.Timeout;
  onReply?: (frame: ServerFrame) => void;
}

/**
 * Connected harness client.
 *
 * Replies are correlated by the `reply_to` id the server echoes; anything
 * without one is a stream event and is emitted on `event` plus a per-kind
 * channel (`client.on("text_delta", ...)`).
 */
export class HoronAgentClient extends EventEmitter {
  private readonly transport: Transport;
  private readonly decoder = new NdjsonDecoder<ServerFrame>();
  private readonly pending = new Map<number, Pending>();
  private readonly maxPendingRequests: number;
  /** Frames accepted by the client but not yet handed to the transport. */
  private readonly outbox: string[] = [];
  /** Set while the transport has said it cannot take more bytes. */
  private transportBlocked = false;
  private readonly requestTimeoutMs: number;
  private readonly socketPath?: string;
  private readonly clientName: string;
  private nextId = 1;
  private closed = false;
  private closeError?: Error;
  private readonly configuringTools = new Set<string>();
  private readonly toolCallbacks = new ToolCallbacks(
    (sessionId, callId, result) => this.submitToolResult(sessionId, callId, result),
    (error) => this.emitSafe("error", error),
  );

  /** Server identity from the handshake, e.g. "horon-agent-harness-api-bridge/0.1.0". */
  server = "";
  /** Capability strings advertised by the server. */
  capabilities: string[] = [];

  /**
   * Whether the server advertises a capability.
   *
   * Capabilities are how a client learns what this particular server can do
   * before depending on it. `permissions` and `asks`, for instance, are how a
   * client knows the server can deliver an approval card or an agent question;
   * a server without them never sends either, so waiting for one waits forever.
   * Check rather than assume.
   */
  supports(capability: string): boolean {
    return this.capabilities.includes(capability);
  }

  private constructor(
    transport: Transport,
    requestTimeoutMs: number,
    socketPath: string | undefined,
    clientName: string,
    maxPendingRequests: number,
  ) {
    super();
    this.setMaxListeners(0);
    this.transport = transport;
    this.requestTimeoutMs = requestTimeoutMs;
    this.socketPath = socketPath;
    this.clientName = clientName;
    this.maxPendingRequests = maxPendingRequests;
    transport.onData((chunk) => this.ingest(chunk));
    transport.onClose((error) => this.handleClose(error));
    transport.onDrain?.(() => {
      this.transportBlocked = false;
      try {
        this.flushOutbox();
      } catch (error) {
        this.emitSafe("error", error);
      }
    });
  }

  /** Set when this client owns a private instance, so `close()` stops it. */
  private instance?: LaunchedInstance;

  /** State directory of the instance this client launched, if any. */
  get instanceHome(): string | undefined {
    return this.instance?.horonAgentHome;
  }

  /**
   * Start a private horon-agent instance and connect to it.
   *
   * This is the entry point for embedding horon-agent as an agent engine. The
   * instance has its own state directory, its own sessions, and its own
   * sockets, so it cannot see or disturb the horon-agent the user runs
   * interactively. `close()` shuts it down.
   *
   * Provider logins are inherited from the user by default, because an
   * instance without credentials cannot reach a model at all. Pass
   * `inheritLogins: false` to start empty.
   */
  static async launch(options: LaunchOptions & ConnectOptions = {}): Promise<HoronAgentClient> {
    const instance = await launchInstance(options);
    try {
      const client = await HoronAgentClient.connect({
        ...options,
        socketPath: instance.socketPath,
      });
      client.instance = instance;
      return client;
    } catch (error) {
      // A launched instance whose client never connected is a stray daemon
      // holding a temp directory open, so tear it down before rethrowing.
      await instance.shutdown();
      throw error;
    }
  }

  /**
   * Connect to the horon-agent already running on this machine.
   *
   * Use this to automate the user's own horon-agent: an editor plugin, a status
   * dashboard, a hotkey tool. It shares the user's live sessions, so anything
   * done here is visible in their terminal. To embed horon-agent as an engine
   * instead, use {@link HoronAgentClient.launch}.
   */
  static async connect(options: ConnectOptions = {}): Promise<HoronAgentClient> {
    const socketPath = options.transport ? undefined : (options.socketPath ?? apiSocketPath());
    const transport = options.transport ?? (await unixSocketTransport(socketPath!));
    const clientName = options.clientName ?? "horon-agent-sdk-ts";
    const client = new HoronAgentClient(
      transport,
      options.requestTimeoutMs ?? 30_000,
      socketPath,
      clientName,
      options.maxPendingRequests ?? 1024,
    );
    const frame = await client.request({
      req: "hello",
      min_version: API_VERSION_MAJOR,
      max_version: API_VERSION_MAJOR,
      client: clientName,
      supports_user_response: options.supportsUserResponse ?? false,
      supports_ask_requests: options.supportsAskRequests ?? false,
    });
    if (!isEventOfKind(frame, "hello_ok")) {
      void client.close();
      throw new HarnessError("handshake_failed", `unexpected reply: ${frame.ev}`);
    }
    // The server string arrives off the wire, so stringify whatever it holds
    // instead of trusting the declared type.
    const server: unknown = frame.server ?? "";
    client.server = typeof server === "string" ? server : String(server);
    client.capabilities = frame.capabilities ?? [];
    return client;
  }

  private ingest(chunk: Buffer | string): void {
    let frames: ServerFrame[];
    try {
      frames = this.decoder.push(chunk);
    } catch (error) {
      this.emitSafe("error", error);
      return;
    }
    for (const frame of frames) {
      const replyTo = frame.reply_to;
      if (typeof replyTo === "number" && this.pending.has(replyTo)) {
        const waiter = this.pending.get(replyTo)!;
        this.pending.delete(replyTo);
        if (waiter.timer) clearTimeout(waiter.timer);
        if (frame.ev === "attached" || frame.ev === "session_forked") {
          // One attachment per connection. Old-session callbacks must not survive a switch.
          this.toolCallbacks.close();
        }
        waiter.onReply?.(frame);
        waiter.resolve(frame);
        continue;
      }
      if (this.dropsUnnegotiated(frame)) continue;
      if (
        frame.ev === "tool_call" &&
        typeof frame.session_id === "string" &&
        typeof frame.call_id === "string" &&
        typeof frame.name === "string"
      ) {
        this.toolCallbacks.dispatch(frame.session_id, frame.call_id, frame.name, frame.input);
      } else if (frame.ev === "turn_done" && typeof frame.session_id === "string") {
        this.toolCallbacks.abort(frame.session_id);
      }
      this.emit("event", frame);
      // Unknown kinds still land on `event` so a client can log them, but
      // per-kind listeners only ever see the tags they asked for.
      //
      // `error` is remapped: Node's EventEmitter treats a bare "error" event
      // with no listener as a fatal throw, so an unsolicited harness error
      // frame would crash the host process instead of being reported. Protocol
      // errors are delivered on "harness_error"; the plain "error" channel is
      // reserved for transport faults and is emitted defensively.
      this.emitSafe(frame.ev === "error" ? "harness_error" : frame.ev, frame);
    }
  }

  /**
   * Whether this frame belongs to a capability the connection never
   * negotiated.
   *
   * The server already filters, so this only matters for a server that ignored
   * the negotiation: the client then keeps the same guarantee, and a subscriber
   * never receives an event it has no branch for.
   */
  private dropsUnnegotiated(frame: ServerFrame): boolean {
    if (frame.ev !== "snapshot_marker" && frame.ev !== "snapshot") return false;
    return !this.supports("snapshot_marker_v1");
  }

  /** Emit without Node's fatal-throw behaviour for unlistened "error". */
  private emitSafe(event: string, payload: unknown): void {
    if (event === "error" && this.listenerCount("error") === 0) return;
    this.emit(event, payload);
  }

  private handleClose(error?: Error): void {
    this.closed = true;
    this.toolCallbacks.close();
    this.closeError = error ?? new Error("harness connection closed");
    for (const [, waiter] of this.pending) {
      if (waiter.timer) clearTimeout(waiter.timer);
      waiter.reject(this.closeError);
    }
    this.pending.clear();
    this.emit("close", error);
  }

  /** Send a raw request and await its reply frame. */
  request(request: ApiRequest): Promise<ServerFrame> {
    return this.requestFrame(request);
  }

  private requestFrame(request: ApiRequest, onReply?: (frame: ServerFrame) => void): Promise<ServerFrame> {
    if (this.closed) return Promise.reject(this.closeError ?? new Error("client closed"));
    if (this.pending.size >= this.maxPendingRequests) {
      return Promise.reject(
        new HarnessError(
          "too_many_requests",
          `${this.pending.size} requests are already awaiting a reply ` +
            `(maxPendingRequests ${this.maxPendingRequests}); await or cancel some before sending ${request.req}`,
        ),
      );
    }
    const id = this.nextId++;
    return new Promise<ServerFrame>((resolve, reject) => {
      const entry: Pending = { resolve, reject, onReply };
      if (this.requestTimeoutMs > 0) {
        entry.timer = setTimeout(() => {
          this.pending.delete(id);
          reject(new HarnessError("timeout", `no reply to ${request.req} within ${this.requestTimeoutMs}ms`));
        }, this.requestTimeoutMs);
        entry.timer.unref?.();
      }
      this.pending.set(id, entry);
      try {
        this.enqueue(encodeFrame({ v: API_VERSION_MAJOR, id, ...request }));
      } catch (error) {
        this.pending.delete(id);
        if (entry.timer) clearTimeout(entry.timer);
        reject(error);
      }
    });
  }

  /**
   * Hand a frame to the transport, honouring its backpressure.
   *
   * A transport that returns `false` is telling the caller to stop writing until
   * `drain`; ignoring that is how a slow peer turns into unbounded memory in the
   * process. Frames queue in order and leave in order, so deferring one never
   * reorders the wire.
   */
  private enqueue(frame: string): void {
    if (this.closed) throw this.closeError ?? new Error("client closed");
    this.outbox.push(frame);
    this.flushOutbox();
  }

  private flushOutbox(): void {
    while (this.outbox.length > 0) {
      if (this.transportBlocked) {
        if (!this.transport.onDrain) {
          this.outbox.length = 0;
          throw new HarnessError(
            "transport_backpressure",
            "transport reported a full write buffer but provides no onDrain signal",
          );
        }
        return;
      }
      const frame = this.outbox.shift()!;
      if (this.transport.write(frame) === false) this.transportBlocked = true;
    }
  }

  /** Send a request, rejecting when the server replies with an error frame. */
  private async requestOk(request: ApiRequest): Promise<ServerFrame> {
    const frame = await this.request(request);
    if (frame.ev === "error") {
      throw new HarnessError(String(frame.code), String(frame.message));
    }
    return frame;
  }

  /**
   * Send a request and assert the reply's event kind, narrowing its type.
   *
   * Without this the typed accessors below would need casts at every call
   * site, which is exactly where a wrong field name slips through.
   */
  private async expectReply<K extends ApiEvent["ev"]>(
    request: ApiRequest,
    kind: K,
  ): Promise<Extract<ApiEvent, { ev: K }>> {
    const frame = await this.requestOk(request);
    if (!isEventOfKind(frame, kind)) {
      throw new HarnessError("unexpected_reply", `expected ${kind}, got ${frame.ev}`);
    }
    return frame;
  }

  // --- Curated surface -----------------------------------------------------

  async listSessions(options: { includeArchived?: boolean; limit?: number } = {}): Promise<SessionInfo[]> {
    const frame = await this.expectReply(
      { req: "list_sessions", include_archived: options.includeArchived, limit: options.limit },
      "sessions",
    );
    return frame.sessions ?? [];
  }

  async listSessionsLimited(limit: number): Promise<SessionInfo[]> {
    return this.listSessions({ limit });
  }

  /** Reversibly hide a session from the default list. No transcript is deleted. */
  async archiveSession(sessionId: string): Promise<void> {
    await this.requestOk({ req: "archive_session", session_id: sessionId });
  }

  async restoreSession(sessionId: string): Promise<void> {
    await this.requestOk({ req: "restore_session", session_id: sessionId });
  }

  /** Auto-archive inactive sessions after this many days; omit to disable. */
  async setRetentionPolicy(archiveAfterDays?: number): Promise<void> {
    await this.requestOk({ req: "set_retention_policy", archive_after_days: archiveAfterDays });
  }

  async createSession(options?: string | CreateSessionOptions): Promise<SessionInfo> {
    const { workingDir, systemPrompt, tools } = typeof options === "string" ? { workingDir: options } : (options ?? {});
    if (tools) {
      this.requireSessionTools();
      prepareTools(tools);
    }
    const frame = await this.expectReply(
      { req: "create_session", working_dir: workingDir, system_prompt: systemPrompt },
      "attached",
    );
    if (tools) await this.configureTools(frame.session.session_id, tools);
    return frame.session;
  }

  private requireSessionTools(): void {
    if (!this.supports("session_tools")) {
      throw new HarnessError(
        "unsupported",
        "This harness does not support session_tools. Update the horon-agent runtime and API bridge.",
      );
    }
  }

  /** Replace the session tool policy while idle. Await before sending another message. */
  async configureTools(sessionId: string, options: SessionToolsOptions): Promise<void> {
    this.requireSessionTools();
    if (this.configuringTools.has(sessionId)) {
      throw new HarnessError("busy", "A tool configuration request is already in flight for this session");
    }
    const prepared = prepareTools(options);
    this.configuringTools.add(sessionId);
    try {
      const frame = await this.requestFrame(
        { req: "configure_tools", session_id: sessionId, tools: prepared.wire },
        (reply) => {
          // Synchronous with ingest: a callback in the same network chunk sees the new table.
          // Until this ack, an existing turn must continue using the old table.
          if (reply.ev === "ok") this.toolCallbacks.sessions.set(sessionId, prepared.handlers);
        },
      );
      if (frame.ev === "error") throw new HarnessError(String(frame.code), String(frame.message));
      if (frame.ev !== "ok") throw new HarnessError("unexpected_reply", `expected ok, got ${frame.ev}`);
    } catch (error) {
      // An ambiguous outcome must not pair new remote definitions with old callbacks.
      if (error instanceof HarnessError && (error.code === "timeout" || error.code === "unexpected_reply")) {
        void this.close();
      }
      throw error;
    } finally {
      this.configuringTools.delete(sessionId);
    }
  }

  /** Inspect the effective model-visible tool definitions. */
  async listTools(sessionId: string): Promise<SessionToolDefinition[]> {
    this.requireSessionTools();
    return (await this.expectReply({ req: "list_tools", session_id: sessionId }, "tools")).tools;
  }

  /**
   * Read the daemon's `[messages]` delivery modes.
   *
   * These are daemon configuration rather than session state, so the answer does
   * not depend on the session existing.
   */
  async getMessageModes(sessionId: string): Promise<MessageModes> {
    return (await this.expectReply({ req: "get_message_modes", session_id: sessionId }, "message_modes")).modes;
  }

  /** Low-level response API for applications handling tool_call events themselves. */
  async submitToolResult(sessionId: string, callId: string, result: string | ToolResult): Promise<void> {
    const value = typeof result === "string" ? { output: result } : result;
    await this.expectReply(
      { req: "tool_result", session_id: sessionId, call_id: callId, output: value.output, error: value.error },
      "ok",
    );
  }

  async attachSession(sessionId: string): Promise<SessionInfo> {
    const frame = await this.expectReply({ req: "attach_session", session_id: sessionId }, "attached");
    return frame.session;
  }

  /** Clone a session's complete persisted context into a new idle session. */
  async forkSession(sessionId: string): Promise<SessionInfo> {
    const frame = await this.expectReply({ req: "fork_session", session_id: sessionId }, "session_forked");
    return frame.session;
  }

  async detachSession(sessionId: string): Promise<void> {
    await this.requestOk({ req: "detach_session", session_id: sessionId });
    this.toolCallbacks.abort(sessionId);
    this.toolCallbacks.sessions.delete(sessionId);
  }

  /**
   * Send a user message.
   *
   * The harness does not reply to `send_message` at the request level: it
   * acknowledges by emitting `message_accepted` once the agent has the
   * message. Awaiting a reply here would always time out, so the frame is
   * written and, by default, the acknowledgement event is awaited instead.
   * Pass `waitForAccept: false` for pure fire-and-forget.
   */
  async sendMessage(sessionId: string, content: string, options?: SendMessageOptions): Promise<void>;
  async sendMessage(
    sessionId: string,
    content: string,
    images?: ImageAttachment[],
    options?: Omit<SendMessageOptions, "images">,
  ): Promise<void>;
  async sendMessage(
    sessionId: string,
    content: string,
    imagesOrOptions: ImageAttachment[] | SendMessageOptions = [],
    legacyOptions: Omit<SendMessageOptions, "images"> = {},
  ): Promise<void> {
    const options: SendMessageOptions = Array.isArray(imagesOrOptions)
      ? { ...legacyOptions, images: imagesOrOptions }
      : imagesOrOptions;
    const images = options.images ?? [];
    if (options.noReply) {
      await this.requestOk({
        req: "send_message",
        session_id: sessionId,
        content,
        images,
        no_reply: true,
      });
      return;
    }
    const waitForAccept = options.waitForAccept ?? true;
    const accepted = waitForAccept
      ? this.waitForEvent(
          (frame) => frame.ev === "message_accepted" && frame.session_id === sessionId,
          options.acceptTimeoutMs ?? 10_000,
        )
      : undefined;
    this.notify({ req: "send_message", session_id: sessionId, content, images });
    await accepted;
  }

  /** Submit a hidden recovery continuation without awaiting message acceptance. */
  async sendSystemReminder(sessionId: string, reminder: string): Promise<void> {
    this.notify({ req: "send_message", session_id: sessionId, content: "", system_reminder: reminder });
  }

  /** Persist a user message as context without starting a model turn. */
  async sendContextMessage(sessionId: string, content: string): Promise<void> {
    await this.requestOk({ req: "send_message", session_id: sessionId, content, no_reply: true });
  }

  /**
   * Send a user message with a hidden system reminder for this turn only,
   * such as a collection's instructions. The reminder guides the model
   * without appearing in the visible transcript.
   */
  async sendMessageWithReminder(
    sessionId: string,
    content: string,
    images: ImageAttachment[] = [],
    systemReminder?: string,
    options: Pick<SendMessageOptions, "waitForAccept" | "acceptTimeoutMs"> = {},
  ): Promise<void> {
    const reminder = systemReminder?.trim();
    const waitForAccept = options.waitForAccept ?? true;
    const accepted = waitForAccept
      ? this.waitForEvent(
          (frame) => frame.ev === "message_accepted" && frame.session_id === sessionId,
          options.acceptTimeoutMs ?? 10_000,
        )
      : undefined;
    this.notify({
      req: "send_message",
      session_id: sessionId,
      content,
      images,
      ...(reminder ? { system_reminder: reminder } : {}),
    });
    await accepted;
  }

  /** Write a request without expecting a request-level reply. */
  notify(request: ApiRequest): void {
    if (this.closed) throw this.closeError ?? new Error("client closed");
    const id = this.nextId++;
    this.enqueue(encodeFrame({ v: API_VERSION_MAJOR, id, ...request }));
  }

  /**
   * Take a snapshot and deliver the events that follow its marker, without
   * losing the ones that raced the reply.
   *
   * A live event can reach this connection before the `snapshot` reply does, so
   * a consumer that subscribes only after awaiting `getSnapshot` would drop it.
   * This buffers events while the request is in flight, then releases them in
   * order once the marker arrives. The buffer is capped: a server that never
   * sends the marker would otherwise grow it without bound, so past the ceiling
   * this throws an explicit reconnect error instead of holding a partial view.
   *
   * Requires `snapshot_marker_v1`.
   */
  async snapshotWithBufferedEvents(
    sessionId: string,
    options: {
      tail?: number;
      maxBytes?: number;
      /** Ceiling for buffered events. Defaults to 1 MiB. */
      maxBufferedBytes?: number;
    } = {},
  ): Promise<{ snapshot: SessionSnapshot; events: ServerFrame[] }> {
    this.requireCapability("snapshot_marker_v1", "snapshotWithBufferedEvents");
    const ceiling = options.maxBufferedBytes ?? 1024 * 1024;
    const buffered: ServerFrame[] = [];
    let bufferedBytes = 0;
    let markerSeen = false;
    let overflow: Error | undefined;

    let done = false;
    const onEvent = (frame: ServerFrame) => {
      if (done) return;
      // Only the bytes BEFORE the marker can overflow: after it the view is
      // bounded by the server, so counting them would make a busy session look
      // like a missing marker.
      if (!markerSeen) {
        bufferedBytes += JSON.stringify(frame).length;
        if (bufferedBytes > ceiling) {
          overflow = new Error(
            `snapshot marker never arrived within ${ceiling} buffered bytes; reconnect before trusting this view`,
          );
          return;
        }
      }
      buffered.push(frame);
      if (frame.ev === "snapshot_marker") markerSeen = true;
    };

    this.on("event", onEvent);
    try {
      const snapshot = await this.getSnapshot(sessionId, options);
      done = true;
      if (overflow) throw overflow;
      // Everything buffered, in arrival order: the marker, then the live events
      // that raced the reply, so a consumer can prove the boundary itself.
      return { snapshot, events: buffered };
    } finally {
      done = true;
      this.off("event", onEvent);
    }
  }

  /** Resolve on the first event matching `predicate`, or on timeout. */
  private waitForEvent(predicate: (frame: ServerFrame) => boolean, timeoutMs: number): Promise<void> {
    return new Promise<void>((resolve) => {
      const finish = () => {
        clearTimeout(timer);
        this.off("event", onEvent);
        this.off("close", finish);
        resolve();
      };
      const onEvent = (frame: ServerFrame) => {
        if (predicate(frame)) finish();
      };
      // Resolving on timeout rather than rejecting keeps a missing ack from
      // failing an otherwise healthy turn: the stream is the source of truth.
      const timer = setTimeout(finish, timeoutMs);
      timer.unref?.();
      this.on("event", onEvent);
      this.once("close", finish);
    });
  }

  async cancel(sessionId: string): Promise<void> {
    await this.requestOk({ req: "cancel", session_id: sessionId });
    this.toolCallbacks.abort(sessionId);
  }

  async softInterrupt(sessionId: string, content: string, urgent = false): Promise<void> {
    await this.softInterruptWithImages(sessionId, content, [], urgent);
  }

  async softInterruptWithImages(
    sessionId: string,
    content: string,
    images: ImageAttachment[],
    urgent = false,
  ): Promise<void> {
    await this.softInterruptWithId(sessionId, content, { images, urgent });
  }

  /**
   * Queue a message for the running turn, tagged with an opaque `clientId`.
   *
   * The id comes back in a `soft_interrupt_injected` event once the model has
   * the message, and {@link cancelSoftInterrupt} can take it back before then.
   * Needs the `soft_interrupt_ids` capability; an older server accepts the
   * message but drops the id.
   */
  async softInterruptWithId(
    sessionId: string,
    content: string,
    options: { clientId?: string; images?: ImageAttachment[]; urgent?: boolean } = {},
  ): Promise<void> {
    const images = options.images ?? [];
    await this.requestOk({
      req: "soft_interrupt",
      session_id: sessionId,
      content,
      images: images.length > 0 ? images : undefined,
      urgent: options.urgent ?? false,
      client_id: options.clientId,
    });
  }

  async getHistory(sessionId: string): Promise<HistoryMessage[]> {
    const { messages } = await this.getHistoryWithImages(sessionId);
    return messages;
  }

  async getHistoryWithImages(sessionId: string): Promise<{ messages: HistoryMessage[]; images: RenderedImage[] }> {
    const frame = await this.expectReply({ req: "get_history", session_id: sessionId }, "history");
    return { messages: frame.messages ?? [], images: frame.images ?? [] };
  }

  async peekSession(sessionId: string, limit?: number): Promise<HistoryMessage[]> {
    const frame = await this.expectReply({ req: "peek_session", session_id: sessionId, limit }, "history");
    return frame.messages ?? [];
  }

  async clear(sessionId: string): Promise<void> {
    await this.requestOk({ req: "clear", session_id: sessionId });
  }

  async rewind(sessionId: string, messageIndex: number): Promise<void> {
    await this.requestOk({ req: "rewind", session_id: sessionId, message_index: messageIndex });
  }

  async respondToPermission(sessionId: string, requestId: string, decision: PermissionDecision): Promise<void> {
    await this.requestOk({
      req: "permission_response",
      session_id: sessionId,
      request_id: requestId,
      decision,
    });
  }

  /**
   * Answer an `ask_requested` event.
   *
   * `askId` echoes the event. A reply for a question set that is no longer
   * pending rejects, so a late answer cannot resolve a newer question.
   */
  async respondToAsk(sessionId: string, askId: string, answers: AskSelection[]): Promise<void> {
    await this.requestOk({
      req: "ask_response",
      session_id: sessionId,
      ask_id: askId,
      answers,
    });
  }

  /**
   * Models this session can switch to, and which one is serving it.
   *
   * Answered from the catalog the daemon pushes on attach, so opening a model
   * picker does not cost a round trip.
   */
  async listModels(sessionId: string): Promise<{ models: string[]; current?: string }> {
    const frame = await this.expectReply({ req: "list_models", session_id: sessionId }, "models");
    return { models: frame.models ?? [], current: frame.current };
  }

  /** Runtime identity, route catalog, protocol metadata, and a live health check. */
  async runtimeCommand(
    sessionId: string,
    command: RuntimeCommand,
    args: string[] = [],
  ): Promise<{
    session_id: string;
    command: RuntimeCommand;
    data: unknown;
    message: string;
  }> {
    if (!this.capabilities.includes("runtime_commands")) {
      throw new HarnessError(
        "unknown_request",
        "This runtime does not support structured commands. Update and restart the daemon and bridge.",
      );
    }
    const frame = await this.expectReply(
      { req: "runtime_command", session_id: sessionId, command, args },
      "runtime_command_result",
    );
    return { session_id: frame.session_id, command: frame.command, data: frame.data, message: frame.message };
  }

  async transferSession(sessionId: string): Promise<SessionInfo> {
    const frame = await this.expectReply({ req: "transfer_session", session_id: sessionId }, "session_forked");
    return frame.session;
  }

  async getRuntimeInfo(sessionId: string): Promise<RuntimeInfo> {
    await this.ping();
    const frame = await this.expectReply({ req: "get_runtime_info", session_id: sessionId }, "runtime_info");
    const routes = frame.routes ?? [];
    return {
      server: this.server,
      protocolVersion: API_VERSION_MAJOR,
      capabilities: [...this.capabilities],
      healthy: true,
      sessionId: frame.session_id,
      provider: frame.provider,
      model: frame.model,
      providers: [
        ...new Set(
          [frame.provider, ...routes.map((route) => route.provider)].filter(
            (provider): provider is string => provider !== undefined,
          ),
        ),
      ],
      routes,
    };
  }

  /** Persist an API key in horon-agent's owner-only provider store and hot-reload it. */
  async setApiKey(provider: string, apiKey: string): Promise<void> {
    await this.expectReply({ req: "set_api_key", provider, api_key: apiKey }, "credential_updated");
  }

  async clearApiKey(provider: string): Promise<void> {
    await this.expectReply({ req: "clear_api_key", provider }, "credential_updated");
  }

  /** Reload credentials saved by an out-of-band OAuth login. No secrets or chat messages. */
  async notifyAuthChanged(provider: string): Promise<void> {
    await this.expectReply({ req: "notify_auth_changed", provider }, "ok");
  }

  /**
   * Tell the daemon a banked usage reset was redeemed for one subscription
   * login (`claude` or `openai`) so it refetches quota. Carries no credentials.
   */
  async invalidateUsage(provider: string, accountLabel?: string): Promise<void> {
    await this.expectReply(
      accountLabel === undefined
        ? { req: "invalidate_usage", provider }
        : { req: "invalidate_usage", provider, account_label: accountLabel },
      "ok",
    );
  }

  async readFile(sessionId: string, path: string, maxBytes?: number): Promise<FileContent> {
    const frame = await this.expectReply(
      { req: "read_file", session_id: sessionId, path, max_bytes: maxBytes },
      "file_content",
    );
    return { path: frame.path, content: frame.content, size: frame.size, truncated: frame.truncated };
  }

  async findFiles(sessionId: string, query: string, limit?: number): Promise<string[]> {
    const frame = await this.expectReply({ req: "find_files", session_id: sessionId, query, limit }, "files");
    return frame.paths;
  }

  async searchText(
    sessionId: string,
    query: string,
    options: { path?: string; limit?: number } = {},
  ): Promise<TextMatch[]> {
    const frame = await this.expectReply(
      { req: "search_text", session_id: sessionId, query, ...options },
      "text_matches",
    );
    return frame.matches;
  }

  /** Persisted todo state for client-side auto-poke and quality gates.
   * Requires the `todo_state` capability. */
  async todoState(sessionId: string): Promise<TodoSnapshot> {
    const frame = await this.expectReply({ req: "get_todo_state", session_id: sessionId }, "todo_state");
    return frame.state;
  }

  /** Apply a follow-up decision's persistent effects. */
  async ackTodoFollowUp(sessionId: string, effects: TodoEffects): Promise<void> {
    if (!effects.clear_gate_observations && !effects.mark_long_session_review_delivered) {
      return;
    }
    await this.expectReply({ req: "ack_todo_follow_up", session_id: sessionId, effects }, "ok");
  }

  async fileStatus(sessionId: string, path: string): Promise<FileStatus> {
    const frame = await this.expectReply({ req: "file_status", session_id: sessionId, path }, "file_status");
    return {
      path: frame.path,
      exists: frame.exists,
      kind: frame.kind,
      size: frame.size,
      modifiedMs: frame.modified_ms,
    };
  }

  /**
   * Switch the session to a different model.
   *
   * `model` is an id from `listModels`. Rejects with `invalid_request` when
   * the model is unknown or the provider refuses it, rather than silently
   * leaving the session on the old one.
   */
  async setModel(sessionId: string, model: string): Promise<void> {
    await this.requestOk({ req: "set_model", session_id: sessionId, model });
  }

  /**
   * Set how much the model deliberates before answering.
   *
   * Typically "minimal" | "low" | "medium" | "high" | "xhigh" | "max", but the
   * accepted set is per-provider, so this takes a string and reports what the
   * provider says rather than guessing at a union that would go stale.
   */
  async setReasoningEffort(sessionId: string, effort: string): Promise<void> {
    await this.requestOk({ req: "set_reasoning_effort", session_id: sessionId, effort });
  }

  /**
   * Schedule compaction of the transcript so far, freeing context.
   *
   * Not synchronous: the daemon summarizes at the next safe point rather than
   * interrupting a turn, so this resolving means the request was accepted. Read
   * the history afterwards to see the result. A refusal (nothing to compact, a
   * turn in flight) rejects with `invalid_request` and the reason.
   */
  async compact(sessionId: string): Promise<string> {
    const frame = await this.expectReply({ req: "compact", session_id: sessionId }, "compacted");
    return frame.message;
  }

  /** Set a session's title. Omit `title` to restore the generated one. */
  async renameSession(sessionId: string, title?: string): Promise<void> {
    await this.requestOk({ req: "rename_session", session_id: sessionId, title });
  }

  /** Bookmark or unbookmark a session. A label also becomes its title. */
  async setSessionSaved(sessionId: string, saved: boolean, label?: string): Promise<void> {
    await this.requestOk({ req: "set_session_saved", session_id: sessionId, saved, label });
  }

  /** Restore the history the last `rewind` removed. */
  async rewindUndo(sessionId: string): Promise<void> {
    await this.requestOk({ req: "rewind_undo", session_id: sessionId });
  }

  /** Drop soft interrupts that are queued but not yet delivered. */
  async cancelSoftInterrupts(sessionId: string): Promise<void> {
    await this.requestOk({ req: "cancel_soft_interrupts", session_id: sessionId });
  }

  /**
   * Take back specific queued soft interrupts by `client_id`.
   *
   * Decided atomically against delivery: an id in `cancelled` never reached
   * the model, and one in `notQueued` already did (or was never queued).
   * Needs the `soft_interrupt_ids` capability.
   */
  async cancelSoftInterruptsById(
    sessionId: string,
    clientIds: string[],
  ): Promise<{ cancelled: string[]; notQueued: string[] }> {
    const frame = await this.expectReply(
      { req: "cancel_soft_interrupts", session_id: sessionId, client_ids: clientIds },
      "soft_interrupts_cancelled",
    );
    return { cancelled: frame.cancelled ?? [], notQueued: frame.not_queued ?? [] };
  }

  /**
   * Take back one queued soft interrupt. Resolves `true` when it was removed
   * before the model saw it, `false` when it was already delivered.
   */
  async cancelSoftInterrupt(sessionId: string, clientId: string): Promise<boolean> {
    const { cancelled } = await this.cancelSoftInterruptsById(sessionId, [clientId]);
    return cancelled.includes(clientId);
  }

  /** Move the running tool call to the background. */
  async backgroundTool(sessionId: string): Promise<void> {
    await this.backgroundToolResult(sessionId);
  }

  /**
   * Move the running tool call to the background and report what moved.
   * A server without the `background_tool_result` capability only answers
   * `ok`, reported as `{ moved: false }` even if a tool was moved.
   */
  async backgroundToolResult(sessionId: string): Promise<{ moved: boolean; toolCallId?: string; toolName?: string }> {
    const frame = await this.requestOk({ req: "background_tool", session_id: sessionId });
    if (frame.ev !== "background_tool_result") return { moved: false };
    const result: { moved: boolean; toolCallId?: string; toolName?: string } = {
      moved: frame.moved === true,
    };
    if (typeof frame.tool_call_id === "string") result.toolCallId = frame.tool_call_id;
    if (typeof frame.tool_name === "string") result.toolName = frame.tool_name;
    return result;
  }

  async ping(): Promise<void> {
    await this.requestOk({ req: "ping" });
  }

  private requireCapability(capability: string, surface: string): void {
    if (this.supports(capability)) return;
    throw new HarnessError(
      "unsupported",
      `${surface} requires the \`${capability}\` capability; update and restart the daemon and the API bridge`,
    );
  }

  /** Run, queue, receipt, and monitor summary. Requires `durable_runtime_v1`. */
  async getRuntimeState(sessionId: string): Promise<RuntimeStateSummary> {
    this.requireCapability("durable_runtime_v1", "getRuntimeState");
    const frame = await this.expectReply({ req: "get_runtime_state", session_id: sessionId }, "runtime_state");
    return frame.state;
  }

  /**
   * Apply one control operation under a caller-chosen id. Requires
   * `durable_runtime_v1`. The same id reports the recorded receipt instead of
   * applying the operation twice.
   */
  async applyControl(
    sessionId: string,
    operationId: string,
    action: string,
    args?: unknown,
  ): Promise<RuntimeControlReceipt> {
    this.requireCapability("durable_runtime_v1", "applyControl");
    const frame = await this.expectReply(
      {
        req: "apply_control",
        session_id: sessionId,
        operation_id: operationId,
        action,
        args,
      },
      "control_receipt",
    );
    return frame.receipt;
  }

  /** The recorded state of one control operation. Requires `durable_runtime_v1`. */
  async getControlReceipt(sessionId: string, operationId: string): Promise<RuntimeControlReceipt> {
    this.requireCapability("durable_runtime_v1", "getControlReceipt");
    const frame = await this.expectReply(
      {
        req: "get_control_receipt",
        session_id: sessionId,
        operation_id: operationId,
      },
      "control_receipt",
    );
    return frame.receipt;
  }

  /**
   * One consistent state view plus a bounded history tail, at a marker the
   * server also sends on this connection as a `snapshot_marker` event.
   * Requires `snapshot_marker_v1`.
   */
  async getSnapshot(
    sessionId: string,
    options: { tail?: number; maxBytes?: number } = {},
  ): Promise<SessionSnapshot> {
    this.requireCapability("snapshot_marker_v1", "getSnapshot");
    const frame = await this.expectReply(
      {
        req: "get_snapshot",
        session_id: sessionId,
        tail: options.tail,
        max_bytes: options.maxBytes,
      },
      "snapshot",
    );
    return frame.snapshot;
  }

  /**
   * Answer an `ask_requested` with a retry-safe answer id. Requires
   * `ask_receipts_v1`.
   */
  async respondToAskWithId(
    sessionId: string,
    askId: string,
    answerId: string,
    answers: AskSelection[],
  ): Promise<void> {
    this.requireCapability("ask_receipts_v1", "respondToAskWithId");
    await this.requestOk({
      req: "ask_response",
      session_id: sessionId,
      ask_id: askId,
      answers,
      answer_id: answerId,
    });
  }

  /**
   * Send a replication request, refusing to build a frame larger than the
   * 16 MiB envelope. `canonical_json` is opaque text: nothing here parses or
   * re-serializes record numbers.
   */
  private async replicationRequest(request: ApiRequest): Promise<ServerFrame> {
    const probe = `${JSON.stringify(request)}`;
    if (Buffer.byteLength(probe, "utf8") + 64 > MAX_REPLICATION_WIRE_BYTES) {
      throw new HarnessError("invalid_request", `replication request exceeds the ${MAX_REPLICATION_WIRE_BYTES} byte wire envelope`);
    }
    return this.requestOk(request);
  }

  private async replicationReply<K extends ApiEvent["ev"]>(
    request: ApiRequest,
    kind: K,
  ): Promise<Extract<ApiEvent, { ev: K }>> {
    const frame = await this.replicationRequest(request);
    if (frame.ev !== kind) {
      throw new HarnessError("unexpected_reply", `expected ${kind}, got ${frame.ev}`);
    }
    return frame as unknown as Extract<ApiEvent, { ev: K }>;
  }

  /** Durable heads and the host-local selection. Requires `session_event_store_v1`. */
  async listSessionHeads(sessionId: string): Promise<SessionHeads> {
    this.requireCapability("session_event_store_v1", "listSessionHeads");
    const frame = await this.replicationReply({ req: "list_session_heads", session_id: sessionId }, "session_heads");
    return { session_id: frame.session_id, heads: frame.heads, selected_head: frame.selected_head };
  }

  /** Select the head execution should use. A live owner answers `busy`. */
  async selectSessionHead(sessionId: string, headEventId: string): Promise<SessionHeads> {
    this.requireCapability("session_event_store_v1", "selectSessionHead");
    const frame = await this.replicationReply({
      req: "select_session_head",
      session_id: sessionId,
      head_event_id: headEventId,
    }, "session_heads");
    return { session_id: frame.session_id, heads: frame.heads, selected_head: frame.selected_head };
  }

  /** One page of visible history on a fixed head, oldest first. Requires `session_history_cursor_v1`. */
  async getHistoryPage(
    sessionId: string,
    options: { headEventId?: string; beforeEventId?: string; limit?: number } = {},
  ): Promise<SessionHistoryPage> {
    this.requireCapability("session_history_cursor_v1", "getHistoryPage");
    const frame = await this.replicationReply({
      req: "get_history_page",
      session_id: sessionId,
      head_event_id: options.headEventId,
      before_event_id: options.beforeEventId,
      limit: options.limit,
    }, "session_history_page");
    return {
      session_id: frame.session_id,
      head: frame.head,
      messages: frame.messages,
      next_cursor: frame.next_cursor,
    };
  }

  /** Export one bounded parent-first page of original events. Requires `session_replication_v1`. */
  async exportSessionEvents(
    sessionId: string,
    options: { heads?: string[]; knownIds?: string[]; cursor?: string } = {},
  ): Promise<SessionEventPage> {
    this.requireCapability("session_replication_v1", "exportSessionEvents");
    const frame = await this.replicationReply({
      req: "export_session_events",
      session_id: sessionId,
      protocol_version: REPLICATION_PROTOCOL_VERSION,
      heads: options.heads,
      known_ids: options.knownIds,
      cursor: options.cursor,
    }, "session_event_page");
    return {
      session_id: frame.session_id,
      heads: frame.heads,
      events: frame.events,
      next_cursor: frame.next_cursor,
      blob_refs: frame.blob_refs,
      record_refs: frame.record_refs,
    };
  }

  /** Export the conversation as a Markdown transcript.
   *
   * Requires `session_replication_v1`. Returns the full markdown text with
   * secrets redacted and long tool results truncated with a marker.
   */
  async exportSessionMarkdown(sessionId: string): Promise<string> {
    this.requireCapability("session_replication_v1", "exportSessionMarkdown");
    const frame = await this.expectReply(
      { req: "export_session_markdown", session_id: sessionId },
      "session_markdown",
    );
    return frame.markdown ?? "";
  }

  /** Import one validated page. Identical retransmissions are idempotent. */
  async importSessionEvents(page: SessionEventPage, retryCursor?: string): Promise<SessionImportResult> {
    this.requireCapability("session_replication_v1", "importSessionEvents");
    const frame = await this.replicationReply({
      req: "import_session_events",
      session_id: page.session_id,
      protocol_version: REPLICATION_PROTOCOL_VERSION,
      heads: page.heads,
      events: page.events,
      next_cursor: page.next_cursor,
      blob_refs: page.blob_refs,
      record_refs: page.record_refs,
      retry_cursor: retryCursor,
    }, "session_import_result");
    return {
      session_id: frame.session_id,
      accepted_ids: frame.accepted_ids,
      heads: frame.heads,
      retry_cursor: frame.retry_cursor,
      conflict_heads: frame.conflict_heads,
    };
  }

  /** Read one 1 MiB chunk of an original payload blob. */
  async exportSessionBlob(sessionId: string, sha256: string, totalSize: number, offset: number): Promise<SessionBlobChunk> {
    this.requireCapability("session_replication_v1", "exportSessionBlob");
    const frame = await this.replicationReply({
      req: "export_session_blob",
      session_id: sessionId,
      sha256,
      total_size: totalSize,
      offset,
    }, "session_blob_chunk");
    return {
      sha256: frame.sha256,
      offset: frame.offset,
      data_base64: frame.data_base64,
      next_offset: frame.next_offset,
      total_size: frame.total_size,
      complete: frame.complete,
    };
  }

  /** Stage one chunk of an original payload blob. */
  async importSessionBlob(sessionId: string, chunk: SessionBlobChunk): Promise<SessionBlobStatus> {
    this.requireCapability("session_replication_v1", "importSessionBlob");
    const frame = await this.replicationReply({ req: "import_session_blob", session_id: sessionId, chunk }, "session_blob_status");
    return {
      session_id: frame.session_id,
      sha256: frame.sha256,
      received: frame.received,
      total_size: frame.total_size,
      complete: frame.complete,
    };
  }

  /** Resume an interrupted payload transfer. */
  async sessionBlobStatus(sessionId: string, sha256: string, totalSize: number): Promise<SessionBlobStatus> {
    this.requireCapability("session_replication_v1", "sessionBlobStatus");
    const frame = await this.replicationReply({
      req: "get_session_blob_status",
      session_id: sessionId,
      sha256,
      total_size: totalSize,
    }, "session_blob_status");
    return {
      session_id: frame.session_id,
      sha256: frame.sha256,
      received: frame.received,
      total_size: frame.total_size,
      complete: frame.complete,
    };
  }

  /** Read one 1 MiB chunk of a whole canonical record carried by reference. */
  async exportSessionRecordBlob(
    sessionId: string,
    reference: CanonicalRecordRef,
    offset: number,
  ): Promise<SessionBlobChunk> {
    this.requireCapability("session_replication_v1", "exportSessionRecordBlob");
    const frame = await this.replicationReply({
      req: "export_session_record_blob",
      session_id: sessionId,
      reference,
      offset,
    }, "session_blob_chunk");
    return {
      sha256: frame.sha256,
      offset: frame.offset,
      data_base64: frame.data_base64,
      next_offset: frame.next_offset,
      total_size: frame.total_size,
      complete: frame.complete,
    };
  }

  /** Stage one chunk of a whole canonical record. */
  async importSessionRecordBlob(
    sessionId: string,
    reference: CanonicalRecordRef,
    chunk: SessionBlobChunk,
  ): Promise<SessionRecordBlobStatus> {
    this.requireCapability("session_replication_v1", "importSessionRecordBlob");
    const frame = await this.replicationReply({
      req: "import_session_record_blob",
      session_id: sessionId,
      reference,
      chunk,
    }, "session_record_blob_status");
    return {
      session_id: frame.session_id,
      reference: frame.reference,
      received: frame.received,
      total_size: frame.total_size,
      complete: frame.complete,
    };
  }

  /** Resume an interrupted whole-record transfer. */
  async sessionRecordBlobStatus(sessionId: string, reference: CanonicalRecordRef): Promise<SessionRecordBlobStatus> {
    this.requireCapability("session_replication_v1", "sessionRecordBlobStatus");
    const frame = await this.replicationReply({
      req: "get_session_record_blob_status",
      session_id: sessionId,
      reference,
    }, "session_record_blob_status");
    return {
      session_id: frame.session_id,
      reference: frame.reference,
      received: frame.received,
      total_size: frame.total_size,
      complete: frame.complete,
    };
  }

  /**
   * Async iterator over stream events, optionally filtered to one session.
   *
   * Buffers frames that arrive between `next()` calls, so a consumer that
   * awaits slow work in the loop body does not silently drop deltas.
   *
   * The buffer is bounded in both count and bytes. Ordinary frames (narration,
   * deltas, snapshots) may hold `maxBufferedEvents` events and
   * `maxBufferedBytes` estimated bytes; control frames (approvals, asks,
   * cancellations, turn terminals, tool outcomes) get an extra quarter of each
   * on top, so a flood of deltas cannot evict the frame that ends a turn. Past
   * the ordinary budget new ordinary frames are dropped and the stream fails
   * with `event_buffer_overflow` once the frames already queued have been
   * delivered; a full control reserve closes the stream immediately with the
   * same error. Either way the stream closes: a consumer never mistakes a
   * truncated stream for a finished one.
   *
   * Typed as the known event union so `switch (event.ev)` narrows each case.
   *
   * Unknown kinds can still arrive at runtime (the harness may add events
   * within protocol v1), so always keep a `default` branch. Use
   * `isKnownEvent` if you need to prove the distinction.
   */
  events(sessionId?: string, options: EventsOptions = {}): EventStream {
    const maxBufferedEvents = options.maxBufferedEvents ?? 10_000;
    const maxBufferedBytes = options.maxBufferedBytes ?? 8 * 1024 * 1024;
    if (!Number.isInteger(maxBufferedEvents) || maxBufferedEvents < 1) {
      throw new HarnessError("invalid_option", "maxBufferedEvents must be a positive integer");
    }
    if (!Number.isInteger(maxBufferedBytes) || maxBufferedBytes < 1) {
      throw new HarnessError("invalid_option", "maxBufferedBytes must be a positive integer");
    }
    const controlReserveEvents = Math.max(1, Math.floor(maxBufferedEvents / 4));
    const controlReserveBytes = Math.max(1024, Math.floor(maxBufferedBytes / 4));

    type Queued = { event: ServerFrame & ApiEvent; control: boolean; bytes: number };

    // A head index keeps consumption O(1): `shift()` moved every remaining
    // frame on each call, which is quadratic for a consumer catching up.
    const queue: Queued[] = [];
    let head = 0;
    let ordinaryEvents = 0;
    let ordinaryBytes = 0;
    let controlEvents = 0;
    let controlBytes = 0;
    let overflow: HarnessError | undefined;
    let pending: { resolve: (result: IteratorResult<ApiEvent>) => void; reject: (error: Error) => void } | undefined;
    let done = false;

    const buffered = (): EventBufferStats => ({
      events: queue.length - head,
      bytes: ordinaryBytes + controlBytes,
    });

    const detach = () => {
      this.off("event", onEvent);
      this.off("close", onClose);
    };

    const clear = () => {
      queue.length = 0;
      head = 0;
      ordinaryEvents = 0;
      ordinaryBytes = 0;
      controlEvents = 0;
      controlBytes = 0;
    };

    const take = (): ApiEvent => {
      const entry = queue[head] as Queued;
      // Clearing the slot releases the frame, so a drained queue holds no
      // references to payloads the consumer already read.
      queue[head] = undefined as unknown as Queued;
      head += 1;
      if (entry.control) {
        controlEvents -= 1;
        controlBytes -= entry.bytes;
      } else {
        ordinaryEvents -= 1;
        ordinaryBytes -= entry.bytes;
      }
      if (head === queue.length) {
        queue.length = 0;
        head = 0;
      } else if (head >= 1024) {
        // Compact in one pass instead of per call; amortised O(1).
        queue.splice(0, head);
        head = 0;
      }
      return entry.event;
    };

    /** Close the stream with a terminal error, releasing everything it held. */
    const fail = (error: HarnessError) => {
      done = true;
      clear();
      detach();
      const waiter = pending;
      pending = undefined;
      if (waiter) {
        // The consumer is already waiting: hand it the error now.
        waiter.reject(error);
        return;
      }
      // Nobody is waiting: the next `next()` rejects once, then reports done.
      overflow = error;
    };

    const stop = (): Promise<IteratorResult<ApiEvent>> => {
      done = true;
      overflow = undefined;
      clear();
      detach();
      if (pending) {
        const resolve = pending.resolve;
        pending = undefined;
        resolve({ value: undefined, done: true });
      }
      return Promise.resolve<IteratorResult<ApiEvent>>({ value: undefined, done: true });
    };

    /**
     * Hand queued frames to a waiting consumer, then surface a recorded
     * overflow. Delivering what was already accepted before failing is what
     * keeps a control frame that arrived during the flood from being lost.
     */
    const deliver = () => {
      while (pending && head < queue.length) {
        const waiter = pending;
        pending = undefined;
        waiter.resolve({ value: take(), done: false });
      }
      if (head < queue.length) return;
      if (overflow) fail(overflow);
    };

    const onEvent = (frame: ServerFrame) => {
      if (sessionId && "session_id" in frame && frame.session_id !== sessionId) return;
      if (!isEventFrame(frame)) return;
      if (done) return;
      const control = isControlEvent(frame);
      const limitEvents = control ? controlReserveEvents : maxBufferedEvents;
      const limitBytes = control ? controlReserveBytes : maxBufferedBytes;
      const usedEvents = control ? controlEvents : ordinaryEvents;
      const usedBytes = control ? controlBytes : ordinaryBytes;
      const bytes = estimateEventBytes(frame, Math.max(0, limitBytes - usedBytes));
      if (usedEvents + 1 > limitEvents || usedBytes + bytes > limitBytes) {
        if (control) {
          // A control frame is the one thing a caller cannot re-derive, so it
          // closes the stream visibly rather than being dropped silently.
          fail(
            new HarnessError(
              "event_buffer_overflow",
              `control event reserve filled ${controlReserveEvents} events / ${controlReserveBytes} bytes; the event stream was closed rather than dropping an approval, ask, cancellation or terminal frame`,
            ),
          );
          return;
        }
        if (!overflow) {
          overflow = new HarnessError(
            "event_buffer_overflow",
            `events() consumer fell behind ${maxBufferedEvents} buffered events / ${maxBufferedBytes} bytes; ordinary frames were dropped and the stream is closing`,
          );
        }
        deliver();
        return;
      }
      queue.push({ event: frame, control, bytes });
      if (control) {
        controlEvents += 1;
        controlBytes += bytes;
      } else {
        ordinaryEvents += 1;
        ordinaryBytes += bytes;
      }
      deliver();
    };
    const onClose = () => {
      void stop();
    };

    this.on("event", onEvent);
    this.once("close", onClose);

    return {
      [Symbol.asyncIterator]() {
        return this;
      },
      get buffered() {
        return buffered();
      },
      next: () => {
        if (head < queue.length) {
          const value = take();
          // A drained queue after an overflow closes the stream now, so the
          // listeners are released even if the consumer stops calling.
          deliver();
          return Promise.resolve({ value, done: false });
        }
        if (overflow) {
          const error = overflow;
          overflow = undefined;
          return Promise.reject(error);
        }
        if (done) return Promise.resolve({ value: undefined, done: true });
        return new Promise<IteratorResult<ApiEvent>>((resolve, reject) => {
          pending = { resolve, reject };
        });
      },
      return: stop,
      throw: stop,
    };
  }

  /**
   * Stream events from every session through per-session child connections.
   *
   * The bridge has one attachment per connection, so a server-wide stream is
   * implemented by discovering persisted sessions, attaching one child client
   * to each, and fanning their events into this iterator. Discovery repeats so
   * sessions created after subscription are included.
   *
   * Delivery is at-least-from-attach, not historical replay: events emitted
   * before a child finishes attaching, or during an unexpected disconnect and
   * reattach, cannot be recovered by protocol v1. Ordering is preserved within
   * each session; no total ordering across sessions is claimed.
   *
   * Calling `return()`, aborting `signal`, or closing this client closes every
   * child connection. Custom transports are rejected because the SDK cannot
   * safely clone an arbitrary transport into independent session connections.
   */
  globalEvents(options: GlobalEventsOptions = {}): AsyncIterableIterator<ApiEvent> {
    if (!this.socketPath) {
      throw new HarnessError(
        "unsupported_transport",
        "globalEvents requires a native socket connection; custom transports cannot be cloned into per-session child connections",
      );
    }
    if (this.closed) {
      throw this.closeError ?? new HarnessError("disconnected", "harness connection closed");
    }

    const discoveryIntervalMs = options.discoveryIntervalMs ?? 1_000;
    const maxBufferedEvents = options.maxBufferedEvents ?? 10_000;
    if (!Number.isFinite(discoveryIntervalMs) || discoveryIntervalMs < 0) {
      throw new HarnessError("invalid_option", "discoveryIntervalMs must be a finite number >= 0");
    }
    if (!Number.isInteger(maxBufferedEvents) || maxBufferedEvents < 1) {
      throw new HarnessError("invalid_option", "maxBufferedEvents must be a positive integer");
    }

    type Waiter = {
      resolve: (result: IteratorResult<ApiEvent>) => void;
      reject: (error: Error) => void;
    };
    type Child = {
      client: HoronAgentClient;
      stream: AsyncIterableIterator<ApiEvent>;
      pump?: Promise<void>;
    };

    const queue: ApiEvent[] = [];
    const children = new Map<string, Child>();
    const starting = new Set<string>();
    const startTasks = new Set<Promise<void>>();
    let waiter: Waiter | undefined;
    let stopped = false;
    let terminalError: Error | undefined;
    let discoveryTimer: NodeJS.Timeout | undefined;
    let cleanupPromise: Promise<void> | undefined;

    const settleWaiter = () => {
      if (!waiter) return;
      const pending = waiter;
      waiter = undefined;
      if (terminalError) pending.reject(terminalError);
      else pending.resolve({ value: undefined, done: true });
    };

    const onParentClose = (error?: Error) => {
      void finish(error);
    };
    const onAbort = () => {
      void finish();
    };

    // Named `failure` rather than `error`: a leading `error` parameter reads as
    // a Node-style callback to the promise linter, which then rejects the
    // promises this cleanup deliberately returns.
    const finish = (failure?: Error): Promise<void> => {
      if (cleanupPromise) return cleanupPromise;
      stopped = true;
      terminalError = failure;
      queue.length = 0;
      if (discoveryTimer) clearTimeout(discoveryTimer);
      this.off("close", onParentClose);
      options.signal?.removeEventListener("abort", onAbort);
      settleWaiter();

      const closing: Promise<unknown>[] = [];
      for (const { client, stream, pump } of children.values()) {
        closing.push(Promise.resolve(stream.return?.(undefined)));
        closing.push(client.close());
        if (pump) closing.push(pump);
      }
      children.clear();
      closing.push(...startTasks);
      cleanupPromise = Promise.allSettled(closing).then(() => undefined);
      return cleanupPromise;
    };

    const enqueue = (event: ApiEvent) => {
      if (stopped) return;
      if (waiter) {
        const pending = waiter;
        waiter = undefined;
        pending.resolve({ value: event, done: false });
        return;
      }
      if (queue.length >= maxBufferedEvents) {
        void finish(
          new HarnessError(
            "event_buffer_overflow",
            `globalEvents consumer fell behind ${maxBufferedEvents} buffered events`,
          ),
        );
        return;
      }
      queue.push(event);
    };

    const startChild = async (sessionId: string): Promise<void> => {
      if (stopped || children.has(sessionId) || starting.has(sessionId)) return;
      starting.add(sessionId);
      let child: HoronAgentClient | undefined;
      try {
        child = await HoronAgentClient.connect({
          socketPath: this.socketPath,
          clientName: `${this.clientName}/global-events`,
          requestTimeoutMs: this.requestTimeoutMs,
        });
        if (stopped) {
          await child.close();
          return;
        }
        const stream = child.events(sessionId);
        const childState: Child = { client: child, stream };
        children.set(sessionId, childState);
        await child.attachSession(sessionId);
        if (stopped) return;

        const pump = (async () => {
          try {
            for await (const event of stream) enqueue(event);
          } finally {
            if (children.get(sessionId) === childState) children.delete(sessionId);
            await childState.client.close();
          }
        })();
        childState.pump = pump;
        void pump;
      } catch (error) {
        if (child && children.get(sessionId)?.client === child) children.delete(sessionId);
        await child?.close();
        if (!stopped && !(error instanceof HarnessError && error.code === "unknown_session")) {
          void finish(error instanceof Error ? error : new Error(String(error)));
        }
      } finally {
        starting.delete(sessionId);
      }
    };

    const scan = async (): Promise<void> => {
      if (stopped) return;
      try {
        const sessions = await this.listSessions({ includeArchived: true });
        if (stopped) return;
        const tasks = sessions.map((session) => {
          const task = startChild(session.session_id);
          startTasks.add(task);
          void task.finally(() => startTasks.delete(task));
          return task;
        });
        await Promise.allSettled(tasks);
      } catch (error) {
        if (!stopped) void finish(error instanceof Error ? error : new Error(String(error)));
      }
      if (!stopped && discoveryIntervalMs > 0) {
        discoveryTimer = setTimeout(() => void scan(), discoveryIntervalMs);
        discoveryTimer.unref?.();
      }
    };

    this.once("close", onParentClose);
    if (options.signal?.aborted) void finish();
    else {
      options.signal?.addEventListener("abort", onAbort, { once: true });
      void scan();
    }

    return {
      [Symbol.asyncIterator]() {
        return this;
      },
      next: () => {
        if (queue.length > 0) {
          return Promise.resolve({ value: queue.shift()!, done: false });
        }
        if (terminalError) return Promise.reject(terminalError);
        if (stopped) {
          return Promise.resolve<IteratorResult<ApiEvent>>({ value: undefined, done: true });
        }
        if (waiter) {
          return Promise.reject(
            new HarnessError("concurrent_next", "globalEvents does not support concurrent next() calls"),
          );
        }
        return new Promise<IteratorResult<ApiEvent>>((resolve, reject) => {
          waiter = { resolve, reject };
        });
      },
      return: async () => {
        await finish();
        return { value: undefined, done: true };
      },
      throw: async (error?: unknown) => {
        await finish();
        throw error;
      },
    };
  }

  /**
   * Send a message and collect the assistant reply until the turn ends.
   *
   * The convenience path for scripts: one call in, the text and tool calls of
   * one turn out. `text` includes all narration in the turn. Use `finalText`
   * for the last framed assistant message. Streaming consumers should use
   * `events()` instead.
   */
  async run(sessionId: string, content: string, options: RunOptions = {}): Promise<TurnResult> {
    const stream = this.events(sessionId);
    await this.sendMessage(sessionId, content, options.images ?? []);
    const result: TurnResult = {
      text: "",
      finalText: "",
      messages: [],
      reasoning: "",
      toolCalls: [],
      usage: undefined,
    };
    const parts: AssistantTextMessage[] = [];
    const messagesById = new Map<string, AssistantTextMessage>();
    const completed = new Set<AssistantTextMessage>();
    const messageFor = (id?: string): AssistantTextMessage => {
      const key = id ?? "";
      let message = messagesById.get(key);
      if (!message) {
        message = { text: "", ...(id === undefined ? {} : { messageId: id }) };
        messagesById.set(key, message);
        parts.push(message);
      }
      return message;
    };
    const finish = (): TurnResult => {
      result.messages = parts.filter((message) => completed.has(message) && message.text.length > 0);
      // Older bridges have no text_done. Do not guess boundaries from
      // reasoning or tools: retain the historical whole-turn result.
      result.finalText = result.messages.at(-1)?.text ?? result.text;
      return result;
    };
    for await (const event of stream) {
      options.onEvent?.(event);
      switch (event.ev) {
        // No casts here: the event union narrows on `ev`. That is the point
        // of keeping the unknown-kind catch-all out of `ApiEvent`, and it is
        // also what stops a renamed wire field from compiling.
        case "text_delta":
          result.text += event.text;
          messageFor(event.message_id).text += event.text;
          break;
        case "text_replace":
          messageFor(event.message_id).text = event.text;
          result.text = parts.map((message) => message.text).join("");
          break;
        case "text_done": {
          const key = event.message_id ?? "";
          const message = messagesById.get(key);
          if (message) completed.add(message);
          // Id-less framing still separates sequential messages. Keep keyed
          // messages addressable for authoritative corrections after completion.
          if (event.message_id === undefined) messagesById.delete(key);
          break;
        }
        case "reasoning_delta":
          result.reasoning += event.text;
          break;
        case "tool_done":
          result.toolCalls.push({
            callId: event.call_id,
            name: event.name,
            output: event.output,
            error: event.error,
          });
          break;
        case "token_usage":
          result.usage = {
            input: event.input,
            output: event.output,
            cacheReadInput: event.cache_read_input,
          };
          break;
        case "permission_request":
          if (options.autoApprove) {
            await this.respondToPermission(sessionId, event.request_id, "allow");
          }
          break;
        case "turn_stopped":
          result.stopReason = event.reason;
          result.stopMessage = event.message;
          break;
        case "turn_done":
          await stream.return?.(undefined);
          return finish();
        case "error":
          await stream.return?.(undefined);
          throw new HarnessError(event.code ?? "internal", event.message ?? "harness error");
      }
    }
    return finish();
  }

  /**
   * Send a message and parse the assistant's answer as JSON validated by schema.
   *
   * This is an SDK-level contract: it does not require a special bridge or model
   * feature. The initial prompt asks for JSON only; if the response is not JSON
   * or fails Ajv validation, the client sends a bounded number of corrective
   * prompts that include the normalized validation errors and the prior text.
   */
  async runStructured<T = unknown>(
    sessionId: string,
    content: string,
    options: RunStructuredOptions<T>,
  ): Promise<StructuredTurnResult<T>> {
    const { schema, maxRetries = 2, ...runOptions } = options;
    assertRetryCount(maxRetries);
    const validate = compileStructuredSchema(schema);
    const attempts: StructuredOutputAttempt[] = [];

    // One attempt per call. Recursion, not a loop: an attempt has to finish
    // before the next prompt can be built from its validation errors.
    const attempt = async (attemptNumber: number, prompt: string): Promise<StructuredTurnResult<T>> => {
      const turn = await this.run(sessionId, prompt, runOptions);
      const validation = validateStructuredText(turn.finalText, validate);
      const record: StructuredOutputAttempt = {
        attempt: attemptNumber,
        text: turn.finalText,
        errors: validation.errors,
      };
      attempts.push(record);
      if (validation.ok) return { ...turn, data: validation.data, attempts };
      if (attemptNumber > maxRetries) throw new StructuredOutputError(attempts);
      return attempt(attemptNumber + 1, buildStructuredCorrectionPrompt(schema, record));
    };

    return attempt(1, buildStructuredPrompt(content, schema));
  }

  /**
   * Disconnect, and stop the instance if this client launched one.
   *
   * Returns a promise so a launched instance can be awaited to a stop; for a
   * plain `connect()` client it resolves immediately and can be ignored.
   */
  close(): Promise<void> {
    this.toolCallbacks.close();
    this.transport.close();
    const instance = this.instance;
    this.instance = undefined;
    return instance ? instance.shutdown() : Promise.resolve();
  }
}

export interface TurnResult {
  /** Absent for natural completion. Failures still throw after onEvent. */
  stopReason?: TurnStopReason;
  stopMessage?: string;
  /** All assistant text deltas in the turn, including process narration. */
  text: string;
  /** Last completed assistant message, or whole-turn text on older bridges. */
  finalText: string;
  /** Completed text messages. Empty when the bridge does not support framing. */
  messages: AssistantTextMessage[];
  reasoning: string;
  toolCalls: Array<{ callId: string; name: string; output: string; error?: string }>;
  usage?: { input: number; output: number; cacheReadInput?: number };
}

export interface AssistantTextMessage {
  /** Stream-local correlation id, not a persisted history message id. */
  messageId?: string;
  text: string;
}

export interface StructuredTurnResult<T> extends TurnResult {
  /** Parsed JSON value validated against the caller's schema. */
  data: T;
  /** All attempts, including invalid retries and the final successful response. */
  attempts: StructuredOutputAttempt[];
}

export { HarnessError };
