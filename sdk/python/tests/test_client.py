from __future__ import annotations

import asyncio
import os
import pathlib
import tempfile
from typing import Any

import pytest

from horon_agent_sdk import (
    ConnectOptions,
    HarnessError,
    HoronAgentClient,
    unix_socket_transport,
)
from mock_harness import start_mock_harness, wait_until


async def test_handshake_records_server_identity_and_capabilities() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    assert client.server == "mock/0.1"
    assert client.capabilities == ["sessions", "streaming"]
    await client.close()
    await server.close()


async def test_replies_are_correlated_by_id_even_when_out_of_order() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "ping":
            # Answer late, after the list_sessions that followed it.
            asyncio.get_running_loop().call_later(0.03, send, {"v": 1, "reply_to": request["id"], "ev": "pong"})
        if request["req"] == "list_sessions":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "sessions",
                    "sessions": [{"session_id": "s1", "status": "idle"}],
                }
            )

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    _first, sessions = await asyncio.gather(client.ping(), client.list_sessions())
    assert sessions == [{"session_id": "s1", "status": "idle"}]
    await client.close()
    await server.close()


async def test_error_frames_raise_harness_error() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        send(
            {"v": 1, "reply_to": request["id"], "ev": "error", "code": "unknown_session", "message": "no such session"}
        )

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    with pytest.raises(HarnessError) as caught:
        await client.attach_session("nope")
    assert caught.value.code == "unknown_session"
    await client.close()
    await server.close()


async def test_run_collects_a_full_turn_and_auto_approves_permissions() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "send_message":
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})
            s = "s1"
            send({"v": 1, "ev": "message_accepted", "session_id": s})
            send({"v": 1, "ev": "reasoning_delta", "session_id": s, "text": "think"})
            send(
                {
                    "v": 1,
                    "ev": "permission_request",
                    "session_id": s,
                    "request_id": "p1",
                    "tool_name": "bash",
                    "description": "ls",
                }
            )
            send({"v": 1, "ev": "text_delta", "session_id": s, "text": "hello "})
            send({"v": 1, "ev": "text_delta", "session_id": s, "text": "world"})
            send({"v": 1, "ev": "tool_done", "session_id": s, "call_id": "c1", "name": "bash", "output": "ok"})
            send({"v": 1, "ev": "token_usage", "session_id": s, "input": 10, "output": 4})
            # A different session must not leak into this turn.
            send({"v": 1, "ev": "text_delta", "session_id": "other", "text": "IGNORE"})
            send({"v": 1, "ev": "turn_done", "session_id": s})
        if request["req"] == "permission_response":
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    turn = await client.run("s1", "hi", auto_approve=True)
    assert turn.text == "hello world"
    assert turn.reasoning == "think"
    assert [(call.call_id, call.name, call.output, call.error) for call in turn.tool_calls] == [
        ("c1", "bash", "ok", None)
    ]
    assert turn.usage is not None
    assert (turn.usage.input, turn.usage.output, turn.usage.cache_read_input) == (10, 4, None)
    await client.close()
    await server.close()


async def test_soft_interrupt_preserves_images_and_the_legacy_text_only_wire_shape() -> None:
    received: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        received.append(request)
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        await client.soft_interrupt_with_images("s1", "look", [("image/png", "aW1hZ2U=")], True)
        await client.soft_interrupt("s1", "text only")
        assert [{key: value for key, value in request.items() if key not in ("v", "id")} for request in received] == [
            {
                "req": "soft_interrupt",
                "session_id": "s1",
                "content": "look",
                "images": [["image/png", "aW1hZ2U="]],
                "urgent": True,
            },
            {"req": "soft_interrupt", "session_id": "s1", "content": "text only", "urgent": False},
        ]
    finally:
        await client.close()
        await server.close()


