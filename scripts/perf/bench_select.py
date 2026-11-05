#!/usr/bin/env python3
"""Turn the benchmark workflow's `suites` input into GitHub step outputs.

Prints one `<suite>=true|false` line per known suite. `all` (or an empty input)
selects every suite. An unknown name is an error, so a typo fails the dispatch
instead of silently measuring nothing.
"""

from __future__ import annotations

import sys

SUITES = ("compare", "startup", "loading", "tools", "reload", "compile")


def select(raw: str) -> dict[str, bool]:
    names = {part.strip() for part in raw.replace(" ", ",").split(",") if part.strip()}
    if not names or "all" in names:
        return {suite: True for suite in SUITES}
    unknown = sorted(names - set(SUITES))
    if unknown:
        raise SystemExit(f"unknown suite(s): {', '.join(unknown)}; known: all, {', '.join(SUITES)}")
    return {suite: suite in names for suite in SUITES}


def main(argv: list[str]) -> int:
    raw = argv[1] if len(argv) > 1 else "all"
    for suite, enabled in select(raw).items():
        print(f"{suite}={'true' if enabled else 'false'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
