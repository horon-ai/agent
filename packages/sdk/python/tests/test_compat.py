"""Small compatibility surfaces ported from the TypeScript suite.

Model-usage ordering, session presence passthrough, create-session wire shapes,
the hello capability flags, and the README error reference.
"""

from __future__ import annotations

import pathlib
import re
from functools import cmp_to_key
from typing import Any

import pytest

from horon_agent_sdk import ConnectOptions, HoronAgentClient, compare_model_usage
from mock_harness import start_mock_harness

SRC = pathlib.Path(__file__).resolve().parents[1] / "src/horon_agent_sdk"
README = pathlib.Path(__file__).resolve().parents[1] / "README.md"
RUST_EVENTS = pathlib.Path(__file__).resolve().parents[4] / "crates/harness-api/src/events.rs"


def test_route_usage_ranks_tracked_turns_and_prior_selections_separately() -> None:
    legacy = {"count": 0, "selection_count": 10, "last_selected_unix_secs": 99}
    frequent = {"count": 2, "selection_count": 0, "last_used_unix_secs": 10, "tracking_started_unix_secs": 1}
    recent = {"count": 1, "selection_count": 0, "last_used_unix_secs": 20, "tracking_started_unix_secs": 1}
    unused = {"count": 0, "selection_count": 0, "tracking_started_unix_secs": 1}
    rows = [unused, legacy, recent, frequent]
    rows.sort(key=cmp_to_key(compare_model_usage))
    assert rows == [frequent, recent, legacy, unused]
    assert compare_model_usage(frequent, frequent) == 0
    assert compare_model_usage({**frequent, "last_used_unix_secs": 11}, frequent) < 0
    assert compare_model_usage({**legacy, "last_selected_unix_secs": 100}, legacy) < 0
    assert compare_model_usage(None, None) == 0
    assert compare_model_usage(None, unused) > 0


@pytest.mark.parametrize("presence", [None, False, True])
async def test_session_user_presence_survives_list_and_attach(presence: bool | None) -> None:
    session: dict[str, Any] = {"session_id": "session-presence", "status": "idle"}
    if presence is not None:
        session["user_connected"] = presence

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] == "list_sessions":
            send({"v": 1, "reply_to": request["id"], "ev": "sessions", "sessions": [session]})
        elif request["req"] == "attach_session":
            send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": session})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        [listed] = await client.list_sessions()
        attached = await client.attach_session("session-presence")
        for result in (listed, attached):
            assert result == session
            assert result.get("user_connected", False) is (presence or False)
            assert ("user_connected" in result) is (presence is not None)
    finally:
        await client.close()
        await server.close()


CASES = [
    (
        "provided",
        {"working_dir": "/project", "system_prompt": "Review precisely.\nKeep Unicode: λ"},
        "/project",
        "Review precisely.\nKeep Unicode: λ",
    ),
    ("empty", {"system_prompt": ""}, None, ""),
    ("omitted", {"working_dir": "/project"}, "/project", None),
    ("undefined", {"system_prompt": None}, None, None),
    ("default options", {}, None, None),
    ("no arguments", None, None, None),
    ("legacy working directory", "/legacy", "/legacy", None),
]


@pytest.mark.parametrize(("name", "options", "working_dir", "prompt"), CASES, ids=[case[0] for case in CASES])
async def test_create_session_system_prompt_wire_behavior(
    name: str, options: Any, working_dir: str | None, prompt: str | None
) -> None:
    requests: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send) -> None:
        requests.append(request)
        send({"v": 1, "reply_to": request["id"], "ev": "attached", "session": {"session_id": "created"}})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        if options is None:
            session = await client.create_session()
        elif isinstance(options, str):
            session = await client.create_session(options)
        elif "system_prompt" in options or "working_dir" in options:
            session = await client.create_session_with_options(
                working_dir=options.get("working_dir"), system_prompt=options.get("system_prompt")
            )
        else:
            session = await client.create_session_with_options()
        assert session["session_id"] == "created"
        create_requests = [request for request in requests if request["req"] == "create_session"]
        assert len(create_requests) == 1
        wire = create_requests[0]
        assert wire["req"] == "create_session"
        assert wire.get("working_dir") == working_dir
        assert wire.get("system_prompt") == prompt
        assert ("system_prompt" in wire) is (prompt is not None)
        assert "systemPrompt" not in wire
    finally:
        await client.close()
        await server.close()


async def test_inline_user_response_capability_is_explicitly_opted_in_at_hello() -> None:
    for enabled in (False, True):
        server = await start_mock_harness()
        client = await HoronAgentClient.connect(
            ConnectOptions(socket_path=server.socket_path, supports_user_response=enabled)
        )
        try:
            hello = server.hellos[0]
            assert hello["req"] == "hello"
            assert hello.get("supports_user_response", False) is enabled
            assert hello.get("supports_ask_requests", False) is False
        finally:
            await client.close()
            await server.close()


def snake_case(variant: str) -> str:
    return re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", variant).lower()


def client_error_codes() -> list[str]:
    codes: set[str] = set()
    for path in SRC.glob("*.py"):
        source = path.read_text(encoding="utf-8")
        codes.update(re.findall(r'HarnessError\(\s*"([a-z_]+)"', source))
        codes.update(re.findall(r'super\(\).__init__\(\s*"([a-z_]+)"', source))
    return sorted(codes)


def server_error_codes() -> list[str]:
    source = RUST_EVENTS.read_text(encoding="utf-8")
    start = source.index("pub enum ErrorCode {")
    body = source[start:].split("\n}", 1)[0]
    return [snake_case(match) for match in re.findall(r"^ {4}([A-Z][A-Za-z0-9]*),$", body, re.MULTILINE)]


@pytest.mark.skipif(not RUST_EVENTS.exists(), reason="vendored build without crates/")
def test_readme_documents_every_stable_sdk_and_server_error_code() -> None:
    readme = README.read_text(encoding="utf-8")
    missing = [code for code in sorted({*client_error_codes(), *server_error_codes()}) if f"`{code}`" not in readme]
    assert missing == [], f"undocumented error codes: {', '.join(missing)}"