async def test_soft_interrupt_ids_tag_cancel_and_background_result() -> None:
    received: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        received.append(request)
        if request["req"] == "cancel_soft_interrupts" and "client_ids" in request:
            ids = request["client_ids"]
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "soft_interrupts_cancelled",
                    "cancelled": [client_id for client_id in ids if client_id != "f1"],
                    "not_queued": [client_id for client_id in ids if client_id == "f1"],
                }
            )
        elif request["req"] == "background_tool":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "background_tool_result",
                    "moved": True,
                    "tool_call_id": "call_1",
                    "tool_name": "bash",
                }
            )
        else:
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(
        capabilities=["soft_interrupt_ids", "background_tool_result"], on_request=on_request
    )
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert client.supports("soft_interrupt_ids")
        await client.soft_interrupt_with_id("s1", "steer", client_id="f1")
        assert await client.cancel_soft_interrupts_by_id("s1", ["f1", "f2"]) == {
            "cancelled": ["f2"],
            "not_queued": ["f1"],
        }
        assert await client.cancel_soft_interrupt("s1", "f1") is False
        assert await client.cancel_soft_interrupt("s1", "f2") is True
        assert await client.background_tool_result("s1") == {
            "moved": True,
            "tool_call_id": "call_1",
            "tool_name": "bash",
        }
        await client.cancel_soft_interrupts("s1")
        first = {key: value for key, value in received[0].items() if key not in ("v", "id")}
        assert first == {
            "req": "soft_interrupt",
            "session_id": "s1",
            "content": "steer",
            "urgent": False,
            "client_id": "f1",
        }
        clear_all = received[-1]
        assert clear_all["req"] == "cancel_soft_interrupts"
        assert "client_ids" not in clear_all
    finally:
        await client.close()
        await server.close()


async def test_background_tool_result_degrades_to_moved_false_against_an_older_bridge() -> None:
    server = await start_mock_harness(
        on_request=lambda request, send: send({"v": 1, "reply_to": request["id"], "ev": "ok"})
    )
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert await client.background_tool_result("s1") == {"moved": False}
        await client.background_tool("s1")
    finally:
        await client.close()
        await server.close()


async def test_send_message_supports_context_only_options_and_waits_for_request_completion() -> None:
    received: dict[str, Any] = {}

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "send_message":
            received.update(request)
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    await client.send_message("s1", "context", [("image/png", "abc")], no_reply=True)
    assert received["no_reply"] is True
    assert received["images"] == [["image/png", "abc"]]
    await client.close()
    await server.close()


async def test_send_message_waits_for_message_accepted_by_default() -> None:
    received: dict[str, Any] = {}

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "send_message":
            received.update(request)
            send({"v": 1, "ev": "message_accepted", "session_id": request["session_id"]})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    await client.send_message("s1", "normal", [("image/jpeg", "xyz")])
    assert received["images"] == [["image/jpeg", "xyz"]]
    assert "no_reply" not in received
    await client.close()
    await server.close()


async def test_send_message_no_reply_does_not_wait_for_turn_events() -> None:
    observed: dict[str, Any] = {}

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "send_message":
            observed.update(request)
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    seen_turn = False

    def on_event(frame: dict[str, Any]) -> None:
        nonlocal seen_turn
        if frame.get("ev") == "turn_done":
            seen_turn = True

    client._add_event_listener(on_event)
    await client.send_message("s1", "context only", no_reply=True)
    assert observed["req"] == "send_message"
    assert observed["session_id"] == "s1"
    assert observed["content"] == "context only"
    assert observed["no_reply"] is True
    assert seen_turn is False
    await client.close()
    await server.close()


async def test_pending_requests_raise_when_the_connection_drops() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    pending = asyncio.create_task(client.ping())
    await asyncio.sleep(0.02)
    await server.close()
    with pytest.raises(HarnessError):
        await pending


