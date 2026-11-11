"""Harness client: handshake, reply correlation, and event streaming.

The TypeScript SDK exposes an ``EventEmitter`` and per-kind listeners. Python's
idiom is an async iterator, so streaming is ``client.events(...)`` /
``client.global_events(...)``, consumed with ``async for``. Nothing else about
the surface changes: the same requests are sent, with the same correlation, the
same capability gating, and the same turn semantics.
"""

from __future__ import annotations

import abc
import asyncio
import contextlib
import errno
import json
import math
import sys
from collections import deque
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, TypedDict, cast

from .errors import HarnessError
from .framing import NdjsonDecoder, encode_frame
from .launch import LaunchedInstance, LaunchOptions, launch_instance
from .protocol import (
    API_VERSION_MAJOR,
    MAX_REPLICATION_WIRE_BYTES,
    REPLICATION_PROTOCOL_VERSION,
    CanonicalRecordRef,
    MessageModes,
    SessionToolDefinition,
)
from .sockets import api_socket_path, transport_endpoint
from .structured import (
    StructuredOutputAttempt,
    StructuredOutputError,
    StructuredOutputSchema,
    assert_retry_count,
    build_structured_correction_prompt,
    build_structured_prompt,
    compile_structured_schema,
    validate_structured_text,
)
from .tools import (
    SessionToolsOptions,
    ToolCallbacks,
    ToolResult,
    prepare_tools,
)

__all__ = [
    "ConnectOptions",
    "CreateSessionOptions",
    "EventBufferStats",
    "EventStream",
    "FileContent",
    "FileStatus",
    "GlobalEventsOptions",
    "HoronAgentClient",
    "RuntimeInfo",
    "Transport",
    "TurnResult",
    "unix_socket_transport",
]


class RuntimeInfo(TypedDict, total=False):
    server: str
    protocol_version: int
    capabilities: list[str]
    healthy: bool
    session_id: str
    provider: str
    model: str
    fallback_models: list[str]
    serving_model: str
    providers: list[str]
    routes: list[dict[str, Any]]


class FileContent(TypedDict):
    path: str
    content: str
    size: int
    truncated: bool


class FileStatus(TypedDict, total=False):
    path: str
    exists: bool
    kind: str
    size: int
    modified_ms: int


class EventBufferStats(TypedDict):
    """Events and estimated bytes a stream is holding for a slow consumer."""

    events: int
    bytes: int


_CONTROL_EVENT_KINDS = frozenset(
    {
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
    }
)

DEFAULT_MAX_BUFFERED_EVENTS = 10_000
DEFAULT_MAX_BUFFERED_BYTES = 8 * 1024 * 1024


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _estimate_event_bytes(value: Any, ceiling: int) -> int:
    """Estimate the serialized size of one frame in bytes, stopping once ``ceiling`` is exceeded.

    Called once per queued frame, never on the way out: re-encoding a large
    payload on every ``__anext__`` would cost more than the buffer saves. UTF-8
    is at most three bytes per UTF-16 code unit (four for a surrogate pair,
    which is two units), so this estimate never understates the wire size.
    """
    total = 0
    stack: list[Any] = [value]
    while stack and total <= ceiling:
        item = stack.pop()
        if item is None:
            total += 4
        elif isinstance(item, bool):
            total += 5
        elif isinstance(item, (int, float)):
            total += 8
        elif isinstance(item, str):
            total += len(item) * 3
        elif isinstance(item, (list, tuple)):
            total += 2
            stack.extend(item)
        elif isinstance(item, dict):
            total += 2
            for key, entry in item.items():
                total += len(str(key)) + 4
                stack.append(entry)
        else:
            total += 4
    return total


class Transport(abc.ABC):
    """Minimal duplex transport so tests and future WebSockets can plug in."""

    #: Called with a listener to invoke when the transport can accept writes
    #: again. ``None`` means the transport never reports backpressure.
    on_drain: Callable[[Callable[[], None]], None] | None = None

    @abc.abstractmethod
    def write(self, data: str) -> bool | None:
        """Write one frame.

        Return ``False`` when the transport's buffer is full: the client then
        holds further frames and resumes on the drain signal instead of piling
        more bytes onto a peer that is not keeping up. Returning ``None`` or
        ``True`` means the frame was accepted.
        """

    @abc.abstractmethod
    def on_data(self, listener: Callable[[bytes | str], None]) -> None: ...

    @abc.abstractmethod
    def on_close(self, listener: Callable[[BaseException | None], None]) -> None: ...

    @abc.abstractmethod
    def close(self) -> None: ...


#: Buffered bytes after which the socket transport reports backpressure.
#:
#: asyncio's ``StreamWriter`` buffers without bound, so unlike a Node socket it
#: never says "stop". Reporting a full buffer at the stream's own default limit
#: keeps the same contract the TypeScript transport has, so the client's outbox
#: bounds memory instead of growing until the peer catches up.
_STREAM_HIGH_WATER = 64 * 1024


class _StreamTransport(Transport):
    """Unix-domain socket transport over asyncio streams."""

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self._reader = reader
        self._writer = writer
        self._data_listeners: list[Callable[[bytes | str], None]] = []
        self._close_listeners: list[Callable[[BaseException | None], None]] = []
        self._drain_listeners: list[Callable[[], None]] = []
        self._drain_task: asyncio.Task[None] | None = None
        self._closed = False
        self.on_drain = self._drain_listeners.append
        self._pump_task = asyncio.get_running_loop().create_task(self._pump())

    async def _pump(self) -> None:
        try:
            while True:
                chunk = await self._reader.read(65536)
                if not chunk:
                    break
                for listener in list(self._data_listeners):
                    listener(chunk)
        except asyncio.CancelledError:
            raise
        except BaseException as error:
            self._finish(error)
            return
        self._finish(None)

    def _finish(self, error: BaseException | None) -> None:
        if self._closed:
            return
        self._closed = True
        for listener in list(self._close_listeners):
            listener(error)

    def write(self, data: str) -> bool | None:
        if self._closed or self._writer.is_closing():
            raise HarnessError("disconnected", "harness connection closed")
        # A peer that vanishes between the closing check and the write surfaces
        # EPIPE, sometimes synchronously. Translate it into a rejected request
        # rather than an uncaught exception; the close handler already fails
        # everything else in flight.
        try:
            self._writer.write(data.encode("utf-8"))
        except (RuntimeError, OSError) as cause:
            raise HarnessError("disconnected", f"write failed: {cause}") from cause
        if self._write_buffer_size() > _STREAM_HIGH_WATER:
            self._start_drain()
            return False
        return None

    def _write_buffer_size(self) -> int:
        transport = self._writer.transport
        getter = getattr(transport, "get_write_buffer_size", None)
        return int(getter()) if getter is not None else 0

    def _start_drain(self) -> None:
        if self._drain_task is not None and not self._drain_task.done():
            return

        async def wait_for_room() -> None:
            try:
                await self._writer.drain()
            except (RuntimeError, OSError) as error:
                self._finish(error)
                return
            for listener in list(self._drain_listeners):
                listener()

        self._drain_task = asyncio.get_running_loop().create_task(wait_for_room())

    def on_data(self, listener: Callable[[bytes | str], None]) -> None:
        self._data_listeners.append(listener)

    def on_close(self, listener: Callable[[BaseException | None], None]) -> None:
        self._close_listeners.append(listener)

    def close(self) -> None:
        self._pump_task.cancel()
        if self._drain_task is not None:
            self._drain_task.cancel()
        with contextlib.suppress(BaseException):
            self._writer.close()
        self._finish(None)


class _PipeProtocol(asyncio.Protocol):
    """Client side of a Windows named-pipe connection."""

    def __init__(self, owner: _WindowsPipeTransport) -> None:
        self._owner = owner

    def connection_made(self, transport: asyncio.BaseTransport) -> None:
        self._owner._bind(transport)

    def data_received(self, data: bytes) -> None:
        self._owner._deliver(data)

    def connection_lost(self, exc: BaseException | None) -> None:
        self._owner._finish(exc)


class _WindowsPipeTransport(Transport):
    def __init__(self) -> None:
        self._pipe: asyncio.BaseTransport | None = None
        self._data_listeners: list[Callable[[bytes | str], None]] = []
        self._close_listeners: list[Callable[[BaseException | None], None]] = []
        self._closed = False
        self._connected: asyncio.Future[None] = asyncio.get_running_loop().create_future()

    def _bind(self, transport: asyncio.BaseTransport) -> None:
        self._pipe = transport
        if not self._connected.done():
            self._connected.set_result(None)

    def _deliver(self, data: bytes) -> None:
        for listener in list(self._data_listeners):
            listener(data)

    def _finish(self, error: BaseException | None) -> None:
        if not self._connected.done():
            self._connected.set_exception(error or HarnessError("connect_failed", "pipe closed during connect"))
            return
        if self._closed:
            return
        self._closed = True
        for listener in list(self._close_listeners):
            listener(error)

    def write(self, data: str) -> bool | None:
        if self._closed or self._pipe is None:
            raise HarnessError("disconnected", "harness connection closed")
        try:
            self._pipe.write(data.encode("utf-8"))  # type: ignore[attr-defined]
        except (RuntimeError, OSError) as cause:
            raise HarnessError("disconnected", f"write failed: {cause}") from cause
        return None

    def on_data(self, listener: Callable[[bytes | str], None]) -> None:
        self._data_listeners.append(listener)

    def on_close(self, listener: Callable[[BaseException | None], None]) -> None:
        self._close_listeners.append(listener)

    def close(self) -> None:
        self._closed = True
        if self._pipe is not None:
            with contextlib.suppress(BaseException):
                self._pipe.close()
        for listener in list(self._close_listeners):
            listener(None)


async def unix_socket_transport(socket_path: str) -> Transport:
    """Connect to the harness API socket, or the named pipe derived from it on Windows."""
    # On Windows the bridge listens on a named pipe derived from this path, not
    # on the path itself.
    endpoint = transport_endpoint(socket_path)
    if sys.platform == "win32":
        loop = asyncio.get_running_loop()
        create_pipe_connection = getattr(loop, "create_pipe_connection", None)
        if create_pipe_connection is None:
            raise HarnessError(
                "connect_failed",
                "this event loop cannot open Windows named pipes; use asyncio.ProactorEventLoop "
                "(the default on Windows)",
            )
        transport = _WindowsPipeTransport()
        try:
            await create_pipe_connection(lambda: _PipeProtocol(transport), endpoint)
        except OSError as cause:
            raise _connect_error(socket_path, cause) from cause
        await transport._connected
        return transport

    try:
        reader, writer = await asyncio.open_unix_connection(path=endpoint)
    except OSError as cause:
        raise _connect_error(socket_path, cause) from cause
    return _StreamTransport(reader, writer)


def _connect_error(socket_path: str, cause: OSError) -> HarnessError:
    """Turn a dial failure into an error that names the likely cause and fix."""
    code = errno.errorcode.get(cause.errno, "unknown") if cause.errno is not None else "unknown"
    if code == "ENOENT":
        hint = (
            f"no harness API socket at {socket_path}. Start the bridge with "
            f"`horon-agent api-bridge`, or set HORON_AGENT_API_SOCKET to its path."
        )
    elif code == "ECONNREFUSED":
        hint = (
            f"nothing is listening on {socket_path}; a stale socket file is left over "
            f"from a bridge that exited. Restart the bridge."
        )
    elif code == "EACCES":
        hint = f"permission denied on {socket_path}: the socket belongs to another user."
    elif code == "ENOTSOCK":
        hint = (
            f"{socket_path} is not a socket; a stale file is left over from a bridge that "
            f"exited. Remove it and restart the bridge."
        )
    else:
        hint = f"could not connect to {socket_path}: {cause}"
    error = HarnessError("connect_failed", hint)
    error.__cause__ = cause
    return error


