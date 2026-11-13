from __future__ import annotations

from typing import Any

from horon_agent_sdk import ConnectOptions, HoronAgentClient
from mock_harness import start_mock_harness

MODES = {"follow_up_mode": "all", "steering_mode": "one-at-a-time", "interrupt_mode": "wait"}


async def test_get_message_modes_returns_the_daemon_modes() -> None:
    requests: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        requests.append(request)
        if request["req"] == "create_session":
            send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": {"session_id": "s1"}})
        elif request["req"] == "get_message_modes":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "message_modes",
                    "session_id": request["session_id"],
                    "modes": MODES,
                }
            )
        else:
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=[], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert await client.get_message_modes("s1") == MODES
        request = next(entry for entry in requests if entry["req"] == "get_message_modes")
        assert request["session_id"] == "s1"
    finally:
        await client.close()
        await server.close()