async def test_a_missing_bridge_socket_explains_how_to_start_it() -> None:
    missing = os.path.join(tempfile.gettempdir(), f"horon-agent-sdk-absent-{os.getpid()}.sock")
    with pytest.raises(HarnessError) as caught:
        await HoronAgentClient.connect(ConnectOptions(socket_path=missing))
    assert isinstance(caught.value, HarnessError)
    assert caught.value.code == "connect_failed"
    assert "horon-agent api-bridge" in str(caught.value)
    assert missing in str(caught.value)


async def test_a_stale_socket_file_reports_a_dead_bridge_not_a_missing_one() -> None:
    stale = os.path.join(tempfile.gettempdir(), f"horon-agent-sdk-stale-{os.getpid()}.sock")
    pathlib.Path(stale).write_text("")
    try:
        with pytest.raises(HarnessError) as caught:
            await HoronAgentClient.connect(ConnectOptions(socket_path=stale))
        assert caught.value.code == "connect_failed"
        assert any(phrase in str(caught.value) for phrase in ("stale socket file", "not a socket", "could not connect"))
    finally:
        pathlib.Path(stale).unlink(missing_ok=True)


async def test_ga_methods_send_stable_request_shapes_and_map_typed_replies() -> None:
    requests: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        requests.append(request)

        def reply(frame: dict[str, Any]) -> None:
            send({"v": 1, "reply_to": request["id"], **frame})

        match request["req"]:
            case "list_sessions":
                reply({"ev": "sessions", "sessions": [{"session_id": "s1", "status": "idle", "archived": True}]})
            case "archive_session" | "restore_session" | "set_retention_policy":
                reply({"ev": "ok"})
            case "ping":
                reply({"ev": "pong"})
            case "get_runtime_info":
                reply(
                    {
                        "ev": "runtime_info",
                        "session_id": "s1",
                        "provider": "anthropic",
                        "model": "claude",
                        "fallback_models": ["claude-backup"],
                        "serving_model": "claude-backup",
                        "routes": [
                            {
                                "model": "claude",
                                "provider": "anthropic",
                                "api_method": "messages",
                                "available": True,
                                "detail": "ready",
                            },
                            {
                                "model": "claude-fast",
                                "provider": "anthropic",
                                "api_method": "messages",
                                "available": True,
                                "detail": "ready",
                            },
                        ],
                    }
                )
            case "set_api_key":
                reply({"ev": "credential_updated", "provider": "gemini", "configured": True})
            case "clear_api_key":
                reply({"ev": "credential_updated", "provider": "horon-agent", "configured": False})
            case "notify_auth_changed":
                reply({"ev": "ok"})
            case "read_file":
                reply(
                    {
                        "ev": "file_content",
                        "session_id": "s1",
                        "path": "src/a.py",
                        "content": "hello",
                        "size": 8,
                        "truncated": True,
                    }
                )
            case "find_files":
                reply({"ev": "files", "session_id": "s1", "paths": ["src/a.py"]})
            case "search_text":
                reply(
                    {
                        "ev": "text_matches",
                        "session_id": "s1",
                        "matches": [{"path": "src/a.py", "line": 2, "column": 3, "preview": "  hello"}],
                    }
                )
            case "file_status":
                reply(
                    {
                        "ev": "file_status",
                        "session_id": "s1",
                        "path": "src/a.py",
                        "exists": True,
                        "kind": "file",
                        "size": 8,
                        "modified_ms": 123,
                    }
                )

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert await client.list_sessions(include_archived=True) == [
            {"session_id": "s1", "status": "idle", "archived": True}
        ]
        await client.archive_session("s1")
        await client.restore_session("s1")
        await client.set_retention_policy(30)

        runtime = await client.get_runtime_info("s1")
        assert runtime["healthy"] is True
        assert runtime["providers"] == ["anthropic"]
        assert runtime["fallback_models"] == ["claude-backup"]
        assert runtime["serving_model"] == "claude-backup"
        assert len(runtime["routes"]) == 2

        await client.set_api_key("gemini-api", "secret")
        await client.clear_api_key("horon-agent")
        await client.notify_auth_changed("openai")
        assert await client.read_file("s1", "src/a.py", 5) == {
            "path": "src/a.py",
            "content": "hello",
            "size": 8,
            "truncated": True,
        }
        assert await client.find_files("s1", "a.py", 4) == ["src/a.py"]
        assert await client.search_text("s1", "hello", path="src", limit=2) == [
            {"path": "src/a.py", "line": 2, "column": 3, "preview": "  hello"}
        ]
        assert await client.file_status("s1", "src/a.py") == {
            "path": "src/a.py",
            "exists": True,
            "kind": "file",
            "size": 8,
            "modified_ms": 123,
        }

        def by_kind(kind: str) -> dict[str, Any]:
            return next(request for request in requests if request["req"] == kind)

        assert by_kind("list_sessions")["include_archived"] is True
        assert by_kind("archive_session")["session_id"] == "s1"
        assert by_kind("set_retention_policy")["archive_after_days"] == 30
        assert (by_kind("set_api_key")["provider"], by_kind("set_api_key")["api_key"]) == ("gemini-api", "secret")
        assert by_kind("read_file")["max_bytes"] == 5
        assert (by_kind("search_text")["path"], by_kind("search_text")["limit"]) == ("src", 2)
    finally:
        await client.close()
        await server.close()


