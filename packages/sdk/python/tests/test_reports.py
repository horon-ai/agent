"""The five requests the SDK used to reach only through ``request()``, and the
seven reply fields its types used to drop.

The mock harness proves which frame the SDK sends, field by field, and which
values it keeps out of the reply. The live e2e script proves the daemon answers
those same shapes, so a field renamed on one side cannot pass both.
"""

from __future__ import annotations

from collections.abc import AsyncIterator, Callable
from contextlib import asynccontextmanager
from typing import Any

from horon_agent_sdk import ConnectOptions, HoronAgentClient
from mock_harness import start_mock_harness

#: One stored session row, so the four listing fields have values to carry.
SESSION = {
    "session_id": "s1",
    "status": "idle",
    "title": "quokka notes",
    "saved": True,
    "save_label": "research",
    "updated_at_ms": 1_700_000_000_000,
    "last_active_at_ms": 1_699_999_000_000,
}

#: A row from a daemon that omitted every optional field, as the wire allows.
BARE_SESSION = {"session_id": "s2", "status": "idle"}

ROUTE = {"model": "claude-opus-5", "provider": "anthropic", "api_method": "messages"}

USAGE = {
    "sessions": [
        {
            "session_id": "s1",
            "model": "claude-opus-5",
            "input": 11,
            "output": 7,
            "cache_read_input": 3,
            "cache_creation_input": 2,
            "requests": 1,
        }
    ],
    "totals": {
        "input": 11,
        "output": 7,
        "cache_read_input": 3,
        "cache_creation_input": 2,
        "requests": 1,
        "sessions": 1,
    },
    "models": [
        {
            "model": "claude-opus-5",
            "input": 11,
            "output": 7,
            "cache_read_input": 3,
            "cache_creation_input": 2,
            "requests": 1,
            "sessions": 1,
        }
    ],
    "pricing": "unknown",
    "estimated_cost": None,
}

ARTIFACT = {"path": "out/chart.html", "kind": "html", "bytes": 1280, "mtime_ms": 1_700_000_500_000}

HIT = {"session_id": "s1", "role": "user", "preview": "...quokka...", "at": "2026-01-02T03:04:05Z"}

#: One provider call as a daemon that accounts for cache creation reports it.
TOKEN_USAGE = {"input": 11, "output": 7, "cache_read_input": 3, "cache_creation_input": 2}

#: The same call from a daemon that predates cache-creation accounting.
TOKEN_USAGE_WITHOUT_CREATION = {"input": 11, "output": 7, "cache_read_input": 3}


async def start_fixture(
    token_usage: dict[str, Any] = TOKEN_USAGE,
) -> tuple[HoronAgentClient, list[dict[str, Any]], Any]:
    requests: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send: Callable[[dict[str, Any]], None]) -> None:
        requests.append(request)
        tag = request["req"]
        if tag == "list_sessions":
            send({"v": 1, "reply_to": request["id"], "ev": "sessions", "sessions": [SESSION, BARE_SESSION]})
        elif tag == "get_runtime_info":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "runtime_info",
                    "session_id": "s1",
                    "provider": "anthropic",
                    "model": "claude-opus-5",
                    "reasoning_effort": "high",
                    "auth_method": "oauth",
                    "routes": [ROUTE],
                }
            )
        elif tag == "search_sessions":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "session_search_results",
                    "query": request["query"],
                    "results": [HIT],
                }
            )
        elif tag == "list_artifacts":
            send(
                {
                    "v": 1,
                    "reply_to": request["id"],
                    "ev": "artifacts",
                    "session_id": request["session_id"],
                    "artifacts": [ARTIFACT],
                }
            )
        elif tag == "usage_report":
            frame: dict[str, Any] = {"v": 1, "reply_to": request["id"], "ev": "usage_report", **USAGE}
            if "session_id" in request:
                frame["session_id"] = request["session_id"]
            send(frame)
        elif tag in ("approval_ack", "failover_ack"):
            # An ack is a message a client sends, so the bridge confirms it with
            # a bare `ok` and broadcasts the typed event to attached clients
            # separately. Answering with the event would pass here and fail
            # against the daemon.
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})
        elif tag == "send_message":
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})
            send({"v": 1, "ev": "message_accepted", "session_id": "s1"})
            send({"v": 1, "ev": "token_usage", "session_id": "s1", **token_usage})
            send({"v": 1, "ev": "turn_done", "session_id": "s1"})
        elif tag == "ping":
            send({"v": 1, "reply_to": request["id"], "ev": "pong"})
        else:
            send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=["sessions", "streaming"], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    return client, requests, server


@asynccontextmanager
async def fixture(
    token_usage: dict[str, Any] = TOKEN_USAGE,
) -> AsyncIterator[tuple[HoronAgentClient, list[dict[str, Any]]]]:
    client, requests, server = await start_fixture(token_usage)
    try:
        yield client, requests
    finally:
        await client.close()
        await server.close()