@dataclass
class ConnectOptions:
    """Options for :meth:`HoronAgentClient.connect`."""

    #: Defaults to the resolved harness API socket path.
    socket_path: str | None = None
    #: Client identity sent in the handshake, e.g. "my-app/1.0".
    client_name: str = "horon-agent-sdk-python"
    #: Enable only when this client renders user_response tool calls inline.
    supports_user_response: bool = False
    #: Enable only when this client renders ``ask_requested`` questions and
    #: answers them with :meth:`respond_to_ask`. Without it the server resolves
    #: every question itself and never waits for this client.
    supports_ask_requests: bool = False
    #: Maximum requests awaiting a reply at once. Defaults to 1024.
    max_pending_requests: int = 1024
    #: Supply a custom transport instead of dialing a Unix socket.
    transport: Transport | None = None
    #: Milliseconds before a request without a reply raises. 0 disables.
    request_timeout_ms: float = 30_000


@dataclass
class CreateSessionOptions:
    """Options fixed when a session is created."""

    working_dir: str | None = None
    #: Replace the entire assembled system prompt, not just its base text.
    #: Default instructions and assembled instruction/context additions are not
    #: appended. Immutable after creation and persisted by the runtime for
    #: resume. Omit for normal prompt assembly. An empty string explicitly
    #: overrides with no text.
    system_prompt: str | None = None
    #: Configured before the create request resolves, before any model turn starts.
    tools: SessionToolsOptions | None = None


@dataclass
class GlobalEventsOptions:
    """Options for :meth:`HoronAgentClient.global_events`."""

    #: Stop discovery and close every per-session child connection when set.
    signal: asyncio.Event | None = None
    #: Milliseconds between checks for newly created sessions. Defaults to 1000.
    discovery_interval_ms: float = 1_000
    #: Maximum events waiting for the consumer before the iterator fails loudly.
    max_buffered_events: int = 10_000


@dataclass(eq=False)
class AssistantTextMessage:
    """One framed assistant message inside a turn."""

    #: Stream-local correlation id, not a persisted history message id.
    message_id: str | None = None
    text: str = ""


@dataclass
class ToolCall:
    call_id: str
    name: str
    output: str
    error: str | None = None


@dataclass
class TurnUsage:
    input: int
    output: int
    cache_read_input: int | None = None


@dataclass
class TurnResult:
    """One turn's collected output."""

    #: Absent for natural completion. Failures still raise after ``on_event``.
    stop_reason: str | None = None
    stop_message: str | None = None
    #: All assistant text deltas in the turn, including process narration.
    text: str = ""
    #: Last completed assistant message, or whole-turn text on older bridges.
    final_text: str = ""
    #: Completed text messages. Empty when the bridge does not support framing.
    messages: list[AssistantTextMessage] = field(default_factory=list)
    reasoning: str = ""
    tool_calls: list[ToolCall] = field(default_factory=list)
    usage: TurnUsage | None = None


@dataclass
class StructuredTurnResult(TurnResult):
    """A turn whose final message validated against the caller's schema."""

    #: Parsed JSON value validated against the caller's schema.
    data: Any = None
    #: All attempts, including invalid retries and the final successful response.
    attempts: list[StructuredOutputAttempt] = field(default_factory=list)


_PENDING_ERROR = "harness connection closed"


class _Pending:
    __slots__ = ("future", "handle", "on_reply")

    def __init__(
        self,
        future: asyncio.Future[dict[str, Any]],
        handle: asyncio.TimerHandle | None,
        on_reply: Callable[[dict[str, Any]], None] | None,
    ) -> None:
        self.future = future
        self.handle = handle
        self.on_reply = on_reply


_STOP = object()