async def test_session_recovery_events_are_buffered_and_filtered_by_session() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events("mine")
    all_stream = client.events()
    try:
        for session_id in ("other", "mine"):
            server.broadcast(
                {
                    "v": 1,
                    "ev": "session_recovery",
                    "session_id": session_id,
                    "continuation_message": "continue task",
                    "reconnect_notice": "reconnected",
                }
            )
        event = await asyncio.wait_for(stream.__anext__(), 2)
        assert event["ev"] == "session_recovery"
        assert event["session_id"] == "mine"
        assert event["continuation_message"] == "continue task"
        assert event["reconnect_notice"] == "reconnected"
        for expected in ("other", "mine"):
            frame = await asyncio.wait_for(all_stream.__anext__(), 2)
            assert frame["ev"] == "session_recovery"
            assert frame["session_id"] == expected
    finally:
        await stream.aclose()
        await all_stream.aclose()
        await client.close()
        await server.close()


async def test_send_system_reminder_writes_hidden_content_without_waiting_for_acceptance() -> None:
    requests: list[dict[str, Any]] = []

    server = await start_mock_harness(on_request=lambda request, send: requests.append(request))
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        await client.send_system_reminder("mine", "continue task")
        await wait_until(lambda: len(requests) == 1)
        assert requests[0]["req"] == "send_message"
        assert requests[0]["session_id"] == "mine"
        assert requests[0]["content"] == ""
        assert requests[0]["system_reminder"] == "continue task"
        assert requests[0].get("no_reply") is not True
    finally:
        await client.close()
        await server.close()


async def test_send_message_with_reminder_sends_visible_content_with_a_hidden_reminder() -> None:
    requests: list[dict[str, Any]] = []

    server = await start_mock_harness(on_request=lambda request, send: requests.append(request))
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        await client.send_message_with_reminder("mine", "hello", [], "  be brief  ", wait_for_accept=False)
        await client.send_message_with_reminder("mine", "plain", [], "   ", wait_for_accept=False)
        await wait_until(lambda: len(requests) == 2)
        assert requests[0]["content"] == "hello"
        assert requests[0]["system_reminder"] == "be brief"
        assert requests[1]["content"] == "plain"
        assert "system_reminder" not in requests[1]
    finally:
        await client.close()
        await server.close()


async def test_run_retains_an_abnormal_stop_without_leaking_another_sessions_reason() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "send_message":
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})
            send({"v": 1, "ev": "message_accepted", "session_id": "s1"})
            send({"v": 1, "ev": "turn_stopped", "session_id": "other", "reason": "crash", "message": "Other session"})
            send({"v": 1, "ev": "turn_stopped", "session_id": "s1", "reason": "interrupted", "message": "Cancelled"})
            send({"v": 1, "ev": "turn_done", "session_id": "s1"})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        result = await client.run("s1", "hello")
        assert result.stop_reason == "interrupted"
        assert result.stop_message == "Cancelled"
    finally:
        await client.close()
        await server.close()


