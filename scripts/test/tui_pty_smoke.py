#!/usr/bin/env python3
"""End-to-end smoke test for the bare `horon` command and the `horon-tui` client.

The bare command hands a terminal launch over to the sibling `horon-tui` binary.
Nothing else in CI exercises that handoff, the terminal client's screen, or the
approval round trip, so this script drives the real binaries through a real PTY
against the mock model server:

1. happy path: `horon` with no arguments replaces itself with `horon-tui`, draws
   its first screen, streams a reply to `ping`, is refused a tool call, answers
   the approval prompt with `y`, and the model's retry gets through; `/quit`
   exits 0.
2. a piped stdin keeps the headless message, its install hint, and exit 1.
3. a missing `horon-tui` does the same on a terminal.
4. `HORON_NO_TUI=1` does the same on a terminal.

Linux and macOS only: the script needs `pty` plus the POSIX executable bit.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import tempfile
import time
import traceback

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MOCK_SERVER = os.path.join(REPO_ROOT, "scripts", "perf", "mock_llm_server.py")

# How long a single expected screen costs before the run fails. The first draw
# waits on the daemon, the provider probe, and the harness bridge, so it gets the
# larger budget; keystroke round trips are local.
STARTUP_TIMEOUT = 90.0
STEP_TIMEOUT = 45.0

HEADLESS_MESSAGE = "the interactive client is unavailable in this headless build"
INSTALL_HINT = "horon-tui"
FIRST_SCREEN = "Send a message to start"
REPLY_TEXT = "mock-ok"
APPROVAL_PROMPT = "approval needed"
TOOL_DONE = "done"


class Failure(RuntimeError):
    """A step that did not show up on screen."""


def log(message: str) -> None:
    print(f"[tui-pty] {message}", flush=True)


# ---------------------------------------------------------------------------
# Mock model server
# ---------------------------------------------------------------------------


def start_mock_server(retry_denied: bool) -> tuple[subprocess.Popen, str]:
    argv = [sys.executable, MOCK_SERVER]
    if retry_denied:
        argv.append("--retry-denied-tool")
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    assert process.stdout is not None
    line = process.stdout.readline()
    if not line:
        stderr = process.stderr.read() if process.stderr else ""
        raise Failure(f"mock server printed no address: {stderr.strip()}")
    payload = json.loads(line)
    log(f"mock server on {payload['url']}")
    return process, payload["url"]


# ---------------------------------------------------------------------------
# Isolated home
# ---------------------------------------------------------------------------


def write_config(root: str, base_url: str) -> str:
    """An isolated HORON_HOME whose only provider is the mock server.

    `approval_mode = "write"` is what makes the first tool call ask: the turn
    types `ls`, the write-classed shell call parks for approval, and the `y`
    keystroke is the answer the round-trip assertion depends on.
    """
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

[tools]
approval_mode = "write"
"""
    with open(os.path.join(home, "config.toml"), "w", encoding="utf-8") as handle:
        handle.write(config)
    return home


def install_binaries(root: str, agent: str, tui: str | None) -> str:
    """A bin directory holding the two binaries under their shipped names.

    The handoff looks for `horon-tui` beside `current_exe`, so the pair must sit
    in one directory with exactly those names regardless of the build layout.
    """
    bin_dir = os.path.join(root, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    suffix = ".exe" if os.name == "nt" else ""
    shutil.copy2(agent, os.path.join(bin_dir, f"horon-agent{suffix}"))
    if tui is not None:
        shutil.copy2(tui, os.path.join(bin_dir, f"horon-tui{suffix}"))
    os.chmod(os.path.join(bin_dir, f"horon-agent{suffix}"), 0o755)
    return bin_dir


def horon_env(root: str, home: str) -> dict:
    """The parent env with every horon variable dropped, plus isolated paths."""
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("HORON_")
    }
    env.update(
        {
            "HORON_HOME": home,
            "HORON_AGENT_RUNTIME_DIR": os.path.join(root, "run"),
            "HORON_AGENT_SOCKET": os.path.join(root, "run", "horon-agent.sock"),
            "HORON_AGENT_API_SOCKET": os.path.join(root, "run", "horon-agent-api.sock"),
            "HORON_AGENT_NO_UPDATE": "1",
            "HORON_AGENT_RUN_MCP": "0",
            "HORON_AGENT_RUN_AUTO_POKE": "0",
            "HORON_AGENT_CLI_BOOTSTRAP_LOGIN": "0",
            "NO_COLOR": "1",
            "TERM": "xterm-256color",
        }
    )
    os.makedirs(env["HORON_AGENT_RUNTIME_DIR"], exist_ok=True)
    return env


# ---------------------------------------------------------------------------
# PTY driver
# ---------------------------------------------------------------------------


