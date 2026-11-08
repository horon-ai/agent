#!/usr/bin/env python3
"""Deterministic local mock of the three model APIs used by coding agents.

The comparison benchmarks need real tool round-trips without spending money, so
this server answers OpenAI Chat Completions, OpenAI Responses (Codex CLI) and
Anthropic Messages (Claude Code) with canned data.

Scenario, evaluated per request:

1. The prompt carries ``[bench:skill]`` -> ask for the fixture skill through the
   agent's own skill tool, a ``skill://`` read, or a plain read of the SKILL.md
   path the prompt names, whichever the tool list supports. The request record
   keeps which route was used (``via``) and whether the fixture sentinel came
   back in a later tool result.
2. The prompt carries ``[bench:mcp]`` -> call the fixture MCP tool
   ``bench_echo`` with a nonce, or read the ``xd://...bench_echo`` route an agent
   prints instead of exposing a function. A later request counts as a round trip
   only when it carries that nonce in a tool result.
3. omp's skill routing-hint request -> one valid hint, so omp does not retry it
   three times in the skill scenario.
4. First request of a conversation that offers a shell-like tool -> the model
   asks for that tool once with ``{"command": "ls"}``.
5. A request whose transcript already carries a tool result -> the model answers
   with the fixed text ``mock-ok`` and stops.
6. A request with no shell tool -> the model answers ``mock-ok`` immediately.

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

# Scenario markers the benchmark harness puts in its prompt. A request without a
# marker keeps the original `ls` plan, so existing one-shot and session numbers
# stay what they were.
SKILL_MARKER = "[bench:skill]"
MCP_MARKER = "[bench:mcp]"
SKILL_NAME = "bench-skill"
# Text inside the fixture SKILL.md; seeing it in a later request proves the skill
# body actually reached the model.
SKILL_SENTINEL = "BENCH-SKILL-SENTINEL"
MCP_TOOL_SUFFIX = "bench_echo"
NONCE_PREFIX = "bench-nonce-"
# omp asks the model to compress every installed skill's description into a
# routing hint before it will list the skill; a short plain answer is rejected
# and retried, which would put three model calls in the skill scenario.
ROUTING_HINT_PROMPT = "Compress into one routing hint"
ROUTING_HINT_ANSWER = "Use when the bench-skill fixture skill is requested."
# The section horon's prompt uses to tell the model how to open a skill; a read
# tool that does not spell `skill://` itself is still reached that way.
SKILLS_SECTION_HINT = "# Available Skills"

# Tool-name families seen across the measured agents.
SKILL_TOOL_NAMES = ("skill", "skills.read", "skill_manage", "skill.read", "skills", "skill.list")
READ_TOOL_NAMES = ("read", "read_file", "file.read", "cat")
WRITE_TOOL_NAMES = ("write", "write_file", "file.write")
SKILL_NAME_PROPS = ("skill", "name", "skill_name")
SKILL_ACTION_PROPS = ("action", "operation", "mode", "command")
PATH_PROPS = ("file_path", "path", "file", "filepath", "target_file", "uri", "url")
# A tool that opens a local file. `uri`/`url` are deliberately absent: an MCP
# resource reader has a `uri` but needs a `server`, and calling it with a path
# just fails.
FILE_PATH_PROPS = ("file_path", "path", "file", "filepath", "target_file")
MCP_TEXT_PROPS = ("text", "input", "message", "content", "value", "query", "prompt")
# A route an agent prints in its prompt for an MCP tool it reaches by reading a
# URI instead of exposing a function (omp).
MCP_ROUTE_RE = re.compile(r"xd://[^\s\"'`)\]}]*" + MCP_TOOL_SUFFIX)
# The fixed MCP surface a session keeps in its cache-stable tool list; a late
# server's tool is reached through it instead of by its own name.
DEFERRED_MCP_TOOL_NAMES = ("mcp_call", "mcp.call")
DEFERRED_SERVER_RE = re.compile(r"server\s*=\s*\\?[\"']([^\"'\\]+)")
# A nonce the mock handed out; finding it in a tool result is the MCP round trip.
NONCE_RE = re.compile(NONCE_PREFIX + r"\d+")
# Claude Code defers MCP tools: the prompt lists `mcp__<server>__<tool>` by name
# and the model loads its schema with `ToolSearch` (`select:<name>`) before the
# tool appears in the request's tool list. The pattern is matched against the
# JSON-encoded body, where a name on its own line follows a literal `\n`, so it
# has no leading word boundary.
TOOL_SEARCH_NAMES = ("toolsearch", "tool_search")
DEFERRED_LISTED_RE = re.compile(r"mcp__[A-Za-z0-9_-]+__" + MCP_TOOL_SUFFIX + r"\b")


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


# A skill or MCP plan calls again only while the fixture's proof is missing.
# An agent whose tool keeps failing would otherwise loop until the harness
# timeout, so the plan stops after this many calls and answers plainly; the
# run then reports a failed round trip with its reason instead of a timeout.
MAX_SCENARIO_TOOL_CALLS = 4


def tool_call_count(kind: str, body: dict) -> int:
    """Tool calls the model already made anywhere in this transcript."""
    if kind == "anthropic":
        return sum(
            1
            for message in body.get("messages") or []
            if isinstance(message, dict) and message.get("role") == "assistant"
            and isinstance(message.get("content"), list)
            for block in message["content"]
            if isinstance(block, dict) and block.get("type") == "tool_use"
        )
    if kind == "responses":
        return sum(
            1
            for item in body.get("input") or []
            if isinstance(item, dict)
            and item.get("type") in ("function_call", "custom_tool_call", "local_shell_call")
        )
    return sum(
        len(message.get("tool_calls") or [])
        for message in body.get("messages") or []
        if isinstance(message, dict) and message.get("role") == "assistant"
    )


def tool_result_text(kind: str, body: dict) -> str:
    """Flatten the tool results of the current (last) user turn.

    The skill plan proves a round trip by finding the fixture sentinel here, so
    the search must cover exactly the results belonging to this turn: an earlier
    turn's skill body would otherwise make a call-less turn look answered.
    """
    parts: list[str] = []
    if kind == "anthropic":
        messages = [m for m in (body.get("messages") or []) if isinstance(m, dict)]
        for index in range(len(messages) - 1, -1, -1):
            if messages[index].get("role") != "user":
                continue
            content = messages[index].get("content")
            if isinstance(content, list):
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        parts.append(_text_of(block.get("content")))
            break
        return "\n".join(parts)
    if kind == "responses":
        parts: list[str] = []
        for item in body.get("input") or []:
            if not isinstance(item, dict):
                continue
            if item.get("type") in ("function_call_output", "custom_tool_call_output", "local_shell_call_output"):
                parts.append(_text_of(item.get("output")))
            elif item.get("role") == "user":
                parts = []
            elif item.get("role") == "tool":
                parts.append(_text_of(item.get("content")))
        return "\n".join(parts)
    messages = [m for m in (body.get("messages") or []) if isinstance(m, dict)]
    for index in range(len(messages) - 1, -1, -1):
        if messages[index].get("role") != "user":
            continue
        for later in messages[index + 1 :]:
            if later.get("role") == "tool":
                parts.append(_text_of(later.get("content")))
        break
    return "\n".join(parts)


def _tool_required(tool: dict) -> list[str]:
    for key in ("input_schema", "parameters", "schema", "inputSchema"):
        schema = tool.get(key)
        if isinstance(schema, dict) and isinstance(schema.get("required"), list):
            return [str(name) for name in schema["required"]]
    fn = tool.get("function")
    if isinstance(fn, dict):
        return _tool_required(fn)
    return []


def _tool_text(tool: dict) -> str:
    """Everything the model can read about a tool, for capability hints."""
    return json.dumps(tool, ensure_ascii=False).lower()


def _first_prop(props: dict, candidates: tuple[str, ...]) -> str | None:
    for candidate in candidates:
        if candidate in props:
            return candidate
    return None


def flatten_tools(tools, namespace: str | None = None) -> list[tuple[dict, str | None]]:
    """Tools in call order, with the namespace a nested tool belongs to.

    Codex exposes an MCP server as one namespace tool holding its tools, so a
    flat scan would never see `bench_echo` at all.
    """
    flat: list[tuple[dict, str | None]] = []
    for tool in tools or []:
        if not isinstance(tool, dict):
            continue
        nested = tool.get("tools")
        if isinstance(nested, list):
            parent = _tool_name(tool) or namespace
            flat.extend(flatten_tools(nested, parent))
            continue
        flat.append((tool, namespace))
    return flat


def _tool_named(tools, names: tuple[str, ...]) -> dict | None:
    wanted = {name.casefold() for name in names}
    for tool, _namespace in flatten_tools(tools):
        if _tool_name(tool).strip().casefold() in wanted:
            return tool
    return None


def find_skill_tool(tools) -> dict | None:
    """The agent's own skill tool (`Skill`, `skill`, `skills.read`, ...)."""
    return _tool_named(tools, SKILL_TOOL_NAMES)


