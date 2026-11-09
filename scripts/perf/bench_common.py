#!/usr/bin/env python3
"""Shared helpers for the horon comparison benchmarks.

Only the Python standard library is used so the same file runs on Linux, macOS
and Windows under the CI workflow. Nothing here reads or writes a user's real
agent configuration: callers pass an isolated HOME/XDG environment.

Exports:

- ``host_info``: os/arch/cpu/python snapshot embedded in every report.
- ``stats``: median/p95/min/max/sample-count for a list of measurements.
- ``process_tree_rss_bytes`` / ``process_tree_pss_bytes``: summed RSS (and PSS on
  Linux) of a process and all descendants. Windows reports the working set.
- ``ipc_connect`` / ``wait_for_socket`` / ``wait_for_port``: the daemon's local
  socket (a Unix socket, or on Windows the named pipe the daemon derives from
  the same path) and readiness polling.
- ``new_group_kwargs`` / ``pgid_of`` / ``terminate_pgroup``: start a process in
  its own group and reliably kill the whole tree.
- ``short_temp_dir``: temp dir under /tmp so unix socket paths stay < 104 bytes.
- ``linear_slope``: least-squares slope used for "each additional session".
- ``build_report`` / ``write_json`` / ``write_markdown``: report plumbing.
"""

from __future__ import annotations

import datetime
import errno
import hashlib
import json
import ntpath
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
IS_WINDOWS = os.name == "nt"


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
    if IS_WINDOWS:
        return _inherited_acl_temp_dir(prefix)
    path = tempfile.mkdtemp(prefix=prefix, dir=root)
    return os.path.realpath(path)


def _inherited_acl_temp_dir(prefix: str) -> str:
    """A Windows temp dir that keeps the ACL inherited from %TEMP%.

    ``tempfile.mkdtemp`` passes mode 0o700, which CPython (3.12.4+, 3.13+)
    turns into a protected ACL granting only SYSTEM, Administrators and the
    owner. On an elevated runner the owner is the Administrators group, so a
    process under a filtered (LUA) token, such as a command inside the Codex
    Windows sandbox, can read nothing below it. A plain ``os.mkdir`` inherits
    the user's own %TEMP% ACL instead.
    """
    base = tempfile.gettempdir()
    for _ in range(100):
        path = os.path.join(base, prefix + os.urandom(4).hex())
        try:
            os.mkdir(path)
        except FileExistsError:
            continue
        return os.path.realpath(path)
    raise FileExistsError(errno.EEXIST, "no unused temp dir name", base)


def isolated_home_env(root: str, prefix: str, home_var: str | None = None) -> dict[str, str]:
    """Base env pointing every agent home at ``root``.

    ``home_var`` names the home variable when it is not ``<prefix>_HOME``:
    horon reads ``HORON_HOME`` while its runtime dir is
    ``HORON_AGENT_RUNTIME_DIR``.
    """
    env = os.environ.copy()
    home = os.path.join(root, "home")
    run = os.path.join(root, "run")
    cache = os.path.join(root, "cache")
    config = os.path.join(root, "config")
    data = os.path.join(root, "data")
    state = os.path.join(root, "state")
    for path in (home, run, cache, config, data, state):
        os.makedirs(path, exist_ok=True)
    env[home_var or prefix + "_HOME"] = home
    env[prefix + "_RUNTIME_DIR"] = run
    env["HOME"] = home
    env["XDG_CACHE_HOME"] = cache
    env["XDG_CONFIG_HOME"] = config
    env["XDG_DATA_HOME"] = data
    env["XDG_STATE_HOME"] = state
    env["TMPDIR"] = os.path.join(root, "tmp")
    os.makedirs(env["TMPDIR"], exist_ok=True)
    if IS_WINDOWS:
        # Windows tools resolve their home from these, not from HOME or XDG.
        env["USERPROFILE"] = home
        env["APPDATA"] = config
        env["LOCALAPPDATA"] = data
        env["TEMP"] = env["TMP"] = env["TMPDIR"]
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
    if IS_WINDOWS:
        children = _windows_children_map()
        lookup = lambda cur: children.get(cur, [])  # noqa: E731
    else:
        lookup = children_of
    seen: set[int] = set()
    stack = [pid]
    while stack:
        cur = stack.pop()
        if cur in seen:
            continue
        seen.add(cur)
        stack.extend(lookup(cur))
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