def fields(request: dict[str, Any]) -> dict[str, Any]:
    """The frame the SDK put on the wire, without the envelope the transport adds."""
    return {key: value for key, value in request.items() if key not in ("v", "id")}


def frame_for(requests: list[dict[str, Any]], req: str) -> dict[str, Any]:
    matches = [entry for entry in requests if entry.get("req") == req]
    assert matches, f"no {req} frame was sent"
    return fields(matches[-1])


async def test_search_sessions_sends_search_sessions_and_returns_the_hits() -> None:
    async with fixture() as (client, requests):
        hits = await client.search_sessions("quokka")

    frame = frame_for(requests, "search_sessions")
    assert frame == {"req": "search_sessions", "query": "quokka"}
    assert "limit" not in frame, "an omitted limit must not reach the wire"
    assert hits == [HIT]


async def test_search_sessions_carries_the_limit_when_one_is_given() -> None:
    async with fixture() as (client, requests):
        await client.search_sessions("quokka", 5)

    assert frame_for(requests, "search_sessions") == {"req": "search_sessions", "query": "quokka", "limit": 5}


async def test_list_artifacts_names_the_session_and_returns_its_artifacts() -> None:
    async with fixture() as (client, requests):
        artifacts = await client.list_artifacts("s1")

    assert frame_for(requests, "list_artifacts") == {"req": "list_artifacts", "session_id": "s1"}
    assert artifacts == [ARTIFACT]


async def test_usage_report_omits_the_session_for_an_account_wide_report() -> None:
    async with fixture() as (client, requests):
        report = await client.usage_report()

    frame = frame_for(requests, "usage_report")
    assert frame == {"req": "usage_report"}
    assert "session_id" not in frame, "an account-wide report names no session"
    assert "session_id" not in report
    assert report["totals"] == USAGE["totals"]
    assert report["models"] == USAGE["models"]
    assert report["pricing"] == "unknown"
    assert report["estimated_cost"] is None


async def test_usage_report_names_the_session_when_one_is_given() -> None:
    async with fixture() as (client, requests):
        report = await client.usage_report("s1")

    assert frame_for(requests, "usage_report") == {"req": "usage_report", "session_id": "s1"}
    assert report["session_id"] == "s1"
    assert report["sessions"] == USAGE["sessions"]


async def test_ack_approval_sends_the_request_id_it_retires_and_returns_the_receipt() -> None:
    async with fixture() as (client, requests):
        receipt = await client.ack_approval("s1", "req-1")

    assert frame_for(requests, "approval_ack") == {
        "req": "approval_ack",
        "session_id": "s1",
        "request_id": "req-1",
    }
    assert receipt == {"request_id": "req-1"}


async def test_ack_failover_carries_the_whole_announcement_key() -> None:
    async with fixture() as (client, requests):
        receipt = await client.ack_failover("s1", "chain-1", "model-a", "model-b", 2)

    assert frame_for(requests, "failover_ack") == {
        "req": "failover_ack",
        "session_id": "s1",
        "chain_id": "chain-1",
        "from": "model-a",
        "to": "model-b",
        "attempt": 2,
    }
    assert receipt == {"chain_id": "chain-1", "from_": "model-a", "to": "model-b", "attempt": 2}
    assert "from" not in receipt, "the receipt spells the reserved word with a trailing underscore"


async def test_list_sessions_keeps_saved_its_label_and_both_timestamps() -> None:
    async with fixture() as (client, _requests):
        sessions = await client.list_sessions()

    saved, bare = sessions
    assert saved["saved"] is True
    assert saved["save_label"] == "research"
    assert saved["updated_at_ms"] == 1_700_000_000_000
    assert saved["last_active_at_ms"] == 1_699_999_000_000
    assert "save_label" not in bare, "a row the daemon sent without a label keeps it absent"
    assert "saved" not in bare
    assert "updated_at_ms" not in bare
    assert "last_active_at_ms" not in bare


async def test_get_runtime_info_keeps_the_reasoning_effort_and_the_auth_method() -> None:
    async with fixture() as (client, _requests):
        info = await client.get_runtime_info("s1")

    assert info["model"] == "claude-opus-5"
    assert info["reasoning_effort"] == "high"
    assert info["auth_method"] == "oauth"


async def test_a_turn_keeps_the_cache_creation_tokens() -> None:
    async with fixture() as (client, _requests):
        turn = await client.run("s1", "hi")

    assert turn.usage is not None
    assert turn.usage.input == 11
    assert turn.usage.output == 7
    assert turn.usage.cache_read_input == 3
    assert turn.usage.cache_creation_input == 2


async def test_a_turn_without_cache_creation_accounting_keeps_its_old_shape() -> None:
    async with fixture(TOKEN_USAGE_WITHOUT_CREATION) as (client, _requests):
        turn = await client.run("s1", "hi")

    assert turn.usage is not None
    assert turn.usage.cache_creation_input is None, "an omitted count is unknown, not zero"
