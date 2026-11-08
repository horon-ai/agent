#!/usr/bin/env python3
"""A whole stdio MCP server in one dependency-free file, for the compare suite.

Each agent under measurement starts one copy of this process from its own
isolated config and is expected to call its single tool ``bench_echo`` once. The
tool returns its ``text`` argument unchanged, so the model-side harness can
prove the round trip by looking for the nonce it passed in.

Framing is one JSON-RPC object per line, per the stdio transport. Anything the
MCP specification does not define for a server is answered with ``-32601``
(method not found), and end of stdin ends the process with status 0 so an agent
that closes the pipe does not leave a zombie behind.

Usage:
    python3 bench_mcp_server.py
"""

from __future__ import annotations

import json
import sys

PROTOCOL_VERSION = "2024-11-05"
SERVER_NAME = "bench-mcp"
TOOL_NAME = "bench_echo"
TOOL_DESCRIPTION = "Echo the text argument back. Benchmark fixture."
TOOL_SCHEMA = {
    "type": "object",
    "properties": {"text": {"type": "string", "description": "Text to echo back."}},
    "required": ["text"],
}


def _response(request_id, result: dict) -> dict:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def _error(request_id, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def handle(message: dict) -> dict | None:
    """One request in, one response out; ``None`` for a notification."""
    method = message.get("method")
    request_id = message.get("id")
    # Notifications carry no id and are never answered, including the
    # initialized notice every client sends right after initialize.
    if request_id is None:
        return None
    if method == "initialize":
        return _response(
            request_id,
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": "1.0.0"},
            },
        )
    if method == "ping":
        return _response(request_id, {})
    if method == "tools/list":
        return _response(
            request_id,
            {
                "tools": [
                    {
                        "name": TOOL_NAME,
                        "description": TOOL_DESCRIPTION,
                        "inputSchema": TOOL_SCHEMA,
                    }
                ]
            },
        )
    if method == "tools/call":
        params = message.get("params") or {}
        if params.get("name") != TOOL_NAME:
            return _error(request_id, -32602, f"unknown tool: {params.get('name')}")
        arguments = params.get("arguments") or {}
        text = arguments.get("text")
        if not isinstance(text, str):
            return _error(request_id, -32602, "text must be a string")
        return _response(request_id, {"content": [{"type": "text", "text": text}], "isError": False})
    return _error(request_id, -32601, f"method not found: {method}")


def main() -> int:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            sys.stdout.write(json.dumps(_error(None, -32700, "parse error")) + "\n")
            sys.stdout.flush()
            continue
        if not isinstance(message, dict):
            sys.stdout.write(json.dumps(_error(None, -32600, "invalid request")) + "\n")
            sys.stdout.flush()
            continue
        response = handle(message)
        if response is not None:
            sys.stdout.write(json.dumps(response) + "\n")
            sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
