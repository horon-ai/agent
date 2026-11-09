#!/usr/bin/env python3
"""One pass of the daemon-backed benchmark suites, as a pass/fail check.

The benchmark workflow runs on demand and after a push to `main`, so nothing on
a pull request ever started these suites: a renamed home variable broke three
of them and the first sign was the post-merge benchmark run. This script runs
the tools, reload and compare suites once each against a built binary and fails
when any of them crashed, skipped, or recorded an error.

The exit code of a suite proves nothing on its own. By the measurement-script
contract a missing binary is a skip with exit code 0, and the compare suite
records a daemon that never came up as an `error` field and still exits 0. The
verdict is therefore read from each suite's JSON report.

Timings are thrown away: one sample on a shared runner measures nothing.

Usage: bench_smoke.py --binary target/release/horon-agent
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Iterator

SCRIPTS = Path(__file__).resolve().parent

# Fields a suite fills only when something went wrong.
ERROR_KEYS = ("error", "errors", "failures")

# Sections the compare suite leaves null when a scenario did not run.
COMPARE_SECTIONS = ("oneshot", "sessions_memory")


def suite_commands(binary: str, out: Path) -> dict[str, list[str]]:
    """The smallest run of each suite that still starts a daemon and a session."""

    def script(name: str) -> list[str]:
        return [sys.executable, str(SCRIPTS / name)]

    return {
        "tools": script("bench_tools.py")
        + ["--binary", binary, "--iterations", "1", "--warmup", "0", "--json-out", str(out / "tools.json")],
        "reload": script("bench_reload_storm.py")
        + ["--binary", binary, "--sessions", "1,2", "--events", "50", "--json-out", str(out / "reload.json")],
        "compare": script("bench_compare.py")
        + ["--bin", f"horon={binary}", "--tools", "horon", "--scenarios", "oneshot", "sessions"]
        + ["--sessions", "1", "2", "--turns", "1", "--oneshot-runs", "1", "--version-runs", "1"]
        + ["--timeout", "60", "--json-out", str(out / "compare.json")],
    }


def recorded_errors(node: object, path: str = "") -> Iterator[str]:
    """Every non-empty error field in a report, with where it sits."""
    if isinstance(node, dict):
        for key, value in node.items():
            where = f"{path}.{key}" if path else key
            if key in ERROR_KEYS:
                if value:
                    yield f"{where}: {value}"
            else:
                yield from recorded_errors(value, where)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            yield from recorded_errors(value, f"{path}[{index}]")


def problems(suite: str, report: dict) -> list[str]:
    """Why this report does not count as a run; empty when it does."""
    found = [f"skipped: {entry.get('reason')}" for entry in report.get("skipped", [])]
    results = report.get("results", [])
    if not results:
        found.append("no results")
    if suite == "compare":
        for row in results:
            found.extend(f"{row.get('name')}: {key} did not run" for key in COMPARE_SECTIONS if not row.get(key))
    found.extend(recorded_errors(results, "results"))
    return found


def run_suite(suite: str, command: list[str], report_path: Path) -> list[str]:
    # The suites print their report to stdout; the report file is what is read.
    completed = subprocess.run(command, stdout=subprocess.DEVNULL, check=False)
    found = [f"exit code {completed.returncode}"] if completed.returncode else []
    if report_path.is_file():
        found.extend(problems(suite, json.loads(report_path.read_text(encoding="utf-8"))))
    elif not found:
        found.append("no report written")
    return found


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", default="./target/release/horon-agent")
    args = parser.parse_args(argv)
    binary = os.path.abspath(args.binary)

    failed = False
    with tempfile.TemporaryDirectory(prefix="horon-bench-smoke-") as tmp:
        out = Path(tmp)
        for suite, command in suite_commands(binary, out).items():
            found = run_suite(suite, command, out / f"{suite}.json")
            print(f"bench smoke: {suite}: {'FAIL' if found else 'ok'}", flush=True)
            for line in found:
                print(f"  {line}", flush=True)
            failed = failed or bool(found)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
