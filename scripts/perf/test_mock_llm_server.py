#!/usr/bin/env python3
"""Framing tests for the deterministic mock model server.

These pin the wire contracts that real clients depend on and that are easy to
break silently:

- an SSE response ends the connection after its terminator, otherwise a client
  blocks forever waiting for more data (this actually hung horon-agent);
- OpenAI chat completions stream a tool-call delta whose concatenated
  fragments form valid JSON, and a `[DONE]` terminator;
- OpenAI Responses emits the event set Codex CLI requires, ending with
  `response.completed` carrying the function call;
- Anthropic Messages emits `message_start` ... `message_stop` with a
  `tool_use` block;
- the scenario only issues a tool call when a shell tool is offered and the
  current turn has not already been answered, and `/__stats` proves it.

Only localhost traffic is used; no external network and no binaries.
"""

from __future__ import annotations

import http.client
import json
import os
import socket
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def start_server() -> tuple[subprocess.Popen, int]:
    proc = subprocess.Popen(
        [sys.executable, str(ROOT / "mock_llm_server.py"), "--port", "0"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    assert proc.stdout is not None
    line = proc.stdout.readline()
    if not line:
        proc.kill()
        raise RuntimeError("mock server failed to start")
    return proc, json.loads(line)["port"]


def stop_server(proc: subprocess.Popen) -> None:
    try:
        proc.terminate()
        proc.wait(timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        proc.kill()


def post(port: int, path: str, body: dict, timeout: float = 15.0) -> tuple[int, dict[str, str], bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        payload = json.dumps(body).encode()
        conn.request("POST", path, body=payload, headers={"content-type": "application/json"})
        response = conn.getresponse()
        return response.status, dict(response.getheaders()), response.read()
    finally:
        conn.close()


def get_json(port: int, path: str) -> dict:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=15.0)
    try:
        conn.request("GET", path)
        response = conn.getresponse()
        return json.loads(response.read() or b"{}")
    finally:
        conn.close()


def sse_events(raw: bytes) -> list[tuple[str | None, str]]:
    events: list[tuple[str | None, str]] = []
    event = None
    for line in raw.decode("utf-8").splitlines():
        if line.startswith("event: "):
            event = line[len("event: ") :]
        elif line.startswith("data: "):
            events.append((event, line[len("data: ") :]))
            event = None
    return events


SHELL_TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Run a shell command",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}}},
        },
    }
]


def chat_request(stream: bool, messages: list[dict], tools=SHELL_TOOLS) -> dict:
    return {"model": "mock-model", "stream": stream, "messages": messages, "tools": tools}


class ServerCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.proc, cls.port = start_server()

    @classmethod
    def tearDownClass(cls) -> None:
        stop_server(cls.proc)

    def setUp(self) -> None:
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        try:
            conn.request("POST", "/__reset", body=b"{}")
            conn.getresponse().read()
        finally:
            conn.close()

