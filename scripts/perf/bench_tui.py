#!/usr/bin/env python3
"""Measure the interactive client (`horon-tui`) on its own.

The build ships two binaries. `horon-agent` is the headless one, and every other
suite in this directory measures it. `horon-tui` is the terminal client and
nothing else in CI times it, so this suite does, and it writes `tui.json` and
`tui.md` next to the other suites' results.

Both versions of the product are measured here, and they are never mixed into
one table: `first_frame_ms` of the bare `horon-agent` (which starts the server
and replaces itself with the client) is compared only with `first_frame_ms` of a
direct `horon-tui` launch, and the difference is reported as `handoff_ms`.

Metrics
-------
first_frame_ms       Spawn of bare `horon-agent` (no arguments, on a PTY) to the
                     first-screen text. Covers the client's own startup, the
                     sibling lookup, the `exec`, and the daemon start the bare
                     path performs.
direct_first_frame_ms  Spawn of `horon-tui` against an already-running daemon to
                     its first-screen text. The reference for the pair above.
handoff_ms           Paired difference, run by run: bare first frame minus the
                     direct first frame of the same run index. Positive means the
                     handoff path costs that much more than launching the client
                     itself; a negative value means the two paths were within
                     noise of each other on that run.
idle_rss_bytes       RSS of the client process 5 s after its first frame.
post_turn_rss_bytes  RSS of the client process after one mock-model turn.
input_echo_p95_ms    PTY key press to the echoed character appearing on screen;
                     the median and the nearest-rank p95 of the samples.
render_p50_ms        Per-frame draw time from `crates/tui`'s `render_bench`
render_p95_ms        example (1000 streamed deltas, drawn on a `TestBackend`),
                     median over the runs of its own p50 and p95.
install_size_bytes   Size of the `horon-tui` binary.

Every repeatable metric reports median, p95, min, max and the sample count, the
same shape the other suites use (`bench_common.summarize`).

Platforms
---------
The PTY metrics need `pty` and a POSIX executable bit, so they run on Linux and
macOS only. Windows still records the render and size metrics and writes the
skip reason `pty unavailable on Windows` for the rest; the suite succeeds. A
missing `horon-tui` is recorded as a skip of this suite alone, never as a
failure of the run.

Failure handling
----------------
A step that times out is recorded with its error and left out of its metric, so
a refusal never looks like a fast number, and the report still holds every
metric that did come back. The process exits 0 whenever the report was written:
`bench_smoke.py` reads the report and is what fails a run on a measurement gap.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_common as common  # noqa: E402

SUITE = "tui"
MOCK_SERVER = str(Path(__file__).resolve().parent / "mock_llm_server.py")
FIRST_SCREEN = "Send a message to start"
TURN_TEXT = "mock-ok"
PTY_ROWS = 40
PTY_COLS = 120
WINDOWS_PTY_SKIP = "pty unavailable on Windows"
# A full-screen client positions each word with a cursor move instead of writing
# spaces, so the transcript is matched with escapes treated as word breaks.
ESCAPE = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])")
WHITESPACE = re.compile(r"\s+")

# The table's rows, in order: (metric key, label, unit, how to format).
# `units` is "ms", "bytes" or "count"; `scalar` metrics print one number.
ROWS = (
    ("first_frame_ms", "First frame (bare `horon-agent`)", "ms", False),
    ("direct_first_frame_ms", "First frame (direct `horon-tui`)", "ms", False),
    ("handoff_ms", "TTY handoff cost", "ms", False),
    ("input_echo_p95_ms", "Key press to echo (p95)", "ms", False),
    ("idle_rss_bytes", "Idle RSS after 5 s", "bytes", False),
    ("post_turn_rss_bytes", "RSS after one turn", "bytes", False),
    ("render_p50_ms", "Frame draw time (p50)", "ms", False),
    ("render_p95_ms", "Frame draw time (p95)", "ms", False),
    ("install_size_bytes", "Install size (`horon-tui`)", "bytes", True),
)

HANDOFF_DEFINITION = (
    "median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) "
    "minus the median first-frame time of a direct `horon-tui` launch against an "
    "already-running daemon, both on the same machine and build. The bare path also "
    "starts the daemon, so the number covers that startup, the sibling lookup and the "
    "`exec`; it is the extra cost of the handoff over launching the client itself."
)


# ---------------------------------------------------------------------------
# Pure logic (unit-tested without binaries)
# ---------------------------------------------------------------------------


def percentile(values, fraction: float) -> float | None:
    """Nearest-rank percentile; None for an empty series.

    The same rank rule as `bench_common.p95`, so a p95 reported here and one
    reported through the shared helper agree on five samples.
    """
    vals = common.summary_values(values)
    if not vals:
        return None
    if len(vals) == 1:
        return vals[0]
    rank = max(1, int(round(fraction * len(vals) + 0.5)))
    return vals[min(rank, len(vals)) - 1]


def summary(values) -> dict | None:
    """median/p95/min/max/samples, or None when nothing was measured."""
    return common.summarize(values)


def echo_summary(samples) -> dict | None:
    """The echo metric in the shared shape but named for its p95."""
    stats = summary(samples)
    if stats is None:
        return None
    return {"median": stats["median"], "p95": stats["p95"], "min": stats["min"],
            "max": stats["max"], "samples": stats["samples"]}


def paired_differences(first: list[float], second: list[float]) -> list[float]:
    """`first[i] - second[i]` for the indices both lists have."""
    return [a - b for a, b in zip(first, second)]


def pty_skip_reason(platform: str, agent: str | None, tui: str | None) -> str | None:
    """Why the PTY metrics cannot run here, or None when they can."""
    if platform.startswith("win"):
        return WINDOWS_PTY_SKIP
    if tui is None:
        return "horon-tui binary not found"
    if agent is None:
        return "horon-agent binary not found"
    return None


def render_skip_reason(path: str | None) -> str | None:
    return None if path else "render bench binary not found"


def _fmt(value: float | None, unit: str) -> str:
    if value is None:
        return "—"
    if unit == "bytes":
        return common.fmt_bytes(float(value))
    if unit == "count":
        return f"{value:,.0f}"
    return common.fmt_ms(float(value))


def _stat_row(name: str, label: str, unit: str, value) -> str:
    """One table row: median, min-max, sample count."""
    if not isinstance(value, dict):
        return f"| {label} | — | — | — |"
    return (f"| {label} | {_fmt(value.get('median'), unit)} | "
            f"{_fmt(value.get('min'), unit)}–{_fmt(value.get('max'), unit)} | "
            f"{value.get('samples', 0)} |")


def markdown(report: dict) -> str:
    """The table bench_docs.py embeds under the TUI section."""
    lines = [
        "### TUI (`horon-tui`)",
        "",
        "Measured version: the interactive client `horon-tui`. The headless "
        "`horon-agent` numbers are in the suites above and are never compared with "
        "these: the two binaries are timed against themselves.",
        "",
        "| Metric | Median | Min–max | Samples |",
        "| --- | --- | --- | --- |",
    ]
    for key, label, unit, _scalar in ROWS:
        value = report.get(key)
        if _scalar:
            row = f"| {label} | {_fmt(value, unit)} | — | 1 |"
        else:
            row = _stat_row(key, label, unit, value)
        lines.append(row)
    lines += ["", f"Handoff: {HANDOFF_DEFINITION}"]
    render = report.get("render")
    if isinstance(render, dict):
        frames = render.get("frames")
        width = render.get("width")
        height = render.get("height")
        if frames:
            lines += ["", f"Render bench: {frames} frames on a {width}×{height} terminal."]
    skipped = [entry for entry in report.get("skipped", []) if isinstance(entry, dict)]
    if skipped:
        # The reason already names what was skipped ("pty unavailable on
        # Windows"), so repeating the entry name would read "pty (pty ...)".
        lines += ["", "Skipped: " + "; ".join(str(entry.get("reason")) for entry in skipped)]
    errors = [entry for entry in report.get("errors", []) if isinstance(entry, dict)]
    if errors:
        lines += ["", "Errors: " + "; ".join(
            f"{entry.get('step')}: {entry.get('error')}" for entry in errors)]
    return "\n".join(lines)


def report(**metrics) -> dict:
    """A report with the required keys present whether or not they were measured."""
    document = {
        "suite": SUITE,
        "schema": common.SCHEMA,
        "host": common.host_info(),
        "started_at": common.utc_now_iso(),
        "handoff_definition": HANDOFF_DEFINITION,
        "skipped": [],
        "errors": [],
    }
    for key, _label, _unit, _scalar in ROWS:
        document.setdefault(key, None)
    document.update({key: value for key, value in metrics.items() if value is not None})
    if "skipped" in metrics:
        document["skipped"] = metrics["skipped"]
    if "errors" in metrics:
        document["errors"] = metrics["errors"]
    return document


# ---------------------------------------------------------------------------
# Mock model server and isolated home
# ---------------------------------------------------------------------------


def start_mock_server() -> tuple[subprocess.Popen, str]:
    process = subprocess.Popen(
        [sys.executable, MOCK_SERVER],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    stdout = process.stdout
    if stdout is None:
        raise RuntimeError("the mock server has no stdout")
    line = stdout.readline()
    if not line:
        stderr = process.stderr.read() if process.stderr else ""
        raise RuntimeError(f"the mock server printed no address: {stderr.strip()}")
    return process, json.loads(line)["url"]


def write_config(root: str, base_url: str) -> str:
    """An isolated home whose only provider is the mock server.

    `approval_mode` is left at its default (`off`) on purpose: this suite times a
    turn, and a parked approval would time the operator instead.
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
"""
    with open(os.path.join(home, "config.toml"), "w", encoding="utf-8") as handle:
        handle.write(config)
    return home


def client_env(root: str, base_url: str) -> dict:
    """Isolated paths for one measurement, with every inherited horon var dropped."""
    env = common.isolated_home_env(root, "HORON_AGENT", home_var="HORON_HOME")
    for key in [key for key in env if key.startswith("HORON_AGENT_HOME")]:
        env.pop(key, None)
    env.update(
        {
            "HORON_HOME": write_config(root, base_url),
            "HORON_AGENT_NO_AUTO_UPDATE": "1",
            "HORON_AGENT_RUN_MCP": "0",
            "HORON_AGENT_RUN_AUTO_POKE": "0",
            "HORON_AGENT_CLI_BOOTSTRAP_LOGIN": "0",
            "NO_COLOR": "1",
            "TERM": "xterm-256color",
            "HORON_AGENT_MOCK_URL": base_url,
        }
    )
    env.pop("HORON_NO_TUI", None)
    return env


# ---------------------------------------------------------------------------
# PTY
# ---------------------------------------------------------------------------


def screen_text(raw: str) -> str:
    return WHITESPACE.sub(" ", ESCAPE.sub(" ", raw))


class Terminal:
    """A child on a real PTY with a growing transcript."""

    def __init__(self, argv: list[str], cwd: str, env: dict) -> None:
        import pty  # noqa: PLC0415 - Windows has no pty module and never gets here

        self.master, slave = pty.openpty()
        # A fresh PTY reports 0x0, and a full-screen client draws nothing into a
        # zero area, so the size is set before the child starts.
        import fcntl  # noqa: PLC0415
        import termios  # noqa: PLC0415

        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", PTY_ROWS, PTY_COLS, 0, 0))
        self.spawned_at = time.monotonic()
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
        self.transcript = ""

    def pump(self, timeout: float) -> None:
        """Read what the child wrote within `timeout` seconds."""
        readable, _, _ = select.select([self.master], [], [], timeout)
        if not readable:
            return
        try:
            chunk = os.read(self.master, 65536)
        except OSError:
            return
        if chunk:
            self.transcript += chunk.decode("utf-8", "replace")

    def wait_text(self, needle: str, timeout: float) -> float:
        """Seconds until `needle` is on screen; raises `TimeoutError` otherwise."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in screen_text(self.transcript):
                return time.monotonic() - self.spawned_at
            self.pump(0.05)
        raise TimeoutError(f"never saw {needle!r}")

    def count(self, needle: str) -> int:
        return screen_text(self.transcript).count(needle)

    def wait_count_above(self, needle: str, previous: int, timeout: float) -> float:
        """Seconds until `needle` appears more than `previous` times."""
        started = time.monotonic()
        deadline = started + timeout
        while time.monotonic() < deadline:
            if self.count(needle) > previous:
                return time.monotonic() - started
            self.pump(0.05)
        raise TimeoutError(f"{needle!r} never appeared (count stayed {previous})")

    def send(self, keys: str) -> None:
        os.write(self.master, keys.encode())

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
            except OSError:
                self.process.kill()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
        try:
            os.close(self.master)
        except OSError:
            pass


