#!/usr/bin/env python3
"""Compare horon-agent with other headless coding agents.

Every number is produced against a local mock model server
(``scripts/perf/mock_llm_server.py``), so a run spends no money and needs no
credentials. Each tool is pointed at that server through its own supported
configuration surface inside a throwaway HOME, never the real one.

Measured:
  version_ms        cold ``--version`` wall time (N repeats)
  oneshot_ms        one headless prompt through to completion, plus peak
                    process-tree RSS and whether a tool round-trip happened
  sessions_memory   idle and post-turn process-tree RSS for N concurrent
                    sessions x K turns, plus the server's own thread and
                    descriptor counts after the turns; "first session" and
                    "each additional session" (least-squares slope)
  skill             one headless prompt that loads and uses the fixture skill
                    (``fixtures/bench_skill``), with the route each agent takes
                    and whether the fixture sentinel came back
  mcp               one headless prompt that starts the fixture stdio MCP server
                    (``fixtures/bench_mcp_server.py``) and calls ``bench_echo``
                    once, with the time to the first request that lists the tool
  fsync_calls       fsync + fdatasync calls made by one one-shot run, counted
                    with strace (``--syscalls``; Linux only, recorded as
                    skipped when strace is missing)
  install_size_bytes  binary or npm package directory size

horon is the baseline; every other row shows its multiple of horon.

A second build of horon is a row too: `--bin horon-base=<path>` measures it with
the same runner right after the candidate, so a before/after pair is produced on
one machine, one mock server, and one load level instead of two runs.

``--scenarios`` picks the phases (default ``oneshot,sessions,skill,mcp``); a
subset really does skip the rest, which is how a skill-only or MCP-only run is
measured.

Usage:
    python3 scripts/perf/bench_compare.py --bin horon=./target/release/horon-agent \
        --bin horon-base=./baseline/target/release/horon-agent \
        --sessions 1 10 100 --turns 3 --json-out out.json --markdown-out out.md
    python3 scripts/perf/bench_compare.py --bin horon=./target/release/horon-agent \
        --scenarios skill,mcp --json-out skill-mcp.json

Missing tools are listed under "skipped" with a reason and do not fail the run.
A measurement that fails records null plus an error string instead of a zero.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_common as bc  # noqa: E402

SUITE = "compare"
DEFAULT_TOOLS = ["horon", "jcode", "claude_code", "codex", "opencode", "pi", "omp", "omo"]
DEFAULT_SCENARIOS = ["oneshot", "sessions", "skill", "mcp"]
SCENARIO_NAMES = ("oneshot", "sessions", "skill", "mcp")
TURN_PROMPT = "List the files in the current directory, then reply with mock-ok."
SESSION_TURNS = [
    "List the files in the current directory.",
    "List the files in the current directory again.",
    "List the files one more time.",
    "Show the directory listing.",
    "Run ls again.",
]

# Scenario prompts. The mock model picks its plan from the leading marker, so the
# scenario never depends on the agent's wording. The skill prompt names the
# fixture's absolute path because the agents that have no skill tool (pi, omp,
# omo, codex) open the file with their own read or shell tool.
SKILL_MARKER = "[bench:skill]"
MCP_MARKER = "[bench:mcp]"
SKILL_NAME = "bench-skill"
MCP_SERVER_NAME = "bench"
MCP_TOOL_NAME = "bench_echo"
FIXTURES = Path(__file__).resolve().with_name("fixtures")
SKILL_FIXTURE = FIXTURES / "bench_skill" / "SKILL.md"
MCP_FIXTURE = FIXTURES / "bench_mcp_server.py"


def skill_prompt(skill_md_path: str) -> str:
    return f"{SKILL_MARKER} Use the {SKILL_NAME} skill from {skill_md_path} and then reply with mock-ok."


def mcp_prompt() -> str:
    # The server name is spelled out because a session whose tool list stays
    # cache-stable reaches the tool through `mcp_call`, which needs it.
    return (
        f'{MCP_MARKER} Call the {MCP_TOOL_NAME} tool on the MCP server="{MCP_SERVER_NAME}" '
        f"once, then reply with mock-ok."
    )


def install_skill(skill_root: str) -> str:
    """Copy the fixture skill under ``skill_root`` and return the SKILL.md path."""
    target_dir = os.path.join(skill_root, SKILL_NAME)
    os.makedirs(target_dir, exist_ok=True)
    target = os.path.join(target_dir, "SKILL.md")
    shutil.copyfile(SKILL_FIXTURE, target)
    return target


def mcp_server_entry() -> dict:
    """One stdio server entry every agent's config format can hold.

    The interpreter running this script is used instead of a bare ``python3``:
    it is the same interpreter on every OS, including the Windows runners.
    """
    return {"command": sys.executable, "args": [str(MCP_FIXTURE)]}


def write_mcp_servers(path: str, servers: dict, wrapper: str = "servers") -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump({wrapper: servers}, fh, indent=2)


def mcp_servers() -> dict:
    return {MCP_SERVER_NAME: mcp_server_entry()}


# ---------------------------------------------------------------------------
# Mock model server
# ---------------------------------------------------------------------------


class MockServer:
    """Owns one mock_llm_server.py process on a free localhost port."""

    def __init__(self) -> None:
        self.proc: subprocess.Popen | None = None
        self.port: int | None = None
        self.base_url: str | None = None
        self.log_path: str | None = None

    def start(self) -> "MockServer":
        script = str(Path(__file__).with_name("mock_llm_server.py"))
        log = open(os.path.join(bc.short_temp_dir("hm-log-"), "mock.log"), "w", encoding="utf-8")
        self.log_path = log.name
        self.proc = subprocess.Popen(
            [sys.executable, script, "--port", "0"],
            stdout=subprocess.PIPE,
            stderr=log,
            text=True,
            **bc.new_group_kwargs(),
        )
        assert self.proc.stdout is not None
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("mock model server did not print its port")
        info = json.loads(line)
        self.port = info["port"]
        self.base_url = info["url"]
        return self

    def _get(self, path: str, timeout: float = 10.0) -> dict:
        with urllib.request.urlopen(f"{self.base_url}{path}", timeout=timeout) as resp:
            return json.loads(resp.read() or b"{}")

    def stats(self) -> dict:
        return self._get("/__stats")

    def reset(self) -> None:
        req = urllib.request.Request(f"{self.base_url}/__reset", data=b"{}", method="POST")
        req.add_header("content-type", "application/json")
        with urllib.request.urlopen(req, timeout=10) as resp:
            resp.read()

    def stop(self) -> None:
        if self.proc is None:
            return
        bc.terminate_pgroup(self.proc.pid)
        try:
            if self.proc.stdout:
                self.proc.stdout.close()
        except OSError:
            pass
        self.proc = None


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------


@dataclass
class ToolSpec:
    name: str
    binary: str | None = None
    version: str | None = None
    skipped_reason: str | None = None


BASE_ENV_DROP = (
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_ORG_ID",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "JCODE_HOME",
    "JCODE_RUNTIME_DIR",
    "HORON_HOME",
    "HORON_AGENT_RUNTIME_DIR",
    "HORON_AGENT_SOCKET",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "PI_CODING_AGENT_DIR",
    "PI_CONFIG_DIR",
    "PI_PACKAGE_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "OMP_CODING_AGENT_DIR",
    "OMP_PROFILE",
    "OMP_PLUGIN_ROOT",
    "OMP_AUTH_BROKER_URL",
    "OMP_AUTH_BROKER_TOKEN",
    "OMP_DAEMON_PROJECT_DIR",
    "OMP_DAEMON_RUNTIME_DIR",
    "OMP_TEXT_PREDICT_AGENT_DIR",
    "OMP_AUTO_UPDATE_TARGET",
    "OMO_CODING_AGENT_DIR",
    "SENPI_CODING_AGENT_DIR",
    "OMO_PACKAGE_DIR",
    "OMO_PLUGIN_ROOT",
    "OMO_DEBUG",
    "OMO_TELEMETRY_ALLOW_AUTOMATION",
    "OPENCODE_CONFIG",
)


def scrub_env(env: dict) -> dict:
    """Remove variables that would point a tool at the real user account."""
    clean = dict(env)
    for key in BASE_ENV_DROP:
        clean.pop(key, None)
    return clean


def resolve_bin(name: str, override: str | None, env: dict) -> str | None:
    if override:
        return override if os.path.exists(override) else None
    candidates = {
        "jcode": ["jcode"],
        "claude_code": ["claude"],
        "codex": ["codex"],
        "opencode": ["opencode"],
        "pi": ["pi"],
        "omp": ["omp"],
        "omo": ["omo"],
    }.get(name, [])
    for candidate in candidates:
        found = shutil.which(candidate, path=env.get("PATH"))
        if found:
            return found
    return None


def tool_version(binary: str, env: dict) -> str | None:
    try:
        out = subprocess.run(
            [binary, "--version"], capture_output=True, text=True, timeout=30, env=env, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    text = (out.stdout or out.stderr or "").strip().splitlines()
    return text[0].strip() if text else None


# ---------------------------------------------------------------------------
# Per-tool configuration
# ---------------------------------------------------------------------------


def write_horon_config(root: str, base_url: str) -> str:
    home = os.path.join(root, "home")
    os.makedirs(os.path.join(home, "config"), exist_ok=True)
    config = f"""[provider]
default_provider = "mock"
default_model = "mock-model"

[providers.mock]
type = "openai-compatible"
base_url = "{base_url}/v1"
auth = "none"
default_model = "mock-model"
requires_api_key = false

[[providers.mock.models]]
id = "mock-model"
context_window = 128000
"""
    path = os.path.join(home, "config.toml")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(config)
    return home


def horon_env(root: str, binary: str) -> dict:
    env = isolated_tool_env(root, "HORON_AGENT", home_var="HORON_HOME")
    env["HORON_AGENT_SOCKET"] = os.path.join(root, "run", "horon-agent.sock")
    env["HORON_AGENT_RUN_MCP"] = "0"
    env["HORON_AGENT_RUN_AUTO_POKE"] = "0"
    env["HORON_AGENT_DEBUG_CONTROL"] = "1"
    env["HORON_AGENT_NO_UPDATE"] = "1"
    env["DISABLE_TELEMETRY"] = "1"
    env["DO_NOT_TRACK"] = "1"
    env["PATH"] = os.path.dirname(binary) + os.pathsep + env.get("PATH", "")
    return env


def horon_turn_argv(binary: str, prompt: str, tools: str = "bash") -> list[str]:
    return [
        binary,
        "--no-update",
        "run",
        "--quiet",
        "--provider-profile",
        "mock",
        "--model",
        "mock-model",
        "--tools",
        tools,
        prompt,
    ]


def horon_oneshot_argv(binary: str) -> list[str]:
    return horon_turn_argv(binary, TURN_PROMPT)


def jcode_env(root: str, binary: str) -> dict:
    env = isolated_tool_env(root, "JCODE")
    env["JCODE_NO_TELEMETRY"] = "1"
    env["JCODE_DEBUG_CONTROL"] = "1"
    env["JCODE_TEMP_SERVER"] = "1"
    env["JCODE_SERVER_OWNER_PID"] = str(os.getpid())
    env["JCODE_MEMORY_ENABLED"] = "0"
    env["DISABLE_TELEMETRY"] = "1"
    env["DO_NOT_TRACK"] = "1"
    env["PATH"] = os.path.dirname(binary) + os.pathsep + env.get("PATH", "")
    return env


def write_jcode_config(root: str, base_url: str) -> None:
    home = os.path.join(root, "home")
    os.makedirs(home, exist_ok=True)
    config = f"""[provider]
default_provider = "mock"
default_model = "mock-model"

[providers.mock]
type = "openai-compatible"
base_url = "{base_url}/v1"
auth = "none"
default_model = "mock-model"
requires_api_key = false

