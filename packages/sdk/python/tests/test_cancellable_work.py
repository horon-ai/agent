from __future__ import annotations

from typing import Any

import pytest

from horon_agent_sdk import ConnectOptions, HoronAgentClient
from mock_harness import start_mock_harness


def _replies(capabilities: list[str], requests: list[dict[str, Any]]):
    def on_request(request: dict[str, Any], send) -> None:
        requests.append(request)
        kind = request["req"]
        if kind == "create_session":
            send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": {"session_id": "s1"}})
        elif kind == "cancel_turn":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "cancel_turn_result",
                    "session_id": request["session_id"],
                    "run_id": request["run_id"],
                    "accepted": False,
                    "reason": "unknown",
                }
            )
        elif kind in ("cancel_tool_call", "cancel_background_terminal"):
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "tool_call_cancel_result",
                    "session_id": request["session_id"],
                    "call_id": request["call_id"],
                    "accepted": True,
                }
            )
        elif kind == "list_background_work":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "background_work_list",
                    "session_id": request["session_id"],
                    "items": [
                        {
                            "call_id": "call-4",
                            "task_id": "bg_9",
                            "kind": "terminal",
                            "command": "sleep 30",
                            "cwd": "/tmp",
                            "output": "aGk=",
                        },
                        {"call_id": "call-5", "task_id": "bg_10", "kind": "tool_call"},
                    ],
                }
            )
        else:
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    return on_request


async def test_cancel_turn_reports_the_refusal_the_daemon_named() -> None:
    requests: list[dict[str, Any]] = []
    server = await start_mock_harness(capabilities=["cancellable_work_v1"], on_request=_replies([], requests))
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert await client.cancel_turn("s1", "run-7") == {"accepted": False, "reason": "unknown"}
        request = next(entry for entry in requests if entry["req"] == "cancel_turn")
        assert request["run_id"] == "run-7"
    finally:
        await client.close()
        await server.close()


async def test_cancel_tool_call_and_background_terminal_echo_the_call() -> None:
    requests: list[dict[str, Any]] = []
    server = await start_mock_harness(capabilities=["cancellable_work_v1"], on_request=_replies([], requests))
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert await client.cancel_tool_call("s1", "call-3") == {"call_id": "call-3", "accepted": True}
        assert await client.cancel_background_terminal("s1", "call-4") == {
            "call_id": "call-4",
            "accepted": True,
        }
        cancels = [entry["req"] for entry in requests if entry["req"].startswith("cancel_")]
        assert cancels == ["cancel_tool_call", "cancel_background_terminal"]
    finally:
        await client.close()
        await server.close()


async def test_list_background_work_keeps_only_the_fields_that_were_sent() -> None:
    requests: list[dict[str, Any]] = []
    server = await start_mock_harness(capabilities=["cancellable_work_v1"], on_request=_replies([], requests))
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        items = await client.list_background_work("s1")
        assert [item["call_id"] for item in items] == ["call-4", "call-5"]
        assert items[0]["output"] == "aGk="
        assert "command" not in items[1]
    finally:
        await client.close()
        await server.close()


async def test_every_cancellable_work_method_refuses_before_writing_without_the_capability() -> None:
    requests: list[dict[str, Any]] = []
    server = await start_mock_harness(capabilities=[], on_request=_replies([], requests))
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        for call in (
            lambda: client.cancel_turn("s1", "run-7"),
            lambda: client.cancel_tool_call("s1", "call-3"),
            lambda: client.cancel_background_terminal("s1", "call-4"),
            lambda: client.list_background_work("s1"),
        ):
            with pytest.raises(Exception, match="cancellable_work_v1"):
                await call()
        assert [entry["req"] for entry in requests if entry["req"] != "create_session"] == []
    finally:
        await client.close()
        await server.close()
