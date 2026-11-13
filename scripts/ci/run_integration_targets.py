#!/usr/bin/env python3
"""Run every integration target listed in `scripts/ci/integration_targets.json`.

The CI compile step builds these executables with `cargo test --tests --no-run`
and writes the reported paths to an artifacts file (#226). This script starts
them, one `run_lib_tests.py` call per target so a failure names the target that
failed, using the executable already on disk instead of asking cargo to rebuild
the selection set (#19).

`--min-passed` comes from the manifest: a target that silently selects no test
reports `0 passed` with exit 0, which is indistinguishable from a healthy run in
a CI summary. The recorded count turns that into a failure.

Usage:
  python3 scripts/ci/run_integration_targets.py --artifacts "$TEST_ARTIFACTS"
  python3 scripts/ci/run_integration_targets.py --artifacts f.jsonl --only sdk
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "scripts/ci/integration_targets.json"
DEFAULT_RUNNER = ROOT / "scripts/ci/run_lib_tests.py"


def skips(target: dict) -> list[str]:
    """The `--skip` arguments of one target.

    A target may name tests that stay out of CI with a recorded reason; libtest
    matches `--skip` by substring, so the row lists the full test name. The
    remaining tests still run, and `min_passed` is the count left.
    """
    arguments: list[str] = []
    for name in target.get("skipped", []):
        arguments.extend(["--skip", name])
    return arguments


def load_targets(path: Path) -> list[dict]:
    document = json.loads(path.read_text(encoding="utf-8"))
    return document["targets"]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--artifacts", required=True, type=Path)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--runner", type=Path, default=DEFAULT_RUNNER)
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument(
        "--only",
        action="append",
        default=[],
        help="restrict to a package name; repeatable",
    )
    args = parser.parse_args(argv)

    if not args.artifacts.is_file():
        print(f"::error::artifacts file not found: {args.artifacts}", file=sys.stderr)
        return 2

    targets = load_targets(args.manifest)
    if args.only:
        targets = [target for target in targets if target["package"] in args.only]
    if not targets:
        print("::error::no integration targets selected", file=sys.stderr)
        return 1

    failures: list[str] = []
    for target in targets:
        label = f"{target['package']}::{target['target']}"
        print(f"::group::integration {label}", flush=True)
        completed = subprocess.run(
            [
                sys.executable,
                str(args.runner),
                "--artifacts",
                str(args.artifacts),
                "--kind",
                "test",
                "--target-name",
                target["target"],
                "--package",
                target["package"],
                "--min-passed",
                str(target["min_passed"]),
                "--timeout",
                str(args.timeout),
                "--",
                *skips(target),
            ],
            cwd=ROOT,
        )
        print("::endgroup::", flush=True)
        if completed.returncode != 0:
            failures.append(label)

    if failures:
        print(
            f"::error::{len(failures)} of {len(targets)} integration targets failed: "
            + ", ".join(failures),
            file=sys.stderr,
        )
        return 1
    print(f"Integration contracts OK: {len(targets)} targets, {len(targets)} passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
