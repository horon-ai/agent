from __future__ import annotations

import json
from typing import Any, TypedDict

from horon_agent_sdk import ConnectOptions, HoronAgentClient, NdjsonDecoder, TurnResult
from mock_harness import start_mock_harness

FAILED = {"call_id": "c1", "name": "bash", "output": "exit status 2", "error": "exit status 2", "verdict": "failed"}


async def turn_with(tool_done: list[dict[str, Any]]) -> TurnResult:
    """Runs one turn against a harness that answers ``send_message`` with the given ``tool_done`` events."""

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "send_message":
            return
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})
        send({"v": 1, "ev": "message_accepted", "session_id": "s1"})
        for event in tool_done:
            send({"v": 1, "ev": "tool_done", "session_id": "s1", **event})
        send({"v": 1, "ev": "turn_done", "session_id": "s1"})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        return await client.run("s1", "hi")
    finally:
        await client.close()
        await server.close()


async def test_a_failed_verdict_reaches_the_tool_call() -> None:
    turn = await turn_with([FAILED])

    assert [(call.call_id, call.name, call.output, call.error, call.verdict) for call in turn.tool_calls] == [
        ("c1", "bash", "exit status 2", "exit status 2", "failed")
    ]


async def test_a_tool_done_without_the_key_has_no_verdict() -> None:
    turn = await turn_with([{"call_id": "c1", "name": "bash", "output": "ok"}])

    assert len(turn.tool_calls) == 1
    assert turn.tool_calls[0].verdict is None, "an absent verdict must not read as ok"


async def test_an_unfamiliar_verdict_name_arrives_unchanged_and_the_turn_goes_on() -> None:
    turn = await turn_with([{"call_id": "c0", "name": "bash", "output": "later", "verdict": "deferred"}, FAILED])

    # A name from a newer daemon is passed through, and the call after it still arrives.
    assert [call.verdict for call in turn.tool_calls] == ["deferred", "failed"]


class ToolDoneBeforeVerdict(TypedDict, total=False):
    """The ``tool_done`` frame as clients read it before protocol 1.19."""

    v: int
    ev: str
    session_id: str
    call_id: str
    name: str
    output: str
    error: str


def test_a_decoder_that_predates_the_verdict_still_reads_the_frame() -> None:
    line = json.dumps({"v": 1, "ev": "tool_done", "session_id": "s1", **FAILED}) + "\n"
    assert '"verdict": "failed"' in line, "the frame under test must carry the verdict"

    frames = NdjsonDecoder[ToolDoneBeforeVerdict]().push(line)

    assert len(frames) == 1
    older = frames[0]
    assert (older["ev"], older["call_id"], older["name"], older["output"], older["error"]) == (
        "tool_done",
        "c1",
        "bash",
        "exit status 2",
        "exit status 2",
    )