if IS_WINDOWS:
    import ctypes as _ctypes
    from ctypes import wintypes as _wintypes

    _kernel32 = _ctypes.WinDLL("kernel32", use_last_error=True)

    class _ProcessEntry32W(_ctypes.Structure):
        _fields_ = [
            ("dwSize", _wintypes.DWORD),
            ("cntUsage", _wintypes.DWORD),
            ("th32ProcessID", _wintypes.DWORD),
            ("th32DefaultHeapID", _ctypes.c_size_t),
            ("th32ModuleID", _wintypes.DWORD),
            ("cntThreads", _wintypes.DWORD),
            ("th32ParentProcessID", _wintypes.DWORD),
            ("pcPriClassBase", _wintypes.LONG),
            ("dwFlags", _wintypes.DWORD),
            ("szExeFile", _wintypes.WCHAR * 260),
        ]

    class _ProcessMemoryCounters(_ctypes.Structure):
        _fields_ = [
            ("cb", _wintypes.DWORD),
            ("PageFaultCount", _wintypes.DWORD),
            ("PeakWorkingSetSize", _ctypes.c_size_t),
            ("WorkingSetSize", _ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", _ctypes.c_size_t),
            ("QuotaPagedPoolUsage", _ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", _ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", _ctypes.c_size_t),
            ("PagefileUsage", _ctypes.c_size_t),
            ("PeakPagefileUsage", _ctypes.c_size_t),
        ]

    _kernel32.CreateToolhelp32Snapshot.argtypes = [_wintypes.DWORD, _wintypes.DWORD]
    _kernel32.CreateToolhelp32Snapshot.restype = _wintypes.HANDLE
    _kernel32.Process32FirstW.argtypes = [_wintypes.HANDLE, _ctypes.POINTER(_ProcessEntry32W)]
    _kernel32.Process32FirstW.restype = _wintypes.BOOL
    _kernel32.Process32NextW.argtypes = [_wintypes.HANDLE, _ctypes.POINTER(_ProcessEntry32W)]
    _kernel32.Process32NextW.restype = _wintypes.BOOL
    _kernel32.OpenProcess.argtypes = [_wintypes.DWORD, _wintypes.BOOL, _wintypes.DWORD]
    _kernel32.OpenProcess.restype = _wintypes.HANDLE
    _kernel32.CloseHandle.argtypes = [_wintypes.HANDLE]
    _kernel32.CloseHandle.restype = _wintypes.BOOL
    _kernel32.K32GetProcessMemoryInfo.argtypes = [
        _wintypes.HANDLE,
        _ctypes.POINTER(_ProcessMemoryCounters),
        _wintypes.DWORD,
    ]
    _kernel32.K32GetProcessMemoryInfo.restype = _wintypes.BOOL
    _kernel32.PeekNamedPipe.argtypes = [
        _wintypes.HANDLE,
        _wintypes.LPVOID,
        _wintypes.DWORD,
        _wintypes.LPVOID,
        _ctypes.POINTER(_wintypes.DWORD),
        _wintypes.LPVOID,
    ]
    _kernel32.PeekNamedPipe.restype = _wintypes.BOOL


_TH32CS_SNAPPROCESS = 0x00000002
_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_PROCESS_VM_READ = 0x0010


def _windows_processes() -> list[tuple[int, int]]:
    """Every running process as (pid, parent pid), from one Toolhelp snapshot."""
    snapshot = _kernel32.CreateToolhelp32Snapshot(_TH32CS_SNAPPROCESS, 0)
    if not snapshot or snapshot == _wintypes.HANDLE(-1).value:
        return []
    entries: list[tuple[int, int]] = []
    try:
        entry = _ProcessEntry32W()
        entry.dwSize = _ctypes.sizeof(_ProcessEntry32W)
        ok = _kernel32.Process32FirstW(snapshot, _ctypes.byref(entry))
        while ok:
            entries.append((int(entry.th32ProcessID), int(entry.th32ParentProcessID)))
            ok = _kernel32.Process32NextW(snapshot, _ctypes.byref(entry))
    finally:
        _kernel32.CloseHandle(snapshot)
    return entries


def _windows_children_map() -> dict[int, list[int]]:
    children: dict[int, list[int]] = {}
    for pid, parent in _windows_processes():
        if pid != parent:
            children.setdefault(parent, []).append(pid)
    return children


def _windows_rss(pid: int) -> int | None:
    """Working set of ``pid``, Windows' resident-memory equivalent of RSS."""
    for access in (_PROCESS_QUERY_LIMITED_INFORMATION | _PROCESS_VM_READ, _PROCESS_QUERY_LIMITED_INFORMATION):
        handle = _kernel32.OpenProcess(access, False, pid)
        if handle:
            break
    else:
        return None
    try:
        counters = _ProcessMemoryCounters()
        counters.cb = _ctypes.sizeof(_ProcessMemoryCounters)
        if not _kernel32.K32GetProcessMemoryInfo(handle, _ctypes.byref(counters), counters.cb):
            return None
        return int(counters.WorkingSetSize)
    finally:
        _kernel32.CloseHandle(handle)



def process_tree_rss_bytes(pid: int) -> int:
    """Summed resident set size of ``pid`` and every descendant (working set on Windows)."""
    if IS_WINDOWS:
        read = _windows_rss
    elif os.path.isdir("/proc"):
        read = _linux_status_rss
    else:
        read = _macos_rss
    total = 0
    for child in process_tree_pids(pid):
        rss = read(child)
        if rss:
            total += rss
    return total


def process_tree_pss_bytes(pid: int) -> int | None:
    """Summed PSS on Linux; None where PSS is unavailable (macOS, Windows)."""
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


def process_tree_status_bytes(pid: int, field: str) -> int | None:
    """Summed ``/proc/<pid>/status`` value (e.g. ``RssAnon``, ``RssFile``) over the tree; Linux only.

    Splitting RSS into anonymous and file-backed pages tells heap and stack
    growth apart from mapped files (the binary, shared libraries) being touched.
    """
    if not os.path.isdir("/proc"):
        return None
    prefix = field + ":"
    total = 0
    found = False
    for child in process_tree_pids(pid):
        try:
            with open(f"/proc/{child}/status", "r", encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    if line.startswith(prefix):
                        total += int(line.split()[1]) * 1024
                        found = True
                        break
        except (OSError, ValueError, IndexError):
            continue
    return total if found else None



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


def new_group_kwargs() -> dict:
    """``Popen`` arguments that start the child as the leader of a new group."""
    if IS_WINDOWS:
        return {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP}
    return {"start_new_session": True}


def pgid_of(pid: int) -> int | None:
    """Group handle for ``terminate_pgroup``: the pgid on POSIX, the pid on Windows."""
    if IS_WINDOWS:
        return pid
    try:
        return os.getpgid(pid)
    except ProcessLookupError:
        return None


def pipe_name(path: str) -> str:
    """Named pipe the daemon binds for socket ``path`` on Windows.

    Mirrors ``path_to_pipe_name`` in crates/platform-windows/src/ipc.rs: the
    file stem filtered to ``[A-Za-z0-9_-]`` and cut to 32 characters, then the
    first 16 hex digits of the SHA-256 of the path with ``\\`` turned into
    ``/`` and ASCII lowercased.
    """
    stem = ntpath.splitext(ntpath.basename(path))[0]
    stem = "".join(ch for ch in stem if (ch.isascii() and ch.isalnum()) or ch in "-_")[:32] or "horon-agent"
    normalized = "".join(chr(ord(ch) + 32) if "A" <= ch <= "Z" else ch for ch in path.replace("\\", "/"))
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()
    return "\\\\.\\pipe\\%s-%s" % (stem, digest[:16])


class PipeStream:
    """The subset of a socket the benchmarks use, over a connected Windows named pipe."""

    def __init__(self, fh) -> None:
        self._fh = fh
        self._timeout: float | None = None

    def settimeout(self, value: float | None) -> None:
        self._timeout = value

    def sendall(self, data: bytes) -> None:
        view = memoryview(data)
        while view:
            written = self._fh.write(view)
            view = view[written:]

    def recv(self, size: int) -> bytes:
        """Bytes already in the pipe, b"" once the server closed it, TimeoutError past the timeout."""
        import msvcrt

        handle = msvcrt.get_osfhandle(self._fh.fileno())
        deadline = None if self._timeout is None else time.perf_counter() + self._timeout
        available = _wintypes.DWORD(0)
        while True:
            if not _kernel32.PeekNamedPipe(handle, None, 0, None, _ctypes.byref(available), None):
                return b""
            if available.value:
                return self._fh.read(min(size, available.value))
            if deadline is not None and time.perf_counter() >= deadline:
                raise TimeoutError("timed out")
            time.sleep(0.002)

    def close(self) -> None:
        self._fh.close()

    def __enter__(self) -> "PipeStream":
        return self

    def __exit__(self, *exc) -> None:
        self.close()


ERROR_PIPE_BUSY = 231


def _pipe_unavailable(exc: OSError) -> bool:
    """True when a named pipe exists but has no free instance at this moment.

    The daemon creates its next pipe instance only after accepting a client, so
    a connect right after another one finds every instance taken (busy) or, in
    the gap, no instance at all (not found). ``open()`` goes through the C
    runtime, which reports busy as errno EINVAL with no ``winerror``.
    """
    if getattr(exc, "winerror", None) == ERROR_PIPE_BUSY:
        return True
    return exc.errno in (errno.EINVAL, errno.EBUSY, errno.ENOENT)


def ipc_connect(path: str, timeout: float | None = None, connect_timeout: float = 5.0):
    """Connect to the daemon socket at ``path``; the result has sendall/recv/settimeout/close.

    On Windows the daemon listens on the named pipe derived from ``path``. A
    pipe with no free instance refuses the open until the server creates the
    next one, so that case is retried for up to ``connect_timeout``.
    """
    if not IS_WINDOWS:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        try:
            sock.connect(path)
        except OSError:
            sock.close()
            raise
        return sock
    name = pipe_name(path)
    deadline = time.perf_counter() + connect_timeout
    while True:
        try:
            fh = open(name, "r+b", buffering=0)  # noqa: SIM115 - owned by PipeStream
            break
        except OSError as exc:
            if not _pipe_unavailable(exc) or time.perf_counter() >= deadline:
                raise
            time.sleep(0.01)
    stream = PipeStream(fh)
    stream.settimeout(timeout)
    return stream


def wait_for_socket(path: str, timeout_s: float) -> bool:
    """True once the daemon socket at ``path`` accepts a connection."""
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        try:
            ipc_connect(path, timeout=0.2, connect_timeout=0.2).close()
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
    """Kill a whole process group (Windows: the process tree), waiting for it to die.

    POSIX sends SIGTERM, then SIGKILL. Windows has no group signal a console-less
    child honours, so the tree is ended with ``taskkill /T /F``.
    """
    if pgid is None:
        return
    if IS_WINDOWS:
        try:
            subprocess.run(
                ["taskkill", "/PID", str(pgid), "/T", "/F"],
                capture_output=True,
                timeout=timeout + 5,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            return
        deadline = time.perf_counter() + timeout
        while time.perf_counter() < deadline and any(pid == pgid for pid, _ in _windows_processes()):
            time.sleep(0.05)
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
