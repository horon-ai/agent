#!/usr/bin/env python3
"""Which files a one-shot run flushes, from one extra strace run.

``fsync_calls`` counts flushes with ``strace -c``, which says how many but not
what for. This module traces one more one-shot run per row with
``strace -f -y``: every fsync/fdatasync line then names the file or directory
behind its descriptor, and the calls are grouped by that path. The run is extra
and untimed, so it never touches the timed or counted samples.

Paths are made comparable between runs and builds: the run's temporary root
becomes ``<root>``, and UUIDs, long hex ids and long digit runs (session and
event ids, timestamps) become ``<id>``. The counts of all groups add up to the
flushes the traced run made.
"""

from __future__ import annotations

import re

MODE = "paths"
SKIPPED = {"skipped": "strace unavailable"}
CALL = re.compile(r"\b(fsync|fdatasync)\((\d+)(?:<([^>]*)>)?")
ID = re.compile(
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
    r"|\b[0-9a-fA-F]{16,}\b|\d{10,}"
)


def strace_argv(argv: list[str], out_path: str, names: tuple[str, ...]) -> list[str]:
    """Wrap a command so every flush is logged with the path of its descriptor.

    ``-f`` follows worker processes, ``-y`` prints the path behind each fd, and
    ``-o`` keeps the trace out of the tool's own stdout. There is no ``-c``:
    the per-call lines are the point.
    """
    return ["strace", "-f", "-y", "-e", "trace=" + ",".join(names), "-o", out_path, *argv]


def normalize(path: str, roots: tuple[str, ...]) -> str:
    for root in roots:
        if root and path.startswith(root):
            path = "<root>" + path[len(root):]
            break
    return ID.sub("<id>", path)


def parse(text: str, roots: tuple[str, ...] = ()) -> list[dict]:
    """Group the flushes in a ``strace -f -y`` log by the path they flushed.

    A call is counted where it starts, so a call strace split into
    ``<unfinished ...>`` and ``<... resumed>`` lines counts once. A descriptor
    strace could not name is grouped as ``fd:<n>``.
    """
    groups: dict[str, dict] = {}
    for line in text.splitlines():
        for name, fd, path in CALL.findall(line):
            key = normalize(path, roots) if path else f"fd:{fd}"
            group = groups.setdefault(key, {"path": key, "count": 0, "fsync": 0, "fdatasync": 0})
            group["count"] += 1
            group[name] += 1
    return sorted(groups.values(), key=lambda group: (-group["count"], group["path"]))


def measure(run_traced, available: bool):
    """The ``fsync_paths`` value for one row: path groups, or why there are none.

    ``run_traced`` makes the extra traced one-shot run and returns its record.
    Without strace the run is not made at all and the row says so, which keeps
    the rest of the suite running.
    """
    if not available:
        return dict(SKIPPED)
    try:
        run = run_traced()
    except Exception as exc:  # noqa: BLE001
        return {"skipped": f"traced run failed: {str(exc)[:200]}"}
    if run.get("error"):
        return {"skipped": f"traced run failed: {str(run['error'])[:200]}"}
    paths = run.get("fsync_paths")
    if paths is None:
        return {"skipped": "strace wrote no trace"}
    return paths
