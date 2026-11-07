#!/usr/bin/env python3
"""Turn the benchmark workflow's inputs into GitHub step outputs.

    bench_select.py <suites>              one `<suite>=true|false` line per known suite
    bench_select.py --platforms <os-list> one `matrix=<json>` line for the OS job matrix

`all` (or an empty input) selects every suite or every OS. An unknown name is an
error, so a typo fails the dispatch instead of silently measuring nothing.
"""

from __future__ import annotations

import json
import sys

SUITES = ("compare", "startup", "loading", "tools", "reload", "compile")

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


def main(argv: list[str]) -> int:
    if len(argv) > 1 and argv[1] == "--platforms":
        raw = argv[2] if len(argv) > 2 else "all"
        print("matrix=" + json.dumps({"include": platforms(raw)}, separators=(",", ":")))
        return 0
    raw = argv[1] if len(argv) > 1 else "all"
    for suite, enabled in select(raw).items():
        print(f"{suite}={'true' if enabled else 'false'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