class Terminal:
    """A child on a real PTY, with a transcript for failure reports."""

    def __init__(self, argv: list[str], cwd: str, env: dict) -> None:
        self.argv = argv
        self.transcript = ""
        self.master, slave = pty.openpty()
        # A fresh PTY reports a 0x0 window, and a full-screen client draws
        # nothing into a zero area, so the size is set before the child starts.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        self.process = subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave)

    def read_until(self, needle: str, timeout: float, what: str) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.transcript:
                return
            readable, _, _ = select.select([self.master], [], [], 0.1)
            if readable:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                self.transcript += chunk.decode("utf-8", "replace")
        if needle not in self.transcript:
            raise Failure(f"{what}: never saw {needle!r}\n--- transcript ---\n{self.transcript}")

    def send(self, keys: str) -> None:
        os.write(self.master, keys.encode())

    def wait(self, timeout: float) -> int:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            readable, _, _ = select.select([self.master], [], [], 0.1)
            if readable:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    break
                if chunk:
                    self.transcript += chunk.decode("utf-8", "replace")
            code = self.process.poll()
            if code is not None:
                # Drain what the child wrote before exiting.
                while True:
                    readable, _, _ = select.select([self.master], [], [], 0.2)
                    if not readable:
                        break
                    try:
                        chunk = os.read(self.master, 65536)
                    except OSError:
                        break
                    if not chunk:
                        break
                    self.transcript += chunk.decode("utf-8", "replace")
                return code
            if code is None and self.transcript and time.monotonic() >= deadline:
                break
        self.process.kill()
        raise Failure(f"process did not exit within {timeout}s\n--- transcript ---\n{self.transcript}")

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
            except OSError:
                self.process.kill()
            self.process.wait(timeout=10)
        try:
            os.close(self.master)
        except OSError:
            pass


# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------


def happy_path(agent: str, tui: str, root: str, base_url: str) -> str:
    home = write_config(root, base_url)
    bin_dir = install_binaries(root, agent, tui)
    env = horon_env(root, home)
    env.pop("HORON_NO_TUI", None)
    argv = [os.path.join(bin_dir, "horon-agent")]
    terminal = Terminal(argv, cwd=root, env=env)
    try:
        terminal.read_until(FIRST_SCREEN, STARTUP_TIMEOUT, "first screen")

        terminal.send("ping\r")
        # The default mock plan starts with a shell call, and
        # `approval_mode = "write"` parks that call: the prompt arrives before
        # any reply text, so the answer has to come first.
        terminal.read_until(APPROVAL_PROMPT, STEP_TIMEOUT, "approval prompt")
        terminal.send("y")
        terminal.read_until(TOOL_DONE, STEP_TIMEOUT, "tool result after approval")

        terminal.read_until(REPLY_TEXT, STEP_TIMEOUT, "streamed reply")

        terminal.send("/quit\r")
        code = terminal.wait(STEP_TIMEOUT)
        if code != 0:
            raise Failure(f"happy path exited {code}\n--- transcript ---\n{terminal.transcript}")
        return terminal.transcript
    finally:
        terminal.close()


def headless_case(agent: str, tui: str | None, root: str, base_url: str, label: str, extra_env: dict | None = None,
                  use_pty: bool = True) -> str:
    """A launch that must stop at the headless message with the install hint."""
    home = write_config(root, base_url)
    bin_dir = install_binaries(root, agent, tui)
    env = horon_env(root, home)
    env.pop("HORON_NO_TUI", None)
    env.update(extra_env or {})
    argv = [os.path.join(bin_dir, "horon-agent")]

    if use_pty:
        terminal = Terminal(argv, cwd=root, env=env)
        try:
            code = terminal.wait(STARTUP_TIMEOUT)
            transcript = terminal.transcript
        finally:
            terminal.close()
    else:
        completed = subprocess.run(
            argv,
            cwd=root,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=STARTUP_TIMEOUT,
            check=False,
        )
        code = completed.returncode
        transcript = completed.stdout or ""

    if HEADLESS_MESSAGE not in transcript:
        raise Failure(f"{label}: headless message missing\n--- transcript ---\n{transcript}")
    if INSTALL_HINT not in transcript:
        raise Failure(f"{label}: install hint missing\n--- transcript ---\n{transcript}")
    if code != 1:
        raise Failure(f"{label}: expected exit 1, got {code}\n--- transcript ---\n{transcript}")
    return transcript


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", required=True, help="path to the built horon-agent binary")
    parser.add_argument("--tui", required=True, help="path to the built horon-tui binary")
    parser.add_argument(
        "--transcript",
        default=None,
        help="write the happy-path transcript here for the CI log artifact",
    )
    args = parser.parse_args(argv)

    for path, label in ((args.agent, "horon-agent"), (args.tui, "horon-tui")):
        if not os.path.isfile(path):
            raise Failure(f"{label} binary not found at {path}")

    server, base_url = start_mock_server(retry_denied=True)
    root = tempfile.mkdtemp(prefix="horon-tui-smoke-")
    try:
        log("case 1: bare horon hands a terminal over to horon-tui")
        transcript = happy_path(args.agent, args.tui, os.path.join(root, "happy"), base_url)
        if args.transcript:
            with open(args.transcript, "w", encoding="utf-8") as handle:
                handle.write(transcript)
        log("case 1 passed")

        log("case 2: piped stdin keeps the headless message")
        headless_case(
            args.agent,
            args.tui,
            os.path.join(root, "pipe"),
            base_url,
            "stdin pipe",
            use_pty=False,
        )
        log("case 2 passed")

        log("case 3: a missing horon-tui keeps the headless message")
        headless_case(args.agent, None, os.path.join(root, "missing"), base_url, "missing horon-tui")
        log("case 3 passed")

        log("case 4: HORON_NO_TUI keeps the headless message")
        headless_case(
            args.agent,
            args.tui,
            os.path.join(root, "disabled"),
            base_url,
            "HORON_NO_TUI",
            extra_env={"HORON_NO_TUI": "1"},
        )
        log("case 4 passed")
        return 0
    except Failure as failure:
        print(f"[tui-pty] FAILED: {failure}", file=sys.stderr, flush=True)
        return 1
    except Exception:  # noqa: BLE001 - a crash must print the trace, not exit silently
        traceback.print_exc()
        return 1
    finally:
        if server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
