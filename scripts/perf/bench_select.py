#!/usr/bin/env python3
"""Turn the benchmark workflow's inputs into GitHub step outputs.

    bench_select.py <suites>                      one `<suite>=true|false` line per known suite
    bench_select.py --platforms <os-list>         `matrix=<json>` for the build job
    bench_select.py --jobs <os-list> <suites>     `matrix=<json>` and `has_jobs=` for the measure job
    bench_select.py --compile <os-list> <suites>  `matrix=<json>` and `has_jobs=` for the compile job
    bench_select.py --expect <suites>             `units=<comma list>` for the merge job

`all` (or an empty input) selects every suite or every OS. An unknown name is an
error, so a typo fails the dispatch instead of silently measuring nothing.

The measure matrix has one entry per (OS, unit) because each unit is one job:
the loading suite runs as two units, `loading-cold` and `loading-warm`, so the
two modes never share a machine, and `compile` is excluded because it builds from
source in a job of its own. `has_jobs` is printed next to the matrix because an
empty `include` cannot be handed to a job matrix: the workflow skips that job on
this flag instead of asking GitHub to expand nothing.
"""

from __future__ import annotations

import json
import sys

SUITES = ("compare", "startup", "loading", "tools", "reload", "tui", "compile")

# The units a suite is measured as. `loading` is two units, and `compile` is not
# a measurement unit of the prebuilt binary: it rebuilds from source. `tui` is
# the second binary's suite and runs as one unit like the others.
SUITE_UNITS = {
    "compare": ("compare",),
    "startup": ("startup",),
    "loading": ("loading-cold", "loading-warm"),
    "tools": ("tools",),
    "reload": ("reload",),
    "tui": ("tui",),
    "compile": ("compile",),
}

# OS key -> GitHub-hosted runner label. Every OS runs as its own job on its own
# machine; the keys are what RESULTS.md and the artifact names use.
PLATFORMS = {
    "linux": "ubuntu-24.04",
    "macos": "macos-latest",
    "windows": "windows-latest",
}


def _names(raw: str) -> set[str]:
    return {part.strip().lower() for part in raw.replace(" ", ",").split(",") if part.strip()}


def select(raw: str) -> dict[str, bool]:
    names = _names(raw)
    if not names or "all" in names:
        return {suite: True for suite in SUITES}
    unknown = sorted(names - set(SUITES))
    if unknown:
        raise SystemExit(f"unknown suite(s): {', '.join(unknown)}; known: all, {', '.join(SUITES)}")
    return {suite: suite in names for suite in SUITES}


def platforms(raw: str) -> list[dict[str, str]]:
    """Matrix entries for the chosen OSes, always in linux, macos, windows order."""
    names = _names(raw)
    if not names or "all" in names:
        names = set(PLATFORMS)
    unknown = sorted(names - set(PLATFORMS))
    if unknown:
        raise SystemExit(f"unknown OS name(s): {', '.join(unknown)}; known: all, {', '.join(PLATFORMS)}")
    return [{"os": key, "runner": runner} for key, runner in PLATFORMS.items() if key in names]


def expected_units(raw: str) -> list[str]:
    """The unit parts the merge job must receive, in suite order, compile last."""
    chosen = select(raw)
    units: list[str] = []
    for suite in SUITES:
        if chosen[suite]:
            units.extend(SUITE_UNITS[suite])
    return units


def measure_units(raw: str) -> list[str]:
    return [unit for unit in expected_units(raw) if unit != "compile"]


def jobs(os_raw: str, suites_raw: str) -> list[dict[str, str]]:
    """One entry per (OS, unit) that measures the prebuilt binary."""
    units = measure_units(suites_raw)
    return [
        {"os": platform["os"], "runner": platform["runner"], "unit": unit}
        for platform in platforms(os_raw)
        for unit in units
    ]


def compile_jobs(os_raw: str, suites_raw: str) -> list[dict[str, str]]:
    """One entry per OS when `compile` is selected; empty otherwise."""
    if not select(suites_raw)["compile"]:
        return []
    return platforms(os_raw)


def _matrix(entries: list[dict[str, str]]) -> str:
    return "matrix=" + json.dumps({"include": entries}, separators=(",", ":"))


def main(argv: list[str]) -> int:
    command = argv[1] if len(argv) > 1 else None
    if command == "--platforms":
        raw = argv[2] if len(argv) > 2 else "all"
        print(_matrix(platforms(raw)))
        return 0
    if command == "--jobs":
        os_raw = argv[2] if len(argv) > 2 else "all"
        suites_raw = argv[3] if len(argv) > 3 else "all"
        entries = jobs(os_raw, suites_raw)
        print(_matrix(entries))
        print(f"has_jobs={'true' if entries else 'false'}")
        return 0
    if command == "--compile":
        os_raw = argv[2] if len(argv) > 2 else "all"
        suites_raw = argv[3] if len(argv) > 3 else "all"
        entries = compile_jobs(os_raw, suites_raw)
        print(_matrix(entries))
        print(f"has_jobs={'true' if entries else 'false'}")
        return 0
    if command == "--expect":
        raw = argv[2] if len(argv) > 2 else "all"
        print("units=" + ",".join(expected_units(raw)))
        return 0
    if command is not None and command.startswith("-"):
        raise SystemExit(f"unknown option {command}; use --platforms, --jobs, --compile, or --expect")
    raw = command if command is not None else "all"
    for suite, enabled in select(raw).items():
        print(f"{suite}={'true' if enabled else 'false'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
