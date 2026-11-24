from __future__ import annotations

from typing import Any

import pytest

from horon_agent_sdk import (
    A2UI_BASIC_CATALOG_V1_0,
    A2UI_VERSION_V1_0,
    A2uiEnvelopeEvent,
    A2uiSessionCapabilities,
    AttachSessionOptions,
    ConnectOptions,
    CreateSessionOptions,
    HarnessError,
    HoronAgentClient,
)
from horon_agent_sdk.protocol import ADVERTISED_CAPABILITIES, API_VERSION_MINOR, EVENT_CAPABILITIES
from mock_harness import start_mock_harness

DEFAULT_CAPS: A2uiSessionCapabilities = {
    "version": A2UI_VERSION_V1_0,
    "supported_catalog_ids": [A2UI_BASIC_CATALOG_V1_0],
    "accepts_inline_catalogs": False,
}


async def test_a2ui_defaults_omit_wire_fields() -> None:
    assert API_VERSION_MINOR == 32
    assert "a2ui_v1" in ADVERTISED_CAPABILITIES
    assert "a2ui_v1" in EVENT_CAPABILITIES
    assert ConnectOptions().a2ui is None
    assert CreateSessionOptions().a2ui is None
    assert AttachSessionOptions().a2ui is None

    seen: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        seen.append(request)
        if request["req"] in ("create_session", "attach_session"):
            send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": {"session_id": "s1"}})

    server = await start_mock_harness(capabilities=["a2ui_v1"], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        assert client.supports("a2ui_v1")
        await client.create_session()
        await client.attach_session("existing")
        assert "a2ui" not in server.hellos[0]
        assert "a2ui" not in seen[0]
        assert "a2ui" not in seen[1]
    finally:
        await client.close()
        await server.close()


async def test_a2ui_opt_in_roundtrip_and_turn_collection() -> None:
    seen: list[dict[str, Any]] = []
    envelope = {"version": "v0.9", "createSurface": {"surfaceId": "main", "catalogId": A2UI_BASIC_CATALOG_V1_0}}

    def on_request(request: dict[str, Any], send) -> None:
        seen.append(request)
        kind = request["req"]
        if kind in ("create_session", "attach_session"):
            send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": {"session_id": "s1"}})
            return
        if kind == "send_message":
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})
            send({"v": 1, "ev": "message_accepted", "session_id": "s1"})
            send(
                {
                    "v": 1,
                    "ev": "a2ui_envelope",
                    "session_id": "s1",
                    "tool_call_id": "call-1",
                    "sequence": 0,
                    "envelope": envelope,
                    "fallback_text": "Card text",
                }
            )
            send({"v": 1, "ev": "text_delta", "session_id": "s1", "text": "Done"})
            send({"v": 1, "ev": "turn_done", "session_id": "s1"})
            return
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=["a2ui_v1"], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path, a2ui=DEFAULT_CAPS))
    try:
        await client.create_session_with_options(CreateSessionOptions(a2ui=DEFAULT_CAPS))
        await client.attach_session_with_options("s1", AttachSessionOptions(a2ui=DEFAULT_CAPS))
        await client.configure_a2ui("s1", DEFAULT_CAPS)
        await client.configure_a2ui("s1", None)
        await client.send_a2ui_event("s1", {"version": "v0.9", "action": {"name": "go", "surfaceId": "main"}}, {"k": 1})
        streamed: list[str] = []
        turn = await client.run("s1", "go", on_event=lambda ev: streamed.append(str(ev.get("ev"))))

        assert server.hellos[0]["a2ui"] == DEFAULT_CAPS
        assert seen[0]["a2ui"] == DEFAULT_CAPS
        assert seen[1]["a2ui"] == DEFAULT_CAPS
        assert seen[2]["capabilities"] == DEFAULT_CAPS
        assert seen[3]["capabilities"] is None
        assert seen[4]["data_model"] == {"k": 1}
        assert "a2ui_envelope" in streamed
        assert turn.a2ui_envelopes == [
            A2uiEnvelopeEvent(tool_call_id="call-1", sequence=0, envelope=envelope, fallback_text="Card text")
        ]
    finally:
        await client.close()
        await server.close()


async def test_a2ui_unsupported_refuses_and_drops_unnegotiated_envelopes() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "send_message":
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})
            send({"v": 1, "ev": "message_accepted", "session_id": "s1"})
            send(
                {
                    "v": 1,
                    "ev": "a2ui_envelope",
                    "session_id": "s1",
                    "tool_call_id": "c1",
                    "sequence": 0,
                    "envelope": {},
                }
            )
            send({"v": 1, "ev": "turn_done", "session_id": "s1"})
            return
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=[], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        with pytest.raises(HarnessError) as exc:
            await client.configure_a2ui("s1", DEFAULT_CAPS)
        assert exc.value.code == "unsupported"
        with pytest.raises(HarnessError) as exc2:
            await client.send_a2ui_event("s1", {"version": "v0.9"})
        assert exc2.value.code == "unsupported"
        turn = await client.run("s1", "hi")
        assert turn.a2ui_envelopes == []
    finally:
        await client.close()
        await server.close()