class MockServerTests(ServerCase):

    def test_chat_stream_tool_call_framing(self) -> None:
        status, headers, raw = post(
            self.port, "/v1/chat/completions", chat_request(True, [{"role": "user", "content": "hi"}])
        )
        self.assertEqual(status, 200)
        self.assertIn("text/event-stream", headers.get("content-type", ""))
        events = sse_events(raw)
        self.assertTrue(events, "no SSE events parsed")
        self.assertEqual(events[-1], (None, "[DONE]"), "SSE must terminate with [DONE]")

        chunks = []
        for _event, data in events[:-1]:
            chunks.append(json.loads(data))
        self.assertTrue(all(chunk["object"] == "chat.completion.chunk" for chunk in chunks))
        tool_deltas = [
            delta_tool
            for chunk in chunks
            for choice in chunk["choices"]
            for delta_tool in (choice.get("delta", {}).get("tool_calls") or [])
        ]
        self.assertTrue(tool_deltas, "expected a tool_calls delta")
        name = tool_deltas[0]["function"]["name"]
        arguments = "".join(tool["function"].get("arguments") or "" for tool in tool_deltas)
        self.assertEqual(name, "bash")
        self.assertEqual(json.loads(arguments), {"command": "ls"})
        finish = [c["choices"][0]["finish_reason"] for c in chunks if c["choices"]]
        self.assertEqual(finish[-1], "tool_calls")

    def test_stream_closes_connection(self) -> None:
        """A client must see EOF, not a socket that stays open forever."""
        sock = socket.create_connection(("127.0.0.1", self.port), timeout=10)
        try:
            body = json.dumps(chat_request(True, [{"role": "user", "content": "hi"}])).encode()
            request = (
                b"POST /v1/chat/completions HTTP/1.1\r\n"
                b"Host: 127.0.0.1\r\n"
                b"Content-Type: application/json\r\n"
                b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body
            )
            sock.sendall(request)
            chunks = []
            while True:
                data = sock.recv(65536)
                if not data:
                    break
                chunks.append(data)
            payload = b"".join(chunks)
            self.assertIn(b"[DONE]", payload)
            self.assertTrue(payload.endswith(b"\n\n"), "stream should end on a blank line")
        finally:
            sock.close()

    def test_tool_result_turn_ends_with_text(self) -> None:
        messages = [
            {"role": "user", "content": "hi"},
            {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "id": "call_mock_1",
                        "type": "function",
                        "function": {"name": "bash", "arguments": '{"command": "ls"}'},
                    }
                ],
            },
            {"role": "tool", "tool_call_id": "call_mock_1", "content": "sample.txt"},
        ]
        status, _headers, raw = post(self.port, "/v1/chat/completions", chat_request(True, messages))
        self.assertEqual(status, 200)
        chunks = [json.loads(data) for _event, data in sse_events(raw) if data != "[DONE]"]
        text = "".join(
            choice.get("delta", {}).get("content") or ""
            for chunk in chunks
            for choice in chunk["choices"]
        )
        self.assertEqual(text, "mock-ok")
        finish = [c["choices"][0]["finish_reason"] for c in chunks if c["choices"]]
        self.assertEqual(finish[-1], "stop")

    def test_no_shell_tool_answers_immediately(self) -> None:
        status, _headers, raw = post(
            self.port,
            "/v1/chat/completions",
            chat_request(True, [{"role": "user", "content": "hi"}], tools=[]),
        )
        self.assertEqual(status, 200)
        chunks = [json.loads(data) for _event, data in sse_events(raw) if data != "[DONE]"]
        self.assertFalse(
            any(
                choice.get("delta", {}).get("tool_calls")
                for chunk in chunks
                for choice in chunk["choices"]
            )
        )

    def test_responses_stream_event_set(self) -> None:
        status, _headers, raw = post(
            self.port,
            "/v1/responses",
            {"model": "mock-model", "stream": True, "input": [{"role": "user", "content": "hi"}],
             "tools": SHELL_TOOLS},
        )
        self.assertEqual(status, 200)
        events = sse_events(raw)
        names = [name for name, _data in events]
        self.assertEqual(names[0], "response.created")
        self.assertIn("response.output_item.added", names)
        self.assertIn("response.output_item.done", names)
        self.assertEqual(names[-1], "response.completed")

        created = json.loads(events[0][1])["response"]
        self.assertEqual(created["status"], "in_progress")
        completed = json.loads(events[-1][1])["response"]
        self.assertEqual(completed["status"], "completed")
        call = completed["output"][0]
        self.assertEqual(call["type"], "function_call")
        self.assertEqual(call["name"], "bash")
        self.assertEqual(json.loads(call["arguments"]), {"command": "ls"})

    def test_anthropic_stream_blocks(self) -> None:
        body = {
            "model": "mock-model",
            "stream": True,
            "max_tokens": 64,
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
            "tools": [
                {
                    "name": "Bash",
                    "description": "run a command",
                    "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}},
                }
            ],
        }
        status, _headers, raw = post(self.port, "/v1/messages", body)
        self.assertEqual(status, 200)
        events = sse_events(raw)
        names = [name for name, _data in events]
        self.assertEqual(names[0], "message_start")
        self.assertEqual(names[-1], "message_stop")
        self.assertIn("content_block_start", names)
        self.assertIn("content_block_delta", names)

        start = next(json.loads(d) for n, d in events if n == "content_block_start")
        self.assertEqual(start["content_block"]["type"], "tool_use")
        self.assertEqual(start["content_block"]["name"], "Bash")
        delta = next(json.loads(d) for n, d in events if n == "content_block_delta")
        self.assertEqual(delta["delta"]["type"], "input_json_delta")
        self.assertEqual(json.loads(delta["delta"]["partial_json"]), {"command": "ls"})
        stop = next(json.loads(d) for n, d in events if n == "message_delta")
        self.assertEqual(stop["delta"]["stop_reason"], "tool_use")

    def test_stats_prove_tool_roundtrip(self) -> None:
        post(self.port, "/v1/chat/completions", chat_request(True, [{"role": "user", "content": "hi"}]))
        answered = [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": None,
             "tool_calls": [{"id": "call_mock_1", "type": "function",
                              "function": {"name": "bash", "arguments": '{"command": "ls"}'}}]},
            {"role": "tool", "tool_call_id": "call_mock_1", "content": "sample.txt"},
        ]
        post(self.port, "/v1/chat/completions", chat_request(True, answered))
        stats = get_json(self.port, "/__stats")
        self.assertEqual(stats["request_count"], 2)
        self.assertEqual(stats["tool_roundtrips"], 1)
        self.assertEqual(stats["tool_results_seen"], 1)
        self.assertTrue(stats["requests"][0]["issued_tool_call"])
        self.assertFalse(stats["requests"][1]["issued_tool_call"])

    def test_models_endpoint(self) -> None:
        models = get_json(self.port, "/v1/models")
        self.assertEqual(models["object"], "list")
        self.assertTrue(models["data"])

    def test_new_user_turn_triggers_fresh_tool_call(self) -> None:
        """Multi-turn sessions must not reuse the previous turn's answer."""
        messages = [
            {"role": "user", "content": "turn one"},
            {"role": "assistant", "content": None,
             "tool_calls": [{"id": "call_mock_1", "type": "function",
                              "function": {"name": "bash", "arguments": '{"command": "ls"}'}}]},
            {"role": "tool", "tool_call_id": "call_mock_1", "content": "sample.txt"},
            {"role": "assistant", "content": "mock-ok"},
            {"role": "user", "content": "turn two"},
        ]
        status, _headers, raw = post(self.port, "/v1/chat/completions", chat_request(True, messages))
        self.assertEqual(status, 200)
        chunks = [json.loads(data) for _event, data in sse_events(raw) if data != "[DONE]"]
        has_tool = any(
            choice.get("delta", {}).get("tool_calls") for chunk in chunks for choice in chunk["choices"]
        )
        self.assertTrue(has_tool, "a new user turn should earn a new tool call")


