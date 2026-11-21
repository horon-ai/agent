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
5. a relaunch over the socket file a killed bridge left behind starts a new
   bridge instead of dialling the dead one (#363); the case first proves the
   stale state, so it cannot pass without reproducing the defect.

`--layout build` (default) takes the two built binaries and copies them into one
directory, which is what the compiler leaves behind. `--layout installed` takes
the `HORON_HOME` an installer produced and drives its `bin/horon` launcher
symlink instead: cases 1 and 2 run through that symlink, and the run first
proves the launcher resolves into `builds/` and that the installed client sits
beside the agent it resolves to.

Linux, macOS and Windows. Unix uses `pty` and `termios`; Windows drives the same
cases through a ConPTY with `pywinpty`, which the Windows job installs.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback

POSIX = os.name != "nt"
if POSIX:
    import fcntl
    import pty
    import struct
    import termios

# A Windows client is driven through a ConPTY, which needs a binding the image
# does not ship. The step that runs it installs `pywinpty`; when that failed the
# run says why instead of failing for an unrelated reason (#432).
CONPTY_IMPORT_ERROR: str | None = None
PtyProcess = None
if not POSIX:
    try:
        from winpty import PtyProcess  # type: ignore[no-redef]
    except ImportError as error:  # pragma: no cover - depends on the runner
        CONPTY_IMPORT_ERROR = str(error)

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MOCK_SERVER = os.path.join(REPO_ROOT, "scripts", "perf", "mock_llm_server.py")

# How long a single expected screen costs before the run fails. The first draw
# waits on the daemon, the provider probe, and the harness bridge, so it gets the
# larger budget; keystroke round trips are local.
STARTUP_TIMEOUT = 90.0
STEP_TIMEOUT = 45.0
# Budget for a launch whose stdio is a pipe and which therefore has to start the
# daemon itself. It has to exceed the product's own readiness budget
# (`READY_POLL_TIMEOUT`, 120 s on Windows, where a spawned server is polled
# rather than signalled), or this harness kills the launch before the product
# can report why it failed and the run shows only an opaque timeout.
PIPE_STARTUP_TIMEOUT = 180.0

# Cases 6-8: the client restores the terminal before it dies by this signal.
# Names rather than values: two of the three do not exist on Windows.
SIGNAL_CASES = ("SIGTERM", "SIGHUP", "SIGQUIT")

HEADLESS_MESSAGE = "the interactive client is unavailable in this headless build"
# The install instruction itself. A headless sentence names it only when the
# client is genuinely missing (#430).
INSTALL_COMMAND = "horon-agent install-release"
# The fallback every headless sentence keeps.
HEADLESS_FALLBACK = "horon-agent serve"
FIRST_SCREEN = "Send a message to start"
REPLY_TEXT = "mock-ok"
APPROVAL_PROMPT = "approval needed"
# The client draws a tool call as its name and status, not its output, so the
# approved retry is proven by the status line plus the file the command writes
# (`APPROVAL_ROUND_TRIP_COMMAND` in mock_llm_server.py), not by printed text.
APPROVAL_RESOLVED = "approval approved"
# The status line draws `provider/model` from `ApiEvent::ModelInfo`, which the
# harness sends over the session event stream once the bridge is serving this
# client, so seeing it means the client is past its first frame and talking to
# a live bridge.
SESSION_READY = "mock/mock-model"
TOOL_FILE = "tui-smoke.txt"
TOOL_FILE_TEXT = "retry"
# The sequences the client writes when it gives the terminal back (#431). A
# signal is not an exit the client runs, so this is what proves the handler
# restored the modes rather than the process dying with them still on:
# mouse capture off (1000 tracking, 1002 button tracking, 1006 SGR encoding),
# bracketed paste off, and the shell's title popped back from the stack.
MOUSE_OFF = ("\x1b[?1000l", "\x1b[?1002l", "\x1b[?1006l")
PASTE_OFF = "\x1b[?2004l"
TITLE_POP = "\x1b[23;2t"


class Failure(RuntimeError):
    """A step that did not show up on screen."""


# A full-screen client positions each word with a cursor move (`ESC[row;colH`)
# instead of writing spaces, so raw PTY output never holds a sentence verbatim.
# Matching therefore runs on a copy where every escape sequence counts as a word
# break and whitespace runs collapse; the raw transcript is kept for reports.
ESCAPE = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])")
WHITESPACE = re.compile(r"\s+")


def screen_text(raw: str) -> str:
    return WHITESPACE.sub(" ", ESCAPE.sub(" ", raw))


def contains(raw: str, needle: str) -> bool:
    return WHITESPACE.sub(" ", needle) in screen_text(raw)


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


def write_config(home: str, base_url: str) -> str:
    """An isolated HORON_HOME whose only provider is the mock server.

    `approval_mode = "write"` is what makes the first tool call ask: the turn
    types `ls`, the write-classed shell call parks for approval, and the `y`
    keystroke is the answer the round-trip assertion depends on.
    """
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


class Launch:
    """Where the client is started from and which HORON_HOME it reads."""

    def __init__(self, argv0: str, home: str, tui: str | None = None) -> None:
        self.argv0 = argv0
        self.home = home
        # The client's own path, for the platform whose launcher gives the
        # console up before the client finishes (see `happy_path`).
        self.tui = tui

    def client_argv(self, cwd: str) -> list[str]:
        """What to start for a terminal session on this platform.

        POSIX replaces the launcher with the client, so the launcher is what a
        person runs. The Windows launcher starts the client and exits at once —
        a documented limit — and the pseudo console is torn down with it, so
        the client is started directly there; the handoff itself is covered by
        the POSIX case and by the Windows launcher tests.
        """
        if POSIX or self.tui is None:
            return [self.argv0]
        return [self.tui, "--cwd", cwd]


