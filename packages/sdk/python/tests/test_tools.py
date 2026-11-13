from __future__ import annotations

import asyncio
from typing import Any

import pytest

from horon_agent_sdk import (
    ConnectOptions,
    HarnessError,
    HoronAgentClient,
    SdkTool,
    SessionToolsOptions,
    ToolExecutionContext,
)
from mock_harness import start_mock_harness, wait_until

SCHEMA = {
    "type": "object",
    "properties": {"value": {"type": "string"}},
    "required": ["value"],
    "additionalProperties": False,
}
DEFINITION = {"name": "lookup", "description": "Look up a value", "parameters": SCHEMA}


def call(call_id: str = "c1", tool_input: Any = None, session: str = "s1", name: str = "lookup") -> dict[str, Any]:
    return {
        "v": 1,
        "ev": "tool_call",
        "session_id": session,
        "call_id": call_id,
        "name": name,
        "input": {"value": "hello"} if tool_input is None else tool_input,
    }


def tool(execute, timeout_ms: int = 60_000) -> SdkTool:
    return SdkTool(
        name=DEFINITION["name"],
        description=DEFINITION["description"],
        parameters=SCHEMA,
        execute=execute,
        timeout_ms=timeout_ms,
    )


async def fixture(extra=None):
    requests: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        requests.append(request)
        if extra is not None and extra(request, send):
            return
        if request["req"] == "create_session":
            send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": {"session_id": "s1"}})
        elif request["req"] == "list_tools":
            send({"v": 1, "reply_to": request["id"], "ev": "tools", "session_id": "s1", "tools": [DEFINITION]})
        else:
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=["session_tools"], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))

    async def result(call_id: str = "c1") -> dict[str, Any]:
        for _ in range(100):
            found = next(
                (request for request in requests if request["req"] == "tool_result" and request["call_id"] == call_id),
                None,
            )
            if found is not None:
                return found
            await asyncio.sleep(0.005)
        raise AssertionError(f"no result for {call_id}")

    return server, client, requests, result


async def test_create_session_configures_tools_before_returning_and_strips_callbacks() -> None:
    server, client, requests, _result = await fixture()
    try:
        session = await client.create_session_with_options(
            working_dir="/project",
            system_prompt="Use only configured tools.",
            tools=SessionToolsOptions(enabled=[], custom=[tool(lambda tool_input, context: "ok")]),
        )
        assert session["session_id"] == "s1"
        assert requests[0]["working_dir"] == "/project"
        assert requests[0]["system_prompt"] == "Use only configured tools."
        assert requests[1]["tools"] == {"enabled": [], "disabled": [], "custom": [DEFINITION]}
        assert await client.list_tools("s1") == [DEFINITION]
    finally:
        await client.close()
        await server.close()


async def test_callbacks_execute_once_and_submit_output_including_immediate_post_ack_events() -> None:
    executions: list[ToolExecutionContext] = []

    def extra(request: dict[str, Any], send) -> bool:
        if request["req"] != "configure_tools":
            return False
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})
        send(call())
        send(call())
        return True

    server, client, _requests, result = await fixture(extra)

    async def execute(tool_input: dict[str, Any], context: ToolExecutionContext):
        executions.append(context)
        assert context.session_id == "s1"
        assert context.call_id == "c1"
        assert context.cancelled is False
        return f"found {tool_input['value']}"

    try:
        await client.configure_tools("s1", SessionToolsOptions(enabled=[], custom=[tool(execute)]))
        assert (await result())["output"] == "found hello"
        assert len(executions) == 1
    finally:
        await client.close()
        await server.close()