[[providers.mock.models]]
id = "mock-model"
context_window = 128000
"""
    with open(os.path.join(home, "config.toml"), "w", encoding="utf-8") as fh:
        fh.write(config)


def jcode_oneshot_argv(binary: str, prompt: str = TURN_PROMPT, tools: str | None = "bash") -> list[str]:
    argv = [
        binary,
        "--no-update",
        "--no-selfdev",
        "run",
        "--provider-profile",
        "mock",
        "--model",
        "mock-model",
    ]
    if tools:
        argv += ["--tools", tools]
    return argv + [prompt]


def claude_env(root: str, base_url: str) -> dict:
    env = isolated_tool_env(root, "CLAUDE")
    env["CLAUDE_CONFIG_DIR"] = os.path.join(root, "home", ".claude")
    env["ANTHROPIC_BASE_URL"] = base_url
    env["ANTHROPIC_AUTH_TOKEN"] = "mock-token"
    env["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"] = "1"
    env["DISABLE_TELEMETRY"] = "1"
    env["DISABLE_AUTOUPDATER"] = "1"
    env["DISABLE_ERROR_REPORTING"] = "1"
    env["DISABLE_BUG_COMMAND"] = "1"
    env["CLAUDE_CODE_DISABLE_UPDATE_CHECK"] = "1"
    env["DO_NOT_TRACK"] = "1"
    return env


def claude_oneshot_argv(
    binary: str, prompt: str = TURN_PROMPT
) -> list[str]:
    argv = [
        binary,
        "-p",
        "--model",
        "mock-model",
        "--permission-mode",
        "bypassPermissions",
        "--output-format",
        "text",
    ]
    return argv + [prompt]


def claude_session_argv(binary: str) -> list[str]:
    return [
        binary,
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "bypassPermissions",
        "--model",
        "mock-model",
    ]


def codex_env(root: str) -> dict:
    env = isolated_tool_env(root, "CODEX")
    env["CODEX_HOME"] = os.path.join(root, "home")
    env["MOCK_PROVIDER_KEY"] = "mock-key"
    env["DISABLE_TELEMETRY"] = "1"
    env["OTEL_SDK_DISABLED"] = "true"
    env["DO_NOT_TRACK"] = "1"
    return env


CODEX_PROVIDER_ARGS = [
    "-c",
    'model_providers.mock={name="mock",base_url="__BASE__",env_key="MOCK_PROVIDER_KEY",wire_api="responses"}',
    "-c",
    "model_provider=mock",
]


def codex_provider_args(base_url: str) -> list[str]:
    return [part.replace("__BASE__", f"{base_url}/v1") for part in CODEX_PROVIDER_ARGS] + codex_sandbox_args()


def codex_sandbox_args() -> list[str]:
    """Pick a Windows sandbox Codex can start on a fresh machine.

    With no `windows.sandbox` set, a GitHub Windows runner has none provisioned
    and every shell command under `-s read-only` comes back `rejected: blocked
    by policy`. `unelevated` is the implementation Codex documents for hosts
    without administrator-approved setup
    (developers.openai.com/codex/windows); the sandbox stays on.
    """
    return ["-c", 'windows.sandbox="unelevated"'] if bc.IS_WINDOWS else []


def codex_oneshot_argv(binary: str, base_url: str, prompt: str = TURN_PROMPT) -> list[str]:
    return [
        binary,
        "exec",
        *codex_provider_args(base_url),
        "-m",
        "mock-model",
        "-s",
        "read-only",
        "-c",
        'approval_policy="never"',
        "--skip-git-repo-check",
        prompt,
    ]


def write_codex_mcp_config(codex_home: str) -> None:
    """The fixture server as a Codex `[mcp_servers]` table.

    JSON string literals are valid TOML basic strings, so a Windows path keeps
    its backslashes escaped. `approval_mode = "approve"` lets the one call run
    under `approval_policy="never"` instead of being cancelled for approval.
    """
    entry = mcp_server_entry()
    args = ", ".join(json.dumps(arg) for arg in entry["args"])
    text = (
        f"[mcp_servers.{MCP_SERVER_NAME}]\n"
        f"command = {json.dumps(entry['command'])}\n"
        f"args = [{args}]\n"
        "\n"
        f"[mcp_servers.{MCP_SERVER_NAME}.tools.{MCP_TOOL_NAME}]\n"
        'approval_mode = "approve"\n'
    )
    os.makedirs(codex_home, exist_ok=True)
    with open(os.path.join(codex_home, "config.toml"), "w", encoding="utf-8") as fh:
        fh.write(text)


def opencode_env(root: str, base_url: str, mcp: dict | None = None) -> dict:
    env = isolated_tool_env(root, "OPENCODE")
    config_dir = os.path.join(root, "config", "opencode")
    os.makedirs(config_dir, exist_ok=True)
    # The built-in `openai` provider accepts a baseURL override, so opencode does
    # not have to fetch @ai-sdk/openai-compatible at runtime. A custom
    # openai-compatible provider makes the first request install that npm
    # package into its cache, which blows past any sane turn timeout.
    config = {
        "$schema": "https://opencode.ai/config.json",
        "provider": {
            "openai": {
                "options": {"baseURL": f"{base_url}/v1", "apiKey": "mock-key"},
                "models": {"mock-model": {"name": "Mock Model"}},
            }
        },
        "model": "openai/mock-model",
    }
    if mcp:
        config["mcp"] = mcp
    with open(os.path.join(config_dir, "opencode.json"), "w", encoding="utf-8") as fh:
        json.dump(config, fh, indent=2)
    env["XDG_CONFIG_HOME"] = os.path.join(root, "config")
    env["OPENCODE_DISABLE_AUTOUPDATE"] = "1"
    env["OPENCODE_DISABLE_MODELS_FETCH"] = "1"
    env["OPENCODE_DISABLE_LSP_DOWNLOAD"] = "1"
    env["OPENCODE_DISABLE_EMBEDDED_WEB_UI"] = "1"
    env["DISABLE_TELEMETRY"] = "1"
    env["DO_NOT_TRACK"] = "1"
    return env


def opencode_oneshot_argv(binary: str, prompt: str = TURN_PROMPT) -> list[str]:
    return [binary, "run", "-m", "openai/mock-model", prompt]


def mock_models_config(base_url: str) -> dict:
    """The ``models.json`` shape the pi family reads: one OpenAI-compatible provider."""
    return {
        "providers": {
            "mock": {
                "baseUrl": f"{base_url}/v1",
                "api": "openai-completions",
                "apiKey": "mock-key",
                "models": [
                    {
                        "id": "mock-model",
                        "name": "Mock Model",
                        "contextWindow": 128000,
                        "maxTokens": 4096,
                        "input": ["text"],
                        "reasoning": False,
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                    }
                ],
            }
        }
    }


def _write_models_json(agent_dir: str, base_url: str) -> None:
    os.makedirs(agent_dir, exist_ok=True)
    with open(os.path.join(agent_dir, "models.json"), "w", encoding="utf-8") as fh:
        json.dump(mock_models_config(base_url), fh, indent=2)


def pi_env(root: str, base_url: str) -> dict:
    env = isolated_tool_env(root, "PI")
    agent_dir = os.path.join(root, "pi")
    _write_models_json(agent_dir, base_url)
    env["PI_CODING_AGENT_DIR"] = agent_dir
    return pi_family_telemetry_env(env)


# Paths ``bc.isolated_home_env`` fills in. It copies ``os.environ``, so the
# scrub must run after it and these keys must be put back on top: without the
# re-apply the scrub would also delete the isolated paths and the tool would
# fall back to the real user home.
ISOLATED_ENV_KEYS = (
    "HOME",
    "TMPDIR",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
)


def isolated_tool_env(root: str, prefix: str, home_var: str | None = None) -> dict:
    """Parent env with every tool-home variable dropped, plus isolated paths."""
    source = bc.isolated_home_env(root, prefix, home_var)
    keys = ISOLATED_ENV_KEYS + (home_var or f"{prefix}_HOME", f"{prefix}_RUNTIME_DIR")
    isolated = {key: source[key] for key in keys if key in source}
    env = scrub_env(source)
    env.update(isolated)
    return env


def _fresh_agent_dir(root: str, tool: str) -> str:
    """A new agent dir per measurement.

    omp caches the provider list and its base URL in ``agent.db``/``models.db``
    inside the agent dir and keeps calling the cached address, so reusing one
    dir across runs would measure a stale endpoint.
    """
    base = os.path.join(root, tool)
    os.makedirs(base, exist_ok=True)
    return tempfile.mkdtemp(prefix="agent-", dir=base)


def pi_family_telemetry_env(env: dict) -> dict:
    """Turn off update checks, telemetry, and any network use beyond the mock server."""
    env["PI_OFFLINE"] = "1"
    env["PI_SKIP_VERSION_CHECK"] = "1"
    env["PI_TELEMETRY"] = "0"
    env["DISABLE_TELEMETRY"] = "1"
    env["DO_NOT_TRACK"] = "1"
    return env


def omp_env(root: str, base_url: str) -> dict:
    env = isolated_tool_env(root, "OMP")
    agent_dir = _fresh_agent_dir(root, "omp")
    _write_models_json(agent_dir, base_url)
    env["PI_CODING_AGENT_DIR"] = agent_dir
    return pi_family_telemetry_env(env)


def omo_env(root: str, base_url: str) -> dict:
    env = isolated_tool_env(root, "OMO")
    agent_dir = _fresh_agent_dir(root, "omo")
    _write_models_json(agent_dir, base_url)
    # omo and the senpi engine it runs each read one of these names.
    env["PI_CODING_AGENT_DIR"] = agent_dir
    env["OMO_CODING_AGENT_DIR"] = agent_dir
    env["SENPI_CODING_AGENT_DIR"] = agent_dir
    return pi_family_telemetry_env(env)


def pi_oneshot_argv(binary: str, prompt: str = TURN_PROMPT, mcp: bool = False) -> list[str]:
    # pi 1.x connects every server in mcp.json; --no-mcp keeps the rows that do
    # not measure MCP free of it, so they stay comparable with the other tools.
    argv = [binary, "-p", "--no-session", "--provider", "mock", "--model", "mock-model"]
    if not mcp:
        argv.append("--no-mcp")
    return argv + [prompt]


def pi_session_argv(binary: str) -> list[str]:
    return [
        binary,
        "--mode",
        "rpc",
        "--no-session",
        "--no-mcp",
        "--provider",
        "mock",
        "--model",
        "mock-model",
    ]


def omp_oneshot_argv(binary: str, prompt: str = TURN_PROMPT) -> list[str]:
    # --no-title keeps a session-title request (one extra model call) out of the
    # measurement, and --no-lsp stops a language-server download attempt.
    return [
        binary,
        "-p",
        "--no-session",
        "--no-title",
        "--model",
        "mock/mock-model",
        "--no-lsp",
        prompt,
    ]


def omp_session_argv(binary: str) -> list[str]:
    return [
        binary,
        "--mode",
        "rpc",
        "--no-session",
        "--no-title",
        "--model",
        "mock/mock-model",
        "--no-lsp",
    ]


def omo_oneshot_argv(binary: str, prompt: str = TURN_PROMPT, tools: str | None = "bash,read") -> list[str]:
    # omo's default tool set has no plain shell tool and --tools is a strict
    # allowlist, so `bash` has to be named for the mock model to find it.
    argv = [binary, "-p", "--no-session", "--model", "mock/mock-model"]
    if tools:
        argv += ["--tools", tools]
    return argv + [prompt]


def omo_session_argv(binary: str) -> list[str]:
    return [
        binary,
        "--mode",
        "rpc",
        "--no-session",
        "--model",
        "mock/mock-model",
        "--tools",
        "bash,read",
    ]


# ---------------------------------------------------------------------------
# Measurement helpers
# ---------------------------------------------------------------------------


@dataclass
class Proc:
    proc: subprocess.Popen
    pgid: int
    extra: dict = field(default_factory=dict)

    def close(self) -> None:
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
        except (OSError, ValueError):
            pass
        bc.terminate_pgroup(self.pgid)


def spawn(argv: list[str], cwd: str, env: dict, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) -> Proc:
    proc = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdin=subprocess.PIPE,
        stdout=stdout,
        stderr=stderr,
        text=True,
        bufsize=1,
        **bc.new_group_kwargs(),
    )
    return Proc(proc=proc, pgid=bc.pgid_of(proc.pid))


SYSCALL_NAMES = ("fsync", "fdatasync")


def strace_available() -> bool:
    """True when one-shot runs can be counted with strace on this host."""
    return sys.platform.startswith("linux") and shutil.which("strace") is not None


def strace_argv(argv: list[str], out_path: str) -> list[str]:
    """Wrap a command in a call-counting strace run.

    ``-f`` follows the tool's worker processes (the session writer may be a
    child of the process measured), ``-c`` prints one summary instead of every
    line, and ``-o`` keeps the summary out of the tool's own stdout.
    """
    return ["strace", "-f", "-c", "-e", "trace=" + ",".join(SYSCALL_NAMES), "-o", out_path, *argv]


def parse_strace_summary(text: str) -> dict | None:
    """Sum the fsync/fdatasync call counts in ``strace -c`` output.

    Rows look like `` 62.50  0.000412  82  5  fsync``; the errors column is
    blank when every call succeeded, so the call count is always the fourth
    field. Returns None when neither syscall row is present, which is what
    strace prints when the filter matched nothing.
    """
    counts = dict.fromkeys(SYSCALL_NAMES, 0)
    seen = False
    for line in text.splitlines():
        parts = line.split()
        if len(parts) < 5 or parts[-1] not in counts:
            continue
        try:
            calls = int(parts[3])
        except ValueError:
            continue
        counts[parts[-1]] += calls
        seen = True
    if not seen:
        return None
    return {
        "total": sum(counts.values()),
        "fsync": counts["fsync"],
        "fdatasync": counts["fdatasync"],
    }


def measure_oneshot(
    argv: list[str], cwd: str, env: dict, mock: MockServer, timeout: float, syscalls: bool = False
) -> dict:
    """Run one headless prompt and time it, sampling peak tree RSS.

    With ``syscalls`` the run is wrapped in a call-counting strace, so the same
    sample also reports how many fsync/fdatasync calls the tool made; the
    summary file lives in a throwaway directory removed before returning.
    """
    trace_dir = bc.short_temp_dir("hcmp-strace-") if syscalls else None
    try:
        return _measure_oneshot(argv, cwd, env, mock, timeout, trace_dir)
    finally:
        if trace_dir:
            shutil.rmtree(trace_dir, ignore_errors=True)


def _measure_oneshot(
    argv: list[str], cwd: str, env: dict, mock: MockServer, timeout: float, trace_dir: str | None
) -> dict:
    trace_path = os.path.join(trace_dir, "summary.txt") if trace_dir else None
    if trace_path:
        argv = strace_argv(argv, trace_path)
    try:
        before = mock.stats()["request_count"]
    except Exception:  # noqa: BLE001
        before = 0
    log_marks = _log_marks(env)
    started = time.perf_counter()
    started_wall = time.time()
    proc = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        **bc.new_group_kwargs(),
    )
    sampler = bc.MemorySampler(proc.pid, interval=0.03).start()
    timed_out = False
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        bc.terminate_pgroup(bc.pgid_of(proc.pid))
        try:
            stdout, stderr = proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            stdout, stderr = "", ""
    elapsed_ms = (time.perf_counter() - started) * 1000
    exit_local_time = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
    peak = sampler.stop()
    sentinel_seen = None
    mcp_ready_ms = None
    via = None
    tools_offered: list[str] = []
    result_excerpt = None
    request_offsets_ms: list[float] = []
    try:
        after = mock.stats()
        requests = after["requests"][before:]
        roundtrip = any(r.get("issued_tool_call") for r in requests) and any(
            r.get("has_tool_result") for r in requests
        )
        saw_requests = len(requests)
        # A skill or MCP scenario counts only when the fixture's sentinel came
        # back; the plain issued/result pair is not enough to prove the body or
        # the echo reached the model.
        sentinel_seen = any(r.get("sentinel_seen") for r in requests)
        exposed = [r["ts"] for r in requests if r.get("bench_echo_exposed")]
        if exposed and not timed_out:
            # Seconds from process start to the first request that lists the MCP
            # tool: server start plus registration, as the model sees it.
            mcp_ready_ms = round((min(exposed) - started_wall) * 1000, 2)
        vias = [r.get("via") for r in requests if r.get("via")]
        via = vias[-1] if vias else None
        # The first request's tool list is the evidence for "the server never
        # connected" versus "its tool had another name"; keeping it in the
        # report means a skipped row can be checked without a re-run.
        if requests:
            tools_offered = list(requests[0].get("tool_names") or [])
        # The last tool result the agent sent back: when a call ran but the
        # sentinel is missing, this says why (a sandbox refusal, a shell error).
        excerpts = [r["result_excerpt"] for r in requests if r.get("result_excerpt")]
        result_excerpt = excerpts[-1] if excerpts else None
        # When each model request arrived, in ms from process start. The gaps
        # split a run into startup, tool round trip and shutdown without a
        # profiler, on every OS the bench runs on.
        request_offsets_ms = [round((r["ts"] - started_wall) * 1000, 2) for r in requests if "ts" in r]
    except Exception:  # noqa: BLE001
        roundtrip = None
        saw_requests = None

    error = None
    if timed_out:
        error = f"timeout after {timeout:.0f}s"
    elif proc.returncode not in (0, None):
        detail = (stderr or "").strip().splitlines()
        error = f"exit {proc.returncode}: " + (detail[-1] if detail else "no stderr")
    syscall_counts = None
    if trace_path:
        try:
            with open(trace_path, encoding="utf-8", errors="replace") as fh:
                syscall_counts = parse_strace_summary(fh.read())
        except OSError:
            syscall_counts = None
    log_tail = _log_tail_since(env, log_marks)
    return {
        "ms": None if timed_out else round(elapsed_ms, 2),
        "peak_rss_bytes": peak or None,
        "tool_roundtrip": roundtrip,
        "mock_requests": saw_requests,
        "exit_code": proc.returncode,
        "error": error,
        "stdout_tail": (stdout or "").strip()[-300:],
        "fsync_calls": (syscall_counts or {}).get("total"),
        "fsync_breakdown": syscall_counts,
        "sentinel_seen": sentinel_seen,
        "mcp_ready_ms": mcp_ready_ms,
        "via": via,
        "tools_offered": tools_offered,
        "result_excerpt": result_excerpt,
        "request_offsets_ms": request_offsets_ms,
        "log_tail": log_tail,
        "exit_local_time": exit_local_time,
    }


def _horon_log_dir(env: dict) -> Path | None:
    home = env.get("HORON_HOME")
    return Path(home, "logs") if home else None


def _log_marks(env: dict) -> dict[str, int]:
    """Size of each horon log file before a run, so only its own lines are read."""
    directory = _horon_log_dir(env)
    if directory is None or not directory.is_dir():
        return {}
    return {entry.name: entry.stat().st_size for entry in directory.iterdir() if entry.is_file()}


def _log_tail_since(env: dict, marks: dict[str, int], limit: int = 40) -> list[str]:
    """horon's own log lines from the end of its turn to exit, for the phase split.

    The lines carry the process's local timestamps; beside ``exit_local_time``
    they show where the time between the last model request and exit goes, on
    every OS, without a profiler.
    """
    directory = _horon_log_dir(env)
    if directory is None or not directory.is_dir():
        return []
    lines: list[str] = []
    for entry in sorted(directory.iterdir()):
        if not entry.is_file():
            continue
        try:
            with open(entry, "rb") as fh:
                fh.seek(marks.get(entry.name, 0))
                lines.extend(fh.read().decode("utf-8", "replace").splitlines())
        except OSError:
            continue
    start = next((i for i, line in enumerate(lines) if "Turn complete" in line), max(0, len(lines) - limit))
    return [line[:200] for line in lines[start:start + limit]]


def measure_version(argv: list[str], env: dict, runs: int) -> dict:
    times: list[float] = []
    error = None
    for _ in range(max(1, runs)):
        started = time.perf_counter()
        try:
            result = subprocess.run(
                argv, capture_output=True, text=True, timeout=60, env=env, check=False
            )
        except (OSError, subprocess.SubprocessError) as exc:
            error = str(exc)[:200]
            break
        times.append((time.perf_counter() - started) * 1000)
        if result.returncode != 0 and not error:
            error = f"exit {result.returncode}"
    summary = bc.summarize(times)
    if summary is None:
        return {"summary": None, "error": error or "no successful runs"}
    if error:
        summary["error"] = error
    return {"summary": summary, "error": error}


def _resolve_npm_package(name: str, from_dir: Path) -> Path | None:
    """Node's lookup: <dir>/node_modules/<name> from from_dir upwards."""
    current = from_dir
    while True:
        candidate = current / "node_modules" / name
        if (candidate / "package.json").is_file():
            return candidate
        if current.parent == current:
            return None
        current = current.parent