def build_launch(case_root: str, agent: str, tui: str | None) -> Launch:
    """A bin directory holding the two binaries under their shipped names.

    The handoff looks for `horon-tui` beside `current_exe`, so the pair must sit
    in one directory with exactly those names regardless of the build layout.
    """
    bin_dir = os.path.join(case_root, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    suffix = ".exe" if os.name == "nt" else ""
    shutil.copy2(agent, os.path.join(bin_dir, f"horon-agent{suffix}"))
    if tui is not None:
        shutil.copy2(tui, os.path.join(bin_dir, f"horon-tui{suffix}"))
    agent_path = os.path.join(bin_dir, f"horon-agent{suffix}")
    os.chmod(agent_path, 0o755)
    tui_path = os.path.join(bin_dir, f"horon-tui{suffix}") if tui is not None else None
    return Launch(agent_path, os.path.join(case_root, "home"), tui_path)


def installed_launch(layout_root: str) -> Launch:
    """The launcher symlink an installer left behind, and its version directory.

    `bin/horon` is the name the installer puts on PATH; it points at the channel
    the update moves, so the client has to be found beside whatever it resolves
    to. An installed client that is not there proves nothing, so it is checked
    before the screen is.
    """
    launcher = os.path.join(layout_root, "bin", "horon")
    if not os.path.islink(launcher):
        raise Failure(f"{launcher} is not the launcher symlink the installer creates")
    resolved = os.path.realpath(launcher)
    builds = os.path.realpath(os.path.join(layout_root, "builds"))
    if os.path.commonpath([resolved, builds]) != builds:
        raise Failure(f"{launcher} resolves to {resolved}, outside {builds}")
    tui = os.path.join(os.path.dirname(resolved), "horon-tui")
    if not os.path.isfile(tui) or not os.access(tui, os.X_OK):
        raise Failure(f"{tui} is missing beside the installed agent {resolved}")
    log(f"installed layout: {launcher} -> {resolved}, client at {tui}")
    return Launch(launcher, layout_root)


def horon_env(root: str, home: str) -> dict:
    """The parent env with every horon variable dropped, plus isolated paths.

    Dropping them is not enough for `HORON_AGENT_CLI_BOOTSTRAP_LOGIN`: the agent
    treats that variable as an opt-in by *presence*, so setting it to "0" turned
    the legacy headless bootstrap probe on instead of off, and on Windows that
    probe runs before the server is spawned and slows every cold launch. The
    cases leave it unset.
    """
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
            "HORON_AGENT_NO_AUTO_UPDATE": "1",
            "HORON_AGENT_RUN_MCP": "0",
            "HORON_AGENT_RUN_AUTO_POKE": "0",
            "NO_COLOR": "1",
            "TERM": "xterm-256color",
        }
    )
    os.makedirs(env["HORON_AGENT_RUNTIME_DIR"], exist_ok=True)
    return env


# ---------------------------------------------------------------------------
# PTY driver
# ---------------------------------------------------------------------------


