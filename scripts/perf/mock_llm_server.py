#!/usr/bin/env python3
"""Deterministic local mock of the three model APIs used by coding agents.

The comparison benchmarks need real tool round-trips without spending money, so
this server answers OpenAI Chat Completions, OpenAI Responses (Codex CLI) and
Anthropic Messages (Claude Code) with canned data.

Scenario, evaluated per request:

1. First request of a conversation that offers a shell-like tool -> the model
   asks for that tool once with {"command": "ls"}.
2. A request whose transcript already carries a tool result -> the model answers
   with the fixed text ``mock-ok`` and stops.
3. A request with no shell tool -> the model answers ``mock-ok`` immediately.

Every request is recorded so the benchmark can prove a tool round-trip actually
happened (``GET /__stats``).

Usage:
    python3 mock_llm_server.py --port 0
    -> prints one JSON line: {"port": 54321, "url": "http://127.0.0.1:54321"}
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

FINAL_TEXT = "mock-ok"
MOCK_MODEL = "mock-model"
# Absolute paths mentioned in a request (workspace hints and tool output).
PATH_RE = re.compile(r"(?:/(?:Users|home|tmp|private|var|opt|workspace|root)[\w./\-]*)")
SHELL_NAME_HINTS = ("bash", "shell", "exec", "terminal", "run_command", "cmd")


def _text_of(content) -> str:
    """Flatten any transcript content shape into plain text."""
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for item in content:
            if isinstance(item, str):
                parts.append(item)
            elif isinstance(item, dict):
                for key in ("text", "content", "output", "arguments", "thinking"):
                    val = item.get(key)
                    if isinstance(val, str):
                        parts.append(val)
                if isinstance(item.get("content"), list):
                    parts.append(_text_of(item["content"]))
        return "\n".join(parts)
    if isinstance(content, dict):
        return _text_of([content])
    return str(content)


def _tool_schema_props(tool: dict) -> dict:
    for key in ("input_schema", "parameters", "schema", "inputSchema"):
        schema = tool.get(key)
        if isinstance(schema, dict) and isinstance(schema.get("properties"), dict):
            return schema["properties"]
    fn = tool.get("function")
    if isinstance(fn, dict):
        return _tool_schema_props(fn)
    return {}


def _tool_name(tool: dict) -> str:
    for key in ("name", "tool_name"):
        val = tool.get(key)
        if isinstance(val, str) and val:
            return val
    fn = tool.get("function")
    if isinstance(fn, dict):
        return _tool_name(fn)
    return ""


def _command_property(props: dict) -> str | None:
    for candidate in ("command", "cmd", "command_line", "script"):
        if candidate in props:
            return candidate
    return None


def find_shell_tool(tools) -> tuple[str, str] | None:
    """Return (tool_name, command_property) for the first shell-like tool."""
    for tool in tools or []:
        if not isinstance(tool, dict):
            continue
        name = _tool_name(tool)
        if not name:
            continue
        lowered = name.lower()
        if not any(hint in lowered for hint in SHELL_NAME_HINTS):
            continue
        prop = _command_property(_tool_schema_props(tool))
        if prop:
            return name, prop
    return None


def has_tool_result(kind: str, body: dict) -> bool:
    """True when the current (last) user turn already produced a tool result.

    Anthropic and OpenAI both append tool results *after* the user message that
    started the turn, so scanning the whole transcript would make every later
    turn look answered. Each new user prompt must earn a fresh tool call.
    """
    if kind == "anthropic":
        messages = [m for m in (body.get("messages") or []) if isinstance(m, dict)]
        for message in reversed(messages):
            if message.get("role") != "user":
                continue
            content = message.get("content")
            if isinstance(content, list):
                return any(
                    isinstance(block, dict) and block.get("type") == "tool_result" for block in content
                )
            return False
        return False
    if kind == "responses":
        saw_prompt = False
        found = False
        for item in body.get("input") or []:
            if not isinstance(item, dict):
                continue
            if item.get("type") in ("function_call_output", "custom_tool_call_output", "local_shell_call_output"):
                found = True
                continue
            if item.get("role") in ("user", "tool"):
                if item.get("role") == "tool":
                    found = True
                    continue
                saw_prompt = True
                found = False
        return saw_prompt and found
    messages = [m for m in (body.get("messages") or []) if isinstance(m, dict)]
    for index in range(len(messages) - 1, -1, -1):
        if messages[index].get("role") != "user":
            continue
        return any(m.get("role") == "tool" for m in messages[index + 1 :])
    return False


def collect_paths(body: dict) -> list[str]:
    found: set[str] = set()
    blob = json.dumps(body, ensure_ascii=False)
    for match in PATH_RE.findall(blob):
        cleaned = match.rstrip(".,;:\"'")
        if len(cleaned) > 2:
            found.add(cleaned)
    return sorted(found)[:20]


class Stats:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.requests: list[dict] = []
        self.started_at = time.time()
        self._call_seq = 0
        self._seq_lock = threading.Lock()

    def next_call_id(self) -> int:
        """Monotonic id so a transcript never repeats a tool-call id across turns."""
        with self._seq_lock:
            self._call_seq += 1
            return self._call_seq

    def record(self, entry: dict) -> None:
        with self.lock:
            self.requests.append(entry)

    def snapshot(self) -> dict:
        with self.lock:
            requests = list(self.requests)
        counts: dict[str, int] = {}
        for entry in requests:
            counts[entry["endpoint"]] = counts.get(entry["endpoint"], 0) + 1
        return {
            "started_at": self.started_at,
            "request_count": len(requests),
            "counts": counts,
            "tool_roundtrips": sum(1 for r in requests if r.get("issued_tool_call")),
            "tool_results_seen": sum(1 for r in requests if r.get("has_tool_result")),
            "requests": requests,
        }


STATS = Stats()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "mock-llm/1"

    # ---- plumbing -------------------------------------------------------
    def log_message(self, fmt, *args):  # noqa: D401 - silence access log
        if self.server.verbose:  # type: ignore[attr-defined]
            sys.stderr.write("mock: " + (fmt % args) + "\n")

    def _read_body(self) -> dict:
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b""
        if not raw:
            return {}
        try:
            return json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return {}

    def _send_json(self, payload: dict, status: int = 200) -> None:
        data = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _start_sse(self) -> None:
        # An SSE body has no content length, so the only terminator is the end
        # of the connection. Keeping the socket alive would leave clients
        # blocked after [DONE] waiting for more data, so the response closes.
        self.close_connection = True
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()

    def _sse(self, event: str | None, payload) -> None:
        chunk = ""
        if event:
            chunk += f"event: {event}\n"
        body = payload if isinstance(payload, str) else json.dumps(payload)
        chunk += f"data: {body}\n\n"
        self.wfile.write(chunk.encode("utf-8"))
        self.wfile.flush()

    # ---- routes ---------------------------------------------------------
    def do_GET(self) -> None:  # noqa: N802
        path = self.path.split("?")[0]
        if path == "/__stats":
            self._send_json(STATS.snapshot())
            return
        if path in ("/health", "/healthz"):
            self._send_json({"ok": True})
            return
        if path in ("/v1/models", "/models"):
            self._send_json(
                {
                    "object": "list",
                    "data": [
                        {
                            "id": MOCK_MODEL,
                            "object": "model",
                            "created": int(STATS.started_at),
                            "owned_by": "mock",
                        }
                    ],
                }
            )
            return
        if path.startswith("/v1/models/") or path.startswith("/models/"):
            self._send_json({"id": path.rsplit("/", 1)[-1], "object": "model", "owned_by": "mock"})
            return
        self._send_json({"error": {"message": f"unknown path {path}"}}, status=404)

    def do_POST(self) -> None:  # noqa: N802
        path = self.path.split("?")[0]
        if path == "/__reset":
            global STATS
            STATS = Stats()
            self._send_json({"ok": True})
            return
        body = self._read_body()
        if path.endswith("/chat/completions"):
            self._handle_chat(body)
        elif path.endswith("/responses"):
            self._handle_responses(body)
        elif path.endswith("/messages"):
            self._handle_messages(body, stream=bool(body.get("stream")))
        elif path.endswith("/messages/count_tokens"):
            self._send_json({"input_tokens": 16})
        elif path.endswith("/models"):
            self._send_json({"data": [], "object": "list"})
        else:
            self._send_json({"error": {"message": f"unknown path {path}"}}, status=404)

    # ---- shared scenario ------------------------------------------------
    def _scenario(self, kind: str, body: dict, endpoint: str) -> dict:
        shell = find_shell_tool(body.get("tools"))
        saw_result = has_tool_result(kind, body)
        calls_tool = shell is not None and not saw_result
        counter = STATS.next_call_id()
        entry = {
            "endpoint": endpoint,
            "kind": kind,
            "stream": bool(body.get("stream")),
            "model": body.get("model"),
            "has_tool_result": saw_result,
            "issued_tool_call": calls_tool,
            "tool_name": shell[0] if shell else None,
            "tool_choice_prop": shell[1] if shell else None,
            "tool_count": len(body.get("tools") or []),
            "paths": collect_paths(body),
            "ts": time.time(),
        }
        STATS.record(entry)
        choice = "tool" if calls_tool else "text"
        return {
            "choice": choice,
            "call_id": f"call_mock_{counter}",
            "tool": shell[0] if shell else None,
            "prop": shell[1] if shell else "command",
            "model": body.get("model") or MOCK_MODEL,
            "stream": bool(body.get("stream")),
        }

    # ---- OpenAI chat completions ---------------------------------------
    def _handle_chat(self, body: dict) -> None:
        plan = self._scenario("openai-chat", body, "/v1/chat/completions")
        rid = f"chatcmpl-mock-{plan['call_id']}"
        created = int(time.time())
        if not plan["stream"]:
            if plan["choice"] == "tool":
                message = {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [
                        {
                            "id": plan["call_id"],
                            "type": "function",
                            "function": {
                                "name": plan["tool"],
                                "arguments": json.dumps({plan["prop"]: "ls"}),
                            },
                        }
                    ],
                }
                finish = "tool_calls"
            else:
                message = {"role": "assistant", "content": FINAL_TEXT}
                finish = "stop"
            self._send_json(
                {
                    "id": rid,
                    "object": "chat.completion",
                    "created": created,
                    "model": plan["model"],
                    "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16},
                }
            )
            return
        self._start_sse()
        base = {
            "id": rid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": plan["model"],
        }
        self._sse(None, {**base, "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]})
        if plan["choice"] == "tool":
            args = json.dumps({plan["prop"]: "ls"})
            self._sse(
                None,
                {
                    **base,
                    "choices": [
                        {
                            "index": 0,
                            "delta": {
                                "tool_calls": [
                                    {
                                        "index": 0,
                                        "id": plan["call_id"],
                                        "type": "function",
                                        "function": {"name": plan["tool"], "arguments": ""},
                                    }
                                ]
                            },
                            "finish_reason": None,
                        }
                    ],
                },
            )
            self._sse(
                None,
                {
                    **base,
                    "choices": [
                        {
                            "index": 0,
                            "delta": {"tool_calls": [{"index": 0, "function": {"arguments": args}}]},
                            "finish_reason": None,
                        }
                    ],
                },
            )
            self._sse(
                None,
                {**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            )
        else:
            self._sse(
                None,
                {**base, "choices": [{"index": 0, "delta": {"content": FINAL_TEXT}, "finish_reason": None}]},
            )
            self._sse(None, {**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        self._sse(
            None,
            {
                **base,
                "choices": [],
                "usage": {"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16},
            },
        )
        self._sse(None, "[DONE]")

    # ---- OpenAI Responses (Codex CLI) ----------------------------------
    def _handle_responses(self, body: dict) -> None:
        plan = self._scenario("responses", body, "/v1/responses")
        rid = f"resp_mock_{plan['call_id']}"
        item_id = f"fc_mock_{plan['call_id']}"
        msg_id = f"msg_mock_{plan['call_id']}"
        created = int(time.time())
        response_base = {
            "id": rid,
            "object": "response",
            "created_at": created,
            "status": "in_progress",
            "model": plan["model"],
            "output": [],
        }
        if not plan["stream"]:
            if plan["choice"] == "tool":
                item = {
                    "id": item_id,
                    "type": "function_call",
                    "status": "completed",
                    "call_id": plan["call_id"],
                    "name": plan["tool"],
                    "arguments": json.dumps({plan["prop"]: "ls"}),
                }
            else:
                item = {
                    "id": msg_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": FINAL_TEXT, "annotations": []}],
                }
            self._send_json(
                {
                    **response_base,
                    "status": "completed",
                    "output": [item],
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 4,
                        "total_tokens": 16,
                        "input_tokens_details": {"cached_tokens": 0},
                        "output_tokens_details": {"reasoning_tokens": 0},
                    },
                }
            )
            return
        self._start_sse()
        self._sse(
            "response.created",
            {"type": "response.created", "response": {**response_base, "output": []}},
        )
        self._sse(
            "response.in_progress",
            {"type": "response.in_progress", "response": {**response_base, "output": []}},
        )
        if plan["choice"] == "tool":
            args = json.dumps({plan["prop"]: "ls"})
            added = {
                "id": item_id,
                "type": "function_call",
                "status": "in_progress",
                "call_id": plan["call_id"],
                "name": plan["tool"],
                "arguments": "",
            }
            self._sse(
                "response.output_item.added",
                {"type": "response.output_item.added", "output_index": 0, "item": added},
            )
            self._sse(
                "response.function_call_arguments.delta",
                {
                    "type": "response.function_call_arguments.delta",
                    "item_id": item_id,
                    "output_index": 0,
                    "delta": args,
                },
            )
            self._sse(
                "response.function_call_arguments.done",
                {
                    "type": "response.function_call_arguments.done",
                    "item_id": item_id,
                    "output_index": 0,
                    "arguments": args,
                },
            )
            done_item = {**added, "status": "completed", "arguments": args}
            self._sse(
                "response.output_item.done",
                {"type": "response.output_item.done", "output_index": 0, "item": done_item},
            )
            output = [done_item]
        else:
            added = {
                "id": msg_id,
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": [],
            }
            self._sse(
                "response.output_item.added",
                {"type": "response.output_item.added", "output_index": 0, "item": added},
            )
            self._sse(
                "response.content_part.added",
                {
                    "type": "response.content_part.added",
                    "item_id": msg_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": "", "annotations": []},
                },
            )
            self._sse(
                "response.output_text.delta",
                {
                    "type": "response.output_text.delta",
                    "item_id": msg_id,
                    "output_index": 0,
                    "content_index": 0,
                    "delta": FINAL_TEXT,
                },
            )
            self._sse(
                "response.output_text.done",
                {
                    "type": "response.output_text.done",
                    "item_id": msg_id,
                    "output_index": 0,
                    "content_index": 0,
                    "text": FINAL_TEXT,
                },
            )
            self._sse(
                "response.content_part.done",
                {
                    "type": "response.content_part.done",
                    "item_id": msg_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": FINAL_TEXT, "annotations": []},
                },
            )
            done_item = {
                "id": msg_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": FINAL_TEXT, "annotations": []}],
            }
            self._sse(
                "response.output_item.done",
                {"type": "response.output_item.done", "output_index": 0, "item": done_item},
            )
            output = [done_item]
        self._sse(
            "response.completed",
            {
                "type": "response.completed",
                "response": {
                    **response_base,
                    "status": "completed",
                    "output": output,
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 4,
                        "total_tokens": 16,
                        "input_tokens_details": {"cached_tokens": 0},
                        "output_tokens_details": {"reasoning_tokens": 0},
                    },
                },
            },
        )

    # ---- Anthropic messages --------------------------------------------
    def _handle_messages(self, body: dict, stream: bool) -> None:
        plan = self._scenario("anthropic", body, "/v1/messages")
        mid = f"msg_mock_{plan['call_id']}"
        if not stream:
            if plan["choice"] == "tool":
                content = [
                    {
                        "type": "tool_use",
                        "id": plan["call_id"].replace("call_", "toolu_"),
                        "name": plan["tool"],
                        "input": {plan["prop"]: "ls"},
                    }
                ]
                stop = "tool_use"
            else:
                content = [{"type": "text", "text": FINAL_TEXT}]
                stop = "end_turn"
            self._send_json(
                {
                    "id": mid,
                    "type": "message",
                    "role": "assistant",
                    "model": plan["model"],
                    "content": content,
                    "stop_reason": stop,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 12, "output_tokens": 4},
                }
            )
            return
        self._start_sse()
        self._sse(
            "message_start",
            {
                "type": "message_start",
                "message": {
                    "id": mid,
                    "type": "message",
                    "role": "assistant",
                    "model": plan["model"],
                    "content": [],
                    "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 12, "output_tokens": 0},
                },
            },
        )
        if plan["choice"] == "tool":
            self._sse(
                "content_block_start",
                {
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": plan["call_id"].replace("call_", "toolu_"),
                        "name": plan["tool"],
                        "input": {},
                    },
                },
            )
            self._sse(
                "content_block_delta",
                {
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": json.dumps({plan["prop"]: "ls"}),
                    },
                },
            )
            self._sse("content_block_stop", {"type": "content_block_stop", "index": 0})
            stop = "tool_use"
        else:
            self._sse(
                "content_block_start",
                {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            )
            self._sse(
                "content_block_delta",
                {
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": FINAL_TEXT},
                },
            )
            self._sse("content_block_stop", {"type": "content_block_stop", "index": 0})
            stop = "end_turn"
        self._sse(
            "message_delta",
            {
                "type": "message_delta",
                "delta": {"stop_reason": stop, "stop_sequence": None},
                "usage": {"output_tokens": 4},
            },
        )
        self._sse("message_stop", {"type": "message_stop"})


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=0, help="0 picks a free port")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args(argv)

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.daemon_threads = True
    server.verbose = args.verbose  # type: ignore[attr-defined]
    port = server.server_address[1]
    print(json.dumps({"port": port, "url": f"http://{args.host}:{port}"}), flush=True)
    try:
        server.serve_forever(poll_interval=0.05)
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