def _npm_closure(package_dir: Path) -> list[Path]:
    """The package plus every installed dependency and optional dependency.

    Launcher packages such as @openai/codex ship their native binary in an
    optional platform package, so the launcher directory alone is a few KB.
    """
    seen: dict[str, Path] = {}
    queue = [package_dir]
    while queue:
        current = queue.pop()
        key = os.path.realpath(current)
        if key in seen:
            continue
        seen[key] = current
        try:
            manifest = json.loads((current / "package.json").read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        names = set(manifest.get("dependencies") or {}) | set(manifest.get("optionalDependencies") or {})
        for name in sorted(names):
            resolved = _resolve_npm_package(name, current)
            if resolved is not None:
                queue.append(resolved)
    return list(seen.values())


def _tree_size(root: Path) -> int:
    total = 0
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            try:
                total += os.lstat(os.path.join(dirpath, name)).st_size
            except OSError:
                continue
    return total


def _npm_shim_target(shim: str) -> str | None:
    """Script an npm ``.cmd`` shim in ``node_modules/.bin`` runs (Windows has no symlink)."""
    try:
        text = Path(shim).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    match = re.search(r'%dp0%\\\.\.\\([^"\r\n]+?)"', text)
    if match is None:
        return None
    target = Path(shim).parent.parent / match.group(1).replace("\\", os.sep)
    return str(target) if target.exists() else None


def measure_install_size(binary: str) -> tuple[int | None, str | None]:
    """Binary size, or the npm package plus its installed dependencies for node tools."""
    try:
        real = os.path.realpath(binary)
    except OSError:
        return None, "binary path not resolvable"
    if not os.path.exists(real):
        return None, "binary missing"
    if real.lower().endswith(".cmd") and Path(real).parent.name == ".bin":
        real = _npm_shim_target(real) or real
    package_dir = None
    current = Path(real).parent
    for _ in range(6):
        if (current / "package.json").is_file():
            package_dir = current
            break
        if current.parent == current:
            break
        current = current.parent
    if package_dir is None or "node_modules" not in str(package_dir):
        size = os.path.getsize(real)
        # A shell launcher shipped beside its payload (`<name>.bin`, as in the
        # jcode Linux release) is a few hundred bytes; count the payload too.
        payload = real + ".bin"
        if os.path.isfile(payload):
            size += os.path.getsize(payload)
        return size, None
    # A dependency nested inside the package is already counted by its parent's
    # walk; skip closure members that live under another member.
    members = sorted(_npm_closure(package_dir), key=lambda p: len(str(p)))
    roots: list[Path] = []
    for member in members:
        if not any(str(member).startswith(str(root) + os.sep) for root in roots):
            roots.append(member)
    return sum(_tree_size(root) for root in roots), None


# ---------------------------------------------------------------------------
# Session harnesses
# ---------------------------------------------------------------------------


def run_turns_parallel(turn_fns: list, timeout: float) -> list[str | None]:
    """Run one turn per session concurrently; return per-session errors."""
    errors: list[str | None] = [None] * len(turn_fns)
    threads = []

    def wrap(index: int, fn) -> None:
        try:
            errors[index] = fn()
        except Exception as exc:  # noqa: BLE001
            errors[index] = str(exc)[:200]

    for index, fn in enumerate(turn_fns):
        thread = threading.Thread(target=wrap, args=(index, fn), daemon=True)
        threads.append(thread)
        thread.start()
    deadline = time.perf_counter() + timeout
    for thread in threads:
        thread.join(max(0.1, deadline - time.perf_counter()))
    return errors


# ---- horon / jcode daemon harness ----------------------------------------


class UnixDebugSession:
    """A session on a daemon that speaks the ``debug_command`` JSON protocol."""

    def __init__(self, socket_path: str, session_id: str, cwd: str):
        self.socket_path = socket_path
        self.session_id = session_id
        self.cwd = cwd

    def command(self, command: str, timeout: float = 180.0) -> str:
        sock = bc.ipc_connect(self.socket_path, timeout=timeout)
        request = {"type": "debug_command", "id": 1, "command": command, "session_id": self.session_id}
        sock.sendall((json.dumps(request) + "\n").encode("utf-8"))
        data = b""
        try:
            deadline = time.time() + timeout
            while time.time() < deadline:
                chunk = sock.recv(1 << 20)
                if not chunk:
                    break
                data += chunk
                if b"\n" in data:
                    line = data.split(b"\n", 1)[0]
                    response = json.loads(line)
                    if response.get("ok") is False:
                        raise RuntimeError(str(response.get("output") or response.get("error"))[:200])
                    return response.get("output", "")
        finally:
            sock.close()
        raise TimeoutError(f"debug command timed out: {command[:40]}")

    def turn(self, prompt: str, timeout: float = 180.0) -> str | None:
        self.command(f"message:{prompt}", timeout=timeout)
        return None


def start_daemon(
    argv: list[str], cwd: str, env: dict, socket_path: str, debug_socket: str, log_path: str, timeout: float
) -> Proc:
    log = open(log_path, "w", encoding="utf-8")
    proc = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        **bc.new_group_kwargs(),
    )
    handle = Proc(proc=proc, pgid=bc.pgid_of(proc.pid))
    if not bc.wait_for_socket(main_socket_path(socket_path), timeout):
        if not bc.wait_for_socket(debug_socket, timeout):
            handle.close()
            raise RuntimeError(f"daemon socket not ready: {log_tail(log_path)}")
    if not bc.wait_for_socket(debug_socket, timeout):
        handle.close()
        raise RuntimeError(f"daemon debug socket not ready: {log_tail(log_path)}")
    return handle


def log_tail(path: str, limit: int = 300) -> str:
    """Last ``limit`` characters of a daemon log, so a failed start says why."""
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            text = fh.read().strip()
    except OSError:
        return f"{path} (unreadable)"
    return " ".join(text[-limit:].split()) or f"{path} (empty)"


def main_socket_path(socket_path: str) -> str:
    return socket_path


def measure_daemon_sessions(
    name: str,
    make_daemon,
    session_count: int,
    turns: int,
    cwd: str,
    timeout: float,
    probe=None,
    diagnostics=None,
) -> dict:
    """Generic N-session harness for a single daemon process.

    ``probe`` optionally returns the daemon's ``server:memory`` payload; its
    thread and descriptor counts are sampled once every session has saved a
    turn, so a per-session cost shows up in the population comparison.
    """
    daemon = None
    sessions: list[UnixDebugSession] = []
    samples: list[dict] = []
    errors: list[str] = []
    try:
        daemon, sessions, create_errors = make_daemon(session_count)
        errors.extend(create_errors)
        time.sleep(1.0)
        idle_rss = bc.process_tree_rss_bytes(daemon.proc.pid)
        samples.append(
            {
                "phase": "idle",
                "rss_bytes": idle_rss,
                "pss_bytes": bc.process_tree_pss_bytes(daemon.proc.pid),
                "live_heap_bytes": live_heap_bytes(daemon.proc.pid),
            }
        )
        turn_times: list[list[float]] = []
        for turn_index in range(turns):
            prompt = SESSION_TURNS[turn_index % len(SESSION_TURNS)]
            started = time.perf_counter()

            def make_turn(session: UnixDebugSession):
                return lambda: session.turn(prompt, timeout=timeout)

            turn_errors = run_turns_parallel([make_turn(s) for s in sessions], timeout + 30)
            elapsed = (time.perf_counter() - started) * 1000
            turn_times.append([round(elapsed, 2)])
            for index, error in enumerate(turn_errors):
                if error:
                    errors.append(f"turn {turn_index + 1} session {index}: {error}")
            time.sleep(0.5)
            samples.append(
                {
                    "phase": f"turn{turn_index + 1}",
                    "rss_bytes": bc.process_tree_rss_bytes(daemon.proc.pid),
                    "pss_bytes": bc.process_tree_pss_bytes(daemon.proc.pid),
                    "turn_ms": round(elapsed, 2),
                }
            )
        counters, counter_error = read_process_counters(probe)
        if samples:
            samples[-1].update(counters)
            # Taken after the RSS sample above so the inspection cannot move it.
            samples[-1]["live_heap_bytes"] = live_heap_bytes(daemon.proc.pid)
            samples[-1]["rss_anon_bytes"] = bc.process_tree_status_bytes(daemon.proc.pid, "RssAnon")
            samples[-1]["rss_file_bytes"] = bc.process_tree_status_bytes(daemon.proc.pid, "RssFile")
            # Last on purpose: a purge changes RSS, so every other sample is taken first.
            if diagnostics is not None:
                samples[-1].update(diagnostics(daemon.proc.pid))
        return {
            "sessions": session_count,
            "samples": samples,
            "turn_ms": turn_times,
            "errors": errors,
            "sessions_created": len(sessions),
            "counters": counters,
            "counter_error": counter_error,
            "thread_names": thread_name_counts(daemon.proc.pid),
        }
    finally:
        for session in sessions:
            del session
        if daemon is not None:
            daemon.close()