SKILL_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "Skill",
            "description": "Invoke one of the available skills by name.",
            "parameters": {"type": "object", "properties": {"skill": {"type": "string"}},
                           "required": ["skill"]},
        },
    }
]

MCP_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "mcp__bench__bench_echo",
            "description": "Echo the text argument back. Benchmark fixture.",
            "parameters": {"type": "object", "properties": {"text": {"type": "string"}},
                           "required": ["text"]},
        },
    }
]


def read_tool(description: str) -> list[dict]:
    return [
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": description,
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}},
                               "required": ["path"]},
            },
        }
    ]


def tool_call_of(raw: bytes) -> tuple[str, dict]:
    chunks = [json.loads(data) for _event, data in sse_events(raw) if data != "[DONE]"]
    deltas = [
        delta_tool
        for chunk in chunks
        for choice in chunk["choices"]
        for delta_tool in (choice.get("delta", {}).get("tool_calls") or [])
    ]
    if not deltas:
        return "", {}
    name = deltas[0]["function"]["name"]
    arguments = "".join(tool["function"].get("arguments") or "" for tool in deltas)
    return name, json.loads(arguments)


def final_text_of(raw: bytes) -> str:
    chunks = [json.loads(data) for _event, data in sse_events(raw) if data != "[DONE]"]
    return "".join(
        choice.get("delta", {}).get("content") or ""
        for chunk in chunks
        for choice in chunk["choices"]
    )


