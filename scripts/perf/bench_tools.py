#!/usr/bin/env python3
"""Per-tool latency benchmark over the horon-agent debug socket.

Port of jcode's `scripts/benchmark_tools.sh`. That script piped JSON at the debug
socket through `socat` and timed each shell round-trip; this one speaks the daemon
socket directly (no socat, no jq, no shell startup in the measurement) so the
number is the server's tool dispatch cost plus one request/response frame.

The whole run is isolated: a throwaway HOME, runtime dir, and fixture workspace
under /tmp, a daemon started on that runtime dir, and one session whose working
directory is the fixture tree. Nothing touches the user's real server, config,
or repository, and no model is called (tools run through
`Agent::execute_tool`, not through a turn).

Measurement
-----------
For every case the script sends
`{"type":"debug_command","command":"tool:<name> <json>","session_id":...}`
and waits for the single `debug_response` line. `latency_ms` is measured from
just before `sendall` to the moment the terminating newline is read, so it
includes the frame write, the tool execution, and the read.

Failure handling
----------------
A tool that answers `ok:false` is recorded with `error` and a null latency
instead of a fast zero, exactly like the memory gate: a tool that refuses to run
must never look like a win. A binary that is missing skips the suite with exit
code 0.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_common as common  # noqa: E402

SUITE = "tools"
SMALL_LINES = 5
LARGE_LINES = 1000
GREP_TREE_FILES = 40
GREP_TREE_LINES = 120
WRITE_SMALL = "hello world\n"
WRITE_LARGE = "x" * 10_000
# The bash tool runs `cmd.exe` on Windows, which has neither `true` nor `ls`.
# The cases keep their labels so one row means the same work on every OS.
SHELL_TRUE = "exit 0" if common.IS_WINDOWS else "true"
SHELL_LS_TREE = "dir /b tree" if common.IS_WINDOWS else "ls tree"


# ---------------------------------------------------------------------------
# Fixture workspace
# ---------------------------------------------------------------------------


def private_dir(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    os.chmod(path, 0o700)
    return path


def write_private(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")
    os.chmod(path, 0o600)


def build_fixture(root: Path) -> dict[str, Path]:
    """A fixed workspace so every run searches the same bytes on every machine."""
    private_dir(root)
    tiny = root / "tiny.txt"
    write_private(tiny, "hello world\n")
    large = root / "large.txt"
    write_private(
        large,
        "".join(
            "Line %d: This is a test line with some content for benchmarking purposes.\n" % index
            for index in range(LARGE_LINES)
        ),
    )
    write_private(root / "edit_test.txt", "The quick brown fox jumps over the lazy dog\n")
    tree = root / "tree"
    private_dir(tree)
    for index in range(GREP_TREE_FILES):
        body = []
        for line in range(GREP_TREE_LINES):
            if line % 30 == 0:
                body.append("pub fn handler_%d_%d() { /* fn main dispatch */ }" % (index, line))
            elif line % 7 == 0:
                body.append("impl Block%d { fn run(&self) -> Result<()> { Ok(()) } }" % line)
            else:
                body.append("// filler line %d in module %d" % (line, index))
        write_private(tree / ("module_%02d.rs" % index), "\n".join(body) + "\n")
    write_private(root / "write_target.txt", "")
    return {"tiny": tiny, "large": large, "edit": edit_target(root), "tree": tree, "root": root}


def edit_target(root: Path) -> Path:
    return root / "edit_test.txt"


def reset_edit(root: Path) -> None:
    write_private(edit_target(root), "The quick brown fox jumps over the lazy dog\n")


# ---------------------------------------------------------------------------
# Debug socket client
# ---------------------------------------------------------------------------


class DebugSocket:
    def __init__(self, path: str, session_id: str, timeout: float = 120.0):
        self.path = path
        self.session_id = session_id
        self.timeout = timeout

    def call(self, command: str) -> tuple[float, int, dict]:
        """Send one debug command; return (latency_ms, response_bytes, payload)."""
        started = time.perf_counter()
        stream = None
        try:
            stream = common.ipc_connect(self.path, timeout=self.timeout)
            request = json.dumps(
                {"type": "debug_command", "id": 1, "command": command, "session_id": self.session_id}
            )
            stream.sendall((request + "\n").encode("utf-8"))
            buffer = bytearray()
            while b"\n" not in buffer:
                chunk = stream.recv(1 << 20)
                if not chunk:
                    break
                buffer.extend(chunk)
        finally:
            if stream is not None:
                stream.close()
        latency_ms = (time.perf_counter() - started) * 1000.0
        line = bytes(buffer).split(b"\n", 1)[0]
        try:
            payload = json.loads(line) if line.strip() else {}
        except ValueError:
            payload = {"type": "unparsed", "raw": line[:200].decode("utf-8", "replace")}
        return latency_ms, len(line), payload


def connect_with_retry(path: str, timeout_s: float) -> None:
    if not common.wait_for_socket(path, timeout_s):
        raise RuntimeError("debug socket never accepted a connection: %s" % path)


# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------


def tool_command(name: str, payload: dict) -> str:
    return "tool:%s %s" % (name, json.dumps(payload, separators=(",", ":")))


def build_cases(root: Path) -> list[dict]:
    tree = root / "tree"
    return [
        {
            "label": "read (tiny file)",
            "tool": "read",
            "payload": lambda: {"file_path": "tiny.txt"},
        },
        {
            "label": "read (1000 lines)",
            "tool": "read",
            "payload": lambda: {"file_path": "large.txt"},
        },
        {
            "label": "read (10 lines @ offset)",
            "tool": "read",
            "payload": lambda: {"file_path": "large.txt", "offset": 500, "limit": 10},
        },
        {
            "label": "read (repo Cargo.toml)",
            "tool": "read",
            "payload": None,  # filled with an absolute path by the caller
        },
        {
            "label": "write (small)",
            "tool": "write",
            "payload": lambda: {"file_path": "write_target.txt", "content": WRITE_SMALL},
        },
        {
            "label": "write (10KB)",
            "tool": "write",
            "payload": lambda: {"file_path": "write_target.txt", "content": WRITE_LARGE},
        },
        {
            "label": "edit (simple replace)",
            "tool": "edit",
            "payload": lambda: {
                "file_path": "edit_test.txt",
                "old_string": "quick brown",
                "new_string": "slow red",
            },
            "reset": lambda: reset_edit(root),
        },
        {
            "label": "ls (scripts dir)",
            "tool": "ls",
            "payload": None,
        },
        {
            "label": "agentgrep (grep in tree)",
            "tool": "agentgrep",
            "payload": lambda: {"mode": "grep", "query": "fn main", "path": str(tree)},
        },
        {
            "label": "agentgrep (find **/*.rs)",
            "tool": "agentgrep",
            "payload": lambda: {"mode": "find", "glob": "**/*.rs", "path": str(tree)},
        },
        {
            "label": "bash (true)",
            "tool": "bash",
            "payload": lambda: {"command": SHELL_TRUE},
        },
        {
            "label": "bash (echo hello)",
            "tool": "bash",
            "payload": lambda: {"command": "echo hello"},
        },
        {
            "label": "bash (ls fixture tree)",
            "tool": "bash",
            "payload": lambda: {"command": SHELL_LS_TREE},
        },
        {
            "label": "invalid tool (dispatch only)",
            "tool": "benchmark_no_such_tool",
            "payload": lambda: {"probe": True},
            "expect_error": True,
        },
    ]


def resolve_payloads(cases: list[dict], repo: Path, root: Path) -> None:
    for case in cases:
        if case["payload"] is None:
            if case["tool"] == "read":
                case["payload"] = lambda repo=repo: {"file_path": str(repo / "Cargo.toml")}
            elif case["tool"] == "ls":
                case["payload"] = lambda repo=repo: {"path": str(repo / "scripts")}
            else:
                raise AssertionError("case %s needs an explicit payload" % case["label"])


def summarize_case(
    label: str,
    tool: str,
    latencies: list[float],
    output_bytes: list[int],
    input_bytes: int,
    errors: list[str],
) -> dict:
    stats = common.summarize(latencies)
    return {
        "name": label,
        "tool": tool,
        "latency_ms": stats,
        "input_bytes": input_bytes,
        "output_bytes": common.summarize(output_bytes),
        "errors": errors,
        "error": None if stats is not None else (errors[0] if errors else "no successful sample"),
    }


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------


def start_daemon(binary: str, root: Path, repo: Path, log_path: Path) -> tuple[subprocess.Popen, dict]:
    # isolated_home_env points HOME, HORON_HOME, HORON_AGENT_RUNTIME_DIR and
    # every XDG dir at the throwaway root, so nothing reads or writes the user's
    # real ~/.horon. It creates the dirs with the process umask, and the
    # session store refuses anything that is not exactly owner-private.
    env = common.isolated_home_env(str(root), "HORON_AGENT", home_var="HORON_HOME")
    env["HORON_AGENT_DEBUG_CONTROL"] = "1"
    for key in ("HORON_HOME", "HORON_AGENT_RUNTIME_DIR"):
        private_dir(Path(env[key]))
    private_dir(Path(env["HORON_HOME"]) / "sessions")
    socket_path = os.path.join(env["HORON_AGENT_RUNTIME_DIR"], "horon-agent.sock")
    log = open(log_path, "w", encoding="utf-8")
    process = subprocess.Popen(
        [binary, "--no-update", "--socket", socket_path, "serve"],
        cwd=str(repo),
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        **common.new_group_kwargs(),
    )
    log.close()
    return process, env


def ensure_session(debug: str, work_dir: Path) -> tuple[str, str]:
    """Create a session bound to the fixture tree; return (session_id, error)."""
    client = DebugSocket(debug, "")
    _, _, payload = client.call("create_session:" + str(work_dir))
    if payload.get("type") != "debug_response" or not payload.get("ok"):
        return "", str(payload.get("output") or payload.get("message") or payload)
    try:
        return json.loads(payload["output"])["session_id"], ""
    except (KeyError, ValueError) as error:
        return "", "create_session returned unusable output: %s" % error


def run(args: argparse.Namespace) -> int:
    started_at = common.utc_now_iso()
    results: list[dict] = []
    skipped: list[dict] = []

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        skipped.append({"name": SUITE, "reason": "horon-agent binary not found at %s" % binary})
        return finish(args, results, skipped, started_at)

    repo = Path(args.repo).resolve() if args.repo else Path(__file__).resolve().parents[2]
    root = Path(common.short_temp_dir("hb-tools-"))
    process = None
    try:
        fixture = build_fixture(root / "workspace")
        log_path = root / "serve.log"
        process, env = start_daemon(binary, root, repo, log_path)
        socket_path = os.path.join(env["HORON_AGENT_RUNTIME_DIR"], "horon-agent.sock")
        debug_path = socket_path.replace(".sock", "-debug.sock")
        if not common.wait_for_socket(socket_path, args.startup_timeout):
            tail = read_tail(log_path)
            skipped.append(
                {"name": SUITE, "reason": "daemon never opened %s%s" % (socket_path, tail)}
            )
            return finish(args, results, skipped, started_at)
        connect_with_retry(debug_path, args.startup_timeout)

        session_id, error = ensure_session(debug_path, fixture["root"])
        if error:
            skipped.append({"name": SUITE, "reason": "could not create a session: %s" % error})
            return finish(args, results, skipped, started_at)

        client = DebugSocket(debug_path, session_id, timeout=args.timeout)
        cases = build_cases(fixture["root"])
        resolve_payloads(cases, repo, fixture["root"])

        for case in cases:
            if case.get("reset"):
                case["reset"]()
            command = tool_command(case["tool"], case["payload"]())
            input_bytes = len(command)
            latencies: list[float] = []
            output_bytes: list[int] = []
            errors: list[str] = []
            expect_error = bool(case.get("expect_error"))

            for _ in range(args.warmup):
                if case.get("reset"):
                    case["reset"]()
                try:
                    client.call(command)
                except OSError:
                    break

            for _ in range(args.iterations):
                if case.get("reset"):
                    case["reset"]()
                try:
                    latency_ms, size, payload = client.call(command)
                except (OSError, socket.timeout) as error:
                    errors.append("%s: %s" % (type(error).__name__, error))
                    continue
                ok = bool(payload.get("ok"))
                if ok == expect_error:
                    detail = payload.get("output") or payload.get("message") or "unexpected ok state"
                    errors.append(str(detail)[:200])
                    continue
                latencies.append(latency_ms)
                output_bytes.append(size)

            results.append(
                summarize_case(case["label"], case["tool"], latencies, output_bytes, input_bytes, errors)
            )
            common.eprint("bench_tools: %-34s %s" % (case["label"], render_stats(latencies)))
    finally:
        if process is not None:
            common.terminate_pgroup(process.pid)
        shutil.rmtree(root, ignore_errors=True)
    return finish(args, results, skipped, started_at)


def render_stats(values: list[float]) -> str:
    stats = common.summarize(values)
    if stats is None:
        return "no samples"
    return "median=%.1fms p95=%.1fms max=%.1fms n=%d" % (
        stats["median"],
        stats["p95"],
        stats["max"],
        stats["samples"],
    )


def read_tail(path: Path, lines: int = 12) -> str:
    try:
        text = path.read_text(encoding="utf-8", errors="replace").strip().splitlines()
    except OSError:
        return ""
    if not text:
        return ""
    return "\n  server log tail:\n    " + "\n    ".join(text[-lines:])


def markdown(results: list[dict], skipped: list[dict]) -> str:
    lines = ["### Tool latency", ""]
    if skipped:
        lines.append("Skipped: " + "; ".join("%s (%s)" % (s["name"], s["reason"]) for s in skipped))
        lines.append("")
    rows = []
    for row in results:
        stats = row["latency_ms"]
        rows.append(
            [
                row["name"],
                common.fmt_ms(stats["median"]) if stats else "—",
                common.fmt_ms(stats["p95"]) if stats else "—",
                common.fmt_ms(stats["min"]) if stats else "—",
                common.fmt_ms(stats["max"]) if stats else "—",
                str(stats["samples"]) if stats else "0",
            ]
        )
    lines.append(common.markdown_table(["Tool case", "median", "p95", "min", "max", "samples"], rows))
    failed = [row["name"] for row in results if row["latency_ms"] is None]
    if failed:
        lines.append("")
        lines.append("Unmeasured: " + ", ".join(failed))
    return "\n".join(lines)


def finish(args: argparse.Namespace, results: list[dict], skipped: list[dict], started_at: str) -> int:
    report = common.build_report(SUITE, results, skipped, started_at)
    if args.json_out:
        common.write_json(args.json_out, report)
    if args.markdown_out:
        common.write_markdown(args.markdown_out, markdown(results, skipped))
    print(json.dumps({k: report[k] for k in ("suite", "schema", "host", "started_at")}, indent=2))
    if skipped and not results:
        common.eprint("bench_tools: skipped — %s" % skipped[0]["reason"])
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", default="./target/release/horon-agent")
    parser.add_argument("--repo", default=None, help="checkout to read from (default: this repository)")
    parser.add_argument("--iterations", type=int, default=5, help="measured calls per case")
    parser.add_argument("--warmup", type=int, default=1, help="discarded warmup calls per case")
    parser.add_argument("--startup-timeout", type=float, default=60.0)
    parser.add_argument("--timeout", type=float, default=120.0, help="per-call socket deadline")
    parser.add_argument("--json-out", default=None)
    parser.add_argument("--markdown-out", default=None)
    args = parser.parse_args(argv)
    if args.iterations < 1:
        parser.error("--iterations must be positive")
    if args.warmup < 0:
        parser.error("--warmup must not be negative")
    return args


def main(argv: list[str]) -> int:
    return run(parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
