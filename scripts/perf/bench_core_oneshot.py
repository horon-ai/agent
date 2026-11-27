#!/usr/bin/env python3
"""Core-profile one-shot: the default tool surface, measured end to end.

The plain ``oneshot`` row passes ``--tools bash`` so its numbers stay
comparable with packages/docs-site/reference/performance/results.mdx. A real turn runs with the core
profile instead: seven eager tools plus ``toolbox`` in the request and the
listed-tool index in the system prompt. This row is the same one-shot turn with
``--tools`` removed, so the request carries that surface. Dropping a flag needs
nothing new from the binary, so a baseline build measures the same way.

A run whose first request offered no tools at all did not see the core surface,
and its time would describe the wrong turn; it is recorded as
``core surface missing`` with no time instead.
"""

from __future__ import annotations

import bench_common as bc
import bench_request_bytes as brb

SCENARIO = "oneshot_core"
MISSING = "core surface missing"
HEADERS = [
    "Tool", "Time (median)", "Min–max", "vs horon", "Peak tree RSS", "vs horon",
    "Via", "Tool round-trip", "Note", "request bytes",
]


def argv(turn_argv: list[str]) -> list[str]:
    """The one-shot argv with its ``--tools`` allow-list removed."""
    out = list(turn_argv)
    if "--tools" in out:
        index = out.index("--tools")
        del out[index:index + 2]
    return out


def check_surface(run: dict) -> dict:
    """Mark a run whose first request offered no tools as an error."""
    if run.get("error"):
        return run
    entries = run.get("request_bytes") or []
    if entries and entries[0].get("tool_count") == 0:
        return {**run, "ms": None, "peak_rss_bytes": None, "error": MISSING}
    return run


def summarize(runs: list[dict]) -> dict:
    """Collapse the repeats into one row with the same keys as a scenario row."""
    times = [run["ms"] for run in runs if run.get("ms") is not None]
    peaks = [run["peak_rss_bytes"] for run in runs if run.get("peak_rss_bytes")]
    summary = bc.summarize(times)
    errors = [str(run["error"]) for run in runs if run.get("error")]
    round_trip = bool(runs) and not errors and all(run.get("tool_roundtrip") for run in runs)
    failure = None
    if not round_trip:
        failure = errors[0] if errors else "the tool call never came back"
    return {
        "ms": summary["median"] if summary else None,
        "summary": summary,
        "peak_rss_bytes": max(peaks) if peaks else None,
        "tool_round_trip": round_trip,
        "status": "ok" if round_trip else "failed",
        "failure": failure,
        "via": None,
        "mock_requests": [run.get("mock_requests") for run in runs],
        "errors": errors,
        "error": errors[0] if errors else None,
        "runs": runs,
    }


def measure(runners: list, results: list[dict], roots: dict, mock, repeats: int, timeout: float) -> None:
    """Run the core one-shot for every row, alternating rows like the plain one-shot.

    Each row reuses the home and workspace of its plain one-shot, so the two
    rows differ only in the tool surface. A row without ``oneshot_core`` is a
    tool with no core profile and is recorded as skipped once.
    """
    by_row: list[list[dict]] = [[] for _ in runners]
    for _ in range(max(1, repeats)):
        for index, runner in enumerate(runners):
            result = results[index]
            if result["error"] or result.get(SCENARIO) is not None:
                continue
            method = getattr(runner, SCENARIO, None)
            if method is None:
                result[SCENARIO] = {"skipped": f"{runner.label} has no core tool profile"}
                continue
            root, cwd = roots[runner.name]
            try:
                run = method(mock, root, cwd, timeout)
            except Exception as exc:  # noqa: BLE001
                run = {"ms": None, "peak_rss_bytes": None, "tool_roundtrip": None,
                       "error": str(exc)[:300]}
            by_row[index].append(run)
    for index, result in enumerate(results):
        if result["error"] or result.get(SCENARIO) is not None:
            continue
        result[SCENARIO] = summarize(by_row[index])


def markdown_section(results: list[dict], scenario_row) -> list[str]:
    """The core one-shot table, one row per result, with its request bytes."""
    rows = [
        scenario_row(result, SCENARIO, results) + [brb.markdown_cell(result.get(SCENARIO))]
        for result in results
    ]
    return [
        "**Core-profile one-shot** (the one-shot turn without `--tools`, so the request "
        "carries the default core surface: the eager tools, `toolbox` and the listed-tool index)",
        "",
        bc.markdown_table(HEADERS, rows),
        "",
    ]
