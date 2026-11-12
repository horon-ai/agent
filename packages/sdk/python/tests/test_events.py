from __future__ import annotations

import asyncio
from typing import Any

import pytest

from horon_agent_sdk import ConnectOptions, HarnessError, HoronAgentClient
from mock_harness import start_mock_harness, wait_until


async def start_counting_harness():
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "ping":
            send({"v": 1, "reply_to": request["id"], "ev": "pong"})

    return await start_mock_harness(on_request=on_request)


FLOOD_TEXT = "x" * 200


def flood_ordinary(server, count: int) -> None:
    for index in range(count):
        server.broadcast({"v": 1, "ev": "text_delta", "session_id": "s1", "text": f"{index}:{FLOOD_TEXT}"})


async def drain_stream(stream) -> dict[str, Any]:
    kinds: list[str] = []
    while True:
        try:
            event = await asyncio.wait_for(stream.__anext__(), 2)
        except StopAsyncIteration:
            return {"kinds": kinds}
        except HarnessError as error:
            return {"kinds": kinds, "error": error}
        kinds.append(str(event["ev"]))


async def test_events_buffer_while_the_consumer_is_busy_and_filter_by_session() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events("s1")
    collected: list[str] = []

    async def consume() -> None:
        async for event in stream:
            if event.get("ev") == "text_delta":
                collected.append(str(event["text"]))
                await asyncio.sleep(0.01)
            if event.get("ev") == "turn_done":
                break

    consumer = asyncio.create_task(consume())
    for text in ("a", "b", "c"):
        server.broadcast({"v": 1, "ev": "text_delta", "session_id": "s1", "text": text})
    server.broadcast({"v": 1, "ev": "text_delta", "session_id": "s2", "text": "x"})
    server.broadcast({"v": 1, "ev": "turn_done", "session_id": "s1"})
    await asyncio.wait_for(consumer, 5)
    assert collected == ["a", "b", "c"]
    await stream.aclose()
    await client.close()
    await server.close()


async def test_events_close_settles_a_pending_next_call() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events("s1")
    pending = asyncio.create_task(stream.__anext__())
    await asyncio.sleep(0.01)
    await stream.aclose()
    with pytest.raises(StopAsyncIteration):
        await asyncio.wait_for(pending, 2)
    await client.close()
    await server.close()


async def test_events_bound_a_consumer_that_never_reads_then_fail_loudly() -> None:
    server = await start_counting_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events("s1", max_buffered_events=8, max_buffered_bytes=4096)
    try:
        flood_ordinary(server, 64)
        # The pong is written after the flood, so every flood frame is decoded by
        # the time this resolves.
        await client.ping()

        stats = stream.buffered
        assert stats["events"] <= 8, f"buffered {stats['events']} events, budget is 8"
        assert stats["bytes"] <= 4096, f"buffered {stats['bytes']} bytes, budget is 4096"

        outcome = await drain_stream(stream)
        error = outcome["error"]
        assert error is not None and error.code == "event_buffer_overflow"
        assert "ordinary frames were dropped" in str(error)
        assert len(outcome["kinds"]) <= 8

        # The stream is closed: no half-buffered event survives the overflow.
        with pytest.raises(StopAsyncIteration):
            await stream.__anext__()
        assert stream.buffered == {"events": 0, "bytes": 0}
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_a_control_frame_survives_an_overflowing_ordinary_flood() -> None:
    server = await start_counting_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events("s1", max_buffered_events=8, max_buffered_bytes=4096)
    try:
        flood_ordinary(server, 64)
        server.broadcast(
            {
                "v": 1,
                "ev": "permission_request",
                "session_id": "s1",
                "request_id": "p1",
                "tool_name": "bash",
                "description": "rm -rf build",
            }
        )
        await client.ping()

        outcome = await drain_stream(stream)
        assert "permission_request" in outcome["kinds"], "the approval was lost in the flood"
        assert outcome["error"] is not None and outcome["error"].code == "event_buffer_overflow"
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_a_full_control_reserve_closes_the_stream_instead_of_dropping_approvals() -> None:
    server = await start_counting_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    stream = client.events("s1", max_buffered_events=8, max_buffered_bytes=4096)
    try:
        flood_ordinary(server, 64)
        for index in range(8):
            server.broadcast(
                {
                    "v": 1,
                    "ev": "permission_request",
                    "session_id": "s1",
                    "request_id": f"p{index}",
                    "tool_name": "bash",
                    "description": "ls",
                }
            )
        await client.ping()

        assert stream.buffered == {"events": 0, "bytes": 0}
        with pytest.raises(HarnessError) as caught:
            await stream.__anext__()
        assert caught.value.code == "event_buffer_overflow"
        assert "control event reserve" in str(caught.value)
        with pytest.raises(StopAsyncIteration):
            await stream.__anext__()
    finally:
        await stream.aclose()
        await client.close()
        await server.close()


async def test_events_release_their_subscription_and_buffer_on_close_and_on_client_close() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    baseline = len(client._event_listeners)
    stream = client.events("s1")
    try:
        assert len(client._event_listeners) == baseline + 1
        server.broadcast({"v": 1, "ev": "text_delta", "session_id": "s1", "text": "x"})
        await wait_until(lambda: stream.buffered["events"] == 1)

        await stream.aclose()
        assert len(client._event_listeners) == baseline
        assert stream.buffered == {"events": 0, "bytes": 0}

        second = client.events("s1")
        assert len(client._event_listeners) == baseline + 1
        await client.close()
        await wait_until(lambda: len(client._event_listeners) == baseline)
        with pytest.raises(StopAsyncIteration):
            await second.__anext__()
        assert second.buffered == {"events": 0, "bytes": 0}
    finally:
        await stream.aclose()
        await client.close()
        await server.close()
