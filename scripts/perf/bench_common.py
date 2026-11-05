#!/usr/bin/env python3
"""Shared helpers for the horon comparison benchmarks.

Only the Python standard library is used so the same file runs on macOS arm64
and Linux x64 under the CI workflow. Nothing here reads or writes a user's real
agent configuration: callers pass an isolated HOME/XDG environment.

Exports:

- ``host_info``: os/arch/cpu/python snapshot embedded in every report.
- ``stats``: median/p95/min/max/sample-count for a list of measurements.
- ``process_tree_rss_bytes`` / ``process_tree_pss_bytes``: summed RSS (and PSS on
  Linux) of a process and all descendants.
- ``wait_for_socket`` / ``wait_for_port``: readiness polling.
- ``terminate_pgroup``: reliable kill of a whole process group.
- ``short_temp_dir``: temp dir under /tmp so unix socket paths stay < 104 bytes.
- ``linear_slope``: least-squares slope used for "each additional session".
- ``build_report`` / ``write_json`` / ``write_markdown``: report plumbing.
"""

from __future__ import annotations

import datetime
import json
import os
import platform
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Iterable

SCHEMA = 1


# ---------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------


def host_info() -> dict:
    return {
        "os": platform.system().lower(),
        "arch": platform.machine(),
        "cpu_count": os.cpu_count() or 1,
        "python": platform.python_version(),
    }


def short_temp_dir(prefix: str = "hb-") -> str:
    """Create a temp dir under /tmp with a short name.

    Unix socket paths are limited to ~104 bytes on macOS (SUN_LEN), and the
    default TMPDIR there is already ~50 characters, so a nested
    ``.../horon-agent.sock`` overflows. A short root under /tmp keeps every
    socket path well inside the limit.
    """
    root = "/tmp" if os.path.isdir("/tmp") else None
    path = tempfile.mkdtemp(prefix=prefix, dir=root)
    return os.path.realpath(path)


def isolated_home_env(root: str, prefix: str) -> dict[str, str]:
    """Base env pointing every agent home at ``root``."""
    env = os.environ.copy()
    home = os.path.join(root, "home")
    run = os.path.join(root, "run")
    cache = os.path.join(root, "cache")
    config = os.path.join(root, "config")
    data = os.path.join(root, "data")
    state = os.path.join(root, "state")
    for path in (home, run, cache, config, data, state):
        os.makedirs(path, exist_ok=True)
    env[prefix + "_HOME"] = home
    env[prefix + "_RUNTIME_DIR"] = run
    env.setdefault("HOME", home)
    env["HOME"] = home
    env["XDG_CACHE_HOME"] = cache
    env["XDG_CONFIG_HOME"] = config
    env["XDG_DATA_HOME"] = data
    env["XDG_STATE_HOME"] = state
    env["TMPDIR"] = os.path.join(root, "tmp")
    os.makedirs(env["TMPDIR"], exist_ok=True)
    return env


# ---------------------------------------------------------------------------
# Statistics
# ---------------------------------------------------------------------------


def summary_values(values: Iterable[float]) -> list[float]:
    return sorted(float(v) for v in values if v is not None)


def p95(values: Iterable[float]) -> float | None:
    """Nearest-rank 95th percentile; None for an empty series.

    Nearest rank keeps a handful of runs meaningful and never invents a value
    between samples, which matters when the workflow only repeats 5 times.
    """
    vals = summary_values(values)
    if not vals:
        return None
    if len(vals) == 1:
        return vals[0]
    rank = max(1, int(round(0.95 * len(vals) + 0.5)))
    return vals[min(rank, len(vals)) - 1]


def summarize(values: Iterable[float]) -> dict | None:
    """median/p95/min/max/sample count. Returns None for an empty series."""
    vals = summary_values(values)
    if not vals:
        return None
    return {
        "median": statistics.median(vals),
        "p95": p95(vals),
        "min": vals[0],
        "max": vals[-1],
        "samples": len(vals),
    }