def find_read_tool(tools) -> dict | None:
    """A tool that opens a local file, used for the skill-body fallback routes."""
    for candidate, _namespace in flatten_tools(tools):
        name = _tool_name(candidate).casefold()
        reads = name in READ_TOOL_NAMES or name.endswith(".read") or name.startswith("read_")
        if not reads or "resource" in name:
            continue
        if _first_prop(_tool_schema_props(candidate), FILE_PATH_PROPS):
            return candidate
    return None


def find_write_tool(tools) -> dict | None:
    """A tool that writes to a path, used to reach a URI-based MCP route."""
    for candidate, _namespace in flatten_tools(tools):
        name = _tool_name(candidate).casefold()
        if name not in WRITE_TOOL_NAMES and not name.endswith(".write"):
            continue
        if _first_prop(_tool_schema_props(candidate), FILE_PATH_PROPS):
            return candidate
    return None


def find_mcp_tool(tools) -> tuple[dict, str | None] | None:
    """The fixture MCP tool and its namespace, matched by the name suffix.

    Agents name a server's tools differently (`mcp__s__t`, `s_t`,
    `mcp__<server>_<tool>`, or a namespace holding the bare tool name), but all
    of them keep the tool's own name at the end.
    """
    for tool, namespace in flatten_tools(tools):
        if _tool_name(tool).strip().casefold().endswith(MCP_TOOL_SUFFIX):
            return tool, namespace
    return None