async def test_callbacks_validate_input_and_convert_throws_to_tool_errors() -> None:
    executions: list[int] = []

    def execute(_tool_input: dict[str, Any], _context: ToolExecutionContext) -> str:
        executions.append(1)
        raise RuntimeError("lookup failed")

    server, client, _requests, result = await fixture()
    try:
        await client.configure_tools("s1", SessionToolsOptions(custom=[tool(execute)]))
        server.broadcast(call("bad", {"value": 5}))
        assert "Invalid tool input" in (await result("bad"))["error"]
        assert executions == []
        server.broadcast(call("throws"))
        assert (await result("throws"))["error"] == "lookup failed"
        assert len(executions) == 1
    finally:
        await client.close()
        await server.close()


async def test_callbacks_are_session_scoped_and_missing_names_fail_promptly() -> None:
    executions: list[int] = []

    def execute(_tool_input: dict[str, Any], _context: ToolExecutionContext) -> str:
        executions.append(1)
        return "ok"

    server, client, requests, result = await fixture()
    try:
        await client.configure_tools("s1", SessionToolsOptions(custom=[tool(execute)]))
        server.broadcast(call("other", {}, "s2"))
        server.broadcast(call("missing", {}, "s1", "unknown"))
        assert "not registered" in (await result("missing"))["error"]
        assert executions == []
        assert not any(request.get("call_id") == "other" for request in requests)
    finally:
        await client.close()
        await server.close()


async def test_callback_timeout_aborts_the_signal_and_returns_an_error() -> None:
    contexts: list[ToolExecutionContext] = []

    async def execute(_tool_input: dict[str, Any], context: ToolExecutionContext) -> str:
        contexts.append(context)
        await asyncio.Event().wait()
        return "never"

    server, client, _requests, result = await fixture()
    try:
        await client.configure_tools("s1", SessionToolsOptions(custom=[tool(execute, timeout_ms=10)]))
        server.broadcast(call())
        assert "timed out" in (await result())["error"]
        assert contexts[0].cancelled is True
    finally:
        await client.close()
        await server.close()


@pytest.mark.parametrize("stop", ["cancel", "close", "turn_done"])
async def test_stop_aborts_callbacks_and_suppresses_late_results(stop: str) -> None:
    contexts: list[ToolExecutionContext] = []
    finish: list[Any] = []

    async def execute(_tool_input: dict[str, Any], context: ToolExecutionContext) -> str:
        contexts.append(context)
        waiter = asyncio.get_running_loop().create_future()
        finish.append(waiter)
        return await waiter

    server, client, requests, _result = await fixture()
    try:
        await client.configure_tools("s1", SessionToolsOptions(custom=[tool(execute)]))
        server.broadcast(call())
        await wait_until(lambda: len(contexts) == 1)
        if stop == "cancel":
            await client.cancel("s1")
        elif stop == "close":
            await client.close()
        else:
            server.broadcast({"v": 1, "ev": "turn_done", "session_id": "s1"})
        await asyncio.sleep(0.02)
        assert contexts[0].cancelled is True
        if not finish[0].done():
            finish[0].set_result("late")
        await asyncio.sleep(0.02)
        assert not any(request["req"] == "tool_result" for request in requests)
    finally:
        await client.close()
        await server.close()


async def test_failed_reconfiguration_keeps_the_existing_callback_and_schema() -> None:
    configurations: list[int] = []

    def extra(request: dict[str, Any], send) -> bool:
        if request["req"] != "configure_tools":
            return False
        configurations.append(1)
        if len(configurations) == 1:
            return False
        send(call("during"))
        send({"v": 1, "reply_to": request["id"], "ev": "error", "code": "busy", "message": "busy"})
        return True

    server, client, _requests, result = await fixture(extra)
    try:
        await client.configure_tools("s1", SessionToolsOptions(custom=[tool(lambda tool_input, context: "old")]))
        with pytest.raises(HarnessError, match="busy"):
            await client.configure_tools("s1", SessionToolsOptions(custom=[tool(lambda tool_input, context: "new")]))
        assert (await result("during"))["output"] == "old"
        server.broadcast(call("after"))
        assert (await result("after"))["output"] == "old"
    finally:
        await client.close()
        await server.close()