class ScenarioPlanTests(ServerCase):
    """The plan markers the compare suite relies on, and their fallbacks.

    Every case runs against the same server class as the framing tests: the plan
    decision is part of the wire contract now, not a test-only helper.
    """

    SKILL_PROMPT = "[bench:skill] Use the bench-skill skill from /tmp/bench/SKILL.md then answer."
    MCP_PROMPT = "[bench:mcp] Call the bench_echo tool once, then reply with mock-ok."

    def chat(self, prompt: str, tools, messages=None) -> bytes:
        conversation = messages or [{"role": "user", "content": prompt}]
        status, _headers, raw = post(
            self.port, "/v1/chat/completions", chat_request(True, conversation, tools=tools)
        )
        self.assertEqual(status, 200)
        return raw

    def requests(self) -> list[dict]:
        return get_json(self.port, "/__stats")["requests"]

    def test_skill_plan_calls_the_skill_tool(self) -> None:
        name, arguments = tool_call_of(self.chat(self.SKILL_PROMPT, SKILL_TOOL))
        self.assertEqual(name, "Skill")
        self.assertEqual(arguments, {"skill": "bench-skill"})
        entry = self.requests()[0]
        self.assertEqual(entry["plan"], "skill")
        self.assertEqual(entry["via"], "skill-tool")
        self.assertTrue(entry["issued_tool_call"])

    def test_skill_plan_uses_a_skill_uri_read_when_documented(self) -> None:
        tools = read_tool("Read a file; `skill://<name>` opens a skill.")
        name, arguments = tool_call_of(self.chat(self.SKILL_PROMPT, tools))
        self.assertEqual(name, "read")
        self.assertEqual(arguments, {"path": "skill://bench-skill"})
        self.assertEqual(self.requests()[0]["via"], "skill-uri")

    def test_skill_plan_falls_back_to_the_prompt_path(self) -> None:
        name, arguments = tool_call_of(self.chat(self.SKILL_PROMPT, read_tool("Read a file.")))
        self.assertEqual(name, "read")
        self.assertEqual(arguments, {"path": "/tmp/bench/SKILL.md"})
        self.assertEqual(self.requests()[0]["via"], "file-read")

    def test_skill_plan_reads_through_a_shell_tool_when_no_read_tool_exists(self) -> None:
        name, arguments = tool_call_of(self.chat(self.SKILL_PROMPT, SHELL_TOOLS))
        self.assertEqual(name, "bash")
        self.assertEqual(arguments, {"command": "cat '/tmp/bench/SKILL.md'"})
        self.assertEqual(self.requests()[0]["via"], "file-read")

    def test_skill_sentinel_proves_the_round_trip(self) -> None:
        messages = [
            {"role": "user", "content": self.SKILL_PROMPT},
            {"role": "assistant", "content": None,
             "tool_calls": [{"id": "call_mock_1", "type": "function",
                              "function": {"name": "read", "arguments": '{"path": "/tmp/bench/SKILL.md"}'}}]},
            {"role": "tool", "tool_call_id": "call_mock_1",
             "content": "# Bench skill\nBENCH-SKILL-SENTINEL\n"},
        ]
        raw = self.chat(self.SKILL_PROMPT, read_tool("Read a file."), messages)
        self.assertEqual(final_text_of(raw), "mock-ok")
        entry = self.requests()[-1]
        self.assertFalse(entry["issued_tool_call"])
        self.assertTrue(entry["sentinel_seen"])

    def test_a_result_without_the_sentinel_is_not_a_round_trip(self) -> None:
        messages = [
            {"role": "user", "content": self.SKILL_PROMPT},
            {"role": "assistant", "content": None,
             "tool_calls": [{"id": "call_mock_1", "type": "function",
                              "function": {"name": "read", "arguments": '{"path": "/tmp/bench/SKILL.md"}'}}]},
            {"role": "tool", "tool_call_id": "call_mock_1", "content": "mock-ok"},
        ]
        self.chat(self.SKILL_PROMPT, read_tool("Read a file."), messages)
        self.assertFalse(self.requests()[-1]["sentinel_seen"])

    def test_mcp_plan_calls_bench_echo_with_a_nonce(self) -> None:
        name, arguments = tool_call_of(self.chat(self.MCP_PROMPT, MCP_TOOL))
        self.assertEqual(name, "mcp__bench__bench_echo")
        self.assertEqual(list(arguments), ["text"])
        self.assertTrue(arguments["text"].startswith("bench-nonce-"))
        entry = self.requests()[0]
        self.assertEqual(entry["plan"], "mcp")
        self.assertEqual(entry["via"], "mcp-tool")
        self.assertTrue(entry["bench_echo_exposed"])

    def test_mcp_nonce_returned_in_the_tool_result_is_a_round_trip(self) -> None:
        nonce = tool_call_of(self.chat(self.MCP_PROMPT, MCP_TOOL))[1]["text"]
        messages = [
            {"role": "user", "content": self.MCP_PROMPT},
            {"role": "assistant", "content": None,
             "tool_calls": [{"id": "call_mock_1", "type": "function",
                              "function": {"name": "mcp__bench__bench_echo",
                                           "arguments": json.dumps({"text": nonce})}}]},
            {"role": "tool", "tool_call_id": "call_mock_1", "content": nonce},
        ]
        raw = self.chat(self.MCP_PROMPT, MCP_TOOL, messages)
        self.assertEqual(final_text_of(raw), "mock-ok")
        self.assertTrue(self.requests()[-1]["sentinel_seen"])

    def test_mcp_plan_without_the_tool_answers_without_a_call(self) -> None:
        raw = self.chat(self.MCP_PROMPT, [])
        self.assertEqual(final_text_of(raw), "mock-ok")
        entry = self.requests()[0]
        self.assertFalse(entry["issued_tool_call"])
        self.assertFalse(entry["bench_echo_exposed"])

    def test_mcp_route_is_read_through_the_write_tool(self) -> None:
        system = "- \"bench_echo\" -> `xd://mcp__bench_echo`"
        messages = [
            {"role": "system", "content": system},
            {"role": "user", "content": self.MCP_PROMPT},
        ]
        write_tools = [
            {
                "type": "function",
                "function": {
                    "name": "write",
                    "description": "Write content to a path or invoke a mounted tool.",
                    "parameters": {"type": "object",
                                   "properties": {"path": {"type": "string"},
                                                  "content": {"type": "string"}},
                                   "required": ["path", "content"]},
                },
            }
        ]
        name, arguments = tool_call_of(self.chat(self.MCP_PROMPT, write_tools, messages))
        self.assertEqual(name, "write")
        self.assertEqual(arguments["path"], "xd://mcp__bench_echo")
        self.assertTrue(json.loads(arguments["content"])["text"].startswith("bench-nonce-"))
        self.assertEqual(self.requests()[0]["via"], "mcp-route")

    def test_namespaced_tool_is_called_with_its_namespace(self) -> None:
        tools = [
            {
                "type": "namespace",
                "name": "mcp__bench",
                "description": "Tools in the mcp__bench namespace.",
                "tools": [
                    {
                        "type": "function",
                        "name": "bench_echo",
                        "description": "Echo the text argument back.",
                        "parameters": {"type": "object",
                                       "properties": {"text": {"type": "string"}},
                                       "required": ["text"]},
                    }
                ],
            }
        ]
        status, _headers, raw = post(
            self.port,
            "/v1/responses",
            {"model": "mock-model", "stream": True, "tools": tools,
             "input": [{"role": "user", "content": self.MCP_PROMPT}]},
        )
        self.assertEqual(status, 200)
        events = sse_events(raw)
        completed = json.loads(events[-1][1])["response"]
        call = completed["output"][0]
        self.assertEqual(call["type"], "function_call")
        self.assertEqual(call["name"], "bench_echo")
        self.assertEqual(call["namespace"], "mcp__bench")
        self.assertEqual(self.requests()[0]["via"], "mcp-tool")

    def test_routing_hint_request_gets_a_hint_not_mock_ok(self) -> None:
        prompt = "Compress into one routing hint of at most 12 words: WHEN to use the skill."
        raw = self.chat(prompt, [])
        self.assertNotEqual(final_text_of(raw), "mock-ok")
        self.assertIn("bench-skill", final_text_of(raw))
        self.assertEqual(self.requests()[0]["plan"], "hint")

    def test_a_request_without_a_marker_keeps_the_ls_plan(self) -> None:
        name, arguments = tool_call_of(
            self.chat("List the files in the current directory, then reply with mock-ok.", SHELL_TOOLS)
        )
        self.assertEqual(name, "bash")
        self.assertEqual(arguments, {"command": "ls"})
        self.assertEqual(self.requests()[0]["plan"], "ls")


if __name__ == "__main__":
    unittest.main()