def find_deferred_mcp_tool(tools) -> dict | None:
    """The fixed `mcp_call` surface, if the session exposes it."""
    for tool, _namespace in flatten_tools(tools):
        if _tool_name(tool).strip().casefold() in DEFERRED_MCP_TOOL_NAMES:
            return tool
    return None


def find_mcp_action_tool(tools) -> dict | None:
    """A management tool that calls MCP tools through `action: "call"`.

    jcode's documented route for providers without native deferred tools is
    `mcp action=search/call`; its schema lists `call` in the `action` enum
    beside `server`, `tool` and `arguments`.
    """
    for tool, _namespace in flatten_tools(tools):
        props = _tool_schema_props(tool)
        action = props.get("action") if isinstance(props, dict) else None
        if not isinstance(action, dict) or "call" not in (action.get("enum") or []):
            continue
        if all(name in props for name in ("server", "tool", "arguments")):
            return tool
    return None


def deferred_server_name(body: dict) -> str | None:
    """The server name the prompt or the transcript's announcement spells."""
    match = DEFERRED_SERVER_RE.search(json.dumps(body, ensure_ascii=False))
    return match.group(1) if match else None


def find_tool_search(tools) -> dict | None:
    """A tool that loads deferred tool schemas by name (`ToolSearch`)."""
    for tool, _namespace in flatten_tools(tools):
        if _tool_name(tool).strip().casefold() in TOOL_SEARCH_NAMES:
            return tool
    return None


def deferred_listed_tool(body: dict) -> str | None:
    """The fixture tool's full name when the prompt lists it as deferred."""
    match = DEFERRED_LISTED_RE.search(json.dumps(body, ensure_ascii=False))
    return match.group(0) if match else None


def find_mcp_route(body: dict) -> str | None:
    """A `xd://...bench_echo` route printed in the prompt, if any."""
    match = MCP_ROUTE_RE.search(json.dumps(body, ensure_ascii=False))
    return match.group(0) if match else None