_HEAP_TOTAL = re.compile(r"All zones: \d+ nodes \((\d+) bytes\)")


def live_heap_bytes(pid: int) -> int | None:
    """Bytes the process's malloc zones hold live, on macOS only.

    Beside the RSS sample this separates live data from memory the allocator
    keeps after it was freed. ``heap(1)`` ships with macOS; elsewhere, or when
    the tool cannot inspect the process, the value is ``None``.
    """
    if sys.platform != "darwin" or shutil.which("heap") is None:
        return None
    try:
        out = subprocess.run(["heap", str(pid)], capture_output=True, text=True, timeout=60).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    match = _HEAP_TOTAL.search(out)
    return int(match.group(1)) if match else None


def thread_name_counts(pid: int) -> dict[str, int] | None:
    """Live threads of ``pid`` grouped by name, on Linux only.

    A per-session thread count says that threads grow with sessions; the names
    say which pool or worker grows, without a profiler on the runner.
    """
    task_dir = Path(f"/proc/{pid}/task")
    if not task_dir.is_dir():
        return None
    counts: dict[str, int] = {}
    for task in task_dir.iterdir():
        try:
            name = (task / "comm").read_text(encoding="utf-8", errors="replace").strip()
        except OSError:
            continue
        counts[name] = counts.get(name, 0) + 1
    return dict(sorted(counts.items()))


def horon_daemon_argv(binary: str, socket_path: str) -> list[str]:
    """argv for the isolated horon daemon under test.

    There is no debug-socket argument. `serve` derives the debug socket from the
    main one by substituting the `-debug.sock` suffix
    (`crates/app-core/src/server/socket.rs`), and debug commands are gated by
    `HORON_AGENT_DEBUG_CONTROL`, which `horon_env` sets.
    """
    return [
        binary,
        "--no-update",
        "serve",
        "--socket",
        socket_path,
        "--provider-profile",
        "mock",
        "--model",
        "mock-model",
        "--tools",
        "bash",
    ]


def debug_socket_for(socket_path: str) -> str:
    """Debug socket the daemon publishes beside its main socket."""
    return socket_path.replace(".sock", "-debug.sock")


def make_horon_daemon(binary: str, root: str, cwd: str):
    socket_path = os.path.join(root, "run", "bench.sock")
    debug_socket = debug_socket_for(socket_path)
    env = horon_env(root, binary)
    argv = horon_daemon_argv(binary, socket_path)

    def factory(count: int):
        daemon = start_daemon(
            argv, cwd, env, socket_path, debug_socket, os.path.join(root, "serve.log"), timeout=60
        )
        sessions = []
        errors = []
        for index in range(count):
            try:
                out = sessions_client(debug_socket, f"create_session:{cwd}")
                session_id = json.loads(out)["session_id"]
                sessions.append(UnixDebugSession(debug_socket, session_id, cwd))
            except Exception as exc:  # noqa: BLE001
                errors.append(f"create_session {index}: {exc}")
                break
        return daemon, sessions, errors

    return factory


def sessions_client(debug_socket: str, command: str, timeout: float = 60.0) -> str:
    sock = bc.ipc_connect(debug_socket, timeout=timeout)
    try:
        sock.sendall((json.dumps({"type": "debug_command", "id": 1, "command": command}) + "\n").encode())
        data = b""
        deadline = time.time() + timeout
        while time.time() < deadline:
            chunk = sock.recv(1 << 20)
            if not chunk:
                break
            data += chunk
            if b"\n" in data:
                response = json.loads(data.split(b"\n", 1)[0])
                if response.get("ok") is False:
                    raise RuntimeError(str(response.get("output") or response.get("error"))[:200])
                return response.get("output", "")
    finally:
        sock.close()
    raise TimeoutError("debug command timed out")


COUNTER_KEYS = ("thread_count", "fd_count")


def server_memory_probe(debug_socket: str):
    """Callable that reads the daemon's own process counters.

    ``server:memory`` is the surface that reports the server process's thread
    and descriptor counts; it belongs to the horon/jcode command family, so it
    is passed only to daemons that speak it and every failure stays a skip
    reason instead of a measurement error.
    """

    def probe() -> dict:
        return json.loads(sessions_client(debug_socket, "server:memory", timeout=30))

    return probe


def horon_memory_diagnostics(debug_socket: str):
    """Callable that splits a horon daemon's post-turn RSS by what holds it.

    ``allocator_live_bytes`` is what the allocator reports as live (glibc
    ``mallinfo2``; ``None`` where the daemon has no allocator stats), and
    ``post_purge_rss_bytes`` is the tree RSS after ``allocator:purge`` hands
    freed pages back (``malloc_trim`` on glibc). RSS minus the post-purge value
    is memory the allocator kept after it was freed; the post-purge value minus
    live bytes is what neither explains (stacks, mapped files, fragmentation).
    The purge changes the process, so this must run after every other sample.
    """

    def run(pid: int) -> dict:
        out: dict = {}
        try:
            info = json.loads(sessions_client(debug_socket, "allocator:info", timeout=30))
            out["allocator_live_bytes"] = (info.get("stats") or {}).get("allocated_bytes")
        except Exception as exc:  # noqa: BLE001
            out["allocator_error"] = str(exc)[:120]
        try:
            sessions_client(debug_socket, "allocator:purge", timeout=30)
            time.sleep(0.3)
            out["post_purge_rss_bytes"] = bc.process_tree_rss_bytes(pid)
        except Exception as exc:  # noqa: BLE001
            out["purge_error"] = str(exc)[:120]
        return out

    return run


def read_process_counters(probe) -> tuple[dict, str | None]:
    """Thread/descriptor counts from a ``server:memory`` payload.

    Returns the counters that were present plus, when any were not, the reason.
    A tool without that debug command, or one whose payload has a different
    process shape, leaves the counters out rather than reporting a zero.
    """
    if probe is None:
        return {}, "no server:memory probe for this tool"
    try:
        payload = probe()
    except Exception as exc:  # noqa: BLE001
        return {}, f"server:memory unavailable: {str(exc)[:120]}"
    process = payload.get("process") if isinstance(payload, dict) else None
    if not isinstance(process, dict):
        return {}, "server:memory reported no process section"
    counters: dict[str, int] = {}
    missing: list[str] = []
    for key in COUNTER_KEYS:
        value = process.get(key)
        if isinstance(value, int) and not isinstance(value, bool):
            counters[key] = value
        else:
            missing.append(key)
    if missing:
        return counters, "server:memory did not report " + ", ".join(missing)
    return counters, None


def make_jcode_daemon(binary: str, root: str, cwd: str):
    socket_path = os.path.join(root, "run", "bench.sock")
    debug_socket = socket_path.replace(".sock", "-debug.sock")
    env = jcode_env(root, binary)
    argv = [
        binary,
        "--no-update",
        "--no-selfdev",
        "serve",
        "--socket",
        socket_path,
        "--provider-profile",
        "mock",
        "--model",
        "mock-model",
        "--tools",
        "bash",
    ]

    def factory(count: int):
        daemon = start_daemon(
            argv, cwd, env, socket_path, debug_socket, os.path.join(root, "serve.log"), timeout=90
        )
        sessions = []
        errors = []
        for index in range(count):
            try:
                out = sessions_client(debug_socket, f"create_session:{cwd}")
                session_id = json.loads(out)["session_id"]
                sessions_client(debug_socket, "set_model:mock-model", timeout=30)
                sessions.append(UnixDebugSession(debug_socket, session_id, cwd))
            except Exception as exc:  # noqa: BLE001
                errors.append(f"create_session {index}: {exc}")
                break
        return daemon, sessions, errors

    return factory


# ---- stdio JSON-line harness (claude, codex, pi) -------------------------


class ClaudeSession:
    def __init__(self, binary: str, cwd: str, env: dict):
        self.handle = spawn(claude_session_argv(binary), cwd, env)
        self.tool_uses = 0

    def turn(self, prompt: str, timeout: float = 180.0) -> str | None:
        message = {"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": prompt}]}}
        assert self.handle.proc.stdin is not None
        try:
            self.handle.proc.stdin.write(json.dumps(message) + "\n")
            self.handle.proc.stdin.flush()
        except (BrokenPipeError, ValueError) as exc:
            return f"stdin closed: {exc}"
        assert self.handle.proc.stdout is not None
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.handle.proc.stdout.readline()
            if not line:
                return "process exited"
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("type") == "assistant":
                self.tool_uses += json.dumps(event).count('"tool_use"')
            if event.get("type") == "result":
                if event.get("is_error"):
                    return str(event.get("result"))[:200]
                return None
        return "timeout"

    def close(self) -> None:
        self.handle.close()


class CodexSession:
    def __init__(self, binary: str, cwd: str, env: dict, base_url: str):
        argv = [binary, *codex_provider_args(base_url), "-m", "mock-model", "-s", "read-only", "-c",
                'approval_policy="never"', "app-server"]
        self.handle = spawn(argv, cwd, env)
        self.next_id = 1
        self.request("initialize", {"clientInfo": {"name": "bench-compare", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "initialized"})
        result = self.request(
            "thread/start",
            {"cwd": cwd, "model": "mock-model", "approvalPolicy": "never", "sandbox": "read-only"},
        )
        self.thread_id = result["thread"]["id"]
        self.tool_uses = 0

    def _send(self, payload: dict) -> None:
        assert self.handle.proc.stdin is not None
        self.handle.proc.stdin.write(json.dumps(payload) + "\n")
        self.handle.proc.stdin.flush()

    def _read_until(self, predicate, timeout: float) -> tuple[str | None, object]:
        assert self.handle.proc.stdout is not None
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.handle.proc.stdout.readline()
            if not line:
                return "process exited", None
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            verdict = predicate(event)
            if verdict is not None:
                return None, verdict
        return "timeout", None

    def request(self, method: str, params: dict, timeout: float = 120.0) -> dict:
        request_id = self.next_id
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        box: dict = {}

        def predicate(event):
            if event.get("id") == request_id and "method" not in event:
                if "error" in event:
                    raise RuntimeError(f"codex {method}: {event['error']}")
                box.update(event.get("result") or {})
                return box
            return None

        error, value = self._read_until(predicate, timeout)
        if error:
            raise RuntimeError(f"codex {method}: {error}")
        return value or {}

    def turn(self, prompt: str, timeout: float = 180.0) -> str | None:
        request_id = self.next_id
        self.next_id += 1
        self._send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "turn/start",
                "params": {"threadId": self.thread_id, "input": [{"type": "text", "text": prompt}]},
            }
        )

        def predicate(event):
            method = event.get("method", "")
            if event.get("id") and method:
                self._send({"jsonrpc": "2.0", "id": event["id"], "result": {"decision": "decline"}})
                return None
            if method == "item/completed":
                item = (event.get("params") or {}).get("item") or {}
                if item.get("type") in ("commandExecution", "mcpToolCall", "fileChange", "webSearch"):
                    self.tool_uses += 1
            if method == "turn/completed":
                turn = (event.get("params") or {}).get("turn") or {}
                status = turn.get("status")
                if status in (None, "completed"):
                    return ""
                return str(turn.get("error") or status)[:200]
            return None

        error, value = self._read_until(predicate, timeout)
        if error:
            return error
        return value or None

    def close(self) -> None:
        self.handle.close()


class PiSession:
    def __init__(self, binary: str, cwd: str, env: dict, argv_fn=pi_session_argv):
        self.handle = spawn(argv_fn(binary), cwd, env)

    def turn(self, prompt: str, timeout: float = 180.0) -> str | None:
        assert self.handle.proc.stdin is not None
        self.handle.proc.stdin.write(json.dumps({"type": "prompt", "message": prompt}) + "\n")
        self.handle.proc.stdin.flush()
        assert self.handle.proc.stdout is not None
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.handle.proc.stdout.readline()
            if not line:
                return "process exited"
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("type") == "response" and event.get("success") is False:
                return str(event.get("error"))[:200]
            if event.get("type") == "agent_end":
                messages = event.get("messages") or []
                last = messages[-1] if messages else {}
                if last.get("stopReason") == "error" or last.get("errorMessage"):
                    return str(last.get("errorMessage") or "pi turn failed")[:200]
                return None
        return "timeout"

    def close(self) -> None:
        self.handle.close()


