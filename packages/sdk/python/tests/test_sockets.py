from __future__ import annotations

import pathlib
import re
import sys

import pytest

from horon_agent_sdk import sockets


@pytest.fixture
def windows(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "platform", "win32")


def test_the_windows_pipe_name_matches_horon_agent_transport_exactly(windows: None) -> None:
    """Parsed with win32 semantics regardless of the host, so the check runs anywhere."""
    cases = [
        (
            "C:\\Users\\user\\AppData\\Local\\horon-agent\\run\\horon-agent-api.sock",
            "\\\\.\\pipe\\horon-agent-api-249ba7051747c288",
        ),
        ("C:\\a\\b\\horon-agent.sock", "\\\\.\\pipe\\horon-agent-69098f328f40a6df"),
    ]
    for socket_path, expected in cases:
        assert sockets.transport_endpoint(socket_path) == expected, f"pipe name for {socket_path} drifted"


def test_case_and_separators_normalize_the_same_way(windows: None) -> None:
    assert sockets.transport_endpoint("C:\\Temp\\horon-agent\\server.sock") == sockets.transport_endpoint(
        "c:/temp/HORON-AGENT/server.sock"
    )


def test_a_path_with_no_usable_characters_still_yields_a_name(windows: None) -> None:
    assert re.match(r"^\\\\\.\\pipe\\horon-agent-[0-9a-f]{16}$", sockets.transport_endpoint("/tmp/!!!.sock"))


def test_transport_endpoint_is_the_path_itself_outside_windows() -> None:
    assert sockets.transport_endpoint("/run/user/1000/horon-agent-api.sock") == "/run/user/1000/horon-agent-api.sock"


def test_transport_endpoint_derives_the_pipe_on_windows(windows: None) -> None:
    assert sockets.transport_endpoint("C:\\a\\b\\horon-agent.sock") == "\\\\.\\pipe\\horon-agent-69098f328f40a6df"


def test_api_socket_path_follows_the_environment(monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path) -> None:
    monkeypatch.setenv("HORON_AGENT_API_SOCKET", str(tmp_path / "explicit.sock"))
    assert sockets.api_socket_path() == str(tmp_path / "explicit.sock")

    monkeypatch.delenv("HORON_AGENT_API_SOCKET")
    monkeypatch.setenv("HORON_AGENT_RUNTIME_DIR", str(tmp_path))
    assert sockets.api_socket_path() == str(tmp_path / "horon-agent-api.sock")
    assert sockets.legacy_socket_path() == str(tmp_path / "horon-agent.sock")
