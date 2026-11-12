"""Session replication and cursor-history surface.

A mock harness proves the SDK builds the right frames and reads the right
replies. The canonical-bytes case matters most: ``canonical_json`` must survive
as text, because a u64 above 2**53, ``1.0``, and ``-0.0`` are destroyed by any
parse-and-restringify round trip.
"""

from __future__ import annotations

from typing import Any

import pytest

from horon_agent_sdk import ConnectOptions, HarnessError, HoronAgentClient
from mock_harness import start_mock_harness

CANONICAL = '{"schema_version":1,"payload":{"big":18446744073709551615,"one":1.0,"negative_zero":-0.0}}'

REPLICATION_CAPABILITIES = [
    "sessions",
    "streaming",
    "session_event_store_v1",
    "session_replication_v1",
    "session_history_cursor_v1",
    "structured_compaction_v1",
]


async def test_the_bridges_four_session_store_capabilities_are_all_readable() -> None:
    server = await start_mock_harness(capabilities=REPLICATION_CAPABILITIES)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        for capability in (
            "session_event_store_v1",
            "session_replication_v1",
            "session_history_cursor_v1",
            "structured_compaction_v1",
        ):
            assert client.supports(capability) is True, f"missing {capability}"
    finally:
        await client.close()
        await server.close()


async def test_replication_capabilities_gate_the_new_methods() -> None:
    server = await start_mock_harness(capabilities=["sessions", "streaming"])
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    calls = [
        lambda: client.list_session_heads("s1"),
        lambda: client.select_session_head("s1", "h1"),
        lambda: client.get_history_page("s1"),
        lambda: client.export_session_events("s1"),
        lambda: client.import_session_events({"session_id": "s1", "heads": [], "events": []}),
        lambda: client.export_session_blob("s1", "a" * 64, 1, 0),
        lambda: client.session_blob_status("s1", "a" * 64, 1),
    ]
    try:
        for call in calls:
            with pytest.raises(HarnessError) as caught:
                await call()
            assert caught.value.code == "unsupported"
            assert "capability" in str(caught.value)
    finally:
        await client.close()
        await server.close()


async def test_heads_selection_and_history_pages_round_trip() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] in ("list_session_heads", "select_session_head"):
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "session_heads",
                    "session_id": request["session_id"],
                    "heads": ["h1", "h2"],
                    "selected_head": request.get("head_event_id", "h1"),
                }
            )
            return
        if request["req"] == "get_history_page":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "session_history_page",
                    "session_id": request["session_id"],
                    "head": request.get("head_event_id", "h1"),
                    "messages": [
                        {
                            "role": "user",
                            "content": f"limit={request.get('limit')} before={request.get('before_event_id')}",
                        }
                    ],
                    "next_cursor": "e0",
                }
            )
            return
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=REPLICATION_CAPABILITIES, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        heads = await client.list_session_heads("s1")
        assert heads == {"session_id": "s1", "heads": ["h1", "h2"], "selected_head": "h1"}

        selected = await client.select_session_head("s1", "h2")
        assert selected["selected_head"] == "h2"

        page = await client.get_history_page("s1", head_event_id="h1", before_event_id="e5", limit=2)
        assert page["head"] == "h1"
        assert page["messages"][0]["content"] == "limit=2 before=e5"
        assert page["next_cursor"] == "e0"
    finally:
        await client.close()
        await server.close()


async def test_exported_canonical_bytes_are_never_parsed_or_reserialized() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "export_session_events":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "session_event_page",
                    "session_id": request["session_id"],
                    "heads": ["h1"],
                    "events": [
                        {
                            "event_id": "11111111-1111-4111-8111-111111111111",
                            "content_hash": "c" * 64,
                            "canonical_json": CANONICAL,
                        }
                    ],
                    "record_refs": [
                        {
                            "position": 0,
                            "event_id": "22222222-2222-4222-8222-222222222222",
                            "content_hash": "d" * 64,
                            "sha256": "e" * 64,
                            "size": 4096,
                        }
                    ],
                }
            )
            return
        if request["req"] == "import_session_events":
            assert request["events"][0]["canonical_json"] == CANONICAL
            assert request["protocol_version"] == 1
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "session_import_result",
                    "session_id": request["session_id"],
                    "accepted_ids": [request["events"][0]["event_id"]],
                    "heads": ["h1"],
                }
            )
            return
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=REPLICATION_CAPABILITIES, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        page = await client.export_session_events("s1")
        assert page["events"][0]["canonical_json"] == CANONICAL
        assert "18446744073709551615" in page["events"][0]["canonical_json"]
        assert "-0.0" in page["events"][0]["canonical_json"]
        assert page["record_refs"][0]["size"] == 4096

        imported = await client.import_session_events(page)
        assert imported["accepted_ids"] == ["11111111-1111-4111-8111-111111111111"]
        assert imported["heads"] == ["h1"]
    finally:
        await client.close()
        await server.close()


async def test_typed_store_details_reach_the_caller_and_stay_optional() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "import_session_events":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "error",
                    "code": "invalid_request",
                    "message": "same id, different hash",
                    "details": {
                        "store_code": "hash_conflict",
                        "retryable": False,
                        "accepted_ids": ["a1"],
                        "conflict_heads": ["h1", "h2"],
                    },
                }
            )
            return
        send(
            {
                "v": 1,
                "reply_to": request["id"],
                "ev": "error",
                "code": "internal",
                "message": "session writer is already owned",
                "details": {"store_code": "busy", "retryable": True},
            }
        )

    server = await start_mock_harness(capabilities=REPLICATION_CAPABILITIES, on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        with pytest.raises(HarnessError) as caught:
            await client.import_session_events({"session_id": "s1", "heads": [], "events": []})
        assert caught.value.code == "invalid_request"

        conflict = await client.request({"req": "import_session_events", "session_id": "s1", "events": []})
        assert conflict["ev"] == "error"
        assert conflict["details"]["store_code"] == "hash_conflict"
        assert conflict["details"]["retryable"] is False

        busy = await client.request(
            {"req": "get_session_blob_status", "session_id": "s1", "sha256": "a" * 64, "total_size": 1}
        )
        assert busy["details"]["store_code"] == "busy"
        assert busy["details"]["retryable"] is True
    finally:
        await client.close()
        await server.close()


async def test_a_replication_request_above_the_wire_envelope_is_refused_locally() -> None:
    server = await start_mock_harness(
        capabilities=REPLICATION_CAPABILITIES,
        on_request=lambda request, send: send({"v": 1, "ev": "ok"}),
    )
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    oversized = {
        "session_id": "s1",
        "heads": [],
        "events": [
            {
                "event_id": "11111111-1111-4111-8111-111111111111",
                "content_hash": "c" * 64,
                "canonical_json": "x" * (17 * 1024 * 1024),
            }
        ],
    }
    try:
        with pytest.raises(HarnessError) as caught:
            await client.import_session_events(oversized)
        assert caught.value.code == "invalid_request"
        assert "16777216" in str(caught.value)
    finally:
        await client.close()
        await server.close()
