"""SDK-provided session tools: definitions on the wire, callbacks locally."""

from __future__ import annotations

import asyncio
import contextlib
import copy
import inspect
import json
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any

import jsonschema

from .errors import HarnessError
from .protocol import SessionToolDefinition, ToolConfiguration


class ToolExecutionContext:
    """What a callback receives besides its input.

    ``cancel_event`` is set on cancellation, disconnect, or callback timeout.
    Cooperate with it to stop side effects; the SDK has already stopped waiting.
    """

    __slots__ = ("call_id", "cancel_event", "session_id")

    def __init__(self, session_id: str, call_id: str, cancel_event: asyncio.Event) -> None:
        self.session_id = session_id
        self.call_id = call_id
        self.cancel_event = cancel_event

    @property
    def cancelled(self) -> bool:
        return self.cancel_event.is_set()


@dataclass
class ToolResult:
    output: str
    error: str | None = None


ToolCallback = Callable[
    [dict[str, Any], ToolExecutionContext],
    "str | ToolResult | Awaitable[str | ToolResult]",
]


@dataclass
class SdkTool:
    """A session-local tool, or an explicit replacement for a tool with the same name."""

    name: str
    description: str
    parameters: dict[str, Any]
    execute: ToolCallback
    #: Callback deadline in milliseconds. Default 60000, maximum 60000.
    timeout_ms: int = 60_000


@dataclass
class SessionToolsOptions:
    """Session tool policy supplied to ``create_session_with_options``/``configure_tools``."""

    # Omit/None to inherit configured defaults. [] exposes no built-in or MCP tools.
    enabled: list[str] | None = None
    # Deny entries take precedence, including over custom tools.
    disabled: list[str] = field(default_factory=list)
    # Custom tools are added to the selection, replacing same-name tools in this session only.
    custom: list[SdkTool] = field(default_factory=list)


class PreparedTools:
    __slots__ = ("handlers", "wire")

    def __init__(self, wire: ToolConfiguration, handlers: dict[str, _Handler]) -> None:
        self.wire = wire
        self.handlers = handlers


@dataclass
class _Handler:
    tool: SdkTool
    validate: Callable[[Any], list[str]]


def _compiled_validator(schema: dict[str, Any]) -> Callable[[Any], list[str]]:
    """Compile ``schema`` into a check that returns the validation problems.

    Returning the problems rather than a bool keeps the tool error message
    useful: a model that passed the wrong shape reads why, the way the
    TypeScript SDK forwards Ajv's error objects.
    """

    validator_cls = jsonschema.validators.validator_for(schema, default=jsonschema.Draft202012Validator)
    validator_cls.check_schema(schema)
    validator = validator_cls(schema)

    def check(value: Any) -> list[str]:
        problems: list[str] = []
        for error in validator.iter_errors(value):
            path = "/" + "/".join(str(part) for part in error.absolute_path) if error.absolute_path else "/"
            problems.append(f"{path}: {error.message}")
        return problems

    return check


def prepare_tools(options: SessionToolsOptions) -> PreparedTools:
    """Validate before changing either the local callback table or the remote policy."""
    handlers: dict[str, _Handler] = {}
    custom: list[SessionToolDefinition] = []
    for tool in options.custom:
        if tool.name in handlers:
            raise HarnessError("invalid_option", f"duplicate tool: {tool.name}")
        if not callable(tool.execute):
            raise HarnessError("invalid_option", f"{tool.name} needs an execute callback")
        timeout = tool.timeout_ms
        if isinstance(timeout, bool) or not isinstance(timeout, int) or not 1 <= timeout <= 60_000:
            raise HarnessError("invalid_option", "tool timeout_ms must be 1..60000")
        # Snapshot definitions so mutations after registration cannot change callback validation.
        parameters = copy.deepcopy(tool.parameters)
        if not isinstance(parameters, dict) or not parameters or parameters.get("type") != "object":
            raise HarnessError("invalid_option", f"{tool.name} parameters must be an object JSON Schema")
        if parameters.get("$async"):
            raise HarnessError("invalid_option", f"invalid schema for {tool.name}: async schemas are not supported")
        try:
            validate = _compiled_validator(parameters)
        except Exception as error:
            raise HarnessError("invalid_option", f"invalid schema for {tool.name}: {error}") from error
        definition: SessionToolDefinition = {
            "name": tool.name,
            "description": tool.description,
            "parameters": parameters,
        }
        handlers[tool.name] = _Handler(
            tool=SdkTool(
                name=definition["name"],
                description=definition["description"],
                parameters=definition["parameters"],
                execute=tool.execute,
                timeout_ms=timeout,
            ),
            validate=validate,
        )
        custom.append(definition)

    wire: ToolConfiguration = {
        "enabled": None if options.enabled is None else list(options.enabled),
        "disabled": list(options.disabled),
        "custom": custom,
    }
    return PreparedTools(wire=wire, handlers=handlers)


