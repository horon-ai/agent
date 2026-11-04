from __future__ import annotations

from typing import Any

import pytest

from horon_agent_sdk import ConnectOptions, HoronAgentClient, StructuredOutputError
from mock_harness import start_mock_harness

SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["summary", "count"],
    "properties": {
        "summary": {"type": "string"},
        "count": {"type": "integer", "minimum": 0},
    },
}


def send_turn(send, session_id: str, text: str) -> None:
    send({"v": 1, "ev": "message_accepted", "session_id": session_id})
    send({"v": 1, "ev": "text_delta", "session_id": session_id, "text": text})
    send({"v": 1, "ev": "turn_done", "session_id": session_id})


async def test_run_structured_validates_json_schema_and_returns_parsed_data() -> None:
    prompts: list[str] = []

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "send_message":
            return
        prompts.append(request["content"])
        send_turn(send, "s1", '```json\n{"summary":"done","count":2}\n```')

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        result = await client.run_structured("s1", "Summarize the work", SCHEMA)
        assert result.data == {"summary": "done", "count": 2}
        assert result.text == '```json\n{"summary":"done","count":2}\n```'
        assert len(result.attempts) == 1
        assert result.attempts[0]["errors"] == []
        assert "Return the answer as JSON only" in prompts[0]
        assert '"additionalProperties": false' in prompts[0]
    finally:
        await client.close()
        await server.close()


async def test_run_structured_sends_a_corrective_retry_after_schema_validation_fails() -> None:
    prompts: list[str] = []
    responses = ['{"summary":42,"count":2}', '{"summary":"fixed","count":2}']

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "send_message":
            return
        prompts.append(request["content"])
        send_turn(send, "s1", responses[len(prompts) - 1])

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        result = await client.run_structured("s1", "Return a summary", SCHEMA, max_retries=1)
        assert result.data == {"summary": "fixed", "count": 2}
        assert len(prompts) == 2
        assert "Validation errors:" in prompts[1]
        assert "summary" in prompts[1]
        assert "must be string" in prompts[1]
        assert '"summary":42' in prompts[1]
        assert len(result.attempts) == 2
        assert result.attempts[0]["errors"][0]["keyword"] == "type"
        assert result.attempts[1]["errors"] == []
    finally:
        await client.close()
        await server.close()


async def test_run_structured_raises_with_validation_details_after_bounded_retries() -> None:
    prompts: list[str] = []

    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "send_message":
            return
        prompts.append(request["content"])
        send_turn(send, "s1", "not json")

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        with pytest.raises(StructuredOutputError) as caught:
            await client.run_structured("s1", "Return a summary", SCHEMA, max_retries=1)
        assert caught.value.code == "structured_output_invalid"
        assert len(caught.value.attempts) == 2
        assert caught.value.validation_errors[0]["keyword"] == "parse"
        assert "after 2 attempts" in str(caught.value)
        assert len(prompts) == 2
    finally:
        await client.close()
        await server.close()


async def test_run_structured_validates_the_final_framed_message_rather_than_narration() -> None:
    def on_request(request: dict[str, Any], send) -> None:
        if request["req"] != "send_message":
            return
        send({"v": 1, "ev": "message_accepted", "session_id": "s1"})
        send({"v": 1, "ev": "text_delta", "session_id": "s1", "message_id": "narration", "text": "Let me check."})
        send({"v": 1, "ev": "text_done", "session_id": "s1", "message_id": "narration"})
        send({"v": 1, "ev": "text_delta", "session_id": "s1", "message_id": "answer", "text": '{"ok":true}'})
        send({"v": 1, "ev": "text_done", "session_id": "s1", "message_id": "answer"})
        send({"v": 1, "ev": "turn_done", "session_id": "s1"})

    server = await start_mock_harness(on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        result = await client.run_structured(
            "s1",
            "Check",
            {"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]},
            max_retries=0,
        )
        assert result.data == {"ok": True}
        assert result.text == 'Let me check.{"ok":true}'
        assert result.attempts[0]["text"] == '{"ok":true}'
    finally:
        await client.close()
        await server.close()


async def test_an_invalid_schema_is_rejected_before_a_turn() -> None:
    server = await start_mock_harness()
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        with pytest.raises(Exception) as caught:
            await client.run_structured("s1", "Check", {"type": "not-a-type"})
        assert "structured_schema_invalid" in str(caught.value)
    finally:
        await client.close()
        await server.close()