async def test_invalid_tool_schemas_duplicate_names_and_deadlines_are_rejected_before_a_request() -> None:
    server, client, requests, _result = await fixture()
    good = tool(lambda tool_input, context: "ok")
    invalid_cases = [
        [good, good],
        [tool(lambda tool_input, context: "ok", timeout_ms=0)],
        [
            SdkTool(
                name="lookup",
                description="Look up a value",
                parameters={"type": "array"},
                execute=lambda tool_input, context: "ok",
            )
        ],
    ]
    try:
        for custom in invalid_cases:
            with pytest.raises(HarnessError):
                await client.configure_tools("s1", SessionToolsOptions(custom=custom))
        assert requests == []
    finally:
        await client.close()
        await server.close()


async def test_async_schema_is_rejected() -> None:
    server, client, requests, _result = await fixture()
    try:
        async_tool = SdkTool(
            name="lookup",
            description="Look up a value",
            parameters={"type": "object", "$async": True},
            execute=lambda tool_input, context: "ok",
        )
        with pytest.raises(HarnessError):
            await client.configure_tools("s1", SessionToolsOptions(custom=[async_tool]))
        assert requests == []
    finally:
        await client.close()
        await server.close()


async def test_older_runtimes_fail_before_creating_an_unrestricted_session() -> None:
    requests: list[dict[str, Any]] = []
    server = await start_mock_harness(on_request=lambda request, send: requests.append(request))
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        with pytest.raises(HarnessError, match="session_tools"):
            await client.create_session_with_options(tools=SessionToolsOptions(enabled=[]))
        assert requests == []
    finally:
        await client.close()
        await server.close()


async def test_concurrent_configuration_is_rejected_without_changing_the_callback_table() -> None:
    release: list[Any] = []

    def extra(request: dict[str, Any], send) -> bool:
        if request["req"] != "configure_tools":
            return False
        release.append(lambda: send({"v": 1, "reply_to": request["id"], "ev": "ok"}))
        return True

    server, client, requests, result = await fixture(extra)
    try:
        first = asyncio.create_task(
            client.configure_tools("s1", SessionToolsOptions(custom=[tool(lambda input_, ctx: "first")]))
        )
        await wait_until(lambda: bool(release))
        with pytest.raises(HarnessError, match="already in flight"):
            await client.configure_tools("s1", SessionToolsOptions(custom=[tool(lambda input_, ctx: "second")]))
        release[0]()
        await first
        assert len([request for request in requests if request["req"] == "configure_tools"]) == 1
        server.broadcast(call())
        assert (await result())["output"] == "first"
    finally:
        await client.close()
        await server.close()


async def test_ambiguous_configuration_timeout_closes_the_connection() -> None:
    server = await start_mock_harness(capabilities=["session_tools"])
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path, request_timeout_ms=30))
    try:
        with pytest.raises(HarnessError, match="no reply"):
            await client.configure_tools("s1", SessionToolsOptions(custom=[tool(lambda input_, ctx: "unexpected")]))
        await wait_until(lambda: client._closed)
        assert client._closed is True
    finally:
        await client.close()
        await server.close()


async def test_switching_attachments_drops_old_callbacks_before_later_events() -> None:
    executions: list[int] = []

    def extra(request: dict[str, Any], send) -> bool:
        if request["req"] != "attach_session":
            return False
        send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": {"session_id": "s2"}})
        send(call("stale", {"value": "old"}, "s1"))
        return True

    def execute(_tool_input: dict[str, Any], _context: ToolExecutionContext) -> str:
        executions.append(1)
        return "old"

    server, client, _requests, _result = await fixture(extra)
    try:
        await client.configure_tools("s1", SessionToolsOptions(custom=[tool(execute)]))
        await client.attach_session("s2")
        server.broadcast(call("another", {"value": "old"}, "s1"))
        await asyncio.sleep(0.02)
        assert executions == []
    finally:
        await client.close()
        await server.close()