class _Active:
    __slots__ = ("call_id", "cancel_event", "reason", "session_id")

    def __init__(self, session_id: str, call_id: str) -> None:
        self.session_id = session_id
        self.call_id = call_id
        self.cancel_event = asyncio.Event()
        self.reason: str | None = None

    def abort(self, reason: str = "cancelled") -> None:
        if self.reason is None:
            self.reason = reason
        self.cancel_event.set()


class ToolCallbacks:
    """Connection-local dispatch. Events on observing connections never execute callbacks."""

    def __init__(
        self,
        submit: Callable[[str, str, ToolResult], Awaitable[None]],
        report: Callable[[BaseException], None],
    ) -> None:
        self.sessions: dict[str, dict[str, _Handler]] = {}
        self._submit = submit
        self._report = report
        self._active: dict[tuple[str, str], _Active] = {}
        self._seen: set[tuple[str, str]] = set()
        self._tasks: set[asyncio.Task[None]] = set()

    def dispatch(self, session_id: str, call_id: str, name: str, tool_input: Any) -> None:
        handlers = self.sessions.get(session_id)
        if handlers is None:
            # Low-level clients can handle tool_call themselves.
            return
        key = (session_id, call_id)
        if key in self._seen:
            return
        self._seen.add(key)
        active = _Active(session_id, call_id)
        self._active[key] = active
        context = ToolExecutionContext(session_id, call_id, active.cancel_event)
        task = asyncio.get_running_loop().create_task(self._run(handlers.get(name), tool_input, context, active, key))
        self._tasks.add(task)
        task.add_done_callback(self._finish_task)

    def _finish_task(self, task: asyncio.Task[None]) -> None:
        self._tasks.discard(task)
        if task.cancelled():
            return
        error = task.exception()
        if error is not None:
            self._report(error)

    async def _run(
        self,
        handler: _Handler | None,
        tool_input: Any,
        context: ToolExecutionContext,
        active: _Active,
        key: tuple[str, str],
    ) -> None:
        try:
            result = await self._execute(handler, tool_input, context, active)
            # Cancellation/disconnect must not send a stale result to a later turn.
            if not active.cancel_event.is_set() or active.reason == "timeout":
                await self._submit(context.session_id, context.call_id, result)
        finally:
            self._active.pop(key, None)

    async def _execute(
        self,
        handler: _Handler | None,
        tool_input: Any,
        context: ToolExecutionContext,
        active: _Active,
    ) -> ToolResult:
        if handler is None:
            return ToolResult(output="", error="SDK tool callback is not registered")
        problems = handler.validate(tool_input)
        if problems:
            return ToolResult(output="", error=f"Invalid tool input: {json.dumps(problems)}")

        loop = asyncio.get_running_loop()
        timer = loop.call_later(handler.tool.timeout_ms / 1000, active.abort, "timeout")
        cancel_wait = loop.create_task(active.cancel_event.wait())
        call = loop.create_task(self._call(handler.tool, tool_input, context))
        try:
            done, _pending = await asyncio.wait({call, cancel_wait}, return_when=asyncio.FIRST_COMPLETED)
            if call in done:
                try:
                    return _coerce_result(call.result())
                except Exception as error:
                    return ToolResult(output="", error=str(error) or type(error).__name__)
            # Cancelled or timed out: stop waiting on application code.
            call.cancel()
            with contextlib.suppress(BaseException):
                await call
            message = "SDK tool callback timed out" if active.reason == "timeout" else "SDK tool callback cancelled"
            return ToolResult(output="", error=message)
        finally:
            timer.cancel()
            cancel_wait.cancel()
            with contextlib.suppress(BaseException):
                await cancel_wait

    async def _call(
        self,
        tool: SdkTool,
        tool_input: Any,
        context: ToolExecutionContext,
    ) -> str | ToolResult:
        if not isinstance(tool_input, dict):
            raise TypeError("SDK tool input must be an object")
        result = tool.execute(tool_input, context)
        if inspect.isawaitable(result):
            return await result
        return result

    def abort(self, session_id: str | None = None) -> None:
        """Cancel in-flight callbacks, and forget call ids for the affected session(s)."""
        for active in list(self._active.values()):
            if session_id is None or active.session_id == session_id:
                active.abort("cancelled")
        # Call ids are unique within a turn. Bound deduplication memory to that turn.
        if session_id is None:
            self._seen.clear()
        else:
            self._seen = {key for key in self._seen if key[0] != session_id}

    def close(self) -> None:
        self.abort()
        self.sessions.clear()


def _coerce_result(value: Any) -> ToolResult:
    if isinstance(value, str):
        return ToolResult(output=value)
    if not isinstance(value, ToolResult):
        raise TypeError("SDK tool must return a string or ToolResult(output, error=None)")
    if not isinstance(value.output, str) or (value.error is not None and not isinstance(value.error, str)):
        raise TypeError("SDK tool must return a string or ToolResult(output, error=None)")
    return value