# Alias kept for internal readability.
stats = summarize


def linear_slope(xs: list[float], ys: list[float]) -> float | None:
    """Least-squares slope of ys over xs; None when x has no spread."""
    if len(xs) != len(ys) or len(xs) < 2:
        return None
    mean_x = sum(xs) / len(xs)
    mean_y = sum(ys) / len(ys)
    denom = sum((x - mean_x) ** 2 for x in xs)
    if denom == 0:
        return None
    return sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys)) / denom


# ---------------------------------------------------------------------------
# Process memory
# ---------------------------------------------------------------------------


def _linux_children(pid: int) -> list[int]:
    kids: list[int] = []
    try:
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            try:
                with open(f"/proc/{entry}/stat", "r", encoding="utf-8", errors="replace") as fh:
                    fields = fh.read().rsplit(")", 1)[-1].split()
                # After the comm field: state, ppid, ...
                if len(fields) > 1 and int(fields[1]) == pid:
                    kids.append(int(entry))
            except (OSError, ValueError, IndexError):
                continue
    except OSError:
        return []
    return kids


def _macos_children(pid: int) -> list[int]:
    try:
        out = subprocess.run(
            ["pgrep", "-P", str(pid)], capture_output=True, text=True, timeout=5, check=False
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    return [int(line) for line in out.split() if line.strip().isdigit()]


def children_of(pid: int) -> list[int]:
    if os.path.isdir("/proc"):
        return _linux_children(pid)
    return _macos_children(pid)


def process_tree_pids(pid: int) -> list[int]:
    seen: set[int] = set()
    stack = [pid]
    while stack:
        cur = stack.pop()
        if cur in seen:
            continue
        seen.add(cur)
        stack.extend(children_of(cur))
    return sorted(seen)


def _linux_smaps(pid: int, field: str) -> int | None:
    total_kb = 0
    found = False
    try:
        with open(f"/proc/{pid}/smaps_rollup", "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if line.startswith(field + ":"):
                    total_kb += int(line.split()[1])
                    found = True
    except (OSError, ValueError):
        return None
    return total_kb * 1024 if found else None


def _linux_status_rss(pid: int) -> int | None:
    try:
        with open(f"/proc/{pid}/status", "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        return None
    return None


def _macos_rss(pid: int) -> int | None:
    try:
        out = subprocess.run(
            ["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True, timeout=5, check=False
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None
    if not out.isdigit():
        return None
    return int(out) * 1024


def process_tree_rss_bytes(pid: int) -> int:
    """Summed resident set size of ``pid`` and every descendant."""
    total = 0
    for child in process_tree_pids(pid):
        rss = _linux_status_rss(child) if os.path.isdir("/proc") else _macos_rss(child)
        if rss:
            total += rss
    return total


def process_tree_pss_bytes(pid: int) -> int | None:
    """Summed PSS on Linux; None where PSS is unavailable (macOS)."""
    if not os.path.isdir("/proc"):
        return None
    total = 0
    for child in process_tree_pids(pid):
        pss = _linux_smaps(child, "Pss")
        if pss is None:
            rss = _linux_status_rss(child)
            if rss is None:
                continue
            pss = rss
        total += pss
    return total


class MemorySampler:
    """Background sampler that keeps the peak tree RSS of a process."""

    def __init__(self, pid: int, interval: float = 0.02):
        self.pid = pid
        self.interval = interval
        self.peak_rss = 0
        self._stop = False

    def sample_once(self) -> int:
        rss = process_tree_rss_bytes(self.pid)
        if rss > self.peak_rss:
            self.peak_rss = rss
        return rss

    def start(self) -> "MemorySampler":
        import threading

        def loop() -> None:
            while not self._stop:
                try:
                    self.sample_once()
                except Exception:  # noqa: BLE001 - sampling must never kill a bench
                    pass
                time.sleep(self.interval)

        self._thread = threading.Thread(target=loop, daemon=True)
        self._thread.start()
        return self

    def stop(self) -> int:
        self._stop = True
        thread = getattr(self, "_thread", None)
        if thread is not None:
            thread.join(timeout=2)
        return self.peak_rss


# ---------------------------------------------------------------------------
# Process control
# ---------------------------------------------------------------------------


def wait_for_socket(path: str, timeout_s: float) -> bool:
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        if os.path.exists(path):
            try:
                sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                sock.settimeout(0.2)
                sock.connect(path)
                sock.close()
                return True
            except OSError:
                pass
        time.sleep(0.02)
    return False


def wait_for_port(host: str, port: int, timeout_s: float) -> bool:
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.25):
                return True
        except OSError:
            time.sleep(0.02)
    return False


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def terminate_pgroup(pgid: int | None, timeout: float = 10.0) -> None:
    """SIGTERM then SIGKILL a whole process group, waiting for it to die."""
    if pgid is None:
        return
    import signal

    for sig, wait in ((signal.SIGTERM, timeout), (signal.SIGKILL, 3.0)):
        try:
            os.killpg(pgid, sig)
        except ProcessLookupError:
            return
        except PermissionError:
            return
        deadline = time.perf_counter() + wait
        while time.perf_counter() < deadline:
            try:
                os.killpg(pgid, 0)
            except ProcessLookupError:
                return
            except PermissionError:
                return
            time.sleep(0.05)


def run_capture(argv: list[str], env: dict | None = None, timeout: float = 30.0, cwd: str | None = None):
    return subprocess.run(
        argv,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
        cwd=cwd,
        check=False,
    )


def which_or_none(name: str, env: dict | None = None) -> str | None:
    return shutil.which(name, path=(env or os.environ).get("PATH"))


# ---------------------------------------------------------------------------
# Reports
# ---------------------------------------------------------------------------


def utc_now_iso() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def build_report(suite: str, results: list[dict], skipped: list[dict], started_at: str | None = None) -> dict:
    return {
        "suite": suite,
        "schema": SCHEMA,
        "host": host_info(),
        "started_at": started_at or utc_now_iso(),
        "results": results,
        "skipped": skipped,
    }


def write_json(path: str, report: dict) -> None:
    directory = os.path.dirname(os.path.abspath(path))
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2, ensure_ascii=False)
        fh.write("\n")


def write_markdown(path: str, markdown: str) -> None:
    directory = os.path.dirname(os.path.abspath(path))
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(markdown if markdown.endswith("\n") else markdown + "\n")


def fmt_bytes(value: float | None) -> str:
    if value is None:
        return "—"
    units = ("B", "KB", "MB", "GB", "TB")
    size = float(value)
    for unit in units:
        if abs(size) < 1024.0 or unit == units[-1]:
            return f"{size:.1f} {unit}"
        size /= 1024.0
    return f"{size:.1f} TB"


def fmt_ms(value: float | None) -> str:
    if value is None:
        return "—"
    if value >= 1000:
        return f"{value / 1000:.2f} s"
    return f"{value:.1f} ms"


def fmt_ratio(value: float | None, better_is_lower: bool = True) -> str:
    """Format ``value`` as a multiple of the baseline (baseline == 1.0x)."""
    if value is None:
        return "—"
    if value <= 0:
        return "—"
    if abs(value - 1.0) < 0.02:
        return "same"
    if value >= 1:
        word = "more" if better_is_lower else "faster"
        return f"{value:.2f}× {word}" if value < 1000 else f"{value:,.0f}× {word}"
    word = "less" if better_is_lower else "slower"
    inverse = 1 / value
    return f"{inverse:.2f}× {word}" if inverse < 1000 else f"{inverse:,.0f}× {word}"


def markdown_table(headers: list[str], rows: list[list[str]]) -> str:
    lines = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    for row in rows:
        lines.append("| " + " | ".join(cell if cell != "" else "—" for cell in row) + " |")
    return "\n".join(lines)


def eprint(*args, **kwargs) -> None:
    print(*args, file=sys.stderr, flush=True)
