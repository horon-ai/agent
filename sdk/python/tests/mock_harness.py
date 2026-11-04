"""In-process mock harness server for SDK tests.

Speaks the same NDJSON frames as the bridge so client tests exercise real
framing and reply correlation instead of stubbed methods.
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import tempfile
from collections.abc import Callable
from typing import Any

from horon_agent_sdk.framing import NdjsonDecoder


async def in_order(items: list[Any], step: Callable[[Any], Any]) -> None:
    """Await each step in order, one at a time."""
    for item in items:
        result = step(item)
        if result is not None:
            await result


async def wait_until(done: Callable[[], bool], attempts: int = 100) -> None:
    """Poll ``done`` every 5 ms, up to ``attempts`` times."""
    for _ in range(attempts):
        if done():
            return
        await asyncio.sleep(0.005)


class MockServer:
    """A listening Unix-socket harness with scripted request handling."""

    def __init__(
        self,
        socket_path: str,
        directory: str,
        server: asyncio.AbstractServer,
        clients: set[asyncio.StreamWriter],
        capabilities: list[str],
        on_request: Callable[[dict[str, Any], Callable[[dict[str, Any]], None]], None] | None,
    ) -> None:
        self.socket_path = socket_path
        self._directory = directory
        self._server = server
        self._clients = clients
        self._capabilities = capabilities
        self._on_request = on_request
        self._tasks: set[asyncio.Task[None]] = set()
        #: Every hello frame a client sent, so tests can assert on its flags.
        self.hellos: list[dict[str, Any]] = []

    def client_count(self) -> int:
        return len(self._clients)

    def broadcast(self, event: dict[str, Any]) -> None:
        """Push an unsolicited event to every connected client."""
        line = json.dumps(event, ensure_ascii=False) + "\n"
        for writer in list(self._clients):
            if not writer.is_closing():
                writer.write(line.encode("utf-8"))

    async def close(self) -> None:
        for writer in list(self._clients):
            writer.close()
        self._server.close()
        for task in list(self._tasks):
            task.cancel()
        if self._tasks:
            await asyncio.gather(*self._tasks, return_exceptions=True)
        await self._server.wait_closed()
        shutil.rmtree(self._directory, ignore_errors=True)

    async def _handle_client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self._clients.add(writer)
        task = asyncio.current_task()
        if task is not None:
            self._tasks.add(task)
        decoder: NdjsonDecoder[dict[str, Any]] = NdjsonDecoder()

        def send(frame: dict[str, Any]) -> None:
            if not writer.is_closing():
                writer.write((json.dumps(frame, ensure_ascii=False) + "\n").encode("utf-8"))

        try:
            while True:
                chunk = await reader.read(65536)
                if not chunk:
                    return
                for request in decoder.push(chunk):
                    if request.get("req") == "hello":
                        send(
                            {
                                "v": 1,
                                "reply_to": request["id"],
                                "ev": "hello_ok",
                                "version": 1,
                                "server": "mock/0.1",
                                "capabilities": self._capabilities,
                            }
                        )
                        self.hellos.append(request)
                        continue
                    if self._on_request is not None:
                        self._on_request(request, send)
        except (asyncio.CancelledError, ConnectionError):
            return
        finally:
            self._clients.discard(writer)
            writer.close()
            if task is not None:
                self._tasks.discard(task)


async def start_mock_harness(
    capabilities: list[str] | None = None,
    on_request: Callable[[dict[str, Any], Callable[[dict[str, Any]], None]], None] | None = None,
) -> MockServer:
    directory = tempfile.mkdtemp(prefix="horon-agent-sdk-test-")
    socket_path = os.path.join(directory, "api.sock")
    clients: set[asyncio.StreamWriter] = set()

    holder: dict[str, MockServer] = {}

    async def handle(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        await holder["server"]._handle_client(reader, writer)

    server = await asyncio.start_unix_server(handle, path=socket_path)
    mock = MockServer(
        socket_path=socket_path,
        directory=directory,
        server=server,
        clients=clients,
        capabilities=capabilities if capabilities is not None else ["sessions", "streaming"],
        on_request=on_request,
    )
    holder["server"] = mock
    return mock