def user_turn_text(kind: str, body: dict) -> str:
    """The text of the prompt that started the current turn."""
    if kind == "anthropic":
        messages = [m for m in (body.get("messages") or []) if isinstance(m, dict)]
        for message in reversed(messages):
            if message.get("role") == "user":
                return _text_of(message.get("content"))
        return ""
    if kind == "responses":
        for item in reversed(body.get("input") or []):
            if isinstance(item, dict) and item.get("role") == "user":
                return _text_of(item.get("content"))
        return ""
    messages = [m for m in (body.get("messages") or []) if isinstance(m, dict)]
    for message in reversed(messages):
        if message.get("role") == "user":
            return _text_of(message.get("content"))
    return ""


def skill_path_from_prompt(prompt: str) -> str | None:
    """The absolute SKILL.md path the harness put in the skill prompt."""
    for token in prompt.replace("`", " ").replace(",", " ").split():
        if token.lower().endswith("skill.md") and (token.startswith("/") or re.match(r"^[A-Za-z]:[\\/]", token)):
            return token
    return None


def _skill_arguments(tool: dict, value: str) -> dict:
    """Build a skill-tool call from the tool's own schema.

    Names, actions and paths differ per agent (`Skill{skill}`, `skill{name}`,
    `skill_manage{action,name}`, `skills.read{name}`); reading the schema beats a
    per-agent branch, and a property the schema does not define is never sent.
    """
    props = _tool_schema_props(tool)
    required = _tool_required(tool)
    arguments: dict = {}
    for name, schema in props.items():
        lowered = name.lower()
        if lowered in SKILL_NAME_PROPS:
            arguments[name] = SKILL_NAME
        elif lowered in SKILL_ACTION_PROPS:
            options = [str(v) for v in (schema.get("enum") or [])] if isinstance(schema, dict) else []
            for preferred in ("load", "read", "invoke", "use"):
                if preferred in options:
                    arguments[name] = preferred
                    break
            else:
                arguments[name] = options[0] if options else "load"
        elif lowered in PATH_PROPS:
            arguments[name] = value
    for name in required:
        if name in arguments:
            continue
        schema = props.get(name) or {}
        options = schema.get("enum") if isinstance(schema, dict) else None
        if isinstance(options, list) and options:
            arguments[name] = options[0]
        elif schema.get("type") == "number" or schema.get("type") == "integer":
            arguments[name] = 0
        elif schema.get("type") == "boolean":
            arguments[name] = True
        else:
            arguments[name] = ""
    return arguments


def _mcp_arguments(tool: dict, nonce: str) -> dict:
    props = _tool_schema_props(tool)
    prop = _first_prop(props, MCP_TEXT_PROPS)
    if prop is None:
        prop = next((name for name in props if props[name].get("type") == "string"), None)
    return {prop or "text": nonce}


