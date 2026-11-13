"""Durable-runtime surface of the Python SDK (protocol minor 14).

What a mock harness can prove: each new method sends the protocol request and
reads the typed reply; a server that never advertised the capability is refused
before anything reaches the wire; and a gated event never reaches a consumer
that did not negotiate it.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from horon_agent_sdk import ConnectOptions, HarnessError, HoronAgentClient
from mock_harness import start_mock_harness, wait_until

MODERN = ["sessions", "durable_runtime_v1", "monitor_v1", "snapshot_marker_v1", "ask_receipts_v1"]
LEGACY = ["sessions", "streaming"]

STATE = {
    "journal": {"exists": True, "generation": 2, "last_seq": 9, "bytes": 512, "records": 9},
    "runs": [{"run_id": "run-1", "started_at": "2026-10-26T00:00:00Z", "input_id": "i1"}],
    "queue": {"live_inputs": 1, "undetermined_inputs": 0, "live_input_bytes": 12},
    "pending_wakes": 0,
    "pending_controls": 1,
}


async def test_the_new_methods_send_protocol_requests_and_read_typed_replies() -> None:
    seen: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        seen.append(request)
        if request["req"] == "get_runtime_state":
            send({"v": 1, "reply_to": request["id"], "ev": "runtime_state", "session_id": "s1", "state": STATE})
            return
        if request["req"] in ("apply_control", "get_control_receipt"):
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "control_receipt",
                    "session_id": "s1",
                    "receipt": {
                        "operation_id": request["operation_id"],
                        "action": request.get("action", "cancel_input"),
                        "outcome": "terminal",
                        "result": {"cancelled": "i1"},
                        "message": "terminal",
                    },
                }
            )
            return
        if request["req"] == "get_snapshot":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "snapshot",
                    "session_id": "s1",
                    "snapshot": {
                        "generation": "gen-1",
                        "seq": 4,
                        "marker_id": "marker-1",
                        "state": STATE,
                        "messages": [{"role": "user", "content": "hi"}],
                        "truncated": False,
                    },
                }
            )
            return
        if request["req"] == "ask_response":
            assert request["answer_id"] == "answer-1", "the answer id must travel"
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=MODERN, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert await client.get_runtime_state("s1") == STATE

        applied = await client.apply_control("s1", "op-1", "cancel_input", {"input_id": "i1"})
        assert applied["outcome"] == "terminal"
        assert applied["operation_id"] == "op-1"

        receipt = await client.get_control_receipt("s1", "op-2")
        assert receipt["operation_id"] == "op-2"

        snapshot = await client.get_snapshot("s1", tail=20)
        assert snapshot["marker_id"] == "marker-1"
        assert len(snapshot["messages"]) == 1

        await client.respond_to_ask_with_id("s1", "ask-1", "answer-1", [{"question_id": "q1", "selected": ["Fast"]}])

        assert len(seen) == 5, f"each call sends one request: {[request['req'] for request in seen]}"
        snapshot_request = next(request for request in seen if request["req"] == "get_snapshot")
        assert snapshot_request["tail"] == 20
        assert "max_bytes" not in snapshot_request
    finally:
        await client.close()
        await server.close()


async def test_the_legacy_answer_path_still_sends_no_answer_id() -> None:
    seen: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        seen.append(request)
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=MODERN, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        await client.respond_to_ask("s1", "ask-1", [{"question_id": "q1", "selected": ["Fast"]}])
        assert len(seen) == 1
        assert seen[0]["req"] == "ask_response"
        assert "answer_id" not in seen[0], "an answer without an id must not carry the field"
    finally:
        await client.close()
        await server.close()


async def test_a_server_without_the_capability_is_refused_before_anything_is_sent() -> None:
    seen: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        seen.append(request)
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=LEGACY, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        for capability in ("durable_runtime_v1", "snapshot_marker_v1", "ask_receipts_v1"):
            assert client.supports(capability) is False, f"{capability} was not advertised"

        calls = [
            lambda: client.get_runtime_state("s1"),
            lambda: client.apply_control("s1", "op-1", "cancel_input"),
            lambda: client.get_control_receipt("s1", "op-1"),
            lambda: client.get_snapshot("s1"),
            lambda: client.respond_to_ask_with_id("s1", "ask-1", "answer-1", []),
        ]
        for call in calls:
            with pytest.raises(HarnessError) as caught:
                await call()
            assert caught.value.code == "unsupported"
        assert seen == [], "a refused call must not reach the wire"
    finally:
        await client.close()
        await server.close()


async def test_a_gated_event_never_reaches_a_consumer_that_did_not_negotiate_it() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "ping":
            return
        send(
            {
                "v": 1,
                "ev": "snapshot_marker",
                "session_id": "s1",
                "marker": {"generation": "gen-1", "seq": 1, "marker_id": "marker-1"},
            }
        )
        send({"v": 1, "ev": "session_status", "session_id": "s1", "status": "idle"})
        send({"v": 1, "reply_to": request["id"], "ev": "pong"})

    server = await start_mock_harness(capabilities=LEGACY, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events()
    try:
        kinds: list[str] = []
        client._add_event_listener(lambda event: kinds.append(event["ev"]))
        await client.ping()
        await wait_until(lambda: len(kinds) > 0)
        assert kinds == ["session_status"], f"the marker must be dropped: {kinds}"
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_a_gated_event_reaches_a_consumer_that_negotiated_it() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "ping":
            return
        send(
            {
                "v": 1,
                "ev": "snapshot_marker",
                "session_id": "s1",
                "marker": {"generation": "gen-1", "seq": 1, "marker_id": "marker-1"},
            }
        )
        send({"v": 1, "reply_to": request["id"], "ev": "pong"})

    server = await start_mock_harness(capabilities=MODERN, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events()
    try:
        markers: list[dict[str, Any]] = []
        client._add_event_listener(lambda event: markers.append(event) if event["ev"] == "snapshot_marker" else None)
        await client.ping()
        await wait_until(lambda: bool(markers))
        assert len(markers) == 1
        assert markers[0]["marker"]["marker_id"] == "marker-1"
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_snapshot_with_buffered_events_keeps_an_event_that_raced_the_reply() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "get_snapshot":
            return
        # The live event arrives BEFORE the reply, which is exactly the race the
        # helper exists for: a consumer that subscribed after awaiting the
        # snapshot would never see it.
        send(
            {
                "v": 1,
                "ev": "snapshot_marker",
                "session_id": "s1",
                "marker": {"generation": "gen-1", "seq": 4, "marker_id": "marker-1"},
            }
        )
        send({"v": 1, "ev": "session_status", "session_id": "s1", "status": "idle"})
        send(
            {
                "v": 1,
                "reply_to": request["id"],
                "ev": "snapshot",
                "session_id": "s1",
                "snapshot": {
                    "generation": "gen-1",
                    "seq": 4,
                    "marker_id": "marker-1",
                    "state": STATE,
                    "messages": [{"role": "user", "content": "hi"}],
                    "truncated": False,
                },
            }
        )

    server = await start_mock_harness(capabilities=MODERN, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        result = await client.snapshot_with_buffered_events("s1", tail=20)
        assert result["snapshot"]["marker_id"] == "marker-1"
        kinds = [event["ev"] for event in result["events"]]
        assert "snapshot_marker" in kinds, f"the marker is buffered: {kinds}"
        assert "session_status" in kinds, f"the racing live event is not lost: {kinds}"
        assert kinds.index("snapshot_marker") < kinds.index("session_status"), (
            f"the marker keeps its place in the order: {kinds}"
        )
    finally:
        await client.close()
        await server.close()


async def test_snapshot_with_buffered_events_refuses_rather_than_buffering_without_a_marker() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "get_snapshot":
            return
        # No marker, ever. The helper must not hold a partial view forever.
        for _index in range(40):
            send(
                {
                    "v": 1,
                    "ev": "session_status",
                    "session_id": "s1",
                    "status": "idle",
                    "padding": "x" * 4096,
                }
            )
        send(
            {
                "v": 1,
                "reply_to": request["id"],
                "ev": "snapshot",
                "session_id": "s1",
                "snapshot": {
                    "generation": "gen-1",
                    "seq": 4,
                    "marker_id": "marker-1",
                    "state": STATE,
                    "messages": [],
                    "truncated": False,
                },
            }
        )

    server = await start_mock_harness(capabilities=MODERN, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        with pytest.raises(HarnessError, match="reconnect before trusting this view"):
            await client.snapshot_with_buffered_events("s1", tail=20, max_buffered_bytes=1024)
    finally:
        await client.close()
        await server.close()


async def test_snapshot_marker_events_are_delivered_in_order_to_a_stream() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "ping":
            return
        send(
            {
                "v": 1,
                "ev": "snapshot_marker",
                "session_id": "s1",
                "marker": {"generation": "gen-1", "seq": 1, "marker_id": "m1"},
            }
        )
        send({"v": 1, "ev": "session_status", "session_id": "s1", "status": "idle"})
        send({"v": 1, "reply_to": request["id"], "ev": "pong"})

    server = await start_mock_harness(capabilities=MODERN, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events("s1")
    try:
        await client.ping()
        first = await asyncio.wait_for(stream.__anext__(), 2)
        second = await asyncio.wait_for(stream.__anext__(), 2)
        assert (first["ev"], second["ev"]) == ("snapshot_marker", "session_status")
    finally:
        await stream.aclose()
        await client.close()
        await server.close()