class EventStream:
    """Async iterator over stream events, optionally filtered to one session.

    Buffers frames that arrive between ``__anext__`` calls, so a consumer that
    awaits slow work in the loop body does not silently drop deltas.

    The buffer is bounded in both count and bytes. Ordinary frames (narration,
    deltas, snapshots) may hold ``max_buffered_events`` events and
    ``max_buffered_bytes`` estimated bytes; control frames (approvals, asks,
    cancellations, turn terminals, tool outcomes) get an extra quarter of each
    on top, so a flood of deltas cannot evict the frame that ends a turn. Past
    the ordinary budget new ordinary frames are dropped and the stream fails
    with ``event_buffer_overflow`` once the frames already queued have been
    delivered; a full control reserve closes the stream immediately with the
    same error. Either way the stream closes: a consumer never mistakes a
    truncated stream for a finished one.

    Unknown kinds can still arrive at runtime (the harness may add events
    within protocol v1), so always keep a fallback branch on ``ev``.
    """

    def __init__(
        self,
        client: HoronAgentClient,
        session_id: str | None,
        max_buffered_events: int,
        max_buffered_bytes: int,
    ) -> None:
        self._client = client
        self._session_id = session_id
        self._max_buffered_events = max_buffered_events
        self._max_buffered_bytes = max_buffered_bytes
        self._control_reserve_events = max(1, max_buffered_events // 4)
        self._control_reserve_bytes = max(1024, max_buffered_bytes // 4)

        self._queue: deque[tuple[dict[str, Any], bool, int]] = deque()
        self._ordinary_events = 0
        self._ordinary_bytes = 0
        self._control_events = 0
        self._control_bytes = 0
        self._overflow: HarnessError | None = None
        self._waiter: asyncio.Future[Any] | None = None
        self._done = False
        self._attached = True

        client._add_event_listener(self._on_event)
        client._add_close_listener(self._on_close)

    # --- async iterator protocol -------------------------------------------

    def __aiter__(self) -> EventStream:
        return self

    async def __anext__(self) -> dict[str, Any]:
        if self._queue:
            value = self._take()
            # A drained queue after an overflow closes the stream now, so the
            # listeners are released even if the consumer stops calling.
            self._deliver()
            return value
        if self._overflow is not None:
            error = self._overflow
            self._overflow = None
            self._close_now()
            raise error
        if self._done:
            raise StopAsyncIteration
        if self._waiter is not None:
            raise HarnessError("concurrent_next", "events() does not support concurrent __anext__ calls")
        self._waiter = asyncio.get_running_loop().create_future()
        try:
            result = await self._waiter
        finally:
            self._waiter = None
        if result is _STOP:
            raise StopAsyncIteration
        if isinstance(result, BaseException):
            raise result
        assert isinstance(result, dict)
        return result

    async def aclose(self) -> None:
        """Release the subscription and any buffered frames."""
        self._stop()

    # --- introspection ------------------------------------------------------

    @property
    def buffered(self) -> EventBufferStats:
        """Current queue size, so a consumer can see how far behind it is."""
        return {"events": len(self._queue), "bytes": self._ordinary_bytes + self._control_bytes}

    # --- internals ----------------------------------------------------------

    def _take(self) -> dict[str, Any]:
        event, control, size = self._queue.popleft()
        if control:
            self._control_events -= 1
            self._control_bytes -= size
        else:
            self._ordinary_events -= 1
            self._ordinary_bytes -= size
        return event

    def _detach(self) -> None:
        if self._attached:
            self._attached = False
            self._client._remove_event_listener(self._on_event)
            self._client._remove_close_listener(self._on_close)

    def _clear(self) -> None:
        self._queue.clear()
        self._ordinary_events = 0
        self._ordinary_bytes = 0
        self._control_events = 0
        self._control_bytes = 0

    def _close_now(self) -> None:
        self._done = True
        self._clear()
        self._detach()

    def _fail(self, error: HarnessError) -> None:
        """Close the stream with a terminal error, releasing everything it held."""
        self._done = True
        self._clear()
        self._detach()
        waiter = self._waiter
        self._waiter = None
        if waiter is not None and not waiter.done():
            # The consumer is already waiting: hand it the error now.
            waiter.set_exception(error)
            return
        # Nobody is waiting: the next ``__anext__`` raises once, then reports done.
        self._overflow = error

    def _stop(self) -> None:
        self._done = True
        self._overflow = None
        self._clear()
        self._detach()
        waiter = self._waiter
        self._waiter = None
        if waiter is not None and not waiter.done():
            waiter.set_result(_STOP)

    def _deliver(self) -> None:
        """Hand queued frames to a waiting consumer, then surface a recorded overflow.

        Delivering what was already accepted before failing is what keeps a
        control frame that arrived during the flood from being lost.
        """
        while self._waiter is not None and self._queue:
            waiter = self._waiter
            self._waiter = None
            if not waiter.done():
                waiter.set_result(self._take())
        if self._queue:
            return
        if self._overflow is not None:
            self._fail(self._overflow)

    def _on_event(self, frame: dict[str, Any]) -> None:
        session_id = self._session_id
        if session_id is not None and "session_id" in frame and frame.get("session_id") != session_id:
            return
        if not isinstance(frame.get("ev"), str):
            return
        if self._done:
            return
        control = frame.get("ev") in _CONTROL_EVENT_KINDS
        limit_events = self._control_reserve_events if control else self._max_buffered_events
        limit_bytes = self._control_reserve_bytes if control else self._max_buffered_bytes
        used_events = self._control_events if control else self._ordinary_events
        used_bytes = self._control_bytes if control else self._ordinary_bytes
        size = _estimate_event_bytes(frame, max(0, limit_bytes - used_bytes))
        if used_events + 1 > limit_events or used_bytes + size > limit_bytes:
            if control:
                # A control frame is the one thing a caller cannot re-derive, so
                # it closes the stream visibly rather than being dropped silently.
                self._fail(
                    HarnessError(
                        "event_buffer_overflow",
                        f"control event reserve filled {self._control_reserve_events} events / "
                        f"{self._control_reserve_bytes} bytes; the event stream was closed rather than dropping "
                        "an approval, ask, cancellation or terminal frame",
                    )
                )
                return
            if self._overflow is None:
                self._overflow = HarnessError(
                    "event_buffer_overflow",
                    f"events() consumer fell behind {self._max_buffered_events} buffered events / "
                    f"{self._max_buffered_bytes} bytes; ordinary frames were dropped and the stream is closing",
                )
            self._deliver()
            return
        self._queue.append((frame, control, size))
        if control:
            self._control_events += 1
            self._control_bytes += size
        else:
            self._ordinary_events += 1
            self._ordinary_bytes += size
        self._deliver()

    def _on_close(self, _error: BaseException | None) -> None:
        self._stop()


@dataclass
class _Child:
    client: HoronAgentClient
    stream: EventStream
    pump: asyncio.Task[None] | None = None


class GlobalEventStream:
    """Events from every session, through per-session child connections.

    The bridge has one attachment per connection, so a server-wide stream is
    implemented by discovering persisted sessions, attaching one child client to
    each, and fanning their events into this iterator. Discovery repeats so
    sessions created after subscription are included.

    Delivery is at-least-from-attach, not historical replay: events emitted
    before a child finishes attaching, or during an unexpected disconnect and
    reattach, cannot be recovered by protocol v1. Ordering is preserved within
    each session; no total ordering across sessions is claimed.

    Closing this iterator (``await stream.aclose()``), setting ``signal``, or
    closing the parent client closes every child connection. Custom transports
    are rejected because the SDK cannot safely clone an arbitrary transport into
    independent session connections.
    """

    def __init__(self, client: HoronAgentClient, options: GlobalEventsOptions) -> None:
        self._client = client
        self._signal = options.signal
        self._interval_ms = options.discovery_interval_ms
        self._max_buffered_events = options.max_buffered_events

        self._queue: deque[dict[str, Any]] = deque()
        self._children: dict[str, _Child] = {}
        self._starting: set[str] = set()
        self._start_tasks: set[asyncio.Task[None]] = set()
        self._waiter: asyncio.Future[Any] | None = None
        self._stopped = False
        self._terminal_error: BaseException | None = None
        self._scan_task: asyncio.Task[None] | None = None
        self._signal_task: asyncio.Task[None] | None = None
        self._cleanup: asyncio.Task[None] | None = None

        client._add_close_listener(self._on_parent_close)
        if options.signal is not None:
            self._signal_task = asyncio.get_running_loop().create_task(self._watch_signal(options.signal))
        if options.signal is not None and options.signal.is_set():
            asyncio.get_running_loop().create_task(self._finish(None))
        else:
            self._scan_task = asyncio.get_running_loop().create_task(self._scan_loop())

    # --- async iterator protocol -------------------------------------------

    def __aiter__(self) -> GlobalEventStream:
        return self

    async def __anext__(self) -> dict[str, Any]:
        if self._queue:
            return self._queue.popleft()
        if self._terminal_error is not None:
            error = self._terminal_error
            self._terminal_error = None
            self._stopped = True
            raise error
        if self._stopped:
            raise StopAsyncIteration
        if self._waiter is not None:
            raise HarnessError("concurrent_next", "global_events() does not support concurrent __anext__ calls")
        self._waiter = asyncio.get_running_loop().create_future()
        try:
            result = await self._waiter
        finally:
            if self._waiter is not None:
                self._waiter = None
        if result is _STOP:
            raise StopAsyncIteration
        assert isinstance(result, dict)
        return result

    async def aclose(self) -> None:
        await self._finish(None)

    # --- discovery and fan-in ----------------------------------------------

    async def _watch_signal(self, signal: asyncio.Event) -> None:
        await signal.wait()
        await self._finish(None)

    def _on_parent_close(self, _error: BaseException | None) -> None:
        if not self._stopped:
            asyncio.get_running_loop().create_task(self._finish(None))

    async def _scan_loop(self) -> None:
        while not self._stopped:
            await self._scan()
            if self._stopped or self._interval_ms <= 0:
                return
            try:
                await asyncio.sleep(self._interval_ms / 1000)
            except asyncio.CancelledError:
                return

    async def _scan(self) -> None:
        try:
            sessions = await self._client.list_sessions(include_archived=True)
        except asyncio.CancelledError:
            raise
        except BaseException as error:
            if not self._stopped:
                await self._finish(error)
            return
        if self._stopped:
            return
        tasks = [
            asyncio.get_running_loop().create_task(self._start_child(str(session["session_id"])))
            for session in sessions
            if isinstance(session, dict) and isinstance(session.get("session_id"), str)
        ]
        self._start_tasks.update(tasks)
        try:
            await asyncio.gather(*tasks, return_exceptions=True)
        finally:
            self._start_tasks.difference_update(tasks)

    async def _start_child(self, session_id: str) -> None:
        if self._stopped or session_id in self._children or session_id in self._starting:
            return
        self._starting.add(session_id)
        child: HoronAgentClient | None = None
        state: _Child | None = None
        try:
            child = await HoronAgentClient.connect(
                ConnectOptions(
                    socket_path=self._client._socket_path,
                    client_name=f"{self._client.client_name}/global-events",
                    request_timeout_ms=self._client._request_timeout_ms,
                )
            )
            if self._stopped:
                await child.close()
                return
            stream = child.events(session_id)
            state = _Child(client=child, stream=stream)
            self._children[session_id] = state
            await child.attach_session(session_id)
            if self._stopped:
                return

            async def pump() -> None:
                try:
                    async for event in stream:
                        self._enqueue(event)
                finally:
                    if self._children.get(session_id) is state:
                        del self._children[session_id]
                    await child.close()

            state.pump = asyncio.get_running_loop().create_task(pump())
        except asyncio.CancelledError:
            raise
        except BaseException as error:
            if state is not None and self._children.get(session_id) is state:
                del self._children[session_id]
            if state is not None and state.pump is not None:
                state.pump.cancel()
            if state is not None:
                await state.stream.aclose()
            if child is not None:
                await child.close()
            if not self._stopped and not (isinstance(error, HarnessError) and error.code == "unknown_session"):
                await self._finish(error)
        finally:
            self._starting.discard(session_id)

    def _enqueue(self, event: dict[str, Any]) -> None:
        if self._stopped:
            return
        waiter = self._waiter
        if waiter is not None:
            self._waiter = None
            if not waiter.done():
                waiter.set_result(event)
            return
        if len(self._queue) >= self._max_buffered_events:
            asyncio.get_running_loop().create_task(
                self._finish(
                    HarnessError(
                        "event_buffer_overflow",
                        f"global_events consumer fell behind {self._max_buffered_events} buffered events",
                    )
                )
            )
            return
        self._queue.append(event)

    async def _finish(self, failure: BaseException | None) -> None:
        if self._cleanup is not None:
            await self._cleanup
            return

        self._stopped = True
        self._terminal_error = failure
        self._queue.clear()
        for task in (self._scan_task, self._signal_task):
            if task is not None and task is not asyncio.current_task():
                task.cancel()

        waiter = self._waiter
        self._waiter = None
        if waiter is not None and not waiter.done():
            if failure is not None:
                waiter.set_exception(failure)
            else:
                waiter.set_result(_STOP)

        children = list(self._children.values())
        self._children.clear()
        pending_starts = list(self._start_tasks)

        async def close_all() -> None:
            closing: list[asyncio.Task[None]] = []
            for child in children:
                closing.append(asyncio.get_running_loop().create_task(child.stream.aclose()))
                closing.append(asyncio.get_running_loop().create_task(child.client.close()))
                if child.pump is not None:
                    child.pump.cancel()
                    closing.append(child.pump)
            for task in pending_starts:
                task.cancel()
                closing.append(task)
            if closing:
                await asyncio.gather(*closing, return_exceptions=True)

        self._cleanup = asyncio.get_running_loop().create_task(close_all())
        await self._cleanup


def _consume_future_exception(future: asyncio.Future[Any]) -> None:
    """Retrieve a stored exception so an unobserved request does not warn at GC."""
    if future.cancelled():
        return
    with contextlib.suppress(BaseException):
        future.exception()


class HoronAgentClient:
    """Connected harness client.

    Replies are correlated by the ``reply_to`` id the server echoes; anything
    without one is a stream event and is delivered by :meth:`events` and
    :meth:`global_events`.
    """

    #: Server identity from the handshake, e.g. ``"horon-agent-harness-api-bridge/0.1.0"``.
    server: str
    #: Capability strings advertised by the server.
    capabilities: list[str]

    def __init__(
        self,
        transport: Transport,
        request_timeout_ms: float = 30_000,
        socket_path: str | None = None,
        client_name: str = "horon-agent-sdk-python",
        max_pending_requests: int = 1024,
    ) -> None:
        self._transport = transport
        self._decoder: NdjsonDecoder[dict[str, Any]] = NdjsonDecoder()
        self._pending: dict[int, _Pending] = {}
        self._max_pending_requests = max_pending_requests
        self._outbox: list[str] = []
        self._transport_blocked = False
        self._request_timeout_ms = request_timeout_ms
        self._socket_path = socket_path
        self.client_name = client_name
        self._next_id = 1
        self._closed = False
        self._close_error: BaseException | None = None
        self._configuring_tools: set[str] = set()
        self._event_listeners: list[Callable[[dict[str, Any]], None]] = []
        self._close_listeners: list[Callable[[BaseException | None], None]] = []
        self._instance: LaunchedInstance | None = None
        self.server = ""
        self.capabilities = []
        self._tool_callbacks = ToolCallbacks(
            submit=self._submit_tool_result,
            report=self._report_callback_error,
        )

        transport.on_data(self._ingest)
        transport.on_close(self._handle_close)
        if transport.on_drain is not None:

            def _drained() -> None:
                self._transport_blocked = False
                try:
                    self._flush_outbox()
                except BaseException as error:
                    self._report_callback_error(error)

            transport.on_drain(_drained)

    # --- construction -------------------------------------------------------

    @staticmethod
    async def launch(
        options: LaunchOptions | None = None,
        connect_options: ConnectOptions | None = None,
    ) -> HoronAgentClient:
        """Start a private horon-agent instance and connect to it.

        This is the entry point for embedding horon-agent as an agent engine.
        The instance has its own state directory, its own sessions, and its own
        sockets, so it cannot see or disturb the horon-agent the user runs
        interactively. ``close()`` shuts it down.

        Provider logins are inherited from the user by default, because an
        instance without credentials cannot reach a model at all. Pass
        ``LaunchOptions(inherit_logins=False)`` to start empty.
        """
        instance = await launch_instance(options)
        try:
            base = connect_options or ConnectOptions()
            client = await HoronAgentClient.connect(
                ConnectOptions(
                    socket_path=instance.socket_path,
                    client_name=base.client_name,
                    supports_user_response=base.supports_user_response,
                    supports_ask_requests=base.supports_ask_requests,
                    max_pending_requests=base.max_pending_requests,
                    transport=base.transport,
                    request_timeout_ms=base.request_timeout_ms,
                )
            )
            client._instance = instance
            return client
        except BaseException:
            # A launched instance whose client never connected is a stray daemon
            # holding a temp directory open, so tear it down before re-raising.
            await instance.shutdown()
            raise

    @staticmethod
    async def connect(options: ConnectOptions | None = None) -> HoronAgentClient:
        """Connect to the horon-agent already running on this machine.

        Use this to automate the user's own horon-agent: an editor plugin, a
        status dashboard, a hotkey tool. It shares the user's live sessions, so
        anything done here is visible in their terminal. To embed horon-agent as
        an engine instead, use :meth:`launch`.
        """
        resolved = options or ConnectOptions()
        socket_path = None if resolved.transport is not None else (resolved.socket_path or api_socket_path())
        transport = resolved.transport or await unix_socket_transport(socket_path or "")
        client = HoronAgentClient(
            transport,
            request_timeout_ms=resolved.request_timeout_ms,
            socket_path=socket_path,
            client_name=resolved.client_name,
            max_pending_requests=resolved.max_pending_requests,
        )
        hello: dict[str, Any] = {
            "req": "hello",
            "min_version": API_VERSION_MAJOR,
            "max_version": API_VERSION_MAJOR,
            "client": resolved.client_name,
            "supports_user_response": resolved.supports_user_response,
            "supports_ask_requests": resolved.supports_ask_requests,
        }
        frame = await client.request(hello)
        if frame.get("ev") != "hello_ok":
            await client.close()
            raise HarnessError("handshake_failed", f"unexpected reply: {frame.get('ev')}")
        # The server string arrives off the wire, so stringify whatever it holds
        # instead of trusting the declared type.
        server = frame.get("server", "")
        client.server = server if isinstance(server, str) else str(server)
        advertised = frame.get("capabilities")
        client.capabilities = list(advertised) if isinstance(advertised, list) else []
        return client

    # --- properties ---------------------------------------------------------

    @property
    def instance_home(self) -> str | None:
        """State directory of the instance this client launched, if any."""
        return self._instance.horon_agent_home if self._instance is not None else None

    async def __aenter__(self) -> HoronAgentClient:
        return self

    async def __aexit__(self, *_exc: object) -> None:
        await self.close()

    # --- event plumbing -----------------------------------------------------

    def _add_event_listener(self, listener: Callable[[dict[str, Any]], None]) -> None:
        self._event_listeners.append(listener)

    def _remove_event_listener(self, listener: Callable[[dict[str, Any]], None]) -> None:
        with contextlib.suppress(ValueError):
            self._event_listeners.remove(listener)

    def _add_close_listener(self, listener: Callable[[BaseException | None], None]) -> None:
        self._close_listeners.append(listener)

    def _remove_close_listener(self, listener: Callable[[BaseException | None], None]) -> None:
        with contextlib.suppress(ValueError):
            self._close_listeners.remove(listener)

    def _emit_event(self, frame: dict[str, Any]) -> None:
        for listener in list(self._event_listeners):
            try:
                listener(frame)
            except BaseException as error:
                asyncio.get_running_loop().call_exception_handler(
                    {"message": "event listener raised", "exception": error}
                )

    def _report_callback_error(self, error: BaseException) -> None:
        asyncio.get_running_loop().call_exception_handler({"message": "SDK tool callback failed", "exception": error})

    def _ingest(self, chunk: bytes | str) -> None:
        try:
            frames = self._decoder.push(chunk)
        except BaseException as error:
            asyncio.get_running_loop().call_exception_handler(
                {"message": "harness frame decoding failed", "exception": error}
            )
            return
        for frame in frames:
            self._dispatch_frame(frame)

    def _dispatch_frame(self, frame: dict[str, Any]) -> None:
        reply_to = frame.get("reply_to")
        if _is_int(reply_to) and reply_to in self._pending:
            waiter = self._pending.pop(reply_to)
            if waiter.handle is not None:
                waiter.handle.cancel()
            if frame.get("ev") in ("attached", "session_forked"):
                # One attachment per connection. Old-session callbacks must not
                # survive a switch.
                self._tool_callbacks.close()
            if waiter.on_reply is not None:
                waiter.on_reply(frame)
            if not waiter.future.done():
                waiter.future.set_result(frame)
            return

        if self._drops_unnegotiated(frame):
            return

        ev = frame.get("ev")
        session_id = frame.get("session_id")
        call_id = frame.get("call_id")
        name = frame.get("name")
        if ev == "tool_call" and isinstance(session_id, str) and isinstance(call_id, str) and isinstance(name, str):
            self._tool_callbacks.dispatch(session_id, call_id, name, frame.get("input"))
        elif ev == "turn_done" and isinstance(session_id, str):
            self._tool_callbacks.abort(session_id)
        self._emit_event(frame)

    def _drops_unnegotiated(self, frame: dict[str, Any]) -> bool:
        """Whether this frame belongs to a capability the connection never negotiated.

        The server already filters, so this only matters for a server that
        ignored the negotiation: the client then keeps the same guarantee, and a
        subscriber never receives an event it has no branch for.
        """
        if frame.get("ev") not in ("snapshot_marker", "snapshot"):
            return False
        return not self.supports("snapshot_marker_v1")

    def _handle_close(self, error: BaseException | None) -> None:
        if self._closed:
            return
        self._closed = True
        self._tool_callbacks.close()
        self._close_error = error or HarnessError("disconnected", _PENDING_ERROR)
        for waiter in list(self._pending.values()):
            if waiter.handle is not None:
                waiter.handle.cancel()
            if not waiter.future.done():
                waiter.future.set_exception(self._close_error)
        self._pending.clear()
        for listener in list(self._close_listeners):
            try:
                listener(error)
            except BaseException as collision:
                asyncio.get_running_loop().call_exception_handler(
                    {"message": "close listener raised", "exception": collision}
                )

    # --- requests -----------------------------------------------------------

    def supports(self, capability: str) -> bool:
        """Whether the server advertises a capability.

        Capabilities are how a client learns what this particular server can do
        before depending on it. ``permissions`` and ``asks``, for instance, are
        how a client knows the server can deliver an approval card or an agent
        question; a server without them never sends either, so waiting for one
        waits forever. Check rather than assume.
        """
        return capability in self.capabilities

    async def request(self, request: dict[str, Any]) -> dict[str, Any]:
        """Send a raw request and return its reply frame."""
        return await self._request_frame(request)

    async def _request_frame(
        self,
        request: dict[str, Any],
        on_reply: Callable[[dict[str, Any]], None] | None = None,
    ) -> dict[str, Any]:
        if self._closed:
            raise self._close_error or HarnessError("disconnected", _PENDING_ERROR)
        if len(self._pending) >= self._max_pending_requests:
            raise HarnessError(
                "too_many_requests",
                f"{len(self._pending)} requests are already awaiting a reply "
                f"(max_pending_requests {self._max_pending_requests}); await or cancel some before sending "
                f"{request.get('req')}",
            )
        request_id = self._next_id
        self._next_id += 1
        loop = asyncio.get_running_loop()
        future: asyncio.Future[dict[str, Any]] = loop.create_future()
        future.add_done_callback(_consume_future_exception)
        handle: asyncio.TimerHandle | None = None
        if self._request_timeout_ms > 0:

            def _timed_out() -> None:
                entry = self._pending.pop(request_id, None)
                if entry is None or entry.future.done():
                    return
                entry.future.set_exception(
                    HarnessError(
                        "timeout",
                        f"no reply to {request.get('req')} within {self._request_timeout_ms}ms",
                    )
                )

            handle = loop.call_later(self._request_timeout_ms / 1000, _timed_out)
        self._pending[request_id] = _Pending(future, handle, on_reply)
        try:
            self._enqueue(encode_frame({"v": API_VERSION_MAJOR, "id": request_id, **request}))
        except BaseException:
            self._pending.pop(request_id, None)
            if handle is not None:
                handle.cancel()
            raise
        return await future

    def _enqueue(self, frame: str) -> None:
        """Hand a frame to the transport, honouring its backpressure.

        A transport that returns ``False`` is telling the caller to stop writing
        until drain; ignoring that is how a slow peer turns into unbounded memory
        in the process. Frames queue in order and leave in order, so deferring
        one never reorders the wire.
        """
        if self._closed:
            raise self._close_error or HarnessError("disconnected", _PENDING_ERROR)
        self._outbox.append(frame)
        self._flush_outbox()

    def _flush_outbox(self) -> None:
        while self._outbox:
            if self._transport_blocked:
                if self._transport.on_drain is None:
                    self._outbox.clear()
                    raise HarnessError(
                        "transport_backpressure",
                        "transport reported a full write buffer but provides no on_drain signal",
                    )
                return
            frame = self._outbox.pop(0)
            if self._transport.write(frame) is False:
                self._transport_blocked = True

    async def _request_ok(self, request: dict[str, Any]) -> dict[str, Any]:
        frame = await self.request(request)
        if frame.get("ev") == "error":
            raise HarnessError(str(frame.get("code")), str(frame.get("message")))
        return frame

    async def _expect_reply(self, request: dict[str, Any], kind: str) -> dict[str, Any]:
        """Send a request and assert the reply's event kind."""
        frame = await self._request_ok(request)
        if frame.get("ev") != kind:
            raise HarnessError("unexpected_reply", f"expected {kind}, got {frame.get('ev')}")
        return frame

    def notify(self, request: dict[str, Any]) -> None:
        """Write a request without expecting a request-level reply."""
        if self._closed:
            raise self._close_error or HarnessError("disconnected", _PENDING_ERROR)
        request_id = self._next_id
        self._next_id += 1
        self._enqueue(encode_frame({"v": API_VERSION_MAJOR, "id": request_id, **request}))

    def _require_capability(self, capability: str, surface: str) -> None:
        if self.supports(capability):
            return
        raise HarnessError(
            "unsupported",
            f"{surface} requires the `{capability}` capability; update and restart the daemon and the API bridge",
        )

    async def _wait_for_event(self, predicate: Callable[[dict[str, Any]], bool], timeout_ms: float) -> None:
        """Resolve on the first event matching ``predicate``, or on timeout.

        Resolving on timeout rather than raising keeps a missing ack from failing
        an otherwise healthy turn: the stream is the source of truth.
        """
        loop = asyncio.get_running_loop()
        future: asyncio.Future[None] = loop.create_future()
        future.add_done_callback(_consume_future_exception)

        def on_event(frame: dict[str, Any]) -> None:
            if not future.done() and predicate(frame):
                future.set_result(None)

        def on_close(_error: BaseException | None) -> None:
            if not future.done():
                future.set_result(None)

        def on_timeout() -> None:
            if not future.done():
                future.set_result(None)

        self._add_event_listener(on_event)
        self._add_close_listener(on_close)
        handle = loop.call_later(timeout_ms / 1000, on_timeout)
        try:
            await future
        finally:
            handle.cancel()
            self._remove_event_listener(on_event)
            self._remove_close_listener(on_close)

    # --- sessions -----------------------------------------------------------

    async def list_sessions(
        self,
        *,
        include_archived: bool | None = None,
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        request: dict[str, Any] = {"req": "list_sessions"}
        if include_archived is not None:
            request["include_archived"] = include_archived
        if limit is not None:
            request["limit"] = limit
        frame = await self._expect_reply(request, "sessions")
        sessions = frame.get("sessions")
        return list(sessions) if isinstance(sessions, list) else []

    async def list_sessions_limited(self, limit: int) -> list[dict[str, Any]]:
        return await self.list_sessions(limit=limit)

    async def archive_session(self, session_id: str) -> None:
        """Reversibly hide a session from the default list. No transcript is deleted."""
        await self._request_ok({"req": "archive_session", "session_id": session_id})

    async def restore_session(self, session_id: str) -> None:
        await self._request_ok({"req": "restore_session", "session_id": session_id})

    async def set_retention_policy(self, archive_after_days: int | None = None) -> None:
        """Auto-archive inactive sessions after this many days; omit to disable."""
        request: dict[str, Any] = {"req": "set_retention_policy"}
        if archive_after_days is not None:
            request["archive_after_days"] = archive_after_days
        await self._request_ok(request)

    async def create_session(self, working_dir: str | None = None) -> dict[str, Any]:
        """Create a session in ``working_dir``, or the runtime default."""
        return await self.create_session_with_options(CreateSessionOptions(working_dir=working_dir))

    async def create_session_with_options(
        self,
        options: CreateSessionOptions | None = None,
        *,
        working_dir: str | None = None,
        system_prompt: str | None = None,
        tools: SessionToolsOptions | None = None,
    ) -> dict[str, Any]:
        """Create a session, optionally replacing the system prompt or configuring tools.

        Pass either a :class:`CreateSessionOptions` or the individual keywords.
        """
        if options is not None and any(value is not None for value in (working_dir, system_prompt, tools)):
            raise HarnessError("invalid_option", "pass CreateSessionOptions or keyword arguments, not both")
        resolved = options or CreateSessionOptions(
            working_dir=working_dir,
            system_prompt=system_prompt,
            tools=tools,
        )
        if resolved.tools is not None:
            self._require_session_tools()
            prepare_tools(resolved.tools)
        request: dict[str, Any] = {"req": "create_session"}
        if resolved.working_dir is not None:
            request["working_dir"] = resolved.working_dir
        if resolved.system_prompt is not None:
            request["system_prompt"] = resolved.system_prompt
        frame = await self._expect_reply(request, "attached")
        session = frame.get("session")
        if not isinstance(session, dict):
            raise HarnessError("unexpected_reply", "attached without a session")
        if resolved.tools is not None:
            await self.configure_tools(str(session.get("session_id")), resolved.tools)
        return session

    def _require_session_tools(self) -> None:
        if not self.supports("session_tools"):
            raise HarnessError(
                "unsupported",
                "This harness does not support session_tools. Update the horon-agent runtime and API bridge.",
            )

    async def configure_tools(self, session_id: str, options: SessionToolsOptions) -> None:
        """Replace the session tool policy while idle. Await before sending another message."""
        self._require_session_tools()
        if session_id in self._configuring_tools:
            raise HarnessError("busy", "A tool configuration request is already in flight for this session")
        prepared = prepare_tools(options)
        self._configuring_tools.add(session_id)

        def on_reply(reply: dict[str, Any]) -> None:
            # Synchronous with ingest: a callback in the same network chunk sees
            # the new table. Until this ack, an existing turn must continue using
            # the old table.
            if reply.get("ev") == "ok":
                self._tool_callbacks.sessions[session_id] = prepared.handlers

        try:
            frame = await self._request_frame(
                {"req": "configure_tools", "session_id": session_id, "tools": prepared.wire},
                on_reply,
            )
            if frame.get("ev") == "error":
                raise HarnessError(str(frame.get("code")), str(frame.get("message")))
            if frame.get("ev") != "ok":
                raise HarnessError("unexpected_reply", f"expected ok, got {frame.get('ev')}")
        except BaseException as error:
            # An ambiguous outcome must not pair new remote definitions with old callbacks.
            if isinstance(error, HarnessError) and error.code in ("timeout", "unexpected_reply"):
                asyncio.get_running_loop().create_task(self.close())
            raise
        finally:
            self._configuring_tools.discard(session_id)

    async def list_tools(self, session_id: str) -> list[SessionToolDefinition]:
        """Inspect the effective model-visible tool definitions."""
        self._require_session_tools()
        frame = await self._expect_reply({"req": "list_tools", "session_id": session_id}, "tools")
        tools = frame.get("tools")
        return list(tools) if isinstance(tools, list) else []

    async def get_message_modes(self, session_id: str) -> MessageModes:
        """Read the daemon's ``[messages]`` delivery modes.

        These are daemon configuration rather than session state, so the answer
        does not depend on the session existing.
        """
        frame = await self._expect_reply({"req": "get_message_modes", "session_id": session_id}, "message_modes")
        modes = frame.get("modes")
        return cast(MessageModes, modes if isinstance(modes, dict) else {})

    async def submit_tool_result(
        self,
        session_id: str,
        call_id: str,
        result: str | ToolResult,
    ) -> None:
        """Low-level response API for applications handling ``tool_call`` events themselves."""
        value = ToolResult(output=result) if isinstance(result, str) else result
        request: dict[str, Any] = {
            "req": "tool_result",
            "session_id": session_id,
            "call_id": call_id,
            "output": value.output,
        }
        if value.error is not None:
            request["error"] = value.error
        await self._expect_reply(request, "ok")

    async def _submit_tool_result(self, session_id: str, call_id: str, result: ToolResult) -> None:
        await self.submit_tool_result(session_id, call_id, result)

    async def attach_session(self, session_id: str) -> dict[str, Any]:
        frame = await self._expect_reply({"req": "attach_session", "session_id": session_id}, "attached")
        session = frame.get("session")
        if not isinstance(session, dict):
            raise HarnessError("unexpected_reply", "attached without a session")
        return session

    async def fork_session(self, session_id: str) -> dict[str, Any]:
        """Clone a session's complete persisted context into a new idle session."""
        frame = await self._expect_reply({"req": "fork_session", "session_id": session_id}, "session_forked")
        session = frame.get("session")
        if not isinstance(session, dict):
            raise HarnessError("unexpected_reply", "session_forked without a session")
        return session

    async def transfer_session(self, session_id: str) -> dict[str, Any]:
        frame = await self._expect_reply({"req": "transfer_session", "session_id": session_id}, "session_forked")
        session = frame.get("session")
        if not isinstance(session, dict):
            raise HarnessError("unexpected_reply", "session_forked without a session")
        return session

    async def detach_session(self, session_id: str) -> None:
        await self._request_ok({"req": "detach_session", "session_id": session_id})
        self._tool_callbacks.abort(session_id)
        self._tool_callbacks.sessions.pop(session_id, None)

    # --- messages and turns ------------------------------------------------

    async def send_message(
        self,
        session_id: str,
        content: str,
        images: list[tuple[str, str]] | None = None,
        *,
        no_reply: bool = False,
        wait_for_accept: bool = True,
        accept_timeout_ms: float = 10_000,
    ) -> None:
        """Send a user message.

        The harness does not reply to ``send_message`` at the request level: it
        acknowledges by emitting ``message_accepted`` once the agent has the
        message. Awaiting a reply here would always time out, so the frame is
        written and, by default, the acknowledgement event is awaited instead.
        Pass ``wait_for_accept=False`` for pure fire-and-forget.
        """
        attached_images = images or []
        if no_reply:
            await self._request_ok(
                {
                    "req": "send_message",
                    "session_id": session_id,
                    "content": content,
                    "images": attached_images,
                    "no_reply": True,
                }
            )
            return
        accepted: asyncio.Task[None] | None = None
        if wait_for_accept:
            accepted = asyncio.get_running_loop().create_task(
                self._wait_for_event(
                    lambda frame: frame.get("ev") == "message_accepted" and frame.get("session_id") == session_id,
                    accept_timeout_ms,
                )
            )
        try:
            self.notify(
                {
                    "req": "send_message",
                    "session_id": session_id,
                    "content": content,
                    "images": attached_images,
                }
            )
            if accepted is not None:
                await accepted
        finally:
            if accepted is not None and not accepted.done():
                accepted.cancel()

    async def send_system_reminder(self, session_id: str, reminder: str) -> None:
        """Submit a hidden recovery continuation without awaiting message acceptance."""
        self.notify(
            {
                "req": "send_message",
                "session_id": session_id,
                "content": "",
                "system_reminder": reminder,
            }
        )

    async def send_context_message(self, session_id: str, content: str) -> None:
        """Persist a user message as context without starting a model turn."""
        await self._request_ok({"req": "send_message", "session_id": session_id, "content": content, "no_reply": True})

    async def send_message_with_reminder(
        self,
        session_id: str,
        content: str,
        images: list[tuple[str, str]] | None = None,
        system_reminder: str | None = None,
        *,
        wait_for_accept: bool = True,
        accept_timeout_ms: float = 10_000,
    ) -> None:
        """Send a user message with a hidden system reminder for this turn only.

        The reminder guides the model without appearing in the visible transcript.
        """
        reminder = system_reminder.strip() if system_reminder is not None else ""
        accepted: asyncio.Task[None] | None = None
        if wait_for_accept:
            accepted = asyncio.get_running_loop().create_task(
                self._wait_for_event(
                    lambda frame: frame.get("ev") == "message_accepted" and frame.get("session_id") == session_id,
                    accept_timeout_ms,
                )
            )
        try:
            request: dict[str, Any] = {
                "req": "send_message",
                "session_id": session_id,
                "content": content,
                "images": images or [],
            }
            if reminder:
                request["system_reminder"] = reminder
            self.notify(request)
            if accepted is not None:
                await accepted
        finally:
            if accepted is not None and not accepted.done():
                accepted.cancel()

    async def cancel(self, session_id: str) -> None:
        await self._request_ok({"req": "cancel", "session_id": session_id})
        self._tool_callbacks.abort(session_id)

    async def soft_interrupt(self, session_id: str, content: str, urgent: bool = False) -> None:
        await self.soft_interrupt_with_images(session_id, content, [], urgent)

    async def soft_interrupt_with_images(
        self,
        session_id: str,
        content: str,
        images: list[tuple[str, str]],
        urgent: bool = False,
    ) -> None:
        await self.soft_interrupt_with_id(session_id, content, images=images, urgent=urgent)

    async def soft_interrupt_with_id(
        self,
        session_id: str,
        content: str,
        *,
        client_id: str | None = None,
        images: list[tuple[str, str]] | None = None,
        urgent: bool = False,
    ) -> None:
        """Queue a message for the running turn, tagged with an opaque ``client_id``.

        The id comes back in a ``soft_interrupt_injected`` event once the model
        has the message, and :meth:`cancel_soft_interrupt` can take it back
        before then. Needs the ``soft_interrupt_ids`` capability; an older server
        accepts the message but drops the id.
        """
        request: dict[str, Any] = {
            "req": "soft_interrupt",
            "session_id": session_id,
            "content": content,
            "urgent": urgent,
        }
        if images:
            request["images"] = images
        if client_id is not None:
            request["client_id"] = client_id
        await self._request_ok(request)

    async def get_history(self, session_id: str) -> list[dict[str, Any]]:
        result = await self.get_history_with_images(session_id)
        return cast(list[dict[str, Any]], result["messages"])

    async def get_history_with_images(
        self,
        session_id: str,
    ) -> dict[str, Any]:
        frame = await self._expect_reply({"req": "get_history", "session_id": session_id}, "history")
        messages = frame.get("messages")
        images = frame.get("images")
        return {
            "messages": list(messages) if isinstance(messages, list) else [],
            "images": list(images) if isinstance(images, list) else [],
        }

    async def peek_session(self, session_id: str, limit: int | None = None) -> list[dict[str, Any]]:
        request: dict[str, Any] = {"req": "peek_session", "session_id": session_id}
        if limit is not None:
            request["limit"] = limit
        frame = await self._expect_reply(request, "history")
        messages = frame.get("messages")
        return list(messages) if isinstance(messages, list) else []

    async def clear(self, session_id: str) -> None:
        await self._request_ok({"req": "clear", "session_id": session_id})

    async def rewind(self, session_id: str, message_index: int) -> None:
        await self._request_ok({"req": "rewind", "session_id": session_id, "message_index": message_index})

    async def rewind_undo(self, session_id: str) -> None:
        """Restore the history the last :meth:`rewind` removed."""
        await self._request_ok({"req": "rewind_undo", "session_id": session_id})

    async def respond_to_permission(self, session_id: str, request_id: str, decision: str) -> None:
        await self._request_ok(
            {
                "req": "permission_response",
                "session_id": session_id,
                "request_id": request_id,
                "decision": decision,
            }
        )

    async def respond_to_ask(self, session_id: str, ask_id: str, answers: list[dict[str, Any]]) -> None:
        """Answer an ``ask_requested`` event.

        ``ask_id`` echoes the event. A reply for a question set that is no longer
        pending raises, so a late answer cannot resolve a newer question.
        """
        await self._request_ok(
            {
                "req": "ask_response",
                "session_id": session_id,
                "ask_id": ask_id,
                "answers": answers,
            }
        )

    async def respond_to_ask_with_id(
        self,
        session_id: str,
        ask_id: str,
        answer_id: str,
        answers: list[dict[str, Any]],
    ) -> None:
        """Answer an ``ask_requested`` with a retry-safe answer id. Requires ``ask_receipts_v1``."""
        self._require_capability("ask_receipts_v1", "respond_to_ask_with_id")
        await self._request_ok(
            {
                "req": "ask_response",
                "session_id": session_id,
                "ask_id": ask_id,
                "answers": answers,
                "answer_id": answer_id,
            }
        )

    async def list_models(self, session_id: str) -> dict[str, Any]:
        """Models this session can switch to, and which one is serving it."""
        frame = await self._expect_reply({"req": "list_models", "session_id": session_id}, "models")
        models = frame.get("models")
        result: dict[str, Any] = {"models": list(models) if isinstance(models, list) else []}
        if "current" in frame:
            result["current"] = frame["current"]
        return result

    async def runtime_command(
        self,
        session_id: str,
        command: str,
        args: list[str] | None = None,
    ) -> dict[str, Any]:
        """Runtime identity, route catalog, protocol metadata, and a live health check."""
        if "runtime_commands" not in self.capabilities:
            raise HarnessError(
                "unknown_request",
                "This runtime does not support structured commands. Update and restart the daemon and bridge.",
            )
        frame = await self._expect_reply(
            {"req": "runtime_command", "session_id": session_id, "command": command, "args": args or []},
            "runtime_command_result",
        )
        return {
            "session_id": frame.get("session_id"),
            "command": frame.get("command"),
            "data": frame.get("data"),
            "message": frame.get("message"),
        }

    async def get_runtime_info(self, session_id: str) -> RuntimeInfo:
        await self.ping()
        frame = await self._expect_reply({"req": "get_runtime_info", "session_id": session_id}, "runtime_info")
        routes = frame.get("routes")
        route_list: list[dict[str, Any]] = list(routes) if isinstance(routes, list) else []
        providers: list[str] = []
        provider = frame.get("provider")
        for candidate in [provider, *(route.get("provider") for route in route_list)]:
            if isinstance(candidate, str) and candidate not in providers:
                providers.append(candidate)
        info: RuntimeInfo = {
            "server": self.server,
            "protocol_version": API_VERSION_MAJOR,
            "capabilities": list(self.capabilities),
            "healthy": True,
            "session_id": str(frame.get("session_id", "")),
            "providers": providers,
            "routes": route_list,
        }
        if isinstance(provider, str):
            info["provider"] = provider
        if isinstance(frame.get("model"), str):
            info["model"] = frame["model"]
        fallbacks = frame.get("fallback_models")
        if isinstance(fallbacks, list):
            info["fallback_models"] = [model for model in fallbacks if isinstance(model, str)]
        if isinstance(frame.get("serving_model"), str):
            info["serving_model"] = frame["serving_model"]
        return info

    # --- credentials --------------------------------------------------------

    async def set_api_key(self, provider: str, api_key: str) -> None:
        """Persist an API key in horon-agent's owner-only provider store and hot-reload it."""
        await self._expect_reply({"req": "set_api_key", "provider": provider, "api_key": api_key}, "credential_updated")

    async def clear_api_key(self, provider: str) -> None:
        await self._expect_reply({"req": "clear_api_key", "provider": provider}, "credential_updated")

    async def notify_auth_changed(self, provider: str) -> None:
        """Reload credentials saved by an out-of-band OAuth login. No secrets or chat messages."""
        await self._expect_reply({"req": "notify_auth_changed", "provider": provider}, "ok")

    async def invalidate_usage(self, provider: str, account_label: str | None = None) -> None:
        """Tell the daemon a banked usage reset was redeemed for one subscription login.

        Carries no credentials.
        """
        request: dict[str, Any] = {"req": "invalidate_usage", "provider": provider}
        if account_label is not None:
            request["account_label"] = account_label
        await self._expect_reply(request, "ok")

    # --- files --------------------------------------------------------------

    async def read_file(self, session_id: str, path: str, max_bytes: int | None = None) -> FileContent:
        request: dict[str, Any] = {"req": "read_file", "session_id": session_id, "path": path}
        if max_bytes is not None:
            request["max_bytes"] = max_bytes
        frame = await self._expect_reply(request, "file_content")
        return {
            "path": str(frame.get("path", "")),
            "content": str(frame.get("content", "")),
            "size": int(frame.get("size", 0)),
            "truncated": bool(frame.get("truncated", False)),
        }

    async def find_files(self, session_id: str, query: str, limit: int | None = None) -> list[str]:
        request: dict[str, Any] = {"req": "find_files", "session_id": session_id, "query": query}
        if limit is not None:
            request["limit"] = limit
        frame = await self._expect_reply(request, "files")
        paths = frame.get("paths")
        return [str(path) for path in paths] if isinstance(paths, list) else []

    async def search_text(
        self,
        session_id: str,
        query: str,
        *,
        path: str | None = None,
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        request: dict[str, Any] = {"req": "search_text", "session_id": session_id, "query": query}
        if path is not None:
            request["path"] = path
        if limit is not None:
            request["limit"] = limit
        frame = await self._expect_reply(request, "text_matches")
        matches = frame.get("matches")
        return list(matches) if isinstance(matches, list) else []

    async def file_status(self, session_id: str, path: str) -> FileStatus:
        frame = await self._expect_reply(
            {"req": "file_status", "session_id": session_id, "path": path},
            "file_status",
        )
        status: FileStatus = {
            "path": str(frame.get("path", "")),
            "exists": bool(frame.get("exists", False)),
            "kind": str(frame.get("kind", "")),
        }
        if isinstance(frame.get("size"), int):
            status["size"] = frame["size"]
        if isinstance(frame.get("modified_ms"), int):
            status["modified_ms"] = frame["modified_ms"]
        return status

    # --- todos and session settings ----------------------------------------

    async def todo_state(self, session_id: str) -> dict[str, Any]:
        """Persisted todo state for client-side auto-poke and quality gates.

        Requires the ``todo_state`` capability.
        """
        frame = await self._expect_reply({"req": "get_todo_state", "session_id": session_id}, "todo_state")
        state = frame.get("state")
        return state if isinstance(state, dict) else {}

    async def ack_todo_follow_up(self, session_id: str, effects: dict[str, Any]) -> None:
        """Apply a follow-up decision's persistent effects."""
        if not effects.get("clear_gate_observations") and not effects.get("mark_long_session_review_delivered"):
            return
        await self._expect_reply(
            {"req": "ack_todo_follow_up", "session_id": session_id, "effects": effects},
            "ok",
        )

    async def set_model(self, session_id: str, model: str) -> None:
        """Switch the session to a different model.

        ``model`` is an id from :meth:`list_models`. Raises ``invalid_request``
        when the model is unknown or the provider refuses it, rather than
        silently leaving the session on the old one.
        """
        await self._request_ok({"req": "set_model", "session_id": session_id, "model": model})

    async def set_reasoning_effort(self, session_id: str, effort: str) -> None:
        """Set how much the model deliberates before answering.

        Typically "minimal" | "low" | "medium" | "high" | "xhigh" | "max", but
        the accepted set is per-provider, so this takes a string and reports what
        the provider says rather than guessing at a union that would go stale.
        """
        await self._request_ok({"req": "set_reasoning_effort", "session_id": session_id, "effort": effort})

    async def compact(self, session_id: str) -> str:
        """Schedule compaction of the transcript so far, freeing context.

        Not synchronous: the daemon summarizes at the next safe point rather than
        interrupting a turn, so this resolving means the request was accepted.
        Read the history afterwards to see the result. A refusal (nothing to
        compact, a turn in flight) raises ``invalid_request`` with the reason.
        """
        frame = await self._expect_reply({"req": "compact", "session_id": session_id}, "compacted")
        return str(frame.get("message", ""))

    async def compact_handoff(self, session_id: str) -> dict[str, Any]:
        """Write a handoff document as the compaction boundary.

        Synchronous, unlike :meth:`compact`: this resolves after the boundary is
        written, and the returned ``handoff`` mapping names the outcome
        (``committed`` with the covered count, ``nothing_to_cover``,
        ``provider_error`` with its detail, or ``cancelled``). A failure or a
        cancellation leaves the transcript unchanged and is reported as that
        outcome rather than raised.
        """
        frame = await self._expect_reply({"req": "compact", "session_id": session_id, "method": "handoff"}, "compacted")
        outcome = frame.get("handoff")
        if not isinstance(outcome, dict):
            raise HarnessError("unexpected_reply", "the daemon answered a handoff compaction without an outcome")
        return dict(outcome)

    async def rename_session(self, session_id: str, title: str | None = None) -> None:
        """Set a session's title. Omit ``title`` to restore the generated one."""
        request: dict[str, Any] = {"req": "rename_session", "session_id": session_id}
        if title is not None:
            request["title"] = title
        await self._request_ok(request)

    async def set_session_saved(self, session_id: str, saved: bool, label: str | None = None) -> None:
        """Bookmark or unbookmark a session. A label also becomes its title."""
        request: dict[str, Any] = {"req": "set_session_saved", "session_id": session_id, "saved": saved}
        if label is not None:
            request["label"] = label
        await self._request_ok(request)

    # --- soft interrupt cancellation ---------------------------------------

    async def cancel_soft_interrupts(self, session_id: str) -> None:
        """Drop soft interrupts that are queued but not yet delivered."""
        await self._request_ok({"req": "cancel_soft_interrupts", "session_id": session_id})

    async def cancel_soft_interrupts_by_id(
        self,
        session_id: str,
        client_ids: list[str],
    ) -> dict[str, list[str]]:
        """Take back specific queued soft interrupts by ``client_id``.

        Decided atomically against delivery: an id in ``cancelled`` never reached
        the model, and one in ``not_queued`` already did (or was never queued).
        Needs the ``soft_interrupt_ids`` capability.
        """
        frame = await self._expect_reply(
            {"req": "cancel_soft_interrupts", "session_id": session_id, "client_ids": client_ids},
            "soft_interrupts_cancelled",
        )
        cancelled = frame.get("cancelled")
        not_queued = frame.get("not_queued")
        return {
            "cancelled": list(cancelled) if isinstance(cancelled, list) else [],
            "not_queued": list(not_queued) if isinstance(not_queued, list) else [],
        }

    async def cancel_soft_interrupt(self, session_id: str, client_id: str) -> bool:
        """Take back one queued soft interrupt.

        Returns ``True`` when it was removed before the model saw it, ``False``
        when it was already delivered.
        """
        result = await self.cancel_soft_interrupts_by_id(session_id, [client_id])
        return client_id in result["cancelled"]

    async def background_tool(self, session_id: str) -> None:
        """Move the running tool call to the background."""
        await self.background_tool_result(session_id)

    async def background_tool_result(self, session_id: str) -> dict[str, Any]:
        """Move the running tool call to the background and report what moved.

        A server without the ``background_tool_result`` capability only answers
        ``ok``, reported as ``{"moved": False}`` even if a tool was moved.
        """
        frame = await self._request_ok({"req": "background_tool", "session_id": session_id})
        if frame.get("ev") != "background_tool_result":
            return {"moved": False}
        result: dict[str, Any] = {"moved": frame.get("moved") is True}
        if isinstance(frame.get("tool_call_id"), str):
            result["tool_call_id"] = frame["tool_call_id"]
        if isinstance(frame.get("tool_name"), str):
            result["tool_name"] = frame["tool_name"]
        return result

    async def ping(self) -> None:
        await self._request_ok({"req": "ping"})

    # --- durable runtime ----------------------------------------------------

    async def get_runtime_state(self, session_id: str) -> dict[str, Any]:
        """Run, queue, receipt, and monitor summary. Requires ``durable_runtime_v1``."""
        self._require_capability("durable_runtime_v1", "get_runtime_state")
        frame = await self._expect_reply({"req": "get_runtime_state", "session_id": session_id}, "runtime_state")
        state = frame.get("state")
        return state if isinstance(state, dict) else {}

    async def apply_control(
        self,
        session_id: str,
        operation_id: str,
        action: str,
        args: Any = None,
    ) -> dict[str, Any]:
        """Apply one control operation under a caller-chosen id.

        Requires ``durable_runtime_v1``. The same id reports the recorded receipt
        instead of applying the operation twice.
        """
        self._require_capability("durable_runtime_v1", "apply_control")
        request: dict[str, Any] = {
            "req": "apply_control",
            "session_id": session_id,
            "operation_id": operation_id,
            "action": action,
        }
        if args is not None:
            request["args"] = args
        frame = await self._expect_reply(request, "control_receipt")
        receipt = frame.get("receipt")
        return receipt if isinstance(receipt, dict) else {}

    async def get_control_receipt(self, session_id: str, operation_id: str) -> dict[str, Any]:
        """The recorded state of one control operation. Requires ``durable_runtime_v1``."""
        self._require_capability("durable_runtime_v1", "get_control_receipt")
        frame = await self._expect_reply(
            {"req": "get_control_receipt", "session_id": session_id, "operation_id": operation_id},
            "control_receipt",
        )
        receipt = frame.get("receipt")
        return receipt if isinstance(receipt, dict) else {}

    async def get_snapshot(
        self, session_id: str, *, tail: int | None = None, max_bytes: int | None = None
    ) -> dict[str, Any]:
        """One consistent state view plus a bounded history tail.

        The server also sends a ``snapshot_marker`` event on this connection.
        Requires ``snapshot_marker_v1``.
        """
        self._require_capability("snapshot_marker_v1", "get_snapshot")
        request: dict[str, Any] = {"req": "get_snapshot", "session_id": session_id}
        if tail is not None:
            request["tail"] = tail
        if max_bytes is not None:
            request["max_bytes"] = max_bytes
        frame = await self._expect_reply(request, "snapshot")
        snapshot = frame.get("snapshot")
        return snapshot if isinstance(snapshot, dict) else {}

    async def snapshot_with_buffered_events(
        self,
        session_id: str,
        *,
        tail: int | None = None,
        max_bytes: int | None = None,
        max_buffered_bytes: int = 1024 * 1024,
    ) -> dict[str, Any]:
        """Take a snapshot and deliver the events that follow its marker.

        A live event can reach this connection before the ``snapshot`` reply
        does, so a consumer that subscribes only after awaiting
        :meth:`get_snapshot` would drop it. This buffers events while the request
        is in flight, then releases them in order once the marker arrives. The
        buffer is capped: a server that never sends the marker would otherwise
        grow it without bound, so past the ceiling this raises an explicit
        reconnect error instead of holding a partial view.

        Returns ``{"snapshot": ..., "events": [...]}``. Requires
        ``snapshot_marker_v1``.
        """
        self._require_capability("snapshot_marker_v1", "snapshot_with_buffered_events")
        ceiling = max_buffered_bytes
        buffered: list[dict[str, Any]] = []
        buffered_bytes = 0
        marker_seen = False
        overflow: Exception | None = None
        done = False

        def on_event(frame: dict[str, Any]) -> None:
            nonlocal buffered_bytes, marker_seen, overflow
            if done:
                return
            # Only the bytes BEFORE the marker can overflow: after it the view is
            # bounded by the server, so counting them would make a busy session
            # look like a missing marker.
            if not marker_seen:
                buffered_bytes += len(json.dumps(frame, ensure_ascii=False))
                if buffered_bytes > ceiling:
                    overflow = HarnessError(
                        "snapshot_marker_missing",
                        f"snapshot marker never arrived within {ceiling} buffered bytes; "
                        "reconnect before trusting this view",
                    )
                    return
            buffered.append(frame)
            if frame.get("ev") == "snapshot_marker":
                marker_seen = True

        self._add_event_listener(on_event)
        try:
            snapshot = await self.get_snapshot(session_id, tail=tail, max_bytes=max_bytes)
            done = True
            if overflow is not None:
                raise overflow
            # Everything buffered, in arrival order: the marker, then the live
            # events that raced the reply, so a consumer can prove the boundary
            # itself.
            return {"snapshot": snapshot, "events": buffered}
        finally:
            done = True
            self._remove_event_listener(on_event)

    # --- replication and cursor history ------------------------------------

    async def _replication_request(self, request: dict[str, Any]) -> dict[str, Any]:
        probe = json.dumps(request, ensure_ascii=False)
        if len(probe.encode("utf-8")) + 64 > MAX_REPLICATION_WIRE_BYTES:
            raise HarnessError(
                "invalid_request",
                f"replication request exceeds the {MAX_REPLICATION_WIRE_BYTES} byte wire envelope",
            )
        return await self._request_ok(request)

    async def _replication_reply(self, request: dict[str, Any], kind: str) -> dict[str, Any]:
        frame = await self._replication_request(request)
        if frame.get("ev") != kind:
            raise HarnessError("unexpected_reply", f"expected {kind}, got {frame.get('ev')}")
        return frame

    async def list_session_heads(self, session_id: str) -> dict[str, Any]:
        """Durable heads and the host-local selection. Requires ``session_event_store_v1``."""
        self._require_capability("session_event_store_v1", "list_session_heads")
        frame = await self._replication_reply({"req": "list_session_heads", "session_id": session_id}, "session_heads")
        return _heads(frame)

    async def select_session_head(self, session_id: str, head_event_id: str) -> dict[str, Any]:
        """Select the head execution should use. A live owner answers ``busy``."""
        self._require_capability("session_event_store_v1", "select_session_head")
        frame = await self._replication_reply(
            {"req": "select_session_head", "session_id": session_id, "head_event_id": head_event_id},
            "session_heads",
        )
        return _heads(frame)

    async def get_history_page(
        self,
        session_id: str,
        *,
        head_event_id: str | None = None,
        before_event_id: str | None = None,
        limit: int | None = None,
    ) -> dict[str, Any]:
        """One page of visible history on a fixed head, oldest first.

        Requires ``session_history_cursor_v1``.
        """
        self._require_capability("session_history_cursor_v1", "get_history_page")
        request: dict[str, Any] = {"req": "get_history_page", "session_id": session_id}
        if head_event_id is not None:
            request["head_event_id"] = head_event_id
        if before_event_id is not None:
            request["before_event_id"] = before_event_id
        if limit is not None:
            request["limit"] = limit
        frame = await self._replication_reply(request, "session_history_page")
        page: dict[str, Any] = {
            "session_id": frame.get("session_id"),
            "head": frame.get("head"),
            "messages": frame.get("messages", []),
        }
        if "next_cursor" in frame:
            page["next_cursor"] = frame["next_cursor"]
        return page

    async def export_session_events(
        self,
        session_id: str,
        *,
        heads: list[str] | None = None,
        known_ids: list[str] | None = None,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        """Export one bounded parent-first page of original events.

        Requires ``session_replication_v1``.
        """
        self._require_capability("session_replication_v1", "export_session_events")
        request: dict[str, Any] = {
            "req": "export_session_events",
            "session_id": session_id,
            "protocol_version": REPLICATION_PROTOCOL_VERSION,
        }
        if heads is not None:
            request["heads"] = heads
        if known_ids is not None:
            request["known_ids"] = known_ids
        if cursor is not None:
            request["cursor"] = cursor
        frame = await self._replication_reply(request, "session_event_page")
        page: dict[str, Any] = {
            "session_id": frame.get("session_id"),
            "heads": frame.get("heads", []),
            "events": frame.get("events", []),
        }
        for key in ("next_cursor", "blob_refs", "record_refs"):
            if key in frame:
                page[key] = frame[key]
        return page

    async def export_session_markdown(
        self,
        session_id: str,
    ) -> dict[str, Any]:
        """Export the conversation as a Markdown transcript.

        Requires ``session_replication_v1``.
        """
        self._require_capability("session_replication_v1", "export_session_markdown")
        request: dict[str, Any] = {"req": "export_session_markdown", "session_id": session_id}
        frame = await self._replication_reply(request, "session_markdown")
        return {"session_id": frame.get("session_id"), "markdown": frame.get("markdown", "")}

    async def import_session_events(
        self,
        page: dict[str, Any],
        retry_cursor: str | None = None,
    ) -> dict[str, Any]:
        """Import one validated page. Identical retransmissions are idempotent."""
        self._require_capability("session_replication_v1", "import_session_events")
        request: dict[str, Any] = {
            "req": "import_session_events",
            "session_id": page.get("session_id"),
            "protocol_version": REPLICATION_PROTOCOL_VERSION,
            "heads": page.get("heads", []),
            "events": page.get("events", []),
        }
        for key in ("next_cursor", "blob_refs", "record_refs"):
            if key in page:
                request[key] = page[key]
        if retry_cursor is not None:
            request["retry_cursor"] = retry_cursor
        frame = await self._replication_reply(request, "session_import_result")
        result: dict[str, Any] = {
            "session_id": frame.get("session_id"),
            "accepted_ids": frame.get("accepted_ids", []),
            "heads": frame.get("heads", []),
        }
        for key in ("retry_cursor", "conflict_heads"):
            if key in frame:
                result[key] = frame[key]
        return result

    async def export_session_blob(
        self,
        session_id: str,
        sha256: str,
        total_size: int,
        offset: int,
    ) -> dict[str, Any]:
        """Read one 1 MiB chunk of an original payload blob."""
        self._require_capability("session_replication_v1", "export_session_blob")
        frame = await self._replication_reply(
            {
                "req": "export_session_blob",
                "session_id": session_id,
                "sha256": sha256,
                "total_size": total_size,
                "offset": offset,
            },
            "session_blob_chunk",
        )
        return _chunk(frame)

    async def import_session_blob(self, session_id: str, chunk: dict[str, Any]) -> dict[str, Any]:
        """Stage one chunk of an original payload blob."""
        self._require_capability("session_replication_v1", "import_session_blob")
        frame = await self._replication_reply(
            {"req": "import_session_blob", "session_id": session_id, "chunk": chunk},
            "session_blob_status",
        )
        return _blob_status(frame)

    async def session_blob_status(self, session_id: str, sha256: str, total_size: int) -> dict[str, Any]:
        """Resume an interrupted payload transfer."""
        self._require_capability("session_replication_v1", "session_blob_status")
        frame = await self._replication_reply(
            {
                "req": "get_session_blob_status",
                "session_id": session_id,
                "sha256": sha256,
                "total_size": total_size,
            },
            "session_blob_status",
        )
        return _blob_status(frame)

    async def export_session_record_blob(
        self,
        session_id: str,
        reference: CanonicalRecordRef,
        offset: int,
    ) -> dict[str, Any]:
        """Read one 1 MiB chunk of a whole canonical record carried by reference."""
        self._require_capability("session_replication_v1", "export_session_record_blob")
        frame = await self._replication_reply(
            {
                "req": "export_session_record_blob",
                "session_id": session_id,
                "reference": reference,
                "offset": offset,
            },
            "session_blob_chunk",
        )
        return _chunk(frame)

    async def import_session_record_blob(
        self,
        session_id: str,
        reference: CanonicalRecordRef,
        chunk: dict[str, Any],
    ) -> dict[str, Any]:
        """Stage one chunk of a whole canonical record."""
        self._require_capability("session_replication_v1", "import_session_record_blob")
        frame = await self._replication_reply(
            {
                "req": "import_session_record_blob",
                "session_id": session_id,
                "reference": reference,
                "chunk": chunk,
            },
            "session_record_blob_status",
        )
        return _record_blob_status(frame)

    async def session_record_blob_status(self, session_id: str, reference: CanonicalRecordRef) -> dict[str, Any]:
        """Resume an interrupted whole-record transfer."""
        self._require_capability("session_replication_v1", "session_record_blob_status")
        frame = await self._replication_reply(
            {"req": "get_session_record_blob_status", "session_id": session_id, "reference": reference},
            "session_record_blob_status",
        )
        return _record_blob_status(frame)

    # --- streaming ----------------------------------------------------------

    def events(
        self,
        session_id: str | None = None,
        *,
        max_buffered_events: int = DEFAULT_MAX_BUFFERED_EVENTS,
        max_buffered_bytes: int = DEFAULT_MAX_BUFFERED_BYTES,
    ) -> EventStream:
        """Async iterator over stream events, optionally filtered to one session.

        Usage::

            stream = client.events(session_id)
            try:
                async for event in stream:
                    if event["ev"] == "turn_done":
                        break
            finally:
                await stream.aclose()

        Leaving the loop does not close the iterator automatically (async
        iterators are not generators), so call :meth:`EventStream.aclose`, or
        let ``run()`` do it for you. ``stream.buffered`` reports how far behind
        the consumer is.
        """
        if not _is_int(max_buffered_events) or max_buffered_events < 1:
            raise HarnessError("invalid_option", "max_buffered_events must be a positive integer")
        if not _is_int(max_buffered_bytes) or max_buffered_bytes < 1:
            raise HarnessError("invalid_option", "max_buffered_bytes must be a positive integer")
        return EventStream(self, session_id, max_buffered_events, max_buffered_bytes)

    def global_events(
        self,
        options: GlobalEventsOptions | None = None,
        *,
        signal: asyncio.Event | None = None,
        discovery_interval_ms: float | None = None,
        max_buffered_events: int | None = None,
    ) -> GlobalEventStream:
        """Stream events from every session through per-session child connections.

        Pass either :class:`GlobalEventsOptions` or the individual keywords. See
        :class:`GlobalEventStream` for the delivery guarantees.
        """
        if options is not None and any(
            value is not None for value in (signal, discovery_interval_ms, max_buffered_events)
        ):
            raise HarnessError("invalid_option", "pass GlobalEventsOptions or keyword arguments, not both")
        defaults = GlobalEventsOptions()
        resolved = options or GlobalEventsOptions(
            signal=signal,
            discovery_interval_ms=(
                defaults.discovery_interval_ms if discovery_interval_ms is None else discovery_interval_ms
            ),
            max_buffered_events=(defaults.max_buffered_events if max_buffered_events is None else max_buffered_events),
        )
        if self._socket_path is None:
            raise HarnessError(
                "unsupported_transport",
                "global_events requires a native socket connection; custom transports cannot be cloned "
                "into per-session child connections",
            )
        if self._closed:
            raise self._close_error or HarnessError("disconnected", _PENDING_ERROR)
        interval = resolved.discovery_interval_ms
        if (
            isinstance(interval, bool)
            or not isinstance(interval, (int, float))
            or not math.isfinite(interval)
            or interval < 0
        ):
            raise HarnessError("invalid_option", "discovery_interval_ms must be a finite number >= 0")
        if not _is_int(resolved.max_buffered_events) or resolved.max_buffered_events < 1:
            raise HarnessError("invalid_option", "max_buffered_events must be a positive integer")
        return GlobalEventStream(self, resolved)

    # --- run ----------------------------------------------------------------

    async def run(
        self,
        session_id: str,
        content: str,
        images: list[tuple[str, str]] | None = None,
        *,
        on_event: Callable[[dict[str, Any]], None] | None = None,
        auto_approve: bool = False,
    ) -> TurnResult:
        """Send a message and collect the assistant reply until the turn ends.

        The convenience path for scripts: one call in, the text and tool calls
        of one turn out. ``text`` includes all narration in the turn. Use
        ``final_text`` for the last framed assistant message. Streaming
        consumers should use :meth:`events` instead.
        """
        stream = self.events(session_id)
        await self.send_message(session_id, content, images or [])

        result = TurnResult()
        parts: list[AssistantTextMessage] = []
        messages_by_id: dict[str, AssistantTextMessage] = {}
        completed: set[int] = set()

        def message_for(message_id: str | None) -> AssistantTextMessage:
            key = message_id or ""
            message = messages_by_id.get(key)
            if message is None:
                message = AssistantTextMessage(message_id=message_id)
                messages_by_id[key] = message
                parts.append(message)
            return message

        def finish() -> TurnResult:
            result.messages = [message for message in parts if id(message) in completed and message.text]
            # Older bridges have no ``text_done``. Do not guess boundaries from
            # reasoning or tools: retain the historical whole-turn result.
            result.final_text = result.messages[-1].text if result.messages else result.text
            return result

        try:
            async for event in stream:
                if on_event is not None:
                    on_event(event)
                kind = event.get("ev")
                if kind == "text_delta":
                    text = str(event.get("text", ""))
                    result.text += text
                    message_for(event.get("message_id")).text += text
                elif kind == "text_replace":
                    message_for(event.get("message_id")).text = str(event.get("text", ""))
                    result.text = "".join(message.text for message in parts)
                elif kind == "text_done":
                    key = event.get("message_id")
                    message = messages_by_id.get(key or "")
                    if message is not None:
                        completed.add(id(message))
                    # Id-less framing still separates sequential messages. Keep
                    # keyed messages addressable for authoritative corrections
                    # after completion.
                    if key is None:
                        messages_by_id.pop("", None)
                elif kind == "reasoning_delta":
                    result.reasoning += str(event.get("text", ""))
                elif kind == "tool_done":
                    result.tool_calls.append(
                        ToolCall(
                            call_id=str(event.get("call_id", "")),
                            name=str(event.get("name", "")),
                            output=str(event.get("output", "")),
                            error=event.get("error"),
                        )
                    )
                elif kind == "token_usage":
                    result.usage = TurnUsage(
                        input=int(event.get("input", 0)),
                        output=int(event.get("output", 0)),
                        cache_read_input=event.get("cache_read_input"),
                    )
                elif kind == "permission_request":
                    if auto_approve:
                        await self.respond_to_permission(session_id, str(event.get("request_id", "")), "allow")
                elif kind == "turn_stopped":
                    result.stop_reason = event.get("reason")
                    result.stop_message = event.get("message")
                elif kind == "turn_done":
                    return finish()
                elif kind == "error":
                    raise HarnessError(
                        str(event.get("code") or "internal"),
                        str(event.get("message") or "harness error"),
                    )
        finally:
            await stream.aclose()

        return finish()

    async def run_structured(
        self,
        session_id: str,
        content: str,
        schema: StructuredOutputSchema,
        *,
        images: list[tuple[str, str]] | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
        auto_approve: bool = False,
        max_retries: int = 2,
    ) -> StructuredTurnResult:
        """Send a message and parse the assistant's answer as JSON validated by schema.

        This is an SDK-level contract: it does not require a special bridge or
        model feature. The initial prompt asks for JSON only; if the response is
        not JSON or fails schema validation, the client sends a bounded number of
        corrective prompts that include the normalized validation errors and the
        prior text.
        """
        assert_retry_count(max_retries)
        validator = compile_structured_schema(schema)
        attempts: list[StructuredOutputAttempt] = []

        async def attempt(attempt_number: int, prompt: str) -> StructuredTurnResult:
            turn = await self.run(
                session_id,
                prompt,
                images,
                on_event=on_event,
                auto_approve=auto_approve,
            )
            validation = validate_structured_text(turn.final_text, validator)
            record: StructuredOutputAttempt = {
                "attempt": attempt_number,
                "text": turn.final_text,
                "errors": validation.errors,
            }
            attempts.append(record)
            if validation.ok:
                return _structured_result(turn, validation.data, list(attempts))
            if attempt_number > max_retries:
                raise StructuredOutputError(attempts)
            return await attempt(attempt_number + 1, build_structured_correction_prompt(schema, record))

        return await attempt(1, build_structured_prompt(content, schema))

    # --- lifecycle ----------------------------------------------------------

    async def close(self) -> None:
        """Disconnect, and stop the instance if this client launched one."""
        self._tool_callbacks.close()
        self._transport.close()
        instance = self._instance
        self._instance = None
        if instance is not None:
            await instance.shutdown()


def _structured_result(turn: TurnResult, data: Any, attempts: list[StructuredOutputAttempt]) -> StructuredTurnResult:
    return StructuredTurnResult(
        stop_reason=turn.stop_reason,
        stop_message=turn.stop_message,
        text=turn.text,
        final_text=turn.final_text,
        messages=turn.messages,
        reasoning=turn.reasoning,
        tool_calls=turn.tool_calls,
        usage=turn.usage,
        data=data,
        attempts=attempts,
    )


def _heads(frame: dict[str, Any]) -> dict[str, Any]:
    result: dict[str, Any] = {
        "session_id": frame.get("session_id"),
        "heads": frame.get("heads", []),
    }
    if "selected_head" in frame:
        result["selected_head"] = frame["selected_head"]
    return result


def _chunk(frame: dict[str, Any]) -> dict[str, Any]:
    return {
        "sha256": frame.get("sha256"),
        "offset": frame.get("offset"),
        "data_base64": frame.get("data_base64"),
        "next_offset": frame.get("next_offset"),
        "total_size": frame.get("total_size"),
        "complete": frame.get("complete"),
    }


def _blob_status(frame: dict[str, Any]) -> dict[str, Any]:
    return {
        "session_id": frame.get("session_id"),
        "sha256": frame.get("sha256"),
        "received": frame.get("received"),
        "total_size": frame.get("total_size"),
        "complete": frame.get("complete"),
    }


def _record_blob_status(frame: dict[str, Any]) -> dict[str, Any]:
    return {
        "session_id": frame.get("session_id"),
        "reference": frame.get("reference"),
        "received": frame.get("received"),
        "total_size": frame.get("total_size"),
        "complete": frame.get("complete"),
    }
