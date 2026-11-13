#!/usr/bin/env python3
"""Fail when a lib test module is selected by no cohort in `ci.yml`.

`ci.yml` compiles the lib tests of a fixed package set and then runs them through
filtered cohort steps (`scripts/ci/run_lib_tests.py ... -- <filter>`). A test
module that no filter names compiles in every run and never executes, so a green
check proves it builds, not that it passes (#226). Two merged pull requests
shipped tests that never ran that way: `crates/tool-types/src/verdict.rs` (#212)
and `crates/app-agent/src/agent/prewalk_tests.rs` (#202).

This gate measures the real selection instead of reading the filters: it runs
each compiled test executable's `--list <filter>` and collects the names libtest
actually selects. Module paths that hold tests but are selected by no cohort are
the uncovered set. That set is frozen in
`scripts/ci/lib_test_coverage.baseline.json`:

- a measured module that is not in the baseline fails, so a new test module
  cannot merge unexecuted;
- a baseline module that is fully covered is reported as a drop and passes, so
  closing a gap needs no baseline edit;
- `--update` re-pins the baseline and is the only way to accept a new module
  without a cohort, which makes each acceptance a visible commit.

Scope: lib targets only. Integration targets are `check_test_targets.py`.

Usage:
  python3 scripts/ci/check_lib_test_coverage.py --artifacts "$RUNNER_TEMP/test-artifacts.jsonl"
  python3 scripts/ci/check_lib_test_coverage.py --artifacts ... --update
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_WORKFLOW = ROOT / ".github/workflows/ci.yml"
DEFAULT_BASELINE = ROOT / "scripts/ci/lib_test_coverage.baseline.json"

CONTINUATION = re.compile(r"\\\n\s*")
RUN_LIB_TESTS = re.compile(r"run_lib_tests\.py\s+([^\n]*)")
PACKAGE_ARG = re.compile(r"--package\s+(\S+)")


def package_name_from_id(package_id: str) -> str:
    if "#" in package_id:
        return package_id.rsplit("#", 1)[1].split("@", 1)[0]
    return package_id.split(" ", 1)[0]


def cohort_filters(workflow: Path) -> dict[str, list[list[str]]]:
    """Package -> list of libtest filter argument lists, from the workflow text."""
    text = CONTINUATION.sub(" ", workflow.read_text(encoding="utf-8"))
    filters: dict[str, list[list[str]]] = defaultdict(list)
    for match in RUN_LIB_TESTS.finditer(text):
        arguments = f" {match.group(1)} "
        package = PACKAGE_ARG.search(arguments)
        if package is None:
            continue
        selected: list[str] = []
        if " -- " in arguments:
            tail = arguments.split(" -- ", 1)[1]
            selected = [token for token in tail.split() if not token.startswith("-")]
        filters[package.group(1)].append(selected)
    return filters


def lib_executables(artifacts: Path) -> dict[str, Path]:
    executables: dict[str, Path] = {}
    with artifacts.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("reason") != "compiler-artifact":
                continue
            target = message.get("target") or {}
            if "lib" not in (target.get("kind") or []):
                continue
            if not (message.get("profile") or {}).get("test"):
                continue
            executable = message.get("executable")
            if not executable:
                continue
            package = package_name_from_id(str(message.get("package_id", "")))
            executables.setdefault(package, Path(executable))
    return executables


def listed_tests(executable: Path, filters: list[str]) -> set[str]:
    command = [str(executable), "--list", *filters]
    completed = subprocess.run(command, capture_output=True, text=True, check=True)
    names: set[str] = set()
    for line in completed.stdout.splitlines():
        if line.endswith(": test"):
            names.add(line.rsplit(": test", 1)[0])
    return names


def module_of(test_name: str) -> str:
    return test_name.rsplit("::", 1)[0] if "::" in test_name else "<root>"


def measure(filters: dict[str, list[list[str]]], executables: dict[str, Path]) -> dict[str, list[str]]:
    uncovered: dict[str, list[str]] = {}
    for package in sorted(filters):
        executable = executables.get(package)
        if executable is None:
            print(f"note: {package} has no lib test executable in this build; skipped", file=sys.stderr)
            continue
        every = listed_tests(executable, [])
        selected: set[str] = set()
        for selected_filters in filters[package]:
            selected |= listed_tests(executable, selected_filters)
        modules = sorted({module_of(name) for name in every - selected})
        if modules:
            uncovered[package] = modules
        print(
            f"{package}: {len(every)} lib tests, {len(selected & every)} selected, "
            f"{len(every - selected)} never run in {len(modules)} modules"
        )
    return uncovered


def load_baseline(path: Path) -> dict[str, list[str]]:
    if not path.is_file():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))["packages"]


def write_baseline(path: Path, packages: dict[str, list[str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    document = {
        "version": 1,
        "description": (
            "Lib test modules that no ci.yml cohort selects. A measured module absent "
            "here fails; regenerate with --update only when the module genuinely cannot "
            "run in a cohort."
        ),
        "packages": {name: sorted(modules) for name, modules in sorted(packages.items())},
    }
    path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--artifacts", type=Path, required=True,
                        help="cargo --message-format=json output of the test compile step")
    parser.add_argument("--workflow", type=Path, default=DEFAULT_WORKFLOW)
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument("--update", action="store_true", help="re-pin the baseline")
    args = parser.parse_args(argv)

    if not args.artifacts.is_file():
        print(f"check_lib_test_coverage: artifacts file not found: {args.artifacts}", file=sys.stderr)
        return 2

    filters = cohort_filters(args.workflow)
    if not filters:
        print(
            f"{args.workflow.name} has no run_lib_tests.py cohort, so no lib test runs at all",
            file=sys.stderr,
        )
        return 1

    executables = lib_executables(args.artifacts)
    # A package compiled with `--lib` but named by no cohort is the worst shape
    # of this gap: `measure` iterates cohorts, so such a package would be
    # invisible rather than reported as fully uncovered.
    uncohorted = sorted(set(executables) - set(filters))
    measured = measure(filters, executables)
    if args.update:
        write_baseline(args.baseline, measured)
        total = sum(len(modules) for modules in measured.values())
        print(f"Wrote {total} module entries in {len(measured)} packages to {args.baseline}")
        return 0

    baseline = load_baseline(args.baseline)
    problems: list[str] = []
    for package in uncohorted:
        problems.append(
            f"{package}: compiled with `--lib` but no {args.workflow.name} cohort names it, "
            "so every one of its lib tests is compiled and never started"
        )
    for package, modules in sorted(measured.items()):
        known = set(baseline.get(package, []))
        for module in modules:
            if module not in known:
                problems.append(
                    f"{package}: {module} holds lib tests that no {args.workflow.name} "
                    "cohort selects, and it is not in the baseline"
                )
    drops = [
        (package, module)
        for package, modules in sorted(baseline.items())
        for module in modules
        if module not in measured.get(package, [])
    ]

    if problems:
        print("Lib test modules that CI compiles but never runs:")
        for problem in problems:
            print(f"  - {problem}")
        print(
            "\nName the module in a cohort step in ci.yml (a filter is a substring of the "
            "test\npath, so `tool::batch::batch_tests::` selects the whole module), or "
            "accept it in\nscripts/ci/lib_test_coverage.baseline.json with --update and a "
            "reason in review."
        )
        return 1

    total = sum(len(modules) for modules in measured.values())
    print(f"Lib test cohort coverage OK: {total} modules in {len(measured)} packages still uncovered")
    for package, module in drops:
        print(f"  covered now: {package} {module} (drop the baseline entry with --update)")
    return 0


if __name__ == "__main__":
    sys.exit(main())