def measure_proc_sessions(make_session, session_count: int, turns: int, timeout: float) -> dict:
    sessions = []
    samples: list[dict] = []
    errors: list[str] = []
    try:
        for index in range(session_count):
            try:
                sessions.append(make_session())
            except Exception as exc:  # noqa: BLE001
                errors.append(f"session {index}: {exc}")
                break
        if not sessions:
            return {"sessions": session_count, "samples": samples, "errors": errors, "sessions_created": 0}
        time.sleep(2.0)
        roots = [s.handle.proc.pid for s in sessions]
        samples.append(
            {
                "phase": "idle",
                "rss_bytes": sum(bc.process_tree_rss_bytes(pid) for pid in roots),
                "pss_bytes": sum_of_pss(roots),
            }
        )
        turn_times: list[list[float]] = []
        for turn_index in range(turns):
            prompt = SESSION_TURNS[turn_index % len(SESSION_TURNS)]
            started = time.perf_counter()

            def make_turn(session):
                return lambda: session.turn(prompt, timeout=timeout)

            turn_errors = run_turns_parallel([make_turn(s) for s in sessions], timeout + 30)
            elapsed = (time.perf_counter() - started) * 1000
            turn_times.append([round(elapsed, 2)])
            for index, error in enumerate(turn_errors):
                if error:
                    errors.append(f"turn {turn_index + 1} session {index}: {error}")
            time.sleep(0.5)
            samples.append(
                {
                    "phase": f"turn{turn_index + 1}",
                    "rss_bytes": sum(bc.process_tree_rss_bytes(pid) for pid in roots),
                    "pss_bytes": sum_of_pss(roots),
                    "turn_ms": round(elapsed, 2),
                }
            )
        return {
            "sessions": session_count,
            "samples": samples,
            "turn_ms": turn_times,
            "errors": errors,
            "sessions_created": len(sessions),
        }
    finally:
        for session in sessions:
            try:
                session.close()
            except Exception:  # noqa: BLE001
                pass


def sum_of_pss(roots: list[int]) -> int | None:
    total = 0
    seen = False
    for pid in roots:
        value = bc.process_tree_pss_bytes(pid)
        if value is not None:
            total += value
            seen = True
    return total if seen else None


# ---- opencode HTTP harness ------------------------------------------------


