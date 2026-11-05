#!/usr/bin/env python3
"""Reload storm: N clients resume N sessions at once, timed to the `history` event.

Port of jcode's `scripts/bench_reload_storm.py`. The protocol is the same shape:
a reconnecting client sends `subscribe` naming `target_session_id`, and the
server answers with a `history` event carrying the restored transcript. The
number this measures is how long N such resumes take, individually and together.

CI has no live sessions, so the script builds them: `fixtures/gen_event_log.py`
writes real canonical `EventEnvelope` records into a throwaway
`HORON_AGENT_HOME/sessions/<id>/events.jsonl`. Every record carries its own
session id and the store refuses a log whose records name another session, so
each session gets its own generated log rather than a copy.

Two numbers are reported per concurrency level:
  * `history_ms`  one client's wall time from connect to its `history` frame.
  * `wall_ms`     the whole storm, from the first connect to the last `history`.

A client that gets an `error` frame or a timeout is reported in `failures` and
contributes no latency, so a refusal can never read as a fast resume.

Daemon reuse and accumulation
-----------------------------
Each concurrency level starts a fresh daemon (unless `--reuse-daemon`), and each
storm resumes sessions no earlier storm opened, so a restore from memory is never
reported as a cold resume. Within one level, repeated iterations still leave the
earlier resumed sessions live in the daemon, so a second iteration reads higher
than the first: `history_ms` is therefore aggregated per level over every
iteration, and `--iterations 1` is the cleanest setting for an absolute number.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_common as common  # noqa: E402

SUITE = "reload"
DEFAULT_EVENTS = 200
SESSION_PREFIX = "bench-storm"


def load_generator(script: Path):
    spec = importlib.util.spec_from_file_location("bench_fixture_generator", script)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load fixture generator at %s" % script)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def private(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    os.chmod(path, 0o700)
    return path


def build_sessions(home: Path, count: int, events: int, seed: int, generator) -> list[str]:
    """Write `count` distinct canonical event logs; return their session ids."""
    sessions_root = private(home / "sessions")
    ids: list[str] = []
    for index in range(count):
        session_id = "%s-%03d" % (SESSION_PREFIX, index)
        directory = private(sessions_root / session_id)
        log = directory / "events.jsonl"
        generator.write_events(log, events, seed + index, session_id)
        os.chmod(log, 0o600)
        ids.append(session_id)
    return ids


def resume_once(socket_path: str, session_id: str, cwd: str, chunk: int, deadline: float) -> dict:
    """Connect, subscribe to `session_id`, and time the `history` frame."""
    started = time.perf_counter()
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(deadline)
    try:
        stream.connect(socket_path)
        request = json.dumps(
            {
                "type": "subscribe",
                "id": 1,
                "working_dir": cwd,
                "target_session_id": session_id,
                "client_instance_id": "bench_" + session_id,
            }
        )
        stream.sendall((request + "\n").encode("utf-8"))
        buffer = bytearray()
        while time.perf_counter() - started < deadline:
            read_size = chunk if chunk > 0 else 1 << 16
            try:
                data = stream.recv(read_size)
            except (TimeoutError, socket.timeout):
                return {"session": session_id, "history_ms": None, "error": "timeout"}
            if not data:
                return {"session": session_id, "history_ms": None, "error": "eof"}
            buffer.extend(data)
            while b"\n" in buffer:
                line, _, rest = bytes(buffer).partition(b"\n")
                buffer.clear()
                buffer.extend(rest)
                if not line.strip():
                    continue
                try:
                    event = json.loads(line)
                except ValueError:
                    continue
                kind = event.get("type")
                if kind == "history":
                    return {
                        "session": session_id,
                        "history_ms": (time.perf_counter() - started) * 1000.0,
                        "messages": len(event.get("messages") or []),
                        "bytes": len(line),
                        "error": None,
                    }
                if kind == "error":
                    return {
                        "session": session_id,
                        "history_ms": None,
                        "error": str(event.get("message") or event)[:200],
                    }
        return {"session": session_id, "history_ms": None, "error": "deadline"}
    except OSError as error:
        return {"session": session_id, "history_ms": None, "error": "%s: %s" % (type(error).__name__, error)}
    finally:
        stream.close()


def storm(socket_path: str, session_ids: list[str], cwd: str, chunk: int, deadline: float) -> dict:
    outcomes: list[dict | None] = [None] * len(session_ids)
    gate = threading.Barrier(len(session_ids) + 1) if session_ids else None

    def worker(index: int, session_id: str) -> None:
        try:
            gate.wait()
        except threading.BrokenBarrierError:
            pass
        outcomes[index] = resume_once(socket_path, session_id, cwd, chunk, deadline)

    threads = [threading.Thread(target=worker, args=(i, sid), daemon=True) for i, sid in enumerate(session_ids)]
    for thread in threads:
        thread.start()
    started = time.perf_counter()
    if gate is not None:
        gate.wait()
    for thread in threads:
        thread.join()
    wall_ms = (time.perf_counter() - started) * 1000.0

    results = [outcome for outcome in outcomes if outcome is not None]
    latencies = [item["history_ms"] for item in results if item["history_ms"] is not None]
    failures = [
        {"session": item["session"], "error": item["error"]}
        for item in results
        if item["history_ms"] is None
    ]
    return {
        "sessions": len(session_ids),
        "history_ms": common.summarize(latencies),
        "wall_ms": wall_ms,
        "bytes": common.summarize([item.get("bytes") or 0 for item in results if item["history_ms"] is not None]),
        "failures": failures,
        "error": None if latencies else (failures[0]["error"] if failures else "no sample"),
        "_latencies": latencies,
    }


def start_daemon(binary: str, root: Path, repo: Path, log_path: Path) -> tuple[subprocess.Popen, str, str]:
    # Every store, cache, and socket lives under the throwaway root; the user's
    # real ~/.horon-agent is never read or written.
    env = common.isolated_home_env(str(root), "HORON_AGENT")
    env["HORON_AGENT_DEBUG_CONTROL"] = "1"
    home = private(Path(env["HORON_AGENT_HOME"]))
    runtime = private(Path(env["HORON_AGENT_RUNTIME_DIR"]))
    private(home / "sessions")
    socket_path = str(runtime / "horon-agent.sock")
    log = open(log_path, "w", encoding="utf-8")
    process = subprocess.Popen(
        [binary, "--no-update", "--socket", socket_path, "serve"],
        cwd=str(repo),
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    log.close()
    return process, socket_path, env["HORON_AGENT_HOME"]


def parse_counts(raw: str) -> list[int]:
    counts = []
    for part in raw.split(","):
        part = part.strip()
        if not part:
            continue
        try:
            value = int(part)
        except ValueError:
            raise SystemExit("--sessions must be comma-separated integers, got %r" % part)
        if value < 1:
            raise SystemExit("--sessions values must be positive")
        counts.append(value)
    if not counts:
        raise SystemExit("--sessions must not be empty")
    return counts


def markdown(results: list[dict], skipped: list[dict], iterations: int) -> str:
    lines = ["### Reload storm", ""]
    if skipped:
        lines.append("Skipped: " + "; ".join("%s (%s)" % (s["name"], s["reason"]) for s in skipped))
        lines.append("")
    lines.append(
        "%d resume(s) per level; each level resumes sessions no other level has opened, from a %s daemon."
        % (iterations, "reused" if iterations and results and results[0].get("reused_daemon") else "freshly started")
    )
    lines.append("")
    rows = []
    for row in results:
        stats = row["history_ms"]
        wall = row["wall_ms"]
        wall_text = common.fmt_ms(wall["median"]) if isinstance(wall, dict) else common.fmt_ms(wall)
        rows.append(
            [
                str(row["sessions"]),
                common.fmt_ms(stats["median"]) if stats else "—",
                common.fmt_ms(stats["p95"]) if stats else "—",
                common.fmt_ms(stats["max"]) if stats else "—",
                wall_text,
                str(row.get("failure_count", len(row["failures"]))),
            ]
        )
    lines.append(common.markdown_table(["Concurrent resumes", "history median", "p95", "max", "storm wall", "failures"], rows))
    failed = [row for row in results if row["history_ms"] is None]
    if failed:
        lines.append("")
        lines.append("Unmeasured: " + ", ".join(str(row["sessions"]) for row in failed))
    return "\n".join(lines)


def finish(args: argparse.Namespace, results: list[dict], skipped: list[dict], started_at: str) -> int:
    report = common.build_report(SUITE, results, skipped, started_at)
    if args.json_out:
        common.write_json(args.json_out, report)
    if args.markdown_out:
        common.write_markdown(args.markdown_out, markdown(results, skipped, args.iterations))
    print(json.dumps({k: report[k] for k in ("suite", "schema", "host", "started_at")}, indent=2))
    if skipped and not results:
        common.eprint("bench_reload_storm: skipped — %s" % skipped[0]["reason"])
    return 0


def run(args: argparse.Namespace) -> int:
    started_at = common.utc_now_iso()
    results: list[dict] = []
    skipped: list[dict] = []

    binary = os.path.abspath(args.binary)
    if not os.path.exists(binary):
        skipped.append({"name": SUITE, "reason": "horon-agent binary not found at %s" % binary})
        return finish(args, results, skipped, started_at)

    generator_path = Path(args.fixture_script).resolve()
    if not generator_path.is_file():
        skipped.append({"name": SUITE, "reason": "fixture generator missing at %s" % generator_path})
        return finish(args, results, skipped, started_at)

    counts = parse_counts(args.sessions)
    generator = load_generator(generator_path)
    repo = Path(args.repo).resolve() if args.repo else Path(__file__).resolve().parents[2]
    root = Path(common.short_temp_dir("hb-reload-"))
    process = None
    try:
        log_path = root / "serve.log"
        process, socket_path, home = start_daemon(binary, root, repo, log_path)
        if not common.wait_for_socket(socket_path, args.startup_timeout):
            tail = read_tail(log_path)
            skipped.append({"name": SUITE, "reason": "daemon never opened %s%s" % (socket_path, tail)})
            return finish(args, results, skipped, started_at)

        total_runs = sum(counts) * args.iterations
        session_ids = build_sessions(Path(home), total_runs, args.events, args.seed, generator)
        common.eprint(
            "bench_reload_storm: %d generated session(s) for %d storm(s)"
            % (total_runs, len(counts) * args.iterations)
        )
        common.eprint(
            "bench_reload_storm: daemon %s per concurrency level"
            % ("reused" if args.reuse_daemon else "restarted")
        )

        cursor = 0
        for count in counts:
            if not args.reuse_daemon:
                # A daemon that already restored sessions answers the next
                # resume from memory, so a reused daemon would report warm
                # re-attaches as the cost of a cold resume. Each level starts
                # clean unless the caller asks to watch the accumulation.
                common.terminate_pgroup(process.pid)
                process, socket_path, _home = start_daemon(binary, root, repo, log_path)
                if not common.wait_for_socket(socket_path, args.startup_timeout):
                    tail = read_tail(log_path)
                    skipped.append(
                        {"name": "%s-%d" % (SUITE, count), "reason": "restarted daemon never opened its socket%s" % tail}
                    )
                    continue
            latencies: list[float] = []
            walls: list[float] = []
            failures: list[dict] = []
            errors: list[str] = []
            for attempt in range(args.iterations):
                # Every storm resumes sessions no other storm has opened, so a
                # warm re-attach never hides the cost of a cold resume.
                window = session_ids[cursor : cursor + count]
                cursor += count
                outcome = storm(socket_path, window, args.cwd, args.chunk, args.timeout)
                walls.append(outcome["wall_ms"])
                failures.extend(outcome["failures"])
                if outcome["error"]:
                    errors.append(outcome["error"])
                for item in outcome.get("_latencies") or []:
                    latencies.append(item)
                common.eprint(
                    "bench_reload_storm: n=%-3d run %d/%d history median=%s wall=%.0fms failures=%d"
                    % (
                        count,
                        attempt + 1,
                        args.iterations,
                        "—" if outcome["history_ms"] is None else "%.0fms" % outcome["history_ms"]["median"],
                        outcome["wall_ms"],
                        len(outcome["failures"]),
                    )
                )
            stats = common.summarize(latencies)
            results.append(
                {
                    "sessions": count,
                    "iterations": args.iterations,
                    "history_ms": stats,
                    "wall_ms": common.summarize(walls),
                    "failures": failures[:20],
                    "failure_count": len(failures),
                    "reused_daemon": args.reuse_daemon,
                    "error": None if stats is not None else (errors[0] if errors else "no successful resume"),
                }
            )
    finally:
        if process is not None:
            common.terminate_pgroup(process.pid)
        shutil.rmtree(root, ignore_errors=True)
    return finish(args, results, skipped, started_at)


def read_tail(path: Path, lines: int = 12) -> str:
    try:
        text = path.read_text(encoding="utf-8", errors="replace").strip().splitlines()
    except OSError:
        return ""
    if not text:
        return ""
    return "\n  server log tail:\n    " + "\n    ".join(text[-lines:])


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", default="./target/release/horon-agent")
    parser.add_argument("--repo", default=None, help="checkout the daemon runs in (default: this repository)")
    parser.add_argument("--sessions", default="1,10,40", help="comma-separated concurrency levels")
    parser.add_argument("--iterations", type=int, default=1, help="storms per concurrency level")
    parser.add_argument("--events", type=int, default=DEFAULT_EVENTS, help="records per generated session")
    parser.add_argument("--seed", type=int, default=20261021)
    parser.add_argument("--chunk", type=int, default=0, help="client read size in bytes (0 = 64 KiB reads)")
    parser.add_argument("--cwd", default=None, help="working_dir sent by the subscribing client")
    parser.add_argument("--timeout", type=float, default=60.0, help="per-client resume deadline")
    parser.add_argument("--startup-timeout", type=float, default=60.0)
    parser.add_argument(
        "--reuse-daemon",
        action="store_true",
        help="keep one daemon across concurrency levels (measures accumulation; default restarts)",
    )
    parser.add_argument(
        "--fixture-script",
        default=str(Path(__file__).resolve().parent / "fixtures" / "gen_event_log.py"),
    )
    parser.add_argument("--json-out", default=None)
    parser.add_argument("--markdown-out", default=None)
    args = parser.parse_args(argv)
    if args.iterations < 1:
        parser.error("--iterations must be positive")
    if args.events < 1:
        parser.error("--events must be positive")
    args.cwd = args.cwd or os.getcwd()
    return args


def main(argv: list[str]) -> int:
    return run(parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
