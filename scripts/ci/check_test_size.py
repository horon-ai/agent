#!/usr/bin/env python3
"""Enforce a ratcheting Rust test-file size budget.

Policy:
- Test Rust files above the configured LOC threshold are tracked in a baseline.
- Existing tracked oversized test files may not grow.
- New oversized test files may not be introduced.
- `--update` refreshes the baseline after intentional cleanup.
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import Any

from _rust_scan import REPO_ROOT, line_count, test_rust_files

BASELINE_FILE = REPO_ROOT / "scripts" / "ci" / "test_size_budget.json"
DEFAULT_THRESHOLD = 1200
SCAN_ROOTS = (REPO_ROOT / "src", REPO_ROOT / "crates", REPO_ROOT / "tests")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true", help="refresh the baseline")
    return parser.parse_args()


def current_oversized_files(threshold: int) -> dict[str, int]:
    files: dict[str, int] = {}
    for path in test_rust_files(SCAN_ROOTS):
        count = line_count(path)
        if count > threshold:
            files[path.relative_to(REPO_ROOT).as_posix()] = count
    return files


def load_baseline() -> dict[str, Any]:
    if not BASELINE_FILE.exists():
        return {"version": 1, "threshold_loc": DEFAULT_THRESHOLD, "tracked_files": {}}
    data = json.loads(BASELINE_FILE.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise SystemExit(f"error: invalid baseline file format: {BASELINE_FILE}")
    threshold = data.get("threshold_loc")
    tracked = data.get("tracked_files")
    if not isinstance(threshold, int) or threshold <= 0:
        raise SystemExit(f"error: invalid threshold_loc in {BASELINE_FILE}")
    if not isinstance(tracked, dict) or any(
        not isinstance(k, str) or not isinstance(v, int) or v <= 0 for k, v in tracked.items()
    ):
        raise SystemExit(f"error: invalid tracked_files in {BASELINE_FILE}")
    return data


def write_baseline(threshold: int, tracked_files: dict[str, int]) -> None:
    BASELINE_FILE.write_text(
        json.dumps(
            {"version": 1, "threshold_loc": threshold, "tracked_files": tracked_files},
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )


def main() -> int:
    args = parse_args()
    baseline = load_baseline()
    threshold = baseline["threshold_loc"]
    current = current_oversized_files(threshold)

    if args.update:
        write_baseline(threshold, current)
        print(
            "Updated test-size baseline: "
            f"tracked={len(baseline['tracked_files'])} -> {len(current)} oversized test files"
        )
        return 0

    tracked: dict[str, int] = baseline["tracked_files"]
    regressions: list[str] = []
    improvements: list[str] = []

    for path, lines in sorted(current.items()):
        old_lines = tracked.get(path)
        if old_lines is None:
            regressions.append(f"new oversized test file exceeds {threshold} LOC: {path} ({lines} LOC)")
        elif lines > old_lines:
            regressions.append(f"oversized test file grew: {path} ({old_lines} -> {lines} LOC)")
        elif lines < old_lines:
            improvements.append(f"oversized test file shrank: {path} ({old_lines} -> {lines} LOC)")

    for path, old_lines in sorted(tracked.items()):
        if path not in current:
            improvements.append(
                f"oversized test file no longer exceeds {threshold} LOC: {path} ({old_lines} -> OK)"
            )

    if regressions:
        print("Test-size budget exceeded:", file=sys.stderr)
        for entry in regressions:
            print(f"  - {entry}", file=sys.stderr)
        print("Run scripts/ci/check_test_size.py --update only after intentional cleanup.", file=sys.stderr)
        return 1

    if improvements:
        print("Test-size budget improved:")
        for entry in improvements:
            print(f"  - {entry}")
        print("Consider running: scripts/ci/check_test_size.py --update")
    else:
        print(f"Test-size budget OK: tracked={len(tracked)} threshold={threshold}LOC")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
