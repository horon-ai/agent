#!/usr/bin/env python3
"""Turn the attach/detach soak's own output into a readable report.

The soak test prints two machine-readable blocks to stderr:

    soak_attach_detach_table
    <markdown table>
    soak_attach_detach_counter=<one-line summary per counter>
    soak_attach_detach_metrics=<a pretty-printed JSON object>

This script reads them from a captured log, writes a Markdown report for the
workflow summary and a JSON file for anyone diffing two runs, and exits non-zero
when the run carried a retention failure so the same file decides the job's
verdict.

Usage:
  python3 scripts/perf/check_soak.py soak.log --output report.md --json report.json
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

METRICS_MARKER = "soak_attach_detach_metrics="
TABLE_MARKER = "soak_attach_detach_table\n"
COUNTER_PREFIX = "soak_attach_detach_counter="


def extract_metrics(text: str) -> dict | None:
    """The soak's metrics object, or `None` when the run never got that far.

    The object is pretty-printed across lines, so it is read with a decoder that
    stops at the end of the first complete value instead of assuming a line.
    """
    index = text.find(METRICS_MARKER)
    if index < 0:
        return None
    start = text.find("{", index + len(METRICS_MARKER))
    if start < 0:
        return None
    decoder = json.JSONDecoder()
    try:
        value, _ = decoder.raw_decode(text[start:])
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def extract_table(text: str) -> str | None:
    """The Markdown table the soak prints, without its marker line."""
    index = text.find(TABLE_MARKER)
    if index < 0:
        return None
    rest = text[index + len(TABLE_MARKER) :]
    lines: list[str] = []
    for line in rest.splitlines():
        if line.startswith("soak_attach_detach_"):
            break
        lines.append(line)
    table = "\n".join(lines).strip("\n")
    return table or None


def extract_counters(text: str) -> list[str]:
    """The one-line per-counter summaries, in the order the soak printed them."""
    return [line[len(COUNTER_PREFIX) :] for line in text.splitlines() if line.startswith(COUNTER_PREFIX)]


def format_number(value) -> str:
    if isinstance(value, float):
        if value.is_integer():
            return str(int(value))
        return f"{value:.2f}"
    return str(value)


def render_report(metrics: dict, table: str | None, counters: list[str], log_name: str) -> str:
    """A Markdown report of one run: the verdict first, then the numbers."""
    offenders = metrics.get("non_returning") or []
    cycles = metrics.get("cycles")
    windows = metrics.get("windows")
    lines = [f"# Attach/detach soak — {cycles} cycles", ""]
    if offenders:
        lines += ["**Retention failure.** These counters did not return to baseline:", ""]
        lines += [f"- {offender}" for offender in offenders]
    else:
        lines.append("**Clean.** Every counter returned to its baseline.")
    lines += [
        "",
        f"- Cycles: {cycles} ({windows} windows)"
        if windows
        else f"- Cycles: {cycles}",
        f"- Rotating sessions: {metrics.get('rotating_sessions')}",
        f"- Turn rounds: {metrics.get('turn_rounds')} (failed: {metrics.get('failed_rounds')})",
        f"- Quiescence misses: {metrics.get('quiescence_misses')}",
        f"- Samples: {metrics.get('samples')}",
    ]
    budgets = metrics.get("budgets") or {}
    if budgets:
        lines.append(
            "- Budgets: "
            + ", ".join(f"{name} {format_number(value)}" for name, value in budgets.items())
        )
    if metrics.get("shared_context_grew"):
        lines.append("- `shared_context` grew during the run (reported, never asserted)")
    lines.append(f"- Source log: `{log_name}`")
    lines.append("")

    if counters:
        lines += ["## Counters", "", "```", *counters, "```", ""]
    if table:
        lines += ["## Table", "", table, ""]
    return "\n".join(lines).rstrip() + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("log", type=Path, help="captured soak output (stdout+stderr)")
    parser.add_argument("--output", type=Path, help="write the Markdown report here")
    parser.add_argument("--json", dest="json_path", type=Path, help="write the raw metrics here")
    parser.add_argument(
        "--require-metrics",
        action="store_true",
        help="fail when the log carries no metrics block (a run that never started)",
    )
    args = parser.parse_args(argv)

    text = args.log.read_text(encoding="utf-8", errors="replace")
    metrics = extract_metrics(text)
    if metrics is None:
        message = f"no {METRICS_MARKER} block in {args.log}: the soak did not reach its report"
        if args.output:
            args.output.write_text(f"# Attach/detach soak\n\n**No report.** {message}\n", encoding="utf-8")
        print(f"::error::{message}", file=sys.stderr)
        return 1 if args.require_metrics else 0

    report = render_report(metrics, extract_table(text), extract_counters(text), args.log.name)
    if args.output:
        args.output.write_text(report, encoding="utf-8")
    if args.json_path:
        args.json_path.write_text(json.dumps(metrics, indent=2) + "\n", encoding="utf-8")
    if not args.output:
        print(report, end="")

    offenders = metrics.get("non_returning") or []
    if offenders:
        print(f"::error::soak retained {len(offenders)} counter(s): {' | '.join(offenders)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
