#!/usr/bin/env python3
"""Benchmark and optionally regression-check horon-agent startup time.

Every measurement runs under a temporary HORON_AGENT_HOME and
HORON_AGENT_RUNTIME_DIR so it does not interfere with the user's real server,
logs, or credentials.

Two surfaces are measured:

- server ready: `horon-agent serve` accepts a connection on its daemon socket.
- harness bridge readiness: `horon-agent api-bridge` reaches the stable harness
  API in three stages, timed separately so a regression names its stage:
  the API socket becomes connectable, the bridge answers `hello` with
  `hello_ok`, and a no-op request (`list_sessions`, limit 1) returns its reply.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import Iterable

API_VERSION_MAJOR = 1
STAGE_TIMEOUT_S = 20.0

@dataclass
class BridgeReadiness:
    socket_ms: float | None
    hello_ms: float | None
    noop_ms: float | None
    total_ms: float | None
    error: str | None = None

@dataclass
class Budget:
    name: str
    actual_ms: float
    limit_ms: float

def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default="./target/release/horon-agent")
    parser.add_argument("--runs", type=int, default=5, help="number of startup runs")
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if startup budgets are exceeded",
    )
    parser.add_argument("--max-help-ms", type=float, default=20.0)
    parser.add_argument("--max-version-ms", type=float, default=20.0)
    parser.add_argument("--max-server-ready-ms", type=float, default=80.0)
    parser.add_argument("--max-bridge-socket-ms", type=float, default=400.0)
    parser.add_argument("--max-bridge-hello-ms", type=float, default=600.0)
    parser.add_argument("--max-bridge-noop-ms", type=float, default=800.0)
    parser.add_argument("--max-bridge-total-ms", type=float, default=900.0)
    return parser.parse_args()

def median(values: Iterable[float]) -> float:
    vals = list(values)
    if not vals:
        raise ValueError("no values")
    return statistics.median(vals)

def median_or_none(values: Iterable[float]) -> float | None:
    vals = list(values)
    if not vals:
        return None
    return statistics.median(vals)

def print_stats(name: str, times: list[float]) -> None:
    if not times:
        print(f"\n{name}: No successful runs")
        return
    print(f"\n{name}:")
    print(f"  Min:    {min(times):.2f} ms")
    print(f"  Max:    {max(times):.2f} ms")
    print(f"  Mean:   {statistics.mean(times):.2f} ms")
    print(f"  Median: {statistics.median(times):.2f} ms")
    if len(times) > 1:
        print(f"  Stdev:  {statistics.stdev(times):.2f} ms")

def run_simple_timing(binary: str, *args: str, runs: int) -> list[float]:
    times: list[float] = []
    for _ in range(runs):
        start = time.perf_counter()
        subprocess.run([binary, *args], capture_output=True, check=False)
        times.append((time.perf_counter() - start) * 1000)
    return times

def isolated_env(root: str) -> dict[str, str]:
    env = os.environ.copy()
    env["HORON_AGENT_HOME"] = os.path.join(root, "home")
    env["HORON_AGENT_RUNTIME_DIR"] = os.path.join(root, "run")
    env["HORON_AGENT_SOCKET"] = os.path.join(env["HORON_AGENT_RUNTIME_DIR"], "horon-agent.sock")
    os.makedirs(env["HORON_AGENT_HOME"], exist_ok=True)
    os.makedirs(env["HORON_AGENT_RUNTIME_DIR"], exist_ok=True)
    return env

def wait_for_socket(path: str, timeout_s: float) -> bool:
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        if os.path.exists(path):
            try:
                sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                sock.connect(path)
                sock.close()
                return True
            except OSError:
                pass
        time.sleep(0.005)
    return False

def measure_server_startup(binary: str, runs: int) -> list[float]:
    times: list[float] = []
    for _ in range(runs):
        root = tempfile.mkdtemp(prefix="horon-agent-server-bench-")
        env = isolated_env(root)
        socket_path = env["HORON_AGENT_SOCKET"]
        proc = None
        try:
            start = time.perf_counter()
            proc = subprocess.Popen(
                [binary, "serve"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env=env,
            )
            if wait_for_socket(socket_path, 5.0):
                times.append((time.perf_counter() - start) * 1000)
        finally:
            if proc is not None:
                proc.terminate()
                try:
                    proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=2)
            shutil.rmtree(root, ignore_errors=True)
    return times

def api_socket_path(env: dict[str, str]) -> str:
    return os.path.join(env["HORON_AGENT_RUNTIME_DIR"], "horon-agent-api.sock")

def send_frame(stream: socket.socket, frame: dict) -> None:
    stream.sendall((json.dumps(frame) + "\n").encode())

def read_frames_until(
    stream: socket.socket, predicate, deadline: float, buffer: bytearray
) -> dict | None:
    while time.perf_counter() < deadline:
        while b"\n" in buffer:
            line, _, rest = bytes(buffer).partition(b"\n")
            buffer.clear()
            buffer.extend(rest)
            if not line.strip():
                continue
            try:
                frame = json.loads(line)
            except json.JSONDecodeError:
                continue
            if predicate(frame):
                return frame
        stream.settimeout(max(0.001, deadline - time.perf_counter()))
        try:
            chunk = stream.recv(1 << 16)
        except (TimeoutError, socket.timeout):
            return None
        except OSError:
            return None
        if not chunk:
            return None
        buffer.extend(chunk)
    return None

def measure_bridge_readiness_once(binary: str) -> BridgeReadiness:
    # Short prefix on purpose: a Unix socket path is capped near 104 bytes on
    # macOS, and the shared long prefixes push the API socket past SUN_LEN.
    root = tempfile.mkdtemp(prefix="ha-bridge-")
    env = isolated_env(root)
    api_socket = api_socket_path(env)
    proc = None
    stream: socket.socket | None = None
    try:
        start = time.perf_counter()
        proc = subprocess.Popen(
            [binary, "--no-update", "api-bridge", "--api-socket", api_socket],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env=env,
            stdin=subprocess.DEVNULL,
            preexec_fn=os.setsid if hasattr(os, "setsid") else None,
        )

        if not wait_for_socket(api_socket, STAGE_TIMEOUT_S):
            return BridgeReadiness(None, None, None, None, "api socket never became connectable")
        socket_ms = (time.perf_counter() - start) * 1000

        stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        stream.connect(api_socket)
        buffer = bytearray()
        send_frame(
            stream,
            {
                "v": API_VERSION_MAJOR,
                "id": 1,
                "req": "hello",
                "min_version": API_VERSION_MAJOR,
                "max_version": API_VERSION_MAJOR,
                "client": "bench-startup",
            },
        )
        hello = read_frames_until(
            stream,
            lambda frame: frame.get("ev") == "hello_ok",
            time.perf_counter() + STAGE_TIMEOUT_S,
            buffer,
        )
        if hello is None:
            return BridgeReadiness(socket_ms, None, None, None, "no hello_ok reply from the bridge")
        hello_ms = (time.perf_counter() - start) * 1000

        send_frame(stream, {"v": API_VERSION_MAJOR, "id": 2, "req": "list_sessions", "limit": 1})
        reply = read_frames_until(
            stream,
            lambda frame: frame.get("reply_to") == 2,
            time.perf_counter() + STAGE_TIMEOUT_S,
            buffer,
        )
        if reply is None:
            return BridgeReadiness(socket_ms, hello_ms, None, None, "no reply to the no-op request")
        noop_ms = (time.perf_counter() - start) * 1000

        return BridgeReadiness(socket_ms, hello_ms, noop_ms, noop_ms)
    except OSError as error:
        return BridgeReadiness(None, None, None, None, f"{type(error).__name__}: {error}")
    finally:
        if stream is not None:
            try:
                stream.close()
            except OSError:
                pass
        if proc is not None:
            try:
                if hasattr(os, "killpg"):
                    os.killpg(os.getpgid(proc.pid), 15)
                else:
                    proc.terminate()
                proc.wait(timeout=5)
            except (subprocess.TimeoutExpired, ProcessLookupError, OSError):
                proc.kill()
                try:
                    proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    pass
        shutil.rmtree(root, ignore_errors=True)

def measure_bridge_startup(binary: str, runs: int) -> list[BridgeReadiness]:
    results: list[BridgeReadiness] = []
    for _ in range(runs):
        readiness = measure_bridge_readiness_once(binary)
        if readiness.error:
            print(f"  bridge run failed: {readiness.error}", file=sys.stderr)
        results.append(readiness)
    return results

def print_bridge_stats(results: list[BridgeReadiness]) -> None:
    print_stats(
        "Bridge stage: api socket connectable",
        [r.socket_ms for r in results if r.socket_ms is not None],
    )
    print_stats("Bridge stage: hello_ok", [r.hello_ms for r in results if r.hello_ms is not None])
    print_stats("Bridge stage: no-op reply", [r.noop_ms for r in results if r.noop_ms is not None])
    print_stats("Bridge total (socket -> no-op)", [r.total_ms for r in results if r.total_ms is not None])

def collect_budgets(
    help_times: list[float],
    version_times: list[float],
    server_times: list[float],
    bridge_results: list[BridgeReadiness],
    args: argparse.Namespace,
) -> list[Budget]:
    budgets = [
        Budget("--help median", median(help_times), args.max_help_ms),
        Budget("--version median", median(version_times), args.max_version_ms),
    ]

    server_ready_median = median_or_none(server_times)
    if server_ready_median is not None:
        budgets.append(Budget("server ready median", server_ready_median, args.max_server_ready_ms))

    bridge_stages = [
        ("bridge socket median", [r.socket_ms for r in bridge_results], args.max_bridge_socket_ms),
        ("bridge hello median", [r.hello_ms for r in bridge_results], args.max_bridge_hello_ms),
        ("bridge no-op median", [r.noop_ms for r in bridge_results], args.max_bridge_noop_ms),
        ("bridge total median", [r.total_ms for r in bridge_results], args.max_bridge_total_ms),
    ]
    for name, values, limit in bridge_stages:
        stage_median = median_or_none(v for v in values if v is not None)
        if stage_median is not None:
            budgets.append(Budget(name, stage_median, limit))
    return budgets

def main() -> int:
    args = parse_args()
    binary = args.binary

    if not os.path.exists(binary):
        print(f"Binary not found: {binary}")
        print("Run: cargo build --release")
        return 1

    print(f"Benchmarking: {binary}")
    print("=" * 60)

    subprocess.run([binary, "--version"], capture_output=True, check=False)

    help_times = run_simple_timing(binary, "--help", runs=args.runs)
    print_stats("--help (binary load)", help_times)

    version_times = run_simple_timing(binary, "--version", runs=args.runs)
    print_stats("--version", version_times)

    print(f"\nMeasuring isolated server startup ({args.runs} runs)...")
    server_times = measure_server_startup(binary, args.runs)
    print_stats("Server ready (isolated socket connectable)", server_times)

    print(f"\nMeasuring harness bridge readiness ({args.runs} runs)...")
    bridge_results = measure_bridge_startup(binary, args.runs)
    print_bridge_stats(bridge_results)

    print("\n" + "=" * 60)
    print("Summary:")
    help_median = median_or_none(help_times)
    server_median = median_or_none(server_times)
    bridge_total_median = median_or_none(
        r.total_ms for r in bridge_results if r.total_ms is not None
    )
    print(
        f"  Binary load median:      ~{help_median:.1f} ms"
        if help_median is not None
        else "  Binary load median:      n/a"
    )
    print(
        f"  Server ready median:     ~{server_median:.1f} ms"
        if server_median is not None
        else "  Server ready median:     n/a"
    )
    print(
        f"  Bridge total median:     ~{bridge_total_median:.1f} ms"
        if bridge_total_median is not None
        else "  Bridge total median:     n/a"
    )

    budgets = collect_budgets(help_times, version_times, server_times, bridge_results, args)
    if args.check:
        failures = [b for b in budgets if b.actual_ms > b.limit_ms]
        print("\nBudget check:")
        for budget in budgets:
            status = "FAIL" if budget.actual_ms > budget.limit_ms else "PASS"
            print(
                f"  [{status}] {budget.name}: {budget.actual_ms:.1f} ms <= {budget.limit_ms:.1f} ms"
            )
        if failures:
            print("\nStartup regression detected.")
            return 2
        print("\nAll startup budgets passed.")

    return 0

if __name__ == "__main__":
    raise SystemExit(main())