def http_json(method: str, url: str, body: dict | None = None, timeout: float = 120.0):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method=method, headers={"content-type": "application/json"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        raw = response.read()
    return json.loads(raw) if raw else None


def measure_opencode_sessions(
    binary: str, root: str, cwd: str, session_count: int, turns: int, timeout: float, model_url: str
) -> dict:
    port = bc.free_port()
    base = f"http://127.0.0.1:{port}"
    # The provider config points at the mock model server; `base` is only where
    # opencode's own HTTP API listens for the benchmark's session calls.
    env = opencode_env(root, model_url)
    # A fresh XDG tree makes `opencode serve` accept a message and then finish
    # its step with no tokens and no HTTP request at all; the same config works
    # once the tool has made one successful model call. Warm the caches with the
    # `run` subcommand first so the session numbers measure per-session cost
    # rather than that first-call initialization (which `oneshot_ms` covers).
    warmup = subprocess.run(
        opencode_oneshot_argv(binary),
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=max(120.0, timeout),
        check=False,
    )
    if warmup.returncode != 0:
        raise RuntimeError(f"opencode warm-up failed: {(warmup.stderr or '').strip()[-200:]}")
    log = open(os.path.join(root, "serve.log"), "w", encoding="utf-8")
    proc = subprocess.Popen(
        [binary, "serve", "--port", str(port), "--hostname", "127.0.0.1"],
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        **bc.new_group_kwargs(),
    )
    pgid = bc.pgid_of(proc.pid)
    sessions: list[str] = []
    samples: list[dict] = []
    errors: list[str] = []
    try:
        time.sleep(4.0)
        deadline = time.time() + 60
        while True:
            try:
                http_json("GET", f"{base}/session", timeout=5)
                break
            except Exception:  # noqa: BLE001
                if time.time() > deadline:
                    raise RuntimeError("opencode server not ready")
                time.sleep(0.5)
        for index in range(session_count):
            try:
                sessions.append(http_json("POST", f"{base}/session", {"title": f"bench-{index}"})["id"])
            except Exception as exc:  # noqa: BLE001
                errors.append(f"session {index}: {exc}")
                break
        time.sleep(1.0)
        samples.append(
            {
                "phase": "idle",
                "rss_bytes": bc.process_tree_rss_bytes(proc.pid),
                "pss_bytes": bc.process_tree_pss_bytes(proc.pid),
            }
        )
        turn_times: list[list[float]] = []
        for turn_index in range(turns):
            prompt = SESSION_TURNS[turn_index % len(SESSION_TURNS)]
            started = time.perf_counter()

            def one(session_id: str):
                def run():
                    reply = http_json(
                        "POST",
                        f"{base}/session/{session_id}/message",
                        {
                            "parts": [{"type": "text", "text": prompt}],
                            "model": {"providerID": "openai", "modelID": "mock-model"},
                        },
                        timeout=timeout,
                    )
                    error = ((reply or {}).get("info") or {}).get("error")
                    if error:
                        return str(error)[:200]
                    return None

                return run

            turn_errors = run_turns_parallel([one(session_id) for session_id in sessions], timeout + 30)
            elapsed = (time.perf_counter() - started) * 1000
            turn_times.append([round(elapsed, 2)])
            for index, error in enumerate(turn_errors):
                if error:
                    errors.append(f"turn {turn_index + 1} session {index}: {error}")
            time.sleep(0.5)
            samples.append(
                {
                    "phase": f"turn{turn_index + 1}",
                    "rss_bytes": bc.process_tree_rss_bytes(proc.pid),
                    "pss_bytes": bc.process_tree_pss_bytes(proc.pid),
                    "turn_ms": round(elapsed, 2),
                }
            )
        return {
            "sessions": session_count,
            "samples": samples,
            "turn_ms": turn_times,
            "errors": errors,
            "sessions_created": len(sessions),
        }
    finally:
        bc.terminate_pgroup(pgid)
        try:
            log.close()
        except OSError:
            pass


# ---------------------------------------------------------------------------
# Tool plugins
# ---------------------------------------------------------------------------


class ToolRunner:
    """Everything the benchmark needs to know about one tool."""

    name = ""
    label = ""

    def __init__(self, binary: str, name: str | None = None, label: str | None = None):
        self.binary = binary
        # A second build of the same agent is its own row, so name and label are
        # per instance rather than per class.
        if name:
            self.name = name
        if label:
            self.label = label

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        raise NotImplementedError

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        raise NotImplementedError

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        """One headless prompt that loads and uses the fixture skill."""
        return unsupported(f"{self.label} has no skill-invocation route the harness knows")

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        """One headless prompt that starts the fixture MCP server and calls it."""
        return unsupported(f"{self.label} has no local stdio MCP support")

    def env_for_version(self, root: str) -> dict:
        raise NotImplementedError


class HoronRunner(ToolRunner):
    name = "horon"
    label = "horon"

    def env_for_version(self, root: str) -> dict:
        return horon_env(root, self.binary)

    def _root(self, root: str, mock: MockServer, run_mcp: bool = False) -> dict:
        # horon_env names root/home as HORON_HOME, where this config lands.
        write_horon_config(root, mock.base_url)
        env = horon_env(root, self.binary)
        # The one-shot and session rows keep MCP off so their numbers stay
        # comparable with docs/performance/RESULTS.md; only the MCP scenario
        # turns server startup into part of the measurement.
        env["HORON_AGENT_RUN_MCP"] = "1" if run_mcp else "0"
        return env

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = self._root(root, mock)
        return measure_oneshot(horon_oneshot_argv(self.binary), cwd, env, mock, timeout, syscalls=syscalls)

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        path = install_skill(os.path.join(cwd, ".horon", "skills"))
        # MCP is on for both new scenarios; with no server configured it costs
        # nothing, and the flag is what a real skill turn runs with.
        env = self._root(root, mock, run_mcp=True)
        argv = horon_turn_argv(self.binary, skill_prompt(path), "read")
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        env = self._root(root, mock, run_mcp=True)
        write_mcp_servers(os.path.join(env["HORON_HOME"], "mcp.json"), mcp_servers())
        # A cache-stable tool list exposes the fixed `mcp_call` surface rather
        # than the server's tool name, so that is what the allow-list names.
        argv = horon_turn_argv(self.binary, mcp_prompt(), "mcp_call")
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        self._root(root, mock)
        return measure_daemon_sessions(
            self.name,
            make_horon_daemon(self.binary, root, cwd),
            count,
            turns,
            cwd,
            timeout,
            probe=server_memory_probe(debug_socket_for(os.path.join(root, "run", "bench.sock"))),
            diagnostics=horon_memory_diagnostics(debug_socket_for(os.path.join(root, "run", "bench.sock"))),
        )


class JcodeRunner(ToolRunner):
    name = "jcode"
    label = "jcode"

    def env_for_version(self, root: str) -> dict:
        write_jcode_config(root, "http://127.0.0.1:1")
        return jcode_env(root, self.binary)

    def _root(self, root: str, mock: MockServer) -> dict:
        write_jcode_config(root, mock.base_url)
        return jcode_env(root, self.binary)

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = self._root(root, mock)
        return measure_oneshot(jcode_oneshot_argv(self.binary), cwd, env, mock, timeout, syscalls=syscalls)

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        path = install_skill(os.path.join(root, "home", "skills"))
        env = self._root(root, mock)
        argv = jcode_oneshot_argv(self.binary, skill_prompt(path), tools=None)
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        env = self._root(root, mock)
        # Both places jcode documents: the global home file and the project one.
        write_mcp_servers(os.path.join(root, "home", "mcp.json"), mcp_servers(), wrapper="mcpServers")
        write_mcp_servers(os.path.join(cwd, ".jcode", "mcp.json"), mcp_servers(), wrapper="mcpServers")
        argv = jcode_oneshot_argv(self.binary, mcp_prompt(), tools=None)
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        self._root(root, mock)
        return measure_daemon_sessions(
            self.name,
            make_jcode_daemon(self.binary, root, cwd),
            count,
            turns,
            cwd,
            timeout,
            probe=server_memory_probe(debug_socket_for(os.path.join(root, "run", "bench.sock"))),
        )


class ClaudeRunner(ToolRunner):
    name = "claude_code"
    label = "Claude Code"

    def env_for_version(self, root: str) -> dict:
        return claude_env(root, "http://127.0.0.1:1")

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = claude_env(root, mock.base_url)
        return measure_oneshot(claude_oneshot_argv(self.binary), cwd, env, mock, timeout, syscalls=syscalls)

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        path = install_skill(os.path.join(root, "home", ".claude", "skills"))
        env = claude_env(root, mock.base_url)
        return measure_oneshot(claude_oneshot_argv(self.binary, skill_prompt(path)), cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        # `--mcp-config` with `--strict-mcp-config` loads only the fixture
        # server. Those servers connect in the background by default, so the
        # first request would leave before the tool is known;
        # `MCP_CONNECTION_NONBLOCKING=0` is the documented way to make startup
        # wait for them (code.claude.com/docs/en/mcp, "Exempt a server from
        # deferral"). Tool search stays at its default: the prompt lists
        # `mcp__bench__bench_echo` as deferred and the model loads its schema
        # with `ToolSearch` (`select:<name>`) before calling it, which is the
        # route the mock takes, so the row includes that extra model request.
        config = os.path.join(root, "mcp.json")
        write_mcp_servers(config, mcp_servers(), wrapper="mcpServers")
        env = claude_env(root, mock.base_url)
        env["MCP_CONNECTION_NONBLOCKING"] = "0"
        argv = claude_oneshot_argv(self.binary, mcp_prompt())
        argv[1:1] = ["--mcp-config", config, "--strict-mcp-config"]
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        env = claude_env(root, mock.base_url)
        return measure_proc_sessions(lambda: ClaudeSession(self.binary, cwd, env), count, turns, timeout)


class CodexRunner(ToolRunner):
    name = "codex"
    label = "Codex CLI"

    def env_for_version(self, root: str) -> dict:
        return codex_env(root)

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = codex_env(root)
        return measure_oneshot(
            codex_oneshot_argv(self.binary, mock.base_url), cwd, env, mock, timeout, syscalls=syscalls
        )

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        # This Codex build has no file-read tool: it lists the skill in the
        # prompt and expects the shell tool to open the SKILL.md. The fixture
        # is a project skill (`<cwd>/.agents/skills`, where Codex's own tests
        # put one) rather than a `$CODEX_HOME/skills` one: the Windows sandbox
        # denies its shell reads under CODEX_HOME (`Access to the path ... is
        # denied`), while the working folder stays readable on every OS.
        path = install_skill(os.path.join(cwd, ".agents", "skills"))
        env = codex_env(root)
        argv = codex_oneshot_argv(self.binary, mock.base_url or "", skill_prompt(path))
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        # Codex reads stdio servers from `[mcp_servers.<name>]` in
        # $CODEX_HOME/config.toml and offers each one to the model as a
        # Responses namespace tool (`mcp__bench` holding `bench_echo`); the mock
        # calls it with the namespace in its own field, as the client expects.
        env = codex_env(root)
        write_codex_mcp_config(env["CODEX_HOME"])
        argv = codex_oneshot_argv(self.binary, mock.base_url or "", mcp_prompt())
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        env = codex_env(root)
        base_url = mock.base_url or ""
        return measure_proc_sessions(
            lambda: CodexSession(self.binary, cwd, env, base_url), count, turns, timeout
        )


class OpencodeRunner(ToolRunner):
    name = "opencode"
    label = "OpenCode"

    def env_for_version(self, root: str) -> dict:
        return opencode_env(root, "http://127.0.0.1:1")

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = opencode_env(root, mock.base_url or "")
        return measure_oneshot(opencode_oneshot_argv(self.binary), cwd, env, mock, timeout, syscalls=syscalls)

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        path = install_skill(os.path.join(root, "config", "opencode", "skills"))
        env = opencode_env(root, mock.base_url or "")
        return measure_oneshot(opencode_oneshot_argv(self.binary, skill_prompt(path)), cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        entry = {
            "type": "local",
            "command": [sys.executable, str(MCP_FIXTURE)],
            "enabled": True,
        }
        env = opencode_env(root, mock.base_url or "", mcp={MCP_SERVER_NAME: entry})
        return measure_oneshot(opencode_oneshot_argv(self.binary, mcp_prompt()), cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        return measure_opencode_sessions(
            self.binary, root, cwd, count, turns, timeout, mock.base_url or ""
        )


class PiRunner(ToolRunner):
    name = "pi"
    label = "pi"

    def env_for_version(self, root: str) -> dict:
        return pi_env(root, "http://127.0.0.1:1")

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = pi_env(root, mock.base_url or "")
        return measure_oneshot(pi_oneshot_argv(self.binary), cwd, env, mock, timeout, syscalls=syscalls)

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        path = install_skill(os.path.join(root, "pi", "skills"))
        env = pi_env(root, mock.base_url or "")
        return measure_oneshot(pi_oneshot_argv(self.binary, skill_prompt(path)), cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        env = pi_env(root, mock.base_url or "")
        # The default `codemode` exposure hides the tool behind a script tool;
        # `direct` declares `mcp__bench__bench_echo` to the model, and the first
        # prompt waits for a server with direct tools before it is sent.
        servers = {name: {**entry, "exposure": "direct"} for name, entry in mcp_servers().items()}
        write_mcp_servers(os.path.join(env["PI_CODING_AGENT_DIR"], "mcp.json"), servers, wrapper="mcpServers")
        return measure_oneshot(pi_oneshot_argv(self.binary, mcp_prompt(), mcp=True), cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        env = pi_env(root, mock.base_url or "")
        return measure_proc_sessions(lambda: PiSession(self.binary, cwd, env), count, turns, timeout)


class OmpRunner(ToolRunner):
    """oh-my-pi's omp: the pi protocol with its own agent directory and flags."""

    name = "omp"
    label = "oh-my-pi (omp)"

    def env_for_version(self, root: str) -> dict:
        return omp_env(root, "http://127.0.0.1:1")

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = omp_env(root, mock.base_url or "")
        return measure_oneshot(omp_oneshot_argv(self.binary), cwd, env, mock, timeout, syscalls=syscalls)

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        env = omp_env(root, mock.base_url or "")
        path = install_skill(os.path.join(env["PI_CODING_AGENT_DIR"], "skills"))
        return measure_oneshot(omp_oneshot_argv(self.binary, skill_prompt(path)), cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        env = omp_env(root, mock.base_url or "")
        # omp prints the MCP tool as an `xd://` route in the prompt instead of
        # exposing a function; the mock model reaches it with the write tool.
        write_mcp_servers(
            os.path.join(env["PI_CODING_AGENT_DIR"], "mcp.json"), mcp_servers(), wrapper="mcpServers"
        )
        return measure_oneshot(omp_oneshot_argv(self.binary, mcp_prompt()), cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        base_url = mock.base_url or ""
        # Each session gets its own agent dir: omp stores its model cache in a
        # SQLite database there, and concurrent processes over one file would
        # serialise on its lock instead of measuring what the session costs.
        return measure_proc_sessions(
            lambda: PiSession(self.binary, cwd, omp_env(root, base_url), omp_session_argv),
            count,
            turns,
            timeout,
        )


class OmoRunner(ToolRunner):
    """oh-my-openagent's omo: the same pi RPC protocol over the senpi engine."""

    name = "omo"
    label = "oh-my-openagent (omo)"

    def env_for_version(self, root: str) -> dict:
        return omo_env(root, "http://127.0.0.1:1")

    def oneshot(
        self, mock: MockServer, root: str, cwd: str, timeout: float, syscalls: bool = False
    ) -> dict:
        env = omo_env(root, mock.base_url or "")
        return measure_oneshot(omo_oneshot_argv(self.binary), cwd, env, mock, timeout, syscalls=syscalls)

    def skill(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        env = omo_env(root, mock.base_url or "")
        path = install_skill(os.path.join(env["PI_CODING_AGENT_DIR"], "skills"))
        argv = omo_oneshot_argv(self.binary, skill_prompt(path), tools="read")
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def mcp(self, mock: MockServer, root: str, cwd: str, timeout: float) -> dict:
        env = omo_env(root, mock.base_url or "")
        write_mcp_servers(
            os.path.join(env["PI_CODING_AGENT_DIR"], "mcp.json"), mcp_servers(), wrapper="mcpServers"
        )
        # `--tools` is a strict allowlist, and naming the MCP tool there is
        # fragile across builds (`mcp_<server>_<tool>`), so the default set is
        # used: it already includes every connected server's tools.
        argv = omo_oneshot_argv(self.binary, mcp_prompt(), tools=None)
        return measure_oneshot(argv, cwd, env, mock, timeout)

    def sessions(self, mock: MockServer, root: str, cwd: str, count: int, turns: int, timeout: float) -> dict:
        base_url = mock.base_url or ""
        return measure_proc_sessions(
            lambda: PiSession(self.binary, cwd, omo_env(root, base_url), omo_session_argv),
            count,
            turns,
            timeout,
        )


RUNNERS = {
    "horon": HoronRunner,
    "jcode": JcodeRunner,
    "claude_code": ClaudeRunner,
    "codex": CodexRunner,
    "opencode": OpencodeRunner,
    "pi": PiRunner,
    "omp": OmpRunner,
    "omo": OmoRunner,
}


def horon_kind(name: str) -> bool:
    """True for the candidate row and for extra builds of the same agent.

    `--bin horon-base=<path>` adds a second horon build as its own row; it uses
    the horon harness because it is the same program at another commit.
    """
    return name == "horon" or name.startswith("horon-")


def runner_class(name: str):
    """Runner for one row, or None for an unknown tool name."""
    if name in RUNNERS:
        return RUNNERS[name]
    return HoronRunner if horon_kind(name) else None


def row_label(name: str) -> str | None:
    """Extra horon builds are labelled by row name; other rows keep theirs."""
    return name if name != "horon" and horon_kind(name) else None


def order_tools(tools: list[str], overrides: dict[str, str]) -> list[str]:
    """Place every extra horon build immediately after the candidate row.

    A baseline build is measured right after the candidate so both rows come
    from one machine, one mock server, and one load level. Running the two
    builds in separate workflow runs would compare two runs instead. The user's
    order for the other tools is kept as given.
    """
    ordered = list(tools)
    if "horon" not in ordered:
        ordered.insert(0, "horon")
    extra = sorted(name for name in overrides if horon_kind(name) and name not in ordered)
    index = ordered.index("horon") + 1
    ordered[index:index] = extra
    return ordered


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", action="append", default=[], metavar="NAME=PATH",
                        help="tool binary override, repeatable")
    parser.add_argument("--sessions", type=int, nargs="+", default=[1, 5, 10],
                        help="concurrent session counts (default: 1 5 10)")
    parser.add_argument("--turns", type=int, default=3, help="turns per session (default: 3)")
    parser.add_argument("--version-runs", type=int, default=5, help="--version repeats (default: 5)")
    parser.add_argument("--oneshot-runs", type=int, default=3, help="one-shot prompt repeats (default: 3)")
    parser.add_argument("--tools", nargs="+", default=DEFAULT_TOOLS, metavar="TOOL",
                        help=f"tools to measure (default: {' '.join(DEFAULT_TOOLS)}); "
                             f"accepted names: {' '.join(RUNNERS)}")
    parser.add_argument("--scenarios", nargs="+", default=DEFAULT_SCENARIOS, metavar="SCENARIO",
                        help=f"scenarios to measure, comma or space separated "
                             f"(default: {','.join(DEFAULT_SCENARIOS)}); "
                             f"accepted names: {','.join(SCENARIO_NAMES)}")
    parser.add_argument("--timeout", type=float, default=180.0, help="per-turn/oneshot timeout seconds")
    parser.add_argument("--syscalls", action="store_true",
                        help="count fsync/fdatasync calls per one-shot with strace (Linux only; "
                             "recorded as skipped elsewhere)")
    parser.add_argument("--json-out")
    parser.add_argument("--markdown-out")
    parser.add_argument("--install-failures", metavar="PATH",
                        help="failures.txt from install_competitors.sh (`name<TAB>reason` lines); a tool "
                             "without a binary is skipped with that reason")
    return parser.parse_args(argv)


def parse_scenarios(values: list[str]) -> list[str]:
    """Normalize ``--scenarios`` and reject a name the harness cannot run.

    A typo silently measuring fewer scenarios would make a report look complete
    while missing a table, so an unknown name ends the run instead.
    """
    chosen: list[str] = []
    for value in values:
        for part in str(value).split(","):
            name = part.strip()
            if not name:
                continue
            if name not in SCENARIO_NAMES:
                raise SystemExit(
                    f"--scenarios expects a subset of {','.join(SCENARIO_NAMES)}, got {name!r}"
                )
            if name not in chosen:
                chosen.append(name)
    if not chosen:
        raise SystemExit(f"--scenarios needs at least one of {','.join(SCENARIO_NAMES)}")
    return chosen


def parse_bin_overrides(values: list[str]) -> dict[str, str]:
    overrides: dict[str, str] = {}
    for value in values:
        if "=" not in value:
            raise SystemExit(f"--bin expects NAME=PATH, got {value!r}")
        name, path = value.split("=", 1)
        overrides[name.strip()] = os.path.abspath(os.path.expanduser(path.strip()))
    return overrides


def read_install_failures(path: str | None) -> dict[str, str]:
    """Map each tool to the reason install_competitors.sh gave for not installing it."""
    if not path or not os.path.isfile(path):
        return {}
    reasons: dict[str, list[str]] = {}
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            name, sep, reason = line.rstrip("\r\n").partition("\t")
            name, reason = name.strip(), reason.strip()
            if sep and name and reason:
                reasons.setdefault(name, []).append(reason)
    return {name: "; ".join(items) for name, items in reasons.items()}


def missing_binary_reason(name: str, install_failures: dict[str, str]) -> str:
    reason = install_failures.get(name)
    if reason:
        return f"install failed: {reason}"
    return "binary not found (pass --bin name=path)"


def normalize_tools(names: list[str]) -> list[str]:
    """Put horon first and drop duplicate names.

    The workflow builds ``--tools horon <competitors>``, so horon arrives twice
    when the input still lists it; measuring the baseline twice would print two
    horon rows and halve the report's meaning.
    """
    tools: list[str] = []
    for name in names:
        if name not in tools:
            tools.append(name)
    if "horon" in tools:
        tools.remove("horon")
    return ["horon"] + tools


def plan_first_session(points: list[dict], key: str) -> float | None:
    for point in points:
        if point["sessions"] == 1 and not point.get("errors"):
            value = point.get(key)
            return float(value) if value else None
    return None


def slope_series(points: list[dict], key: str) -> float | None:
    """Least-squares slope over clean points only.

    A point whose turns timed out still records *some* RSS, but mixing that with
    a healthy point produces a meaningless "each additional session" figure,
    which reads as a real result. A negative slope means growth stayed inside
    RSS sampling noise (a downsized heap or a freed cache between samples), so
    it is reported as 0 rather than as a negative allocation.
    """
    xs, ys = [], []
    for point in points:
        if point.get("errors"):
            continue
        value = point.get(key)
        if value:
            xs.append(float(point["sessions"]))
            ys.append(float(value))
    slope = bc.linear_slope(xs, ys)
    if slope is None:
        return None
    return max(0.0, slope)


def summarize_sessions(points: list[dict], turns: int) -> dict:
    """Per-population summary: the first session's cost and each additional one.

    Memory slopes come from the RSS/PSS tree samples. Thread and descriptor
    slopes come from the daemon's own ``server:memory`` counters and stay
    ``None`` for a tool that does not report them; the reason is kept beside the
    null so a missing counter is not read as a zero-cost session.
    """
    skipped = sorted({point["counter_error"] for point in points if point.get("counter_error")})
    return {
        "turns": turns,
        "points": points,
        "first_session_idle_rss_bytes": plan_first_session(points, "idle_rss_bytes"),
        "each_additional_idle_rss_bytes": slope_series(points, "idle_rss_bytes"),
        "first_session_post_turn_rss_bytes": plan_first_session(points, "post_turn_rss_bytes"),
        "each_additional_post_turn_rss_bytes": slope_series(points, "post_turn_rss_bytes"),
        "first_session_idle_pss_bytes": plan_first_session(points, "idle_pss_bytes"),
        "each_additional_idle_pss_bytes": slope_series(points, "idle_pss_bytes"),
        "each_additional_idle_live_heap_bytes": slope_series(points, "idle_live_heap_bytes"),
        "each_additional_post_turn_live_heap_bytes": slope_series(points, "post_turn_live_heap_bytes"),
        "each_additional_post_turn_rss_anon_bytes": slope_series(points, "post_turn_rss_anon_bytes"),
        "each_additional_post_turn_rss_file_bytes": slope_series(points, "post_turn_rss_file_bytes"),
        "each_additional_post_turn_allocator_live_bytes": slope_series(points, "post_turn_allocator_live_bytes"),
        "each_additional_post_purge_rss_bytes": slope_series(points, "post_purge_rss_bytes"),
        "first_session_threads": plan_first_session(points, "thread_count"),
        "each_additional_threads": slope_series(points, "thread_count"),
        "first_session_fds": plan_first_session(points, "fd_count"),
        "each_additional_fds": slope_series(points, "fd_count"),
        "skipped": skipped,
    }


def summarize_oneshot(runs: list[dict], syscall_runs: list[dict] | None = None) -> dict:
    """Collapse repeated one-shot runs into one comparable row.

    ``ms`` is the median so a single slow first process (page cache, JIT warm
    up) does not set the headline number; the individual runs stay in the JSON so
    a spread is visible rather than hidden behind one statistic.

    ``syscall_runs`` are the separate strace-counted runs. Their times are not
    used: a tracer stops the tool on every system call, so a traced run's wall
    time grows with the tool's call count rather than with the work it does.
    """
    times = [run["ms"] for run in runs if run.get("ms") is not None]
    peaks = [run["peak_rss_bytes"] for run in runs if run.get("peak_rss_bytes")]
    summary = bc.summarize(times)
    errors = [run["error"] for run in runs if run.get("error")]
    roundtrips = [run.get("tool_roundtrip") for run in runs if run.get("tool_roundtrip") is not None]
    counted = runs if syscall_runs is None else syscall_runs
    syscall_totals = [run["fsync_calls"] for run in counted if run.get("fsync_calls") is not None]
    syscall_summary = bc.summarize(syscall_totals)
    return {
        "ms": summary["median"] if summary else None,
        "summary": summary,
        "peak_rss_bytes": max(peaks) if peaks else None,
        "tool_roundtrip": (all(roundtrips) if roundtrips else None),
        "mock_requests": [run.get("mock_requests") for run in runs],
        "fsync_calls": syscall_summary["median"] if syscall_summary else None,
        "fsync_calls_runs": syscall_totals,
        "fsync_calls_summary": syscall_summary,
        "errors": errors,
        "error": errors[0] if errors else None,
        "runs": runs,
        "syscall_runs": syscall_runs or [],
    }


def summarize_scenario(runs: list[dict]) -> dict:
    """Collapse the repeats of a skill or MCP scenario into one row.

    ``tool_round_trip`` is true only when every run saw the fixture sentinel:
    an agent that answers ``mock-ok`` without the body, or whose tool call the
    agent silently drops, must not be reported as a successful round trip.
    """
    times = [run["ms"] for run in runs if run.get("ms") is not None]
    peaks = [run["peak_rss_bytes"] for run in runs if run.get("peak_rss_bytes")]
    summary = bc.summarize(times)
    errors = [run["error"] for run in runs if run.get("error")]
    ready = [run["mcp_ready_ms"] for run in runs if run.get("mcp_ready_ms") is not None]
    ready_summary = bc.summarize(ready)
    vias = [run.get("via") for run in runs if run.get("via")]
    round_trip = bool(runs) and all(run.get("sentinel_seen") for run in runs)
    failure = None
    if not round_trip:
        errors = [run.get("error") for run in runs if run.get("error")]
        if errors:
            failure = str(errors[0])
        elif runs and all((run.get("mock_requests") or 0) <= 1 for run in runs):
            # The agent answered on its first request, so the fixture's tool was
            # never offered (or never called) and the numbers are startup only.
            failure = "no tool that could reach the fixture was offered (one request, no tool call)"
        else:
            failure = "the fixture sentinel never came back"
    return {
        "ms": summary["median"] if summary else None,
        "summary": summary,
        "peak_rss_bytes": max(peaks) if peaks else None,
        "tool_round_trip": round_trip,
        "status": "ok" if round_trip else "failed",
        "failure": failure,
        "mcp_ready_ms": ready_summary["median"] if ready_summary else None,
        "mcp_ready_summary": ready_summary,
        "via": vias[-1] if vias else None,
        "mock_requests": [run.get("mock_requests") for run in runs],
        "errors": errors,
        "error": errors[0] if errors else None,
        "runs": runs,
    }


def unsupported(reason: str) -> dict:
    """A scenario this tool cannot run, with the reason kept in the report."""
    return {"skipped": reason}


def _prepare_measure_root(runner: ToolRunner) -> tuple[str, str]:
    # Unix socket paths are length-limited, so the prefix stays short and the
    # row name contributes only its alphanumerics.
    tag = "".join(ch for ch in runner.name if ch.isalnum())[:8]
    root = bc.short_temp_dir(f"hcmp-{tag}-")
    cwd = os.path.join(root, "ws")
    os.makedirs(cwd, exist_ok=True)
    with open(os.path.join(cwd, "sample.txt"), "w", encoding="utf-8") as fh:
        fh.write("benchmark fixture\n")
    return root, cwd


def _prepare_scenario_root(tool_root: str, scenario: str, index: int) -> tuple[str, str]:
    """A fresh home and workspace for one scenario run.

    A reused home would let an MCP schema cache or a compressed skill hint from
    an earlier run skip the startup work the scenario is meant to measure.
    """
    root = os.path.join(tool_root, scenario, f"run{index + 1}")
    cwd = os.path.join(root, "ws")
    os.makedirs(cwd, exist_ok=True)
    return root, cwd


def _empty_result(runner: ToolRunner) -> dict:
    return {
        "name": runner.name,
        "label": runner.label,
        "binary": runner.binary,
        "version": None,
        "version_ms": None,
        "oneshot": None,
        "sessions_memory": None,
        "skill": None,
        "mcp": None,
        "install_size_bytes": None,
        "error": None,
    }


def _measure_metadata(runner: ToolRunner, result: dict, root: str, version_runs: int) -> None:
    """Version string, `--version` timing, and install size for one row."""
    version_env = runner.env_for_version(root)
    result["version"] = tool_version(runner.binary, version_env)
    result["version_ms"] = measure_version([runner.binary, "--version"], version_env, version_runs)
    size, size_error = measure_install_size(runner.binary)
    result["install_size_bytes"] = size
    if size_error:
        result["install_size_error"] = size_error


def _measure_session_point(runner: ToolRunner, mock: MockServer, root: str, cwd: str,
                           count: int, turns: int, timeout: float) -> dict:
    """One session-count sample for one row."""
    mock.reset()
    try:
        measurement = runner.sessions(mock, root, cwd, count, turns, timeout)
    except Exception as exc:  # noqa: BLE001
        return {"sessions": count, "error": str(exc)[:300], "samples": [], "errors": [str(exc)[:300]]}
    idle = next((s for s in measurement["samples"] if s["phase"] == "idle"), None)
    post = measurement["samples"][-1] if measurement["samples"] else None
    errors = list(measurement.get("errors") or [])
    created = measurement.get("sessions_created")
    if created is not None and created != count:
        errors.append(f"only {created}/{count} sessions were created")
    return {
        "sessions": count,
        "idle_rss_bytes": (idle or {}).get("rss_bytes"),
        "idle_pss_bytes": (idle or {}).get("pss_bytes"),
        "post_turn_rss_bytes": (post or {}).get("rss_bytes"),
        "post_turn_pss_bytes": (post or {}).get("pss_bytes"),
        "idle_live_heap_bytes": (idle or {}).get("live_heap_bytes"),
        "post_turn_live_heap_bytes": (post or {}).get("live_heap_bytes"),
        "thread_count": (post or {}).get("thread_count"),
        "fd_count": (post or {}).get("fd_count"),
        "counter_error": measurement.get("counter_error"),
        "samples": measurement["samples"],
        "turn_ms": measurement.get("turn_ms"),
        "errors": errors,
        "sessions_created": created,
        "thread_names": measurement.get("thread_names"),
        "post_turn_rss_anon_bytes": (post or {}).get("rss_anon_bytes"),
        "post_turn_rss_file_bytes": (post or {}).get("rss_file_bytes"),
        "post_turn_allocator_live_bytes": (post or {}).get("allocator_live_bytes"),
        "post_purge_rss_bytes": (post or {}).get("post_purge_rss_bytes"),
        "diagnostic_error": (post or {}).get("allocator_error") or (post or {}).get("purge_error"),
    }


def row_groups(runners: list[ToolRunner]) -> list[list[ToolRunner]]:
    """Measurement groups: every horon build together, then one row each.

    A before/after pair has to share one machine, one mock server, and one load
    level, so all horon builds are measured as one interleaved group. A single
    horon build is the same code path with a group of one, and a competitor is
    never interleaved with anything because it is a separate tool.
    """
    horon_rows = [runner for runner in runners if horon_kind(runner.name)]
    other_rows = [runner for runner in runners if not horon_kind(runner.name)]
    return ([horon_rows] if horon_rows else []) + [[runner] for runner in other_rows]


def measure_tools(
    runners: list[ToolRunner],
    mock: MockServer,
    session_counts: list[int],
    turns: int,
    version_runs: int,
    oneshot_runs: int,
    timeout: float,
    syscalls: bool = False,
    scenarios: list[str] | None = None,
) -> list[dict]:
    """Measure one or more rows, alternating them phase by phase.

    A single row is the plain case. Two horon builds pass together: every
    one-shot repeat and every session count visits both rows before moving on,
    so a load change on the shared runner lands on both sides of the pair
    instead of on whichever row ran second.

    ``scenarios`` selects which phases run; a phase that is not selected is left
    as ``None`` on every row, so a subset run really does skip it.
    """
    selected = set(scenarios if scenarios is not None else SCENARIO_NAMES)
    results = [_empty_result(runner) for runner in runners]
    roots: dict[str, tuple[str, str]] = {}
    try:
        for runner, result in zip(runners, results):
            try:
                root, cwd = _prepare_measure_root(runner)
                roots[runner.name] = (root, cwd)
                _measure_metadata(runner, result, root, version_runs)
            except Exception as exc:  # noqa: BLE001
                # A row that cannot even be set up records the reason and is
                # left out of the phases below, instead of failing the run.
                result["error"] = str(exc)[:400]

        if "oneshot" in selected:
            oneshot_runs_by_row: list[list[dict]] = [[] for _ in runners]
            syscall_runs_by_row: list[list[dict]] = [[] for _ in runners]
            # The timed runs never carry a tracer; with ``syscalls`` the same
            # number of extra runs is made under strace only to count fsync.
            passes = [(oneshot_runs_by_row, False)]
            if syscalls:
                passes.append((syscall_runs_by_row, True))
            for by_row, traced in passes:
                for _ in range(max(1, oneshot_runs)):
                    for index, runner in enumerate(runners):
                        if results[index]["error"]:
                            continue
                        root, cwd = roots[runner.name]
                        try:
                            by_row[index].append(runner.oneshot(mock, root, cwd, timeout, syscalls=traced))
                        except Exception as exc:  # noqa: BLE001
                            by_row[index].append(
                                {"ms": None, "peak_rss_bytes": None, "tool_roundtrip": None,
                                 "error": str(exc)[:300]}
                            )
            for index, result in enumerate(results):
                if not result["error"]:
                    result["oneshot"] = summarize_oneshot(
                        oneshot_runs_by_row[index], syscall_runs_by_row[index] if syscalls else None
                    )

        if "sessions" in selected:
            points_by_row: list[list[dict]] = [[] for _ in runners]
            for count in session_counts:
                for index, runner in enumerate(runners):
                    if results[index]["error"]:
                        continue
                    root, cwd = roots[runner.name]
                    points_by_row[index].append(
                        _measure_session_point(runner, mock, root, cwd, count, turns, timeout)
                    )
            for index, result in enumerate(results):
                if not result["error"]:
                    result["sessions_memory"] = summarize_sessions(points_by_row[index], turns)

        for scenario in ("skill", "mcp"):
            if scenario not in selected:
                continue
            runs_by_row: list[list[dict]] = [[] for _ in runners]
            for repeat in range(max(1, oneshot_runs)):
                for index, runner in enumerate(runners):
                    result = results[index]
                    if result["error"] or result.get(scenario) is not None:
                        # A broken row has nothing to measure, and a scenario
                        # the tool cannot run is decided on its first attempt.
                        continue
                    tool_root, _tool_cwd = roots[runner.name]
                    scenario_root, scenario_cwd = _prepare_scenario_root(
                        tool_root, scenario, repeat
                    )
                    try:
                        run = getattr(runner, scenario)(mock, scenario_root, scenario_cwd, timeout)
                    except Exception as exc:  # noqa: BLE001
                        run = {"ms": None, "peak_rss_bytes": None, "sentinel_seen": False,
                               "error": str(exc)[:300]}
                    if "skipped" in run:
                        result[scenario] = dict(run)
                        continue
                    runs_by_row[index].append(run)
            for index, result in enumerate(results):
                if result["error"] or result.get(scenario) is not None:
                    continue
                result[scenario] = summarize_scenario(runs_by_row[index])
    finally:
        for root, _cwd in roots.values():
            shutil.rmtree(root, ignore_errors=True)
    return results


# ---------------------------------------------------------------------------
# Report rendering
# ---------------------------------------------------------------------------


def baseline_value(results: list[dict], path: tuple[str, ...]):
    for result in results:
        if result["name"] != "horon":
            continue
        value = result
        for key in path:
            if not isinstance(value, dict):
                return None
            value = value.get(key)
        return value if isinstance(value, (int, float)) else None
    return None


def relative(value, baseline, lower_is_better: bool = True) -> str:
    if value is None or baseline in (None, 0):
        return "—"
    ratio = value / baseline
    return bc.fmt_ratio(ratio, better_is_lower=lower_is_better)


def fmt_calls(value) -> str:
    """Call counts are integers; a missing sample stays visible as an em dash."""
    return str(round(value)) if value is not None else "—"


def fmt_slope(value) -> str:
    """Per-session count slope; two decimals keep a sub-1 slope visible."""
    return f"{value:.2f}" if isinstance(value, (int, float)) else "—"


def scenario_cell(text: str, limit: int = 90) -> str:
    """One markdown cell: no pipe breaks a table row, and reasons stay short."""
    cleaned = " ".join(str(text).split()).replace("|", "/")
    return cleaned[:limit]


def scenario_row(result: dict, scenario: str, results: list[dict], with_ready: bool = False) -> list[str]:
    """One row of the skill or MCP table: a time, or the reason it has none."""
    data = result.get(scenario)
    label = result["label"]
    if data is None:
        reason = result.get("error") or "not measured in this run"
        cells = [label, "—", "", "—", "—", "—", "—", "—"]
        if with_ready:
            cells.append("—")
        return cells + [scenario_cell(reason)]
    if "skipped" in data:
        cells = [label, "—", "", "—", "—", "—", "—", "—"]
        if with_ready:
            cells.append("—")
        return cells + [scenario_cell(f"skipped: {data['skipped']}")]
    ms = data.get("ms")
    summary = data.get("summary") or {}
    spread = ""
    if summary.get("samples", 0) > 1:
        spread = f"{bc.fmt_ms(summary['min'])}–{bc.fmt_ms(summary['max'])}"
    rss = data.get("peak_rss_bytes")
    roundtrip = data.get("tool_round_trip")
    notes = data.get("errors") or ([data["error"]] if data.get("error") else [])
    if not notes and data.get("tool_round_trip") is False:
        notes = ["the fixture sentinel never came back"]
    if data.get("status") == "failed" or data.get("tool_round_trip") is False:
        # A run whose round trip failed measured the wrong thing, so its time
        # and memory are not comparable and are left out of the table.
        cells = [label, "—", "", "—", "—", "—", data.get("via") or "—", "no"]
        if with_ready:
            cells.append("—")
        reason = data.get("failure") or (notes[0] if notes else "the round trip failed")
        return cells + [scenario_cell(f"failed: {reason}")]
    note = scenario_cell(notes[0]) if notes else ""
    cells = [
        label,
        bc.fmt_ms(ms),
        spread,
        relative(ms, baseline_value(results, (scenario, "ms")))
        if result["name"] != "horon"
        else "baseline",
        bc.fmt_bytes(rss),
        relative(rss, baseline_value(results, (scenario, "peak_rss_bytes")))
        if result["name"] != "horon"
        else "baseline",
        data.get("via") or "—",
        "yes" if roundtrip else ("no" if roundtrip is False else "—"),
    ]
    if with_ready:
        cells.append(bc.fmt_ms(data.get("mcp_ready_ms")))
    return cells + [note]


def render_markdown(
    report: dict, session_counts: list[int], turns: int, scenarios: list[str] | None = None
) -> str:
    results = report["results"]
    if scenarios is None:
        selected = (report.get("parameters") or {}).get("scenarios")
        # A hand-built report (or one from before --scenarios existed) shows
        # every table; only an explicit selection hides a phase.
        scenarios = list(selected) if selected is not None else list(SCENARIO_NAMES)
    lines = ["# Coding agent comparison (headless, mock model)", ""]
    host = report["host"]
    lines.append(
        f"Host: {host['os']}/{host['arch']}, {host['cpu_count']} CPUs, Python {host['python']}. "
        f"Baseline: **horon**."
    )
    lines.append("")
    horon_rows = [result["label"] for result in results if horon_kind(result["name"])]
    if len(horon_rows) > 1:
        lines.append(
            "Extra horon builds (" + ", ".join(f"`{label}`" for label in horon_rows[1:]) + ") "
            "were measured in this same job, alternating with `horon` phase by phase, so the "
            "ratio between them is a same-machine before/after rather than two runs. `horon` is "
            "still the 1.0× baseline, so an extra build reading \"more\" than 1.0× took more time "
            "or memory than the candidate."
        )
        lines.append("")

    skipped = report.get("skipped") or []
    missing = missing_tools(report)
    if missing:
        lines.append(
            f"> **Warning: {len(missing)} tool(s) were not measured**, so this report is "
            "missing rows: " + ", ".join(f"{s['name']} ({s['reason']})" for s in missing)
        )
        lines.append("")
    if skipped:
        lines.append("Skipped: " + ", ".join(f"{s['name']} ({s['reason']})" for s in skipped))
        lines.append("")

    # Version / binary
    rows = []
    for result in results:
        # A tool that could not be executed at all has version_ms.summary == None;
        # that is a missing number, not a reason to fail the whole report.
        version_median = ((result.get("version_ms") or {}).get("summary") or {}).get("median")
        rows.append(
            [
                result["label"],
                result.get("version") or "—",
                bc.fmt_ms(version_median) if version_median else "—",
                bc.fmt_bytes(result.get("install_size_bytes")),
            ]
        )
    lines.append(bc.markdown_table(["Tool", "Version", "`--version` (median)", "Install size"], rows))
    lines.append("")

    if "oneshot" in scenarios:
        # One-shot
        base_ms = baseline_value(results, ("oneshot", "ms"))
        base_rss = baseline_value(results, ("oneshot", "peak_rss_bytes"))
        show_syscalls = any((r.get("oneshot") or {}).get("fsync_calls") is not None for r in results)
        rows = []
        for result in results:
            oneshot = result.get("oneshot") or {}
            ms = oneshot.get("ms")
            rss = oneshot.get("peak_rss_bytes")
            roundtrip = oneshot.get("tool_roundtrip")
            notes = oneshot.get("errors") or ([oneshot["error"]] if oneshot.get("error") else [])
            note = notes[0] if notes else ""
            summary = oneshot.get("summary") or {}
            spread = ""
            if summary.get("samples", 0) > 1:
                spread = f"{bc.fmt_ms(summary['min'])}–{bc.fmt_ms(summary['max'])}"
            row = [
                result["label"],
                bc.fmt_ms(ms),
                spread,
                relative(ms, base_ms) if result["name"] != "horon" else "baseline",
                bc.fmt_bytes(rss),
                relative(rss, base_rss) if result["name"] != "horon" else "baseline",
                "yes" if roundtrip else ("no" if roundtrip is False else "—"),
            ]
            if show_syscalls:
                row.append(fmt_calls(oneshot.get("fsync_calls")))
            row.append(note[:60])
            rows.append(row)
        lines.append("**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)")
        lines.append("")
        lines.append(
            bc.markdown_table(
                ["Tool", "Time (median)", "Min–max", "vs horon", "Peak tree RSS", "vs horon",
                 "Tool round-trip"]
                + (["fsync calls"] if show_syscalls else [])
                + ["Note"],
                rows,
            )
        )
        lines.append("")

    # Skill invocation
    if "skill" in scenarios:
        lines.append(
            "**Skill invocation** (process start to final answer, including skill discovery and "
            "one load of the fixture skill)"
        )
        lines.append("")
        lines.append(
            bc.markdown_table(
                ["Tool", "Time (median)", "Min–max", "vs horon", "Peak tree RSS", "vs horon",
                 "Via", "Tool round-trip", "Note"],
                [scenario_row(result, "skill", results) for result in results],
            )
        )
        lines.append("")

    # MCP tool call
    if "mcp" in scenarios:
        lines.append(
            "**MCP tool call** (process start to final answer, including starting the fixture "
            "stdio MCP server and one `bench_echo` call)"
        )
        lines.append("")
        lines.append(
            bc.markdown_table(
                ["Tool", "Time (median)", "Min–max", "vs horon", "Peak tree RSS", "vs horon",
                 "Via", "Tool round-trip", "MCP ready", "Note"],
                [scenario_row(result, "mcp", results, with_ready=True) for result in results],
            )
        )
        lines.append("")

    if "sessions" in scenarios:
        # Sessions
        base_additional = baseline_value(results, ("sessions_memory", "each_additional_idle_rss_bytes"))
        rows = []
        for result in results:
            memory = result.get("sessions_memory") or {}
            first = memory.get("first_session_idle_rss_bytes")
            additional = memory.get("each_additional_idle_rss_bytes")
            post_first = memory.get("first_session_post_turn_rss_bytes")
            post_additional = memory.get("each_additional_post_turn_rss_bytes")
            rows.append(
                [
                    result["label"],
                    bc.fmt_bytes(first),
                    bc.fmt_bytes(additional),
                    relative(additional, base_additional) if result["name"] != "horon" else "baseline",
                    fmt_slope(memory.get("each_additional_threads")),
                    fmt_slope(memory.get("each_additional_fds")),
                    bc.fmt_bytes(post_first),
                    bc.fmt_bytes(post_additional),
                ]
            )
        lines.append(
            f"**Session memory** (idle after {turns} turn(s) per session; sizes are process-tree sums, "
            f"RSS; PSS also recorded on Linux). \"Each additional\" is the least-squares slope across the "
            f"measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and "
            f"descriptors come from the daemon's own `server:memory` after the turns."
        )
        lines.append("")
        lines.append(
            bc.markdown_table(
                ["Tool", "First session", "Each additional", "vs horon (additional)", "Threads/session",
                 "FDs/session", "First (post-turn)", "Each additional (post-turn)"],
                rows,
            )
        )
        lines.append("")
        counter_skips = []
        for result in results:
            reasons = (result.get("sessions_memory") or {}).get("skipped") or []
            if reasons:
                counter_skips.append(f"{result['label']} ({'; '.join(reasons)})")
        if counter_skips:
            lines.append("Threads/descriptors unavailable: " + ", ".join(counter_skips))
            lines.append("")

        # Per-count detail
        rows = []
        for result in results:
            memory = result.get("sessions_memory") or {}
            by_count = {p["sessions"]: p for p in memory.get("points") or []}
            row = [result["label"]]
            for count in session_counts:
                point = by_count.get(count) or {}
                value = point.get("post_turn_rss_bytes")
                row.append(bc.fmt_bytes(value))
            rows.append(row)
        lines.append("**Post-turn tree RSS per session count**")
        lines.append("")
        lines.append(bc.markdown_table(["Tool"] + [f"{n} session(s)" for n in session_counts], rows))
        lines.append("")

    errors = []
    for result in results:
        if result.get("error"):
            errors.append(f"- {result['label']}: {result['error']}")
        for point in (result.get("sessions_memory") or {}).get("points") or []:
            for error in point.get("errors") or []:
                errors.append(f"- {result['label']} @ {point['sessions']} sessions: {error}")
    if errors:
        lines.append("**Errors recorded during measurement**")
        lines.append("")
        lines.extend(errors[:20])
        lines.append("")
    return "\n".join(lines)


# Skip entries that name a measurement feature rather than a tool row.
NON_TOOL_SKIPS = frozenset({"syscalls"})


def missing_tools(report: dict) -> list[dict]:
    """Skipped entries that are tools, in report order."""
    return [s for s in report.get("skipped") or [] if s.get("name") not in NON_TOOL_SKIPS]


def _annotation_escape(text: str) -> str:
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def github_warnings(missing: list[dict]) -> list[str]:
    """One GitHub Actions `::warning::` command per tool that was not measured."""
    return [
        f"::warning title=compare: {_annotation_escape(s['name'])} not measured::"
        f"{_annotation_escape(s['name'])}: {_annotation_escape(s['reason'])}"
        for s in missing
    ]


def announce_missing_tools(missing: list[dict], env: dict) -> None:
    """Make a missing tool loud on a CI run: an annotation and a step-summary line.

    The report already names the reason, but a report nobody opens hides a
    dropped competitor; the run page shows annotations without a download.
    """
    if not missing or env.get("GITHUB_ACTIONS") != "true":
        return
    for line in github_warnings(missing):
        print(line, flush=True)
    summary = env.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(f"### compare: {len(missing)} tool(s) not measured\n\n")
            for s in missing:
                handle.write(f"- `{s['name']}`: {s['reason']}\n")
            handle.write("\n")


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    scenarios = parse_scenarios(args.scenarios)
    overrides = parse_bin_overrides(args.bin)
    install_failures = read_install_failures(args.install_failures)
    started_at = bc.utc_now_iso()
    results: list[dict] = []
    skipped: list[dict] = []

    syscalls = args.syscalls and strace_available()
    if args.syscalls and not syscalls:
        skipped.append({"name": "syscalls", "reason": "strace not available"})

    args.tools = order_tools(normalize_tools(args.tools), overrides)

    runners: list[ToolRunner] = []
    for name in args.tools:
        runner_cls = runner_class(name)
        if runner_cls is None:
            skipped.append({"name": name, "reason": "unknown tool"})
            continue
        binary = resolve_bin(name, overrides.get(name), os.environ)
        if not binary:
            skipped.append({"name": name, "reason": missing_binary_reason(name, install_failures)})
            continue
        runners.append(runner_cls(binary, name=name, label=row_label(name)))

    by_name: dict[str, dict] = {}
    mock = MockServer().start()
    try:
        for group in row_groups(runners):
            names = ", ".join(runner.name for runner in group)
            bc.eprint(f"[bench_compare] measuring {names}")
            for result in measure_tools(
                group,
                mock,
                args.sessions,
                args.turns,
                args.version_runs,
                args.oneshot_runs,
                args.timeout,
                syscalls=syscalls,
                scenarios=scenarios,
            ):
                by_name[result["name"]] = result
            bc.eprint(f"[bench_compare] finished {names}")
    finally:
        mock.stop()
    results = [by_name[runner.name] for runner in runners if runner.name in by_name]

    report = bc.build_report(SUITE, results, skipped, started_at)
    report["parameters"] = {
        "sessions": args.sessions,
        "turns": args.turns,
        "version_runs": args.version_runs,
        "oneshot_runs": args.oneshot_runs,
        "timeout_s": args.timeout,
        "syscalls": syscalls,
        "scenarios": scenarios,
        # Which rows this report contains and where each binary came from, so a
        # before/after pair can be traced back to its two commits.
        "rows": [{"name": result["name"], "binary": result["binary"]} for result in results],
    }
    markdown = render_markdown(report, args.sessions, args.turns, scenarios)
    if args.json_out:
        bc.write_json(args.json_out, report)
        bc.eprint(f"[bench_compare] wrote {args.json_out}")
    if args.markdown_out:
        bc.write_markdown(args.markdown_out, markdown)
        bc.eprint(f"[bench_compare] wrote {args.markdown_out}")
    if not args.json_out and not args.markdown_out:
        print(markdown)
    announce_missing_tools(missing_tools(report), os.environ)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