def _plan_name(body: dict, prompt: str) -> str:
    """Which scenario this request belongs to, from the harness's prompt marker.

    The whole transcript is searched, not only the newest user message: some
    agents send a turn's tool results as a user message, so the prompt that
    carried the marker is no longer the last one by the time the tool result
    comes back.
    """
    blob = prompt + "\n" + json.dumps(body, ensure_ascii=False)
    if SKILL_MARKER in blob:
        return "skill"
    if MCP_MARKER in blob:
        return "mcp"
    return "ls"


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
            "sentinels_seen": sum(1 for r in requests if r.get("sentinel_seen")),
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
        """Decide what the model asks for next, and record why.

        A request's plan comes from a marker in the request's prompt:
        `[bench:skill]` invokes the fixture skill, `[bench:mcp]` calls the
        fixture MCP tool, and anything else keeps the original `ls` behaviour
        (so one-shot and session numbers are unchanged). omp's skill-routing
        hint request carries no marker; answering it plainly would make omp
        retry, so it gets a valid hint instead.
        """
        tools = body.get("tools")
        prompt = user_turn_text(kind, body)
        plan_name = _plan_name(body, prompt)
        if ROUTING_HINT_PROMPT in prompt:
            plan_name = "hint"
        saw_result = has_tool_result(kind, body)
        results = tool_result_text(kind, body)
        transcript = json.dumps(body, ensure_ascii=False)
        counter = STATS.next_call_id()
        nonce = f"{NONCE_PREFIX}{counter}"

        tool_name = None
        arguments: dict = {}
        namespace = None
        via = None
        if plan_name == "hint":
            pass
        elif plan_name == "skill":
            skill_tool = find_skill_tool(tools)
            read_tool = find_read_tool(tools)
            shell = find_shell_tool(tools)
            path = skill_path_from_prompt(prompt)
            # The body can arrive as a tool result or as an injected user
            # message (Claude Code does the latter), so the whole transcript is
            # searched; only the fixture contains the sentinel.
            if SKILL_SENTINEL in transcript:
                pass
            elif skill_tool is not None:
                tool_name = _tool_name(skill_tool)
                via = "skill-tool"
                arguments = _skill_arguments(skill_tool, SKILL_NAME)
            elif read_tool is not None and (
                "skill://" in _tool_text(read_tool)
                or (SKILLS_SECTION_HINT in transcript and "skill://" in transcript)
            ):
                prop = _first_prop(_tool_schema_props(read_tool), PATH_PROPS) or "file_path"
                tool_name = _tool_name(read_tool)
                via = "skill-uri"
                arguments = {prop: f"skill://{SKILL_NAME}"}
            elif read_tool is not None and path:
                prop = _first_prop(_tool_schema_props(read_tool), PATH_PROPS) or "file_path"
                tool_name = _tool_name(read_tool)
                via = "file-read"
                arguments = {prop: path}
            elif shell is not None and path:
                # Codex has no file-read tool; its shell tool is the documented
                # way to open a skill file.
                tool_name = shell[0]
                via = "file-read"
                arguments = {shell[1]: f"cat '{path}'"}
        elif plan_name == "mcp":
            mcp = find_mcp_tool(tools)
            route = find_mcp_route(body)
            deferred = find_deferred_mcp_tool(tools)
            action_tool = find_mcp_action_tool(tools)
            server = deferred_server_name(body)
            tool_search = find_tool_search(tools)
            listed = deferred_listed_tool(body)
            if NONCE_RE.search(results):
                pass
            elif mcp is not None:
                mcp_tool, namespace = mcp
                # A tool nested in a namespace is called by its own name with
                # the namespace in a separate `namespace` field, the shape the
                # Codex client deserializes (`ResponseItem::FunctionCall`); a
                # dotted `namespace.tool` name is never dispatched.
                tool_name = _tool_name(mcp_tool)
                via = "mcp-tool"
                arguments = _mcp_arguments(mcp_tool, nonce)
            elif deferred is not None and server:
                # horon keeps its tool list cache-stable and announces a late
                # server's tool in the transcript, so the call goes through the
                # fixed mcp_call surface.
                tool_name = _tool_name(deferred)
                via = "mcp-call"
                arguments = {"server": server, "tool": MCP_TOOL_SUFFIX,
                             "arguments": {"text": nonce}}
            elif action_tool is not None and server:
                # jcode keeps MCP tools out of an OpenAI-compatible tool list
                # and dispatches them through `mcp action=call`.
                tool_name = _tool_name(action_tool)
                via = "mcp-action"
                arguments = {"action": "call", "server": server, "tool": MCP_TOOL_SUFFIX,
                             "arguments": {"text": nonce}}
            elif tool_search is not None and listed:
                # Claude Code lists the server's tool as deferred; its schema
                # is loaded first, and the next request offers the tool itself.
                prop = _first_prop(_tool_schema_props(tool_search), ("query",)) or "query"
                tool_name = _tool_name(tool_search)
                via = "tool-search"
                arguments = {prop: f"select:{listed}"}
            elif route and find_write_tool(tools) is not None:
                write_tool = find_write_tool(tools)
                assert write_tool is not None
                prop = _first_prop(_tool_schema_props(write_tool), PATH_PROPS) or "path"
                tool_name = _tool_name(write_tool)
                via = "mcp-route"
                arguments = {prop: route, "content": json.dumps({"text": nonce})}
            elif route and find_read_tool(tools) is not None:
                read_tool = find_read_tool(tools)
                assert read_tool is not None
                prop = _first_prop(_tool_schema_props(read_tool), PATH_PROPS) or "path"
                tool_name = _tool_name(read_tool)
                via = "mcp-route"
                arguments = {prop: route}
        else:
            shell = find_shell_tool(tools)
            if shell is not None and not saw_result:
                tool_name = shell[0]
                arguments = {shell[1]: "ls"}

        capped = plan_name in ("skill", "mcp") and tool_call_count(kind, body) >= MAX_SCENARIO_TOOL_CALLS
        if capped:
            tool_name, arguments, namespace, via = None, {}, None, None
        calls_tool = tool_name is not None
        if plan_name == "skill":
            sentinel_seen = SKILL_SENTINEL in transcript
        elif plan_name == "mcp":
            sentinel_seen = NONCE_RE.search(results) is not None
        else:
            sentinel_seen = False
        entry = {
            "endpoint": endpoint,
            "kind": kind,
            "stream": bool(body.get("stream")),
            "model": body.get("model"),
            "plan": plan_name,
            "has_tool_result": saw_result,
            "issued_tool_call": calls_tool,
            "tool_name": tool_name,
            "tool_choice_prop": next(iter(arguments), None),
            "via": via,
            "sentinel_seen": sentinel_seen,
            "capped": capped,
            "bench_echo_exposed": plan_name == "mcp"
            and bool(
                find_mcp_tool(tools) is not None
                or find_mcp_route(body)
                or (find_deferred_mcp_tool(tools) is not None and deferred_server_name(body))
                or (find_mcp_action_tool(tools) is not None and deferred_server_name(body))
                or (find_tool_search(tools) is not None and deferred_listed_tool(body))
            ),
            "tool_count": len(tools or []),
            # Every tool name the request offered, so a missing MCP server can
            # be told apart from a misnamed one without re-running the agent.
            "tool_names": [_tool_name(tool) for tool, _ns in flatten_tools(tools)][:60],
            "paths": collect_paths(body),
            # The tail of the tool results so far, kept only for the skill and
            # MCP plans: a call that ran without returning the fixture's proof
            # is diagnosed from this instead of a re-run.
            "result_excerpt": results[-400:] if plan_name in ("skill", "mcp") and results else None,
            "ts": time.time(),
        }
        STATS.record(entry)
        return {
            "choice": "tool" if calls_tool else "text",
            "call_id": f"call_mock_{counter}",
            "tool": tool_name,
            "arguments": arguments,
            "namespace": namespace if calls_tool else None,
            "text": ROUTING_HINT_ANSWER if plan_name == "hint" else FINAL_TEXT,
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
                                "arguments": json.dumps(plan["arguments"]),
                            },
                        }
                    ],
                }
                finish = "tool_calls"
            else:
                message = {"role": "assistant", "content": plan["text"]}
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
            args = json.dumps(plan["arguments"])
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
                {**base, "choices": [{"index": 0, "delta": {"content": plan["text"]}, "finish_reason": None}]},
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
                    **({"namespace": plan["namespace"]} if plan.get("namespace") else {}),
                    "arguments": json.dumps(plan["arguments"]),
                }
            else:
                item = {
                    "id": msg_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": plan["text"], "annotations": []}],
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
            args = json.dumps(plan["arguments"])
            added = {
                "id": item_id,
                "type": "function_call",
                "status": "in_progress",
                "call_id": plan["call_id"],
                "name": plan["tool"],
                **({"namespace": plan["namespace"]} if plan.get("namespace") else {}),
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
                    "delta": plan["text"],
                },
            )
            self._sse(
                "response.output_text.done",
                {
                    "type": "response.output_text.done",
                    "item_id": msg_id,
                    "output_index": 0,
                    "content_index": 0,
                    "text": plan["text"],
                },
            )
            self._sse(
                "response.content_part.done",
                {
                    "type": "response.content_part.done",
                    "item_id": msg_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": plan["text"], "annotations": []},
                },
            )
            done_item = {
                "id": msg_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": plan["text"], "annotations": []}],
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
                        "input": plan["arguments"],
                    }
                ]
                stop = "tool_use"
            else:
                content = [{"type": "text", "text": plan["text"]}]
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
                        "partial_json": json.dumps(plan["arguments"]),
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
                    "delta": {"type": "text_delta", "text": plan["text"]},
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