async def test_global_events_discovers_persisted_and_new_sessions_and_cleans_up_children() -> None:
    sessions = ["persisted-1", "persisted-2"]
    list_requests: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "list_sessions":
            list_requests.append(request)
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "sessions",
                    "sessions": [{"session_id": session_id, "status": "idle"} for session_id in sessions],
                }
            )
        elif request["req"] == "attach_session":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "attached",
                    "session": {"session_id": request["session_id"], "status": "attached"},
                }
            )
            asyncio.get_running_loop().call_soon(
                send,
                {"v": 1, "ev": "text_delta", "session_id": request["session_id"], "text": request["session_id"]},
            )

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.global_events(discovery_interval_ms=10)
    try:
        first = await asyncio.wait_for(stream.__anext__(), 2)
        second = await asyncio.wait_for(stream.__anext__(), 2)
        assert {first["session_id"], second["session_id"]} == {"persisted-1", "persisted-2"}
        assert list_requests[0]["include_archived"] is True

        sessions.append("new-3")
        third = await asyncio.wait_for(stream.__anext__(), 2)
        assert third["session_id"] == "new-3"
        await wait_until(lambda: server.client_count() == 4)

        await stream.aclose()
        await wait_until(lambda: server.client_count() == 1)
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_global_events_aborts_a_pending_consumer_and_closes_every_child() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "list_sessions":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "sessions",
                    "sessions": [{"session_id": "s1", "status": "idle"}],
                }
            )
        elif request["req"] == "attach_session":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "attached",
                    "session": {"session_id": "s1", "status": "attached"},
                }
            )

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    abort = asyncio.Event()
    stream = client.global_events(signal=abort, discovery_interval_ms=0)
    try:
        await wait_until(lambda: server.client_count() == 2)
        pending = asyncio.create_task(stream.__anext__())
        await asyncio.sleep(0.02)
        abort.set()
        with pytest.raises(StopAsyncIteration):
            await asyncio.wait_for(pending, 2)
        await wait_until(lambda: server.client_count() == 1)
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_global_events_fails_loudly_when_its_bounded_queue_overflows() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "list_sessions":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "sessions",
                    "sessions": [{"session_id": "s1", "status": "idle"}],
                }
            )
        elif request["req"] == "attach_session":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "attached",
                    "session": {"session_id": "s1", "status": "attached"},
                }
            )
            for text in ("one", "two"):
                asyncio.get_running_loop().call_later(
                    0.01, send, {"v": 1, "ev": "text_delta", "session_id": "s1", "text": text}
                )

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.global_events(discovery_interval_ms=0, max_buffered_events=1)
    try:
        await asyncio.sleep(0.05)
        with pytest.raises(HarnessError) as caught:
            await asyncio.wait_for(stream.__anext__(), 2)
        assert caught.value.code == "event_buffer_overflow"
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_global_events_explicitly_rejects_custom_transports() -> None:
    server = await start_mock_harness()
    transport = await unix_socket_transport(server.socket_path)
    client = await HoronAgentClient.connect(ConnectOptions(transport=transport))
    try:
        with pytest.raises(HarnessError) as caught:
            client.global_events()
        assert caught.value.code == "unsupported_transport"
    finally:
        await client.close()
        await server.close()


async def test_unknown_event_kinds_still_surface_on_the_generic_stream() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events()
    try:
        server.broadcast({"v": 1, "ev": "some_future_event", "payload": 1})
        frame = await asyncio.wait_for(stream.__anext__(), 2)
        assert frame["ev"] == "some_future_event"
    finally:
        await stream.aclose()
        await client.close()
        await server.close()