class PosixTerminal:
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

    def read_until(self, needle: str, timeout: float, what: str, *, since: int = 0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if contains(self.transcript[since:], needle):
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
        if not contains(self.transcript[since:], needle):
            raise Failure(f"{what}: never saw {needle!r}\n--- transcript ---\n{self.transcript}")

    def send(self, keys: str) -> None:
        os.write(self.master, keys.encode())

    def wait(self, timeout: float) -> int:
        deadline = time.monotonic() + timeout
        # A read of the master fails with EIO on Linux once every slave
        # descriptor is closed and returns nothing on macOS; both mean "no more
        # output", never "the child is gone". The exit status comes from poll(),
        # so the wait keeps polling after the output side is finished.
        master_open = True
        while time.monotonic() < deadline:
            if master_open:
                readable, _, _ = select.select([self.master], [], [], 0.05)
                if readable:
                    try:
                        chunk = os.read(self.master, 65536)
                    except OSError:
                        master_open = False
                    else:
                        if chunk:
                            self.transcript += chunk.decode("utf-8", "replace")
                        else:
                            master_open = False
            else:
                # A finished master stays readable, so polling it would spin.
                time.sleep(0.05)
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



class WindowsTerminal:
    """A child on a Windows pseudo console, with a transcript for failure reports.

    `pywinpty` hands back the ConPTY output as it arrives, so a reader thread
    keeps the transcript and the case methods wait on it; the client draws into
    that console exactly as it does into a pty on Unix.
    """

    def __init__(self, argv: list[str], cwd: str, env: dict) -> None:
        if PtyProcess is None:
            raise Failure(
                "pywinpty is not installed, so no ConPTY is available: "
                f"{CONPTY_IMPORT_ERROR or 'import failed'}"
            )
        self.transcript = ""
        self._lock = threading.Lock()
        # A list, not a command line: `spawn` splits a string with
        # `shlex.split(posix=False)`, which keeps the quotes and then fails to
        # find the executable.
        self.process = PtyProcess.spawn(argv, cwd=cwd, env=env, dimensions=(40, 120))
        self._reader = threading.Thread(target=self._read_loop, name="conpty-reader", daemon=True)
        self._reader.start()

    def _read_loop(self) -> None:
        while True:
            try:
                chunk = self.process.read(4096)
            except (EOFError, ValueError):
                return
            except OSError:
                return
            if not chunk:
                return
            with self._lock:
                self.transcript += chunk

    def read_until(self, needle: str, timeout: float, what: str, *, since: int = 0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._lock:
                if contains(self.transcript[since:], needle):
                    return
            time.sleep(0.05)
        with self._lock:
            text = self.transcript
        # An empty transcript has two very different causes: a client that is
        # still starting (it has drawn nothing yet) and one that died before its
        # first write. The console cannot tell them apart, the process can.
        state = "still running" if self.process.isalive() else f"exited {self.process.exitstatus!r}"
        raise Failure(f"{what}: never saw {needle!r} (client {state})\n--- transcript ---\n{text}")

    def send(self, keys: str) -> None:
        try:
            self.process.write(keys)
        except EOFError as closed:
            # The client left before it could be typed at: say what it printed
            # and how it ended, which is the whole diagnosis on a runner whose
            # console dies with it.
            raise Failure(
                f"the client was gone before it could be typed at ({closed}; "
                f"exitstatus={self.process.exitstatus})\n--- transcript ---\n{self.transcript}"
            ) from closed

    def wait(self, timeout: float) -> int:
        # The launcher starts the client and exits at once (a documented limit),
        # so the console closing — the reader reaching EOF — is what says the
        # client is gone; the code below is the launcher's, which is all Windows
        # reports.
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self._reader.is_alive():
                return int(self.process.exitstatus or 0)
            time.sleep(0.05)
        self.close()
        with self._lock:
            text = self.transcript
        raise Failure(f"process did not exit within {timeout}s\n--- transcript ---\n{text}")

    def close(self) -> None:
        try:
            if self.process.isalive():
                self.process.terminate(force=True)
        except Exception:  # noqa: BLE001 - the case result is what matters
            pass


def Terminal(argv: list[str], cwd: str, env: dict):
    """The platform's pseudo console for `argv`."""
    if POSIX:
        return PosixTerminal(argv, cwd, env)
    return WindowsTerminal(argv, cwd, env)

# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------


def prime_server(launch: Launch, cwd: str, env: dict) -> subprocess.Popen:
    """Start the daemon the client needs and keep a handle on it.

    The launcher is what starts the server on a terminal, but on Windows its
    console is gone before the client finishes, so the client is started
    directly and the server is started here with the documented `serve`
    subcommand. The launcher's own piped run is not usable for this: it starts
    the server and then waits on its own first-run work, and the case already
    covers that path as the headless message check.
    """
    server = subprocess.Popen(
        [launch.argv0, "serve"],
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    # The daemon binds its endpoint after it has read its settings; a pause
    # saves a client that would otherwise dial before it is up.
    time.sleep(5.0)
    return server


def daemon_log_tail(home: str, limit: int = 60) -> str:
    """The end of the daemon's own log, for a failure that printed nothing.

    A client that never draws has no transcript to read, so the log is the only
    place the daemon says what it did: whether it bound, which provider it
    loaded, and why a session was refused.
    """
    directory = os.path.join(home, "logs")
    try:
        names = sorted(name for name in os.listdir(directory) if name.endswith(".log"))
    except OSError:
        return f"(no log directory at {directory})"
    if not names:
        return f"(no log files in {directory})"
    with open(os.path.join(directory, names[-1]), encoding="utf-8", errors="replace") as handle:
        return "".join(handle.readlines()[-limit:])


def pipe_state(socket_path: str) -> str:
    """Whether the daemon bound its named pipe, asked from outside the launch.

    A launch that waits for the server and never sees it has two causes that
    need different fixes: nothing bound the pipe, or the pipe is bound and the
    launcher never looked at it. Repeating the derivation and the open from this
    process separates them without another run.
    """
    if POSIX:
        return "(unix socket, binding is a pathname)"
    stem = "".join(ch for ch in os.path.splitext(os.path.basename(socket_path))[0]
                   if ch.isalnum() or ch in "-_")[:32] or "horon-agent"
    digest = hashlib.sha256(socket_path.replace("\\", "/").lower().encode()).hexdigest()
    name = "\\\\.\\pipe\\%s-%s" % (stem, digest[:16])
    try:
        # Opening read+write is what the launcher's probe does; the instance is
        # only taken for the instant this handle lives.
        with open(name, "r+b", buffering=0):
            return f"{name}: bound (this process opened it)"
    except OSError as error:
        return f"{name}: {error}"


def case_diagnostics(case_root: str, home: str) -> str:
    """Everything a launch that never finished leaves behind, for one message."""
    run_dir = os.path.join(case_root, "run")
    try:
        listing = sorted(os.listdir(run_dir))
    except OSError as error:
        listing = [f"(unreadable: {error})"]
    socket_path = os.path.join(run_dir, "horon-agent.sock")
    return (
        f"pipe: {pipe_state(socket_path)}"
        f"\nruntime dir {run_dir}: {listing}"
        f"\n--- daemon log ---\n{daemon_log_tail(home)}"
    )


def happy_path(launch: Launch, case_root: str, base_url: str) -> str:
    try:
        return _happy_path(launch, case_root, base_url)
    except Failure as failure:
        raise Failure(f"{failure}\n--- daemon log ---\n{daemon_log_tail(launch.home)}") from failure


def todo_edit_smoke(terminal: Terminal, home: str) -> None:
    from pathlib import Path

    events = [path for path in Path(home).glob("sessions/*/events.jsonl")
              if TOOL_FILE in path.read_text(encoding="utf-8")]
    if len(events) != 1:
        raise Failure(f"expected a single active session, got {len(events)}")
    todo = Path(home, "todos", f"{events[0].parent.name}.json")
    todo.parent.mkdir(exist_ok=True)
    rows = [{"id": f"todo-{i}", "content": f"fixture item {i:02}", "status": "pending", "priority": "medium"}
            for i in range(1, 13)]
    todo.write_text(json.dumps(rows), encoding="utf-8")
    terminal.send("/todo\r")
    terminal.read_until("fixture item 01", STEP_TIMEOUT, "full todo picker is loaded")
    rows[0]["status"] = "in_progress"
    todo.write_text(json.dumps(rows), encoding="utf-8")
    terminal.send("\r")
    terminal.read_until("changed on the server", STEP_TIMEOUT, "stale todo edit was refused")
    if json.loads(todo.read_text(encoding="utf-8"))[0]["status"] != "in_progress":
        raise Failure("stale todo edit overwrote the newer list")
    terminal.send("\r")
    terminal.read_until("is now completed", STEP_TIMEOUT, "refreshed todo edit applied")
    if json.loads(todo.read_text(encoding="utf-8"))[0]["status"] != "completed":
        raise Failure("the updated todo was not saved")
    terminal.send("\x1b")


def context_request_smoke(terminal: Terminal, home: str, base_url: str) -> None:
    from pathlib import Path
    from urllib.request import urlopen

    marker = "[tui:context-proof]"

    def send(label: str, has_original: bool) -> None:
        prompt = f"{marker} {label}"
        start = len(terminal.transcript)
        terminal.send(f"{prompt}\r")
        terminal.read_until(f"context-verified-{label.rsplit(' ', 1)[-1]}", STEP_TIMEOUT, f"reply to {label}", since=start)
        with urlopen(base_url + "/__stats", timeout=5) as response:
            requests = json.load(response)["requests"]
        request = next(
            (entry for entry in reversed(requests) if prompt in entry.get("context_proof_prompt", "")),
            None,
        )
        if request is None:
            raise Failure(f"the mock recorded no model request for {label}")
        if request["context_proof_contains_original"] != has_original:
            raise Failure(f"model request for {label} had original={not has_original}, expected {has_original}")

    send("original", True)
    terminal.send("/clear\r")
    send("after display clear", True)
    start = len(terminal.transcript)
    terminal.send("/clear-context\r")
    terminal.read_until("previous conversation remains", STEP_TIMEOUT, "reset model context", since=start)
    send("after context reset", False)
    if not any(f"{marker} original" in path.read_text(encoding="utf-8")
               for path in Path(home).glob("sessions/*/events.jsonl")):
        raise Failure("the original user message disappeared from the saved event log")


def configure_mcp_probe(home: str, case_root: str) -> str:
    fixture = os.path.join(REPO_ROOT, "scripts", "perf", "fixtures", "bench_mcp_server.py")
    calls = os.path.join(case_root, "mcp-tool-called")
    wrapper = os.path.join(case_root, "mcp_probe_fixture.py")
    with open(wrapper, "w", encoding="utf-8") as handle:
        handle.write(
            "import json, pathlib, runpy, sys\n"
            "handle = runpy.run_path(sys.argv[1])['handle']\n"
            "for line in sys.stdin:\n"
            "    request = json.loads(line)\n"
            "    if request.get('method') == 'tools/call':\n"
            "        pathlib.Path(sys.argv[2]).touch()\n"
            "    response = handle(request)\n"
            "    if response is not None:\n"
            "        print(json.dumps(response), flush=True)\n"
        )
    server = {"command": sys.executable, "args": [wrapper, fixture, calls]}
    for path, name in ((os.path.join(home, "mcp.json"), "trusted-fixture"),
                       (os.path.join(case_root, ".mcp.json"), "denied-fixture")):
        with open(path, "w", encoding="utf-8") as handle:
            json.dump({"mcpServers": {name: server}}, handle)
    return calls


def _happy_path(launch: Launch, case_root: str, base_url: str) -> str:
    home = write_config(launch.home, base_url)
    env = horon_env(case_root, home)
    env.pop("HORON_NO_TUI", None)
    archive_label = "archive-pty-fixture"
    seeded = subprocess.run(
        [launch.argv0, "run", archive_label],
        cwd=case_root,
        env={**env, "HORON_AGENT_TOOL_PROFILE": "none"},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=STEP_TIMEOUT,
        text=True,
    )
    if seeded.returncode != 0:
        raise Failure(f"could not seed a saved session to archive\n{seeded.stdout}")
    tool_calls = configure_mcp_probe(home, case_root)
    env["HORON_AGENT_WORKSPACE_TRUST"] = "deny"
    server = prime_server(launch, case_root, env) if not POSIX else None
    terminal = Terminal(launch.client_argv(case_root), cwd=case_root, env=env)
    try:
        terminal.read_until(FIRST_SCREEN, STARTUP_TIMEOUT, "first screen")
        terminal.read_until(SESSION_READY, STEP_TIMEOUT, "session ready before the context commands")
        terminal.send("/compact\r")
        terminal.read_until("nothing to compact", STEP_TIMEOUT, "empty context is unchanged after compaction")
        start = len(terminal.transcript)
        terminal.send("/clear-context\r")
        terminal.read_until(
            "previous conversation remains in saved history",
            STEP_TIMEOUT,
            "daemon confirmed reset",
            since=start,
        )

        terminal.send("/mcp\r")
        terminal.read_until("trusted-fixture", STEP_TIMEOUT, "trusted MCP server listed")
        terminal.send("/mcp probe trusted-fixture\r")
        terminal.read_until("Transport: stdio", STEP_TIMEOUT, "MCP probe transport")
        terminal.read_until("1 available", STEP_TIMEOUT, "MCP probe capability and tool count")
        terminal.send("/mcp probe denied-fixture\r")
        terminal.read_until("not in the trusted MCP configuration", STEP_TIMEOUT, "untrusted MCP probe refused")
        if os.path.exists(tool_calls):
            raise Failure(f"MCP inspection invoked a fixture tool\n--- transcript ---\n{terminal.transcript}")

        name = "PTY review session"
        terminal.send(f"/rename {name}\r")
        terminal.read_until("session title updated", STEP_TIMEOUT, "saved session title")
        terminal.send("/rename\r")
        terminal.read_until("enter a title without control characters", STEP_TIMEOUT, "empty title refusal")
        if POSIX:
            terminal.send("/new\r")
            terminal.read_until("a new session session_", STEP_TIMEOUT, "switch away from the renamed session")
            picker_start = len(terminal.transcript)
            terminal.send("/resume\r")
            terminal.read_until("Resume session", STEP_TIMEOUT, "session picker")
            terminal.read_until(name, STEP_TIMEOUT, "persisted title in session picker", since=picker_start)
            terminal.send(f"{name}\r")
            terminal.read_until("resumed ", STEP_TIMEOUT, "reattach to the renamed session", since=picker_start)

        terminal.send("ping\r")
        # The default mock plan starts with a shell call, and
        # `approval_mode = "write"` parks that call: the prompt arrives before
        # any reply text, so the answer has to come first.
        terminal.read_until(APPROVAL_PROMPT, STEP_TIMEOUT, "approval prompt")
        reply_start = len(terminal.transcript)
        terminal.send("y")
        terminal.read_until(APPROVAL_RESOLVED, STEP_TIMEOUT, "approval resolved")

        terminal.read_until(REPLY_TEXT, STEP_TIMEOUT, "streamed reply")
        written = os.path.join(case_root, TOOL_FILE)
        try:
            with open(written, encoding="utf-8") as handle:
                content = handle.read().strip()
        except OSError as error:
            raise Failure(f"approved tool call left no {TOOL_FILE}: {error}\n--- transcript ---\n{terminal.transcript}")
        if content != TOOL_FILE_TEXT:
            raise Failure(f"{TOOL_FILE} holds {content!r}, expected {TOOL_FILE_TEXT!r}")
        if POSIX:
            todo_edit_smoke(terminal, home)
            context_request_smoke(terminal, home, base_url)

        terminal.read_until("idle", STEP_TIMEOUT, "turn ended before archiving", since=reply_start)
        terminal.send("/resume\r")
        terminal.read_until(archive_label, STEP_TIMEOUT, "saved session in recent list")
        filter_start = len(terminal.transcript)
        terminal.send(archive_label)
        terminal.read_until("1/1", STEP_TIMEOUT, "select the matching session", since=filter_start)
        terminal.send("\x01")
        terminal.read_until("session archived", STEP_TIMEOUT, "daemon confirmed archive")
        terminal.send("/resume\r")
        terminal.read_until("enter attach", STEP_TIMEOUT, "recent picker reopened")
        terminal.send(archive_label)
        terminal.read_until("matches that filter", STEP_TIMEOUT, "archived session left recent list")
        terminal.send("\x1b")
        time.sleep(0.15)
        terminal.send("/archived\r")
        terminal.read_until("enter restore", STEP_TIMEOUT, "archived picker loaded")
        terminal.send(f"{archive_label}\r")
        terminal.read_until("session restored", STEP_TIMEOUT, "daemon confirmed restoration")

        terminal.send("/quit\r")
        code = terminal.wait(STEP_TIMEOUT)
        if code != 0:
            raise Failure(f"happy path exited {code}\n--- transcript ---\n{terminal.transcript}")
        if POSIX and f"\x1b]0;horon · {name}" not in terminal.transcript:
            raise Failure(f"window title did not change to {name!r}\n--- transcript ---\n{terminal.transcript}")
        if POSIX:
            previous_log = daemon_log_tail(home, limit=2000)
            resumed = Terminal([launch.argv0, "--continue"], cwd=case_root, env=env)
            try:
                resumed.read_until("Continuing session session_", STARTUP_TIMEOUT, "named continued session")
                resumed.read_until(SESSION_READY, STEP_TIMEOUT, "continued session ready")
                selected = re.search(r"Continuing session (session_[A-Za-z0-9_]+)", resumed.transcript)
                if selected is None:
                    raise Failure(f"continue did not print a session ID\n{resumed.transcript}")
                attached = r"phase=resume_(?:restored|live_attach)_done"
                receipt = rf"{attached} .*target_session_id={re.escape(selected[1])}(?:\s|$)"
                current_log = daemon_log_tail(home, limit=2000)
                if len(re.findall(receipt, current_log)) <= len(re.findall(receipt, previous_log)):
                    raise Failure(f"continue did not restore session {selected[1]}\n{resumed.transcript}")
                resumed.send("/quit\r")
                if resumed.wait(STEP_TIMEOUT) != 0:
                    raise Failure(f"continue launch did not exit cleanly\n{resumed.transcript}")
                return terminal.transcript + resumed.transcript
            finally:
                resumed.close()
        return terminal.transcript
    finally:
        terminal.close()
        if server is not None:
            server.terminate()


def headless_case(
    launch: Launch,
    case_root: str,
    base_url: str,
    label: str,
    extra_env: dict | None = None,
    use_pty: bool = True,
    expect_install_hint: bool = False,
) -> str:
    """A launch that must stop at the headless message with exit 1.

    Only the missing-client condition is answered by installing the client
    (#430): a pipe and `HORON_NO_TUI` happen with the client already in place,
    so naming an install there told the operator a correct install was broken.
    `expect_install_hint` is therefore off by default and the condition's own
    remedy is asserted instead.
    """
    home = write_config(launch.home, base_url)
    env = horon_env(case_root, home)
    env.pop("HORON_NO_TUI", None)
    env.update(extra_env or {})

    if use_pty:
        terminal = Terminal([launch.argv0], cwd=case_root, env=env)
        try:
            code = terminal.wait(STARTUP_TIMEOUT)
            transcript = terminal.transcript
        finally:
            terminal.close()
    else:
        try:
            completed = subprocess.run(
                [launch.argv0],
                cwd=case_root,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=PIPE_STARTUP_TIMEOUT,
                check=False,
            )
        except subprocess.TimeoutExpired as expired:
            # The launcher reports on stderr, which `run` has captured and not
            # shown; a killed run would otherwise fail with the timeout alone.
            raise Failure(
                f"{label}: the launch did not finish within {PIPE_STARTUP_TIMEOUT:.0f}s"
                f"\n--- output so far ---\n{expired.stdout or ''}"
                f"\n--- state ---\n{case_diagnostics(case_root, home)}"
            ) from None
        code = completed.returncode
        transcript = completed.stdout or ""

    if not contains(transcript, HEADLESS_MESSAGE):
        raise Failure(f"{label}: headless message missing\n--- transcript ---\n{transcript}")
    if expect_install_hint:
        if not contains(transcript, INSTALL_COMMAND):
            raise Failure(f"{label}: install hint missing\n--- transcript ---\n{transcript}")
    elif contains(transcript, INSTALL_COMMAND):
        raise Failure(
            f"{label}: the sentence names an install for a client that is present\n--- transcript ---\n{transcript}"
        )
    if not contains(transcript, HEADLESS_FALLBACK):
        raise Failure(f"{label}: headless fallback missing\n--- transcript ---\n{transcript}")
    if code != 1:
        raise Failure(f"{label}: expected exit 1, got {code}\n--- transcript ---\n{transcript}")
    return transcript


def case_bridge_pids(case_root: str) -> list[int]:
    """PIDs of the API bridges serving this case, and no other case's.

    The bridge is spawned with `api-bridge` as its only argument and takes its
    socket path from the environment, so the command line names the case only
    through the binary, and every case copies the binaries under its own root.
    Both readings are anchored on the whole argv[0] followed by `api-bridge`, so
    a daemon (`<binary> serve`) or a shell that merely mentions the path is never
    counted. Paths are compared after resolution because macOS `/tmp` is a
    symlink to `/private/tmp` and `current_exe()` hands the resolved form on.
    """
    binary = os.path.join(case_root, "bin", "horon-agent")
    resolved = os.path.realpath(binary)

    def is_case_binary(argv0: str) -> bool:
        return argv0 in (binary, resolved) or os.path.realpath(argv0) == resolved

    if sys.platform == "darwin":
        listing = subprocess.run(
            ["ps", "-axww", "-o", "pid=,command="],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        # BSD `ps` renders argv space-separated and can append an escaped `\012`
        # and environment bytes after it, so the line is matched on its prefix:
        # this case's binary followed by the bridge's only argument. A daemon
        # (`<binary> serve`) or a shell that mentions the path never matches.
        prefixes = (f"{binary} api-bridge", f"{resolved} api-bridge")
        pids = []
        for line in listing.splitlines():
            pid, _, command = line.strip().partition(" ")
            if pid.isdigit() and command.startswith(prefixes):
                pids.append(int(pid))
        return pids
    # Linux: /proc names argv exactly, which `ps` column output does not promise.
    pids = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(os.path.join("/proc", entry, "cmdline"), "rb") as handle:
                argv = handle.read().split(b"\0")
        except OSError:
            continue
        argv = [os.fsdecode(argument) for argument in argv if argument]
        if argv[1:] and "api-bridge" in argv[1:] and is_case_binary(argv[0]):
            pids.append(int(entry))
    return pids


def wait_for_case_bridges(case_root: str, expected: int, timeout: float, what: str) -> None:
    """Wait for this case to have `expected` bridge processes, or fail."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if len(case_bridge_pids(case_root)) == expected:
            return
        time.sleep(0.05)
    raise Failure(f"{what}: expected {expected} api-bridge processes, found {case_bridge_pids(case_root)}")


def wait_for_refused_connect(path: str, timeout: float, what: str) -> None:
    """Wait until a connect to `path` is refused, or fail.

    Only `ConnectionRefusedError` is the dead state; every other exception keeps
    propagating, so a probe that fails for any other reason is a failure, not a
    pass. The wait exists because a killed server's last descriptor can outlive
    its process entry: a process-count check reaching zero says nothing about
    the listening socket still accepting one more connect.
    """
    deadline = time.monotonic() + timeout
    while True:
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            probe.settimeout(5.0)
            probe.connect(path)
        except ConnectionRefusedError:
            return
        finally:
            probe.close()
        if time.monotonic() >= deadline:
            raise Failure(what)
        time.sleep(0.05)


def stale_socket_case(launch: Launch, case_root: str, base_url: str, transcript: str | None) -> str:
    """A relaunch over the socket file a killed bridge left behind (#363).

    Closing the terminal kills the group the bridge lives in, so the bridge dies
    and its socket file stays on disk. A launch that read that file as "the
    bridge is up" skipped starting a bridge and dialled the dead endpoint, which
    is the reported `no harness answered at ...horon-agent-api.sock` refusal.
    """

    def note(line: str) -> None:
        log(line)
        if transcript:
            with open(transcript, "a", encoding="utf-8") as handle:
                handle.write(line + "\n")

    home = write_config(launch.home, base_url)
    env = horon_env(case_root, home)
    env.pop("HORON_NO_TUI", None)
    api_socket = env["HORON_AGENT_API_SOCKET"]

    first = Terminal([launch.argv0], cwd=case_root, env=env)
    second = None
    try:
        first.read_until(FIRST_SCREEN, STARTUP_TIMEOUT, "first screen")
        wait_for_case_bridges(case_root, 1, STEP_TIMEOUT, "one bridge for the first client")

        # A second client must attach to the bridge already serving this case
        # instead of adding one: the bridge's instance lock is what makes the
        # on-demand spawn idempotent.
        second = Terminal([launch.argv0], cwd=case_root, env=env)
        second.read_until(FIRST_SCREEN, STARTUP_TIMEOUT, "second client's first screen")
        wait_for_case_bridges(case_root, 1, STEP_TIMEOUT, "still one bridge with a second client")

        # The first client still answers while the second one is attached, so
        # the single bridge really serves both.
        first.send("ping\r")
        first.read_until(APPROVAL_PROMPT, STEP_TIMEOUT, "the first client still answers")

        # SIGKILL both groups: the bridge dies with the terminal that started it,
        # the daemon detaches itself with setsid, and the socket file stays.
        second.close()
        second = None
        first.close()
        first = None
        wait_for_case_bridges(case_root, 0, STEP_TIMEOUT, "the bridge dies with its terminal")

        # Prove the stale state before launching again, so this case cannot pass
        # without reproducing the defect it guards.
        if not os.path.exists(api_socket):
            raise Failure(f"{api_socket} went away with its bridge: the stale state was not reproduced")
        # A live bridge keeps answering, so this raises; the stale state only
        # passes once the endpoint really refuses.
        wait_for_refused_connect(
            api_socket,
            STEP_TIMEOUT,
            f"{api_socket} still answered after its bridge died: the stale state was not reproduced",
        )
        note(f"stale socket reproduced: {api_socket} exists and refuses a connect")

        relaunch = Terminal([launch.argv0], cwd=case_root, env=env)
        try:
            relaunch.read_until(FIRST_SCREEN, STARTUP_TIMEOUT, "first screen after the stale socket")
            # The first frame is drawn once the client is connected, which over
            # a stale socket already says a bridge answered, but not that the
            # session behind it works. The status line carries `provider/model`
            # from the harness's session event, so seeing it means data reached
            # this client through the bridge it started itself: a stronger
            # proof of recovery than the first frame alone.
            relaunch.read_until(SESSION_READY, STEP_TIMEOUT, "session ready after the stale socket")
            relaunch.send("/quit\r")
            code = relaunch.wait(STEP_TIMEOUT)
            if code != 0:
                raise Failure(
                    f"a relaunch over a stale socket exited {code}\n--- transcript ---\n{relaunch.transcript}"
                )
            note("stale socket case passed: the relaunch started a new bridge and exited 0")
            return relaunch.transcript
        finally:
            relaunch.close()
    finally:
        if second is not None:
            second.close()
        if first is not None:
            first.close()


# Case 5 scans `/proc` or `ps` for the case's bridge processes and 6-8 send the
# client a POSIX signal: only a POSIX host can drive either.
POSIX_ONLY_CASES = {5, 6, 7, 8}
ALL_CASES = tuple(range(1, 9))
DEFAULT_CASES = {"windows": (1, 2), "posix": ALL_CASES}
def signal_teardown_case(launch: Launch, root: str, base_url: str, signum: int) -> str:
    """A client killed by a signal still gives the terminal back (#431).

    Every way out the client runs itself restores the modes; a signal was not
    one of them, so `SIGTERM` left bracketed paste and mouse reporting on for
    the shell that came back. The handler now runs the same restore sequence
    and then re-raises, so the exit status still reports the signal.
    """
    home = write_config(launch.home, base_url)
    env = horon_env(root, home)
    env.pop("HORON_NO_TUI", None)
    terminal = Terminal([launch.argv0], cwd=root, env=env)
    try:
        # The client is fully started, so every mode the handler restores is on
        # and the title has been pushed.
        terminal.read_until(FIRST_SCREEN, STARTUP_TIMEOUT, "first screen")
        terminal.read_until(SESSION_READY, STEP_TIMEOUT, "session ready")

        # `start_new_session=True` in `Terminal` makes the client a process
        # group leader, so this reaches the client and nothing else.
        os.killpg(os.getpgid(terminal.process.pid), signum)
        code = terminal.wait(STEP_TIMEOUT)
        if code != -signum:
            raise Failure(
                f"a client killed by {signal.Signals(signum).name} exited {code}, "
                f"expected death by that signal ({-signum} in subprocess terms)"
                f"\n--- transcript ---\n{terminal.transcript}"
            )
        missing = [sequence for sequence in (*MOUSE_OFF, PASTE_OFF, TITLE_POP) if sequence not in terminal.transcript]
        if missing:
            shown = [sequence.encode("unicode_escape").decode() for sequence in missing]
            raise Failure(
                "the signal path left the terminal unprepared: "
                f"{', '.join(shown)} never written\n--- transcript ---\n{terminal.transcript}"
            )
        return terminal.transcript
    finally:
        terminal.close()


def title_disabled_case(launch: Launch, root: str, from_flag: bool) -> str:
    """The opt-out keeps the shell title in place from startup through exit."""
    if launch.tui is None:
        raise Failure("the title opt-out needs the TUI binary")
    config = os.path.join(root, "title.toml")
    with open(config, "w", encoding="utf-8") as handle:
        handle.write(f"[title]\nenabled = {str(from_flag).lower()}\nstate = true\n")
    argv = [launch.tui, "--config", config]
    if from_flag:
        argv.append("--no-title")
    env = horon_env(root, launch.home)
    env.pop("HORON_TUI_TITLE_ENABLED", None)
    terminal = Terminal(argv, cwd=root, env=env)
    try:
        terminal.read_until(SESSION_READY, STEP_TIMEOUT, "session ready with title disabled")
        terminal.send("/quit\r")
        code = terminal.wait(STEP_TIMEOUT)
        if code != 0 or any(seq in terminal.transcript for seq in ("\x1b[22;2t", TITLE_POP, "\x1b]0;", "\x1b]2;")):
            raise Failure(f"title opt-out failed (exit {code})\n--- transcript ---\n{terminal.transcript}")
        return terminal.transcript
    finally:
        terminal.close()


def selected_cases(requested: str | None) -> set[int]:
    """The cases to run, defaulting to what this platform can drive."""
    if requested:
        try:
            cases = {int(value) for value in requested.split(",") if value.strip()}
        except ValueError:
            raise Failure(f"--cases takes numbers separated by commas, not {requested!r}") from None
        unknown = cases - set(ALL_CASES)
        if unknown:
            raise Failure(f"no such case: {sorted(unknown)}")
        if not POSIX and cases & POSIX_ONLY_CASES:
            raise Failure(f"case {sorted(cases & POSIX_ONLY_CASES)} needs a POSIX host")
        return cases
    return set(DEFAULT_CASES["posix" if POSIX else "windows"])


def run_build_layout(
    agent: str, tui: str, root: str, base_url: str, transcript: str | None, cases: set[int]
) -> int:
    if 1 in cases:
        log("case 1: bare horon hands a terminal over to horon-tui")
        launch = build_launch(os.path.join(root, "happy"), agent, tui)
        text = happy_path(launch, os.path.join(root, "happy"), base_url)
        if POSIX:
            if TITLE_POP not in text or "\x1b[22;2t" not in text or "\x1b]0;horon" not in text:
                raise Failure(f"default title was not set and restored\n--- transcript ---\n{text}")
            for from_flag in (True, False):
                source = "--no-title" if from_flag else "title.enabled=false"
                text += f"\n--- {source} ---\n" + title_disabled_case(launch, os.path.join(root, "happy"), from_flag)
                log(f"{source}: no title or title stack sequences")
        if transcript:
            with open(transcript, "w", encoding="utf-8") as handle:
                handle.write(text)
        log("case 1 passed")

    if 2 in cases:
        log("case 2: piped stdin keeps the headless message")
        headless_case(
            build_launch(os.path.join(root, "pipe"), agent, tui),
            os.path.join(root, "pipe"),
            base_url,
            "stdin pipe",
            use_pty=False,
        )
        log("case 2 passed")

    if 3 in cases:
        log("case 3: a missing horon-tui keeps the headless message")
        headless_case(
            build_launch(os.path.join(root, "missing"), agent, None),
            os.path.join(root, "missing"),
            base_url,
            "missing horon-tui",
            # The one reason installing answers, so the sentence names the install.
            expect_install_hint=True,
        )
        log("case 3 passed")

    if 4 in cases:
        log("case 4: HORON_NO_TUI keeps the headless message")
        headless_case(
            build_launch(os.path.join(root, "disabled"), agent, tui),
            os.path.join(root, "disabled"),
            base_url,
            "HORON_NO_TUI",
            extra_env={"HORON_NO_TUI": "1"},
        )
        log("case 4 passed")

    if 5 in cases:
        log("case 5: a relaunch over the socket file a killed bridge left behind")
        stale_socket_case(
            build_launch(os.path.join(root, "dead"), agent, tui),
            os.path.join(root, "dead"),
            base_url,
            transcript,
        )
        log("case 5 passed")

    # By name, because `signal.SIGHUP` and `signal.SIGQUIT` do not exist on
    # Windows: reading them up front failed the whole run there before any case
    # was selected (the signal cases are POSIX-only).
    for index, name in enumerate(SIGNAL_CASES, start=6):
        if index not in cases:
            continue
        signum = getattr(signal, name)
        log(f"case {index}: {signal.Signals(signum).name} restores the terminal before the client dies")
        case_root = os.path.join(root, f"signal-{signal.Signals(signum).name.lower()}")
        signal_teardown_case(build_launch(case_root, agent, tui), case_root, base_url, signum)
        log(f"case {index} passed")
    return 0


def run_installed_layout(layout_root: str, root: str, base_url: str, transcript: str | None) -> int:
    # The launcher is resolved once: an installed client that is absent or
    # outside the channel it should live in is the failure this layout catches.
    launch = installed_launch(layout_root)

    log("case 1: the installed horon launcher reaches the installed horon-tui")
    case_root = os.path.join(root, "installed")
    text = happy_path(launch, case_root, base_url)
    if transcript:
        with open(transcript, "w", encoding="utf-8") as handle:
            handle.write(text)
    log("case 1 passed")

    log("case 2: piped stdin still keeps the headless message")
    headless_case(launch, case_root, base_url, "stdin pipe", use_pty=False)
    log("case 2 passed")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--agent", help="path to the built horon-agent binary (build layout)")
    parser.add_argument("--tui", help="path to the built horon-tui binary (build layout)")
    parser.add_argument(
        "--layout",
        choices=("build", "installed"),
        default="build",
        help="build: copy the two built binaries into one directory (default); "
        "installed: drive the launcher symlink an installer left in --layout-root",
    )
    parser.add_argument(
        "--layout-root",
        help="the HORON_HOME an installer populated; required by --layout installed",
    )
    parser.add_argument(
        "--cases",
        default=None,
        help="comma-separated case numbers to run; defaults to every case the platform can drive "
        "(Windows: 1,2)",
    )
    parser.add_argument(
        "--transcript",
        default=None,
        help="write the happy-path transcript here for the CI log artifact",
    )
    args = parser.parse_args(argv)

    if args.layout == "installed":
        if not args.layout_root:
            parser.error("--layout installed needs --layout-root <HORON_HOME>")
        if args.agent or args.tui:
            parser.error("--layout installed reads the binaries from --layout-root; drop --agent/--tui")
        if not os.path.isdir(args.layout_root):
            raise Failure(f"layout root not found at {args.layout_root}")
    else:
        missing = [flag for flag, value in (("--agent", args.agent), ("--tui", args.tui)) if not value]
        if missing:
            parser.error(f"--layout build needs {' and '.join(missing)}")
        for path, label in ((args.agent, "horon-agent"), (args.tui, "horon-tui")):
            if not os.path.isfile(path):
                raise Failure(f"{label} binary not found at {path}")

    cases = selected_cases(args.cases)
    server, base_url = start_mock_server(retry_denied=True)
    # Unix socket paths are capped near 104 bytes (SUN_LEN), and macOS puts the
    # default temp dir under a long /var/folders/... path, so the run root lives
    # directly under /tmp to keep `<root>/<case>/run/horon-agent-api.sock` short.
    # Windows has no /tmp and no path-length cap here: its named pipes live in
    # the pipe namespace, not the filesystem.
    root = tempfile.mkdtemp(prefix="htui-", dir="/tmp" if POSIX else None)
    try:
        if args.layout == "installed":
            return run_installed_layout(args.layout_root, root, base_url, args.transcript)
        return run_build_layout(args.agent, args.tui, root, base_url, args.transcript, cases)
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
