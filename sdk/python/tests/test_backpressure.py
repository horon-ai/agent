"""Write-side backpressure and request admission.

A client that ignores the transport's own backpressure signal turns a slow peer
into unbounded memory in the caller's process, and an unbounded stream of
requests grows the pending map the same way. Both are checked here against a
transport the test controls byte for byte.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from horon_agent_sdk import ConnectOptions, HarnessError, HoronAgentClient, Transport
from mock_harness import wait_until


class ScriptedTransport(Transport):
    """An in-memory transport whose write result and drain signal the test drives."""

    def __init__(self, block_after: int | None, drainable: bool = True) -> None:
        self.written: list[str] = []
        self._data_listeners: list[Any] = []
        self._drain_listeners: list[Any] = []
        self._blocked = False
        if drainable:
            # A transport that reports backpressure without a drain signal has to
            # be modelled without the attribute at all: an ``on_drain`` that
            # silently drops its listener is exactly the trap the client refuses
            # to fall into.
            self.on_drain = self._drain_listeners.append
        self._block_after = block_after

    def write(self, data: str) -> bool | None:
        if self._blocked:
            raise AssertionError("written while blocked")
        self.written.append(data)
        if self._block_after is not None and len(self.written) == self._block_after:
            self._blocked = True
            return False
        return True

    def on_data(self, listener) -> None:
        self._data_listeners.append(listener)

    def on_close(self, listener) -> None:
        pass

    def close(self) -> None:
        pass

    def deliver(self, frame: dict[str, Any]) -> None:
        text = json.dumps(frame, ensure_ascii=False) + "\n"
        for listener in self._data_listeners:
            listener(text)

    def drain(self) -> None:
        self._blocked = False
        for listener in list(self._drain_listeners):
            listener()

    def request_ids(self) -> list[int]:
        ids: list[int] = []
        for line in self.written:
            line = line.strip()
            if line:
                ids.append(int(json.loads(line)["id"]))
        return ids


async def connect_with(transport: ScriptedTransport, max_pending_requests: int | None = None) -> HoronAgentClient:
    connecting = asyncio.create_task(
        HoronAgentClient.connect(
            ConnectOptions(
                transport=transport,
                max_pending_requests=max_pending_requests or 1024,
                request_timeout_ms=0,
            )
        )
    )
    # The handshake is itself a request, so its reply has to arrive while
    # ``connect`` is still awaiting it.
    await wait_until(lambda: len(transport.written) > 0)
    transport.deliver(
        {"v": 1, "reply_to": 1, "ev": "hello_ok", "version": 1, "server": "scripted/1", "capabilities": []}
    )
    return await connecting


async def test_a_blocked_transport_holds_frames_and_releases_them_on_drain_in_order() -> None:
    transport = ScriptedTransport(2)
    client = await connect_with(transport)
    assert transport.request_ids() == [1]

    first = asyncio.create_task(client.request({"req": "ping"}))
    await asyncio.sleep(0)
    assert len(transport.written) == 2, "the first request is written"

    second = asyncio.create_task(client.request({"req": "ping"}))
    third = asyncio.create_task(client.request({"req": "ping"}))
    await asyncio.sleep(0)
    assert transport.request_ids() == [1, 2], "a transport that reported backpressure must not be written to again"

    transport.drain()
    assert transport.request_ids() == [1, 2, 3, 4], "queued frames flush in order on drain"

    for request_id in (2, 3, 4):
        transport.deliver({"v": 1, "reply_to": request_id, "ev": "pong"})
    assert (await first)["ev"] == "pong"
    assert (await second)["ev"] == "pong"
    assert (await third)["ev"] == "pong"
    await client.close()


async def test_a_transport_that_returns_nothing_keeps_working_unchanged() -> None:
    transport = ScriptedTransport(None)
    client = await connect_with(transport)
    reply = asyncio.create_task(client.request({"req": "ping"}))
    await asyncio.sleep(0)
    assert transport.request_ids() == [1, 2]
    transport.deliver({"v": 1, "reply_to": 2, "ev": "pong"})
    assert (await reply)["ev"] == "pong"
    await client.close()


async def test_a_transport_that_reports_backpressure_without_a_drain_signal_is_refused() -> None:
    transport = ScriptedTransport(2, drainable=False)
    client = await connect_with(transport)
    first = asyncio.create_task(client.request({"req": "ping"}))
    second = asyncio.create_task(client.request({"req": "ping"}))
    await asyncio.sleep(0)
    with pytest.raises(HarnessError) as caught:
        await second
    assert caught.value.code == "transport_backpressure"
    transport.deliver({"v": 1, "reply_to": 2, "ev": "pong"})
    assert (await first)["ev"] == "pong"
    await client.close()


async def test_the_outstanding_request_limit_is_enforced_and_surfaced() -> None:
    transport = ScriptedTransport(None)
    client = await connect_with(transport, max_pending_requests=2)

    in_flight = [
        asyncio.create_task(client.request({"req": "ping"})),
        asyncio.create_task(client.request({"req": "ping"})),
    ]
    await asyncio.sleep(0)
    with pytest.raises(HarnessError) as caught:
        await client.request({"req": "ping"})
    assert caught.value.code == "too_many_requests"
    assert "max_pending_requests 2" in str(caught.value)
    assert len(transport.request_ids()) == 3, "the refused request never reaches the wire"

    transport.deliver({"v": 1, "reply_to": 2, "ev": "pong"})
    await in_flight[0]
    # Admission is released by the reply, so the next request is accepted.
    accepted = asyncio.create_task(client.request({"req": "ping"}))
    await asyncio.sleep(0)
    transport.deliver({"v": 1, "reply_to": 4, "ev": "pong"})
    assert (await accepted)["ev"] == "pong"
    await client.close()
