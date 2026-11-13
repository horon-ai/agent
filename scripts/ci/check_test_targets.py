#!/usr/bin/env python3
"""Fail when an integration test target is never run by CI.

`ci.yml` builds lib tests and runs them through `run_lib_tests.py`; that surface
is covered by `scripts/ci/check_lib_test_coverage.py`. Integration targets
(`tests/*.rs`, `crates/*/tests/*.rs`) are a separate gap (#226): only
`--test e2e` was named anywhere, so every other target was either never built or
built and never executed.

This makes the decision explicit per target. A target either

- is listed in `scripts/ci/integration_targets.json`, which is what the
  `Run integration contracts` step executes, or
- has an entry in `scripts/ci/test_target_allowlist.json` saying why it does not
  run in CI.

Both files are data, not code. A target that appears without a run and without
an entry fails, and an entry or manifest row whose target no longer exists also
fails, so neither list can rot in either direction. An entry needs a non-empty
reason: the point of the list is that each exclusion is a decision someone wrote
down.

The scan is static (JSON plus `cargo metadata`) so it runs in seconds in the
guardrail job without building anything.

Usage:
  python3 scripts/ci/check_test_targets.py
  python3 scripts/ci/check_test_targets.py --workflow .github/workflows/ci.yml
  python3 scripts/ci/check_test_targets.py --update   # rebaseline the allowlist
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_WORKFLOW = ROOT / ".github/workflows/ci.yml"
DEFAULT_MANIFEST = ROOT / "scripts/ci/integration_targets.json"
DEFAULT_ALLOWLIST = ROOT / "scripts/ci/test_target_allowlist.json"

# A shell line continued with a trailing backslash: joined so the arguments of
# one `run_lib_tests.py` call are readable as one string.
CONTINUATION = re.compile(r"\\\n\s*")
RUNNER_CALL = re.compile(r"run_integration_targets\.py")
RUN_LIB_TESTS = re.compile(r"run_lib_tests\.py\s+([^\n]*)")
PACKAGE_ARG = re.compile(r"--package\s+(\S+)")
TARGET_NAME_ARG = re.compile(r"--target-name\s+(\S+)")
# `cargo test ... --test <name>`; the name is the argument right after `--test`.
CARGO_TEST_TARGET = re.compile(r"--test\s+([A-Za-z0-9_]+)")
# `packages=(-p a -p b)` and `packages+=(-p c)` in the compile step.
PACKAGE_ARRAY = re.compile(r"packages\+?=\(([^)]*)\)")
PACKAGE_FLAG = re.compile(r"-p\s+([A-Za-z0-9_.-]+)")


def joined_workflow(path: Path) -> str:
    return CONTINUATION.sub(" ", path.read_text(encoding="utf-8"))


def run_lib_packages(text: str) -> list[tuple[str, list[str]]]:
    """Every `run_lib_tests.py` call as (package, forwarded libtest filters)."""
    calls: list[tuple[str, list[str]]] = []
    for match in RUN_LIB_TESTS.finditer(text):
        arguments = match.group(1)
        package = PACKAGE_ARG.search(arguments)
        if package is None:
            continue
        filters: list[str] = []
        # Everything after the first bare `--` is libtest's own argument list.
        if " -- " in f" {arguments} ":
            tail = f" {arguments} ".split(" -- ", 1)[1]
            filters = [token for token in tail.split() if not token.startswith("-")]
        calls.append((package.group(1), filters))
    return calls


def compiled_packages(text: str) -> set[str]:
    """Packages whose lib/bin tests the compile step builds."""
    packages: set[str] = set()
    for line in text.splitlines():
        if "--lib --bins" in line:
            packages.update(PACKAGE_FLAG.findall(line))
    for array in PACKAGE_ARRAY.findall(text):
        packages.update(PACKAGE_FLAG.findall(array))
    return packages


def named_test_targets(text: str) -> set[str]:
    return set(CARGO_TEST_TARGET.findall(text))


def runs_manifest(text: str) -> bool:
    """True when the workflow executes the manifest-driven runner."""
    return bool(RUNNER_CALL.search(text))


def metadata_test_targets() -> list[dict]:
    """Integration targets of every workspace member, from cargo metadata."""
    completed = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    metadata = json.loads(completed.stdout)
    workspace_root = Path(metadata["workspace_root"])
    targets: list[dict] = []
    for package in metadata["packages"]:
        for target in package["targets"]:
            if "test" not in target["kind"]:
                continue
            targets.append(
                {
                    "package": package["name"],
                    "target": target["name"],
                    "path": str(Path(target["src_path"]).relative_to(workspace_root)),
                }
            )
    return targets


def load_manifest(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    return json.loads(path.read_text(encoding="utf-8"))["targets"]


def load_allowlist(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    return json.loads(path.read_text(encoding="utf-8"))["entries"]


def write_allowlist(path: Path, entries: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    document = {"version": 1, "entries": entries}
    path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--workflow", type=Path, default=DEFAULT_WORKFLOW)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--allowlist", type=Path, default=DEFAULT_ALLOWLIST)
    parser.add_argument(
        "--update",
        action="store_true",
        help="rewrite the allowlist with every target that does not run today",
    )
    args = parser.parse_args(argv)

    text = joined_workflow(args.workflow)
    packages = compiled_packages(text)
    calls = run_lib_packages(text)
    named = named_test_targets(text)
    manifest = load_manifest(args.manifest)
    targets = metadata_test_targets()

    problems: list[str] = []

    if not packages:
        problems.append(
            f"{args.workflow.name} has no compile step (a `cargo test ... --lib --bins` "
            "line), so the guard cannot tell what CI builds"
        )
    for package, _ in calls:
        if package not in packages:
            problems.append(
                f"{package}: run by run_lib_tests.py but absent from the compile step, "
                "so its test executable is never built"
            )

    if manifest and not runs_manifest(text):
        problems.append(
            f"{args.workflow.name} lists {len(manifest)} integration targets in "
            f"{args.manifest.name} but never runs run_integration_targets.py, so none "
            "of them starts"
        )
    for row in manifest:
        if not row.get("target") or not row.get("package"):
            problems.append(f"manifest row without package/target: {row!r}")
            continue
        if not isinstance(row.get("min_passed"), int) or row["min_passed"] < 1:
            problems.append(
                f"manifest row {row['package']}::{row['target']} needs a positive "
                "`min_passed`: a target that selects nothing must fail, not pass"
            )
        if row["package"] not in packages:
            problems.append(
                f"manifest row {row['package']}::{row['target']} names a package the "
                "compile step does not build"
            )
        skipped = row.get("skipped", [])
        if not isinstance(skipped, list) or not all(
            isinstance(name, str) and name.strip() for name in skipped
        ):
            problems.append(
                f"manifest row {row['package']}::{row['target']} has a `skipped` list "
                "that is not made of test names"
            )
        elif skipped and not row.get("skip_reason", "").strip():
            problems.append(
                f"manifest row {row['package']}::{row['target']} skips "
                f"{len(skipped)} test(s) without a `skip_reason`: a test left out of "
                "CI must say why"
            )

    allowlist = load_allowlist(args.allowlist)
    excused = {(entry["package"], entry["target"]) for entry in allowlist}
    listed = {(row["package"], row["target"]) for row in manifest}
    known = {(target["package"], target["target"]) for target in targets}
    unrun = [
        target
        for target in targets
        if target["target"] not in named
        and (target["package"], target["target"]) not in excused
        and (target["package"], target["target"]) not in listed
    ]

    if args.update:
        entries = [
            {
                "package": target["package"],
                "target": target["target"],
                "path": target["path"],
                "reason": "TODO: why this target does not run in ci.yml",
            }
            for target in sorted(unrun, key=lambda item: (item["package"], item["target"]))
        ]
        write_allowlist(args.allowlist, entries)
        print(f"Wrote {len(entries)} entries to {args.allowlist}")
        return 0

    for target in unrun:
        problems.append(
            f"{target['package']}: integration target {target['target']} "
            f"({target['path']}) is never run by {args.workflow.name} and has no "
            "allowlist entry"
        )
    for package, target in sorted((excused | listed) - known):
        problems.append(
            f"{package}::{target} names no integration target; remove the stale "
            "manifest row or allowlist entry"
        )
    for entry in allowlist:
        if not entry.get("reason", "").strip() or entry["reason"].startswith("TODO"):
            problems.append(
                f"allowlist entry {entry['package']}::{entry['target']} has no reason"
            )
    overlap = excused & listed
    for package, target in sorted(overlap):
        problems.append(
            f"{package}::{target} is both run and allowlisted; drop the allowlist entry"
        )

    if problems:
        print("Integration test targets CI never runs:")
        for problem in problems:
            print(f"  - {problem}")
        print(
            "\nA compiled test that never runs proves only that it builds. Add it to "
            "scripts/ci/integration_targets.json,\nor record in "
            "scripts/ci/test_target_allowlist.json why it cannot run in CI."
        )
        return 1

    print(
        f"Integration test targets OK: {len(targets)} targets, "
        f"{len(listed)} run by {args.manifest.name}, "
        f"{len(named)} named as `--test`, {len(allowlist)} allowlisted"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