def process_rss_bytes(pid: int) -> int | None:
    """Resident set size of one process, without its children."""
    status = f"/proc/{pid}/status"
    if os.path.exists(status):
        try:
            with open(status, encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    if line.startswith("VmRSS:"):
                        return int(line.split()[1]) * 1024
        except (OSError, IndexError, ValueError):
            return None
        return None
    try:
        completed = subprocess.run(
            ["ps", "-o", "rss=", "-p", str(pid)],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    text = completed.stdout.strip().splitlines()
    if not text:
        return None
    try:
        return int(text[0].strip()) * 1024
    except ValueError:
        return None


ECHO_KEYS = "qwertyuiop"


def measure_bare(agent: str, root: str, base_url: str, idle_seconds: float,
                 echo_samples: int, errors: list[dict]) -> dict:
    """One bare `horon-agent` run: first frame, idle RSS, echo, one turn."""
    env = client_env(root, base_url)
    measured: dict = {}
    terminal = Terminal([agent], cwd=root, env=env)
    try:
        measured["first_frame_ms"] = terminal.wait_text(FIRST_SCREEN, 120.0) * 1000.0
        samples = []
        for index in range(echo_samples):
            key = ECHO_KEYS[index % len(ECHO_KEYS)]
            before = terminal.count(key)
            terminal.send(key)
            try:
                timestamp = terminal.wait_count_above(key, before, 20.0)
            except TimeoutError as error:
                errors.append({"step": f"echo[{index}]", "error": str(error)})
                continue
            samples.append(timestamp * 1000.0)
        if samples:
            measured["input_echo_ms"] = samples
        time.sleep(idle_seconds)
        measured["idle_rss_bytes"] = process_rss_bytes(terminal.process.pid)
        terminal.send("\r")
        terminal.send("ping\r")
        try:
            terminal.wait_text(TURN_TEXT, 120.0)
            measured["post_turn_rss_bytes"] = process_rss_bytes(terminal.process.pid)
        except TimeoutError as error:
            errors.append({"step": "turn", "error": str(error)})
    finally:
        terminal.close()
    return measured


def measure_direct(tui: str, root: str, base_url: str, errors: list[dict]) -> float | None:
    """First frame of `horon-tui` alone; None when it never drew."""
    env = client_env(root, base_url)
    terminal = Terminal([tui], cwd=root, env=env)
    try:
        return terminal.wait_text(FIRST_SCREEN, 120.0) * 1000.0
    except TimeoutError as error:
        errors.append({"step": "direct-first-frame", "error": str(error)})
        return None
    finally:
        terminal.close()


# ---------------------------------------------------------------------------
# Render bench and size
# ---------------------------------------------------------------------------


def run_render_bench(path: str, runs: int, errors: list[dict]) -> dict | None:
    """Run the `render_bench` example and keep the median of its own figures."""
    p50: list[float] = []
    p95: list[float] = []
    detail: dict | None = None
    for index in range(runs):
        try:
            completed = subprocess.run(
                [path], capture_output=True, text=True, timeout=300, check=False
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            errors.append({"step": f"render[{index}]", "error": str(error)})
            continue
        if completed.returncode != 0:
            errors.append({"step": f"render[{index}]",
                           "error": f"exit {completed.returncode}: {completed.stderr.strip()[:400]}"})
            continue
        try:
            decoded = json.loads(completed.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError) as error:
            errors.append({"step": f"render[{index}]", "error": f"unreadable output: {error}"})
            continue
        if not isinstance(decoded, dict):
            errors.append({"step": f"render[{index}]", "error": "output is not a JSON object"})
            continue
        try:
            p50.append(float(decoded["render_p50_ms"]))
            p95.append(float(decoded["render_p95_ms"]))
        except (KeyError, TypeError, ValueError) as error:
            errors.append({"step": f"render[{index}]", "error": f"missing metric: {error}"})
            continue
        detail = {
            "frames": decoded.get("frames"),
            "width": decoded.get("width"),
            "height": decoded.get("height"),
            "runs": len(p50),
        }
    if not p50 or detail is None:
        return None
    return {
        "render_p50_ms": summary(p50),
        "render_p95_ms": summary(p95),
        "render": detail,
    }


def install_size(path: str | None) -> int | None:
    if not path:
        return None
    try:
        return os.path.getsize(path)
    except OSError:
        return None


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


def measure(args: argparse.Namespace) -> dict:
    errors: list[dict] = []
    skipped: list[dict] = []
    metrics: dict = {}

    render_path = args.render_bench if args.render_bench and os.path.isfile(args.render_bench) else None
    render_reason = render_skip_reason(render_path)
    if render_reason:
        skipped.append({"name": "render", "reason": render_reason})
    else:
        measured = run_render_bench(render_path, args.runs, errors)
        if measured:
            metrics.update(measured)

    size = install_size(args.tui if args.tui and os.path.isfile(args.tui) else None)
    if size is not None:
        metrics["install_size_bytes"] = size

    tui = args.tui if args.tui and os.path.isfile(args.tui) else None
    agent = args.binary if args.binary and os.path.isfile(args.binary) else None
    pty_reason = pty_skip_reason(sys.platform, agent, tui)
    if pty_reason:
        skipped.append({"name": "pty", "reason": pty_reason})
        return report(**metrics, skipped=skipped, errors=errors)
    if tui is None or agent is None:  # pragma: no cover - pty_skip_reason covered both
        return report(**metrics, skipped=skipped, errors=errors)

    server, base_url = start_mock_server()
    root = common.short_temp_dir("htui-")
    try:
        first_frames: list[float] = []
        direct_frames: list[float] = []
        echoes: list[float] = []
        idle: list[float] = []
        post_turn: list[float] = []
        for index in range(args.runs):
            run_root = os.path.join(root, f"run{index}")
            os.makedirs(run_root, exist_ok=True)
            measured = measure_bare(agent, run_root, base_url, args.idle_seconds,
                                    args.echo_samples, errors)
            if "first_frame_ms" in measured:
                first_frames.append(measured["first_frame_ms"])
            echoes.extend(measured.get("input_echo_ms") or [])
            for key, bucket in (("idle_rss_bytes", idle), ("post_turn_rss_bytes", post_turn)):
                if measured.get(key) is not None:
                    bucket.append(float(measured[key]))
            direct_root = os.path.join(root, f"direct{index}")
            os.makedirs(direct_root, exist_ok=True)
            direct = measure_direct(tui, direct_root, base_url, errors)
            if direct is not None:
                direct_frames.append(direct)

        metrics["first_frame_ms"] = summary(first_frames)
        metrics["direct_first_frame_ms"] = summary(direct_frames)
        handoff = paired_differences(first_frames, direct_frames)
        metrics["handoff_ms"] = summary(handoff)
        metrics["input_echo_p95_ms"] = echo_summary(echoes)
        metrics["idle_rss_bytes"] = summary(idle)
        metrics["post_turn_rss_bytes"] = summary(post_turn)
    finally:
        if server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
        shutil.rmtree(root, ignore_errors=True)
    return report(**metrics, skipped=skipped, errors=errors)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", help="path to the built horon-agent binary")
    parser.add_argument("--tui", help="path to the built horon-tui binary")
    parser.add_argument("--render-bench", help="path to the built render_bench example")
    parser.add_argument("--runs", type=int, default=5, help="repeats per PTY and render metric")
    parser.add_argument("--idle-seconds", type=float, default=5.0,
                        help="wait between the first frame and the idle RSS sample")
    parser.add_argument("--echo-samples", type=int, default=10,
                        help="keystrokes timed for the echo metric")
    parser.add_argument("--json-out", help="write the report here")
    parser.add_argument("--markdown-out", help="write the table here")
    args = parser.parse_args(argv)

    if args.runs < 5:
        parser.error("--runs must be at least 5: a median over fewer runs is not a measurement")

    document = measure(args)
    text = markdown(document)
    if args.json_out:
        common.write_json(args.json_out, document)
    if args.markdown_out:
        common.write_markdown(args.markdown_out, text)
    if not args.json_out and not args.markdown_out:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
