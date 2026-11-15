#!/usr/bin/env python3
"""Per-request byte sizes for a benchmark run record.

The mock model server records, for every request it receives, how large the
JSON body was and how much of it was the tool catalog. Those numbers are what
a prompt or tool-description change actually moves, so each run record keeps
them beside its timings instead of leaving them in the mock server's log.

A key the mock server did not report stays ``None``: a missing measurement is
never written as a zero, because zero is a real value here (a request that
offers no tools has ``tools_bytes == 0``).
"""

from __future__ import annotations

KEYS = ("body_bytes", "tools_bytes", "tool_count")


def _count(value) -> int | None:
    # bool is an int subclass; a stray True must not be read as one byte.
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value


def request_bytes(requests: list[dict]) -> list[dict]:
    """One ``{body_bytes, tools_bytes, tool_count}`` entry per model request."""
    return [{key: _count(request.get(key)) for key in KEYS} for request in requests]


def first_request(cell: dict | None) -> dict | None:
    """The first model request of the first run that recorded any.

    The first request is the one that carries the full static prefix and tool
    catalog with no conversation behind it, so it is the comparable number
    between two builds.
    """
    for run in (cell or {}).get("runs") or []:
        entries = run.get("request_bytes") or []
        if entries:
            return entries[0]
    return None


def _fmt(value: int | None) -> str:
    return f"{value:,}" if value is not None else "—"


def markdown_cell(cell: dict | None) -> str:
    """``body / tools (N tools)`` for the first request, or an em dash."""
    entry = first_request(cell)
    if entry is None or all(entry.get(key) is None for key in KEYS):
        return "—"
    return (
        f"{_fmt(entry.get('body_bytes'))} / {_fmt(entry.get('tools_bytes'))} "
        f"({_fmt(entry.get('tool_count'))} tools)"
    )


MARKDOWN_NOTE = (
    "`request bytes` is the first model request of the first run as the mock server "
    "received it: JSON body bytes / tool catalog bytes (tools offered)."
)
