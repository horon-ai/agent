#!/usr/bin/env python3
"""Decide what the package compatibility reports prove and what the install
guide may claim.

`.github/workflows/package-compat.yml` runs `tests/package_end_to_end.rs` on a
native runner for each of the six package targets, against the real agent binary
and the real worked example, and each job uploads one JSON report per target
(the path in `HORON_E2E_REPORT`). This script is the gate over those reports:

    package_compat.py check --reports <dir> [--require-targets t1,t2] \\
        [--matrix-out <file>]
    package_compat.py docs-claims --install packages/docs-site/reference/release/install.mdx \\
        --reports <dir>

`check` refuses a report whose `cross_compiled` is not false, one whose host
triple is not one of the six native targets below, one that lacks a required
step for either package kind, one whose step failed, a second report for the
same target, and (with `--require-targets`) a required target with no report. It
always prints the support matrix: one row per kind and step, one column per
target.

`docs-claims` reads the table that lists package targets in an install guide and
refuses a row that says the target is verified when no passing report exists for
it, so the page can only advertise a target the workflow ran to completion. A
target the workflow does not run is refused in that table as well.

Both commands read files only: no network, no build, standard library.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path

# The six targets the workflow runs, which are also the six keys a declaration
# may name: the release matrix minus FreeBSD, which publishes the agent alone.
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
)

KINDS = ("extension", "plugin")

# The steps every report must hold, in the order the matrix shows them. A plugin
# carries one extra step because it is the kind that bundles resources (a skill,
# rule, prompt, MCP server or command) beside its executable.
STEP_ORDER = (
    "install",
    "next_turn_tool_call",
    "resource_call",
    "cancel",
    "replace",
    "remove_blocks_new_calls",
    "descendants_reaped",
    "bad_target_refused",
    "integrity_refused",
)

REQUIRED = {
    "extension": frozenset(
        {
            "install",
            "next_turn_tool_call",
            "cancel",
            "replace",
            "remove_blocks_new_calls",
            "descendants_reaped",
            "bad_target_refused",
            "integrity_refused",
        }
    ),
    "plugin": frozenset(STEP_ORDER),
}

CELL_OK = "ok"
CELL_FAILED = "FAIL"
CELL_MISSING = "missing"
CELL_NO_REPORT = "no report"
CELL_NOT_APPLICABLE = "n/a"

# The marker an install guide puts in the verified column. The Korean page uses
# the same machine-readable words translated, and both are accepted so one rule
# covers both languages.
MARKER_YES = frozenset({"yes", "예"})
MARKER_NO = frozenset({"no", "아니요", "아니오"})


class ReportError(Exception):
    """A report that cannot be read as evidence at all."""


class ClaimsError(Exception):
    """An install guide whose support table cannot be read."""


@dataclass(frozen=True)
class Report:
    """One target's report, already shape-checked."""

    path: str
    target: str
    os: str
    arch: str
    steps: dict[str, dict[str, bool]]

    def problems(self) -> list[str]:
        """Every reason this report is not proof, in a stable order."""
        found: list[str] = []
        for kind in KINDS:
            ran = self.steps.get(kind)
            if ran is None:
                found.append(f"{self.path}: no {kind} steps")
                continue
            for step in STEP_ORDER:
                if step not in REQUIRED[kind]:
                    continue
                if step not in ran:
                    found.append(f"{self.path}: {kind} has no {step} step")
                elif not ran[step]:
                    found.append(f"{self.path}: {kind} {step} failed")
        return found


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ReportError(message)


def load_report(path: Path) -> Report:
    """Parse one report file, raising `ReportError` when its shape is wrong.

    A step that failed is not an error here: the report keeps it so the matrix
    can show it, and `Report.problems` reports it to the caller.
    """
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise ReportError(f"{path.name}: cannot read the report: {exc}") from exc
    try:
        data = json.loads(text)
    except json.JSONDecodeError as exc:
        raise ReportError(f"{path.name}: not JSON: {exc}") from exc

    _require(isinstance(data, dict), f"{path.name}: the report must be a JSON object")
    _require(
        data.get("schema") == 1,
        f"{path.name}: schema must be 1, got {data.get('schema')!r}",
    )
    _require(
        data.get("cross_compiled") is False,
        f"{path.name}: cross_compiled must be false; "
        "a cross-compiled log is not native execution evidence",
    )
    target = data.get("host_triple")
    _require(
        target in TARGETS,
        f"{path.name}: host_triple {target!r} is not one of the six native targets",
    )
    for key in ("os", "arch"):
        value = data.get(key)
        _require(
            isinstance(value, str) and value.strip() != "",
            f"{path.name}: {key} must be a non-empty string",
        )
    entries = data.get("steps")
    _require(
        isinstance(entries, list) and entries != [],
        f"{path.name}: steps must be a non-empty list",
    )

    steps: dict[str, dict[str, bool]] = {}
    for index, entry in enumerate(entries):
        where = f"{path.name}: steps[{index}]"
        _require(isinstance(entry, dict), f"{where} must be an object")
        kind = entry.get("kind")
        _require(kind in KINDS, f"{where} has kind {kind!r}, expected one of {KINDS}")
        step = entry.get("step")
        _require(
            step in REQUIRED[kind],
            f"{where} has step {step!r}, which is not a {kind} step",
        )
        ok = entry.get("ok")
        _require(isinstance(ok, bool), f"{where} needs a boolean ok")
        ran = steps.setdefault(kind, {})
        _require(step not in ran, f"{where} repeats the {kind} {step} step")
        ran[step] = ok

    return Report(
        path=path.name,
        target=target,
        os=data["os"],
        arch=data["arch"],
        steps=steps,
    )


def collect(reports_dir: Path) -> tuple[dict[str, Report], list[str]]:
    """Every readable report by target, and every reason one was not usable."""
    problems: list[str] = []
    if not reports_dir.is_dir():
        return {}, [f"{reports_dir}: the report directory does not exist"]
    files = sorted(
        path
        for path in reports_dir.iterdir()
        if path.is_file() and path.suffix == ".json"
    )
    if not files:
        return {}, [f"{reports_dir}: no *.json reports"]

    reports: dict[str, Report] = {}
    for path in files:
        try:
            report = load_report(path)
        except ReportError as exc:
            problems.append(str(exc))
            continue
        if report.target in reports:
            problems.append(
                f"{path.name}: a second report for {report.target} "
                f"(already {reports[report.target].path})"
            )
            continue
        reports[report.target] = report
        problems.extend(report.problems())
    return reports, problems


def passing(reports: dict[str, Report]) -> dict[str, Report]:
    """The reports that prove their target: complete and every step true."""
    return {
        target: report for target, report in reports.items() if not report.problems()
    }


def render_matrix(reports: dict[str, Report]) -> str:
    """The support matrix: kind and step down, target across."""
    lines = [
        "# Package support matrix",
        "",
        f"Reports: {len(reports)} of {len(TARGETS)} targets.",
        "",
        "| Kind | Step | " + " | ".join(TARGETS) + " |",
        "| --- | --- | " + " | ".join("---" for _ in TARGETS) + " |",
    ]
    for kind in KINDS:
        for step in STEP_ORDER:
            cells: list[str] = []
            for target in TARGETS:
                report = reports.get(target)
                if report is None:
                    cells.append(CELL_NO_REPORT)
                elif step not in REQUIRED[kind]:
                    cells.append(CELL_NOT_APPLICABLE)
                else:
                    ran = report.steps.get(kind, {})
                    if step not in ran:
                        cells.append(CELL_MISSING)
                    elif ran[step]:
                        cells.append(CELL_OK)
                    else:
                        cells.append(CELL_FAILED)
            lines.append(f"| {kind} | {step} | " + " | ".join(cells) + " |")
    lines.append("")
    return "\n".join(lines)


def requested_targets(raw: str) -> list[str]:
    """The comma-separated `--require-targets` value, refusing unknown names."""
    names = [name.strip() for name in raw.split(",") if name.strip() != ""]
    unknown = [name for name in names if name not in TARGETS]
    if unknown:
        raise SystemExit(
            "package_compat: --require-targets names "
            f"{', '.join(unknown)}, which the workflow does not run"
        )
    return names


def command_check(reports_dir: Path, required: list[str], matrix_out: Path | None) -> int:
    reports, problems = collect(reports_dir)
    for target in required:
        if target not in reports:
            problems.append(f"no report for the required target {target}")

    matrix = render_matrix(reports)
    sys.stdout.write(matrix)
    if matrix_out is not None:
        matrix_out.parent.mkdir(parents=True, exist_ok=True)
        matrix_out.write_text(matrix, encoding="utf-8")

    if problems:
        for problem in problems:
            print(f"package_compat: {problem}", file=sys.stderr)
        return 1
    print(
        f"package_compat: {len(passing(reports))} native target reports passed "
        "every required step",
        file=sys.stderr,
    )
    return 0


def _cells(line: str) -> list[str]:
    body = line.strip()
    if body.startswith("|"):
        body = body[1:]
    if body.endswith("|"):
        body = body[:-1]
    return [cell.strip() for cell in body.split("|")]


def _is_separator(cell: str) -> bool:
    return cell != "" and set(cell) <= set(":-") and "-" in cell


def advertised_targets(page: Path) -> list[tuple[str, bool]]:
    """The package target rows of one install guide, as (target, verified)."""
    try:
        text = page.read_text(encoding="utf-8")
    except OSError as exc:
        raise ClaimsError(f"{page}: cannot read the page: {exc}") from exc

    blocks: list[list[str]] = []
    current: list[str] = []
    for line in text.splitlines():
        if line.strip().startswith("|"):
            current.append(line)
        elif current:
            blocks.append(current)
            current = []
    if current:
        blocks.append(current)

    tables: list[list[list[str]]] = []
    for block in blocks:
        rows = [_cells(line) for line in block]
        if len(rows) < 3 or not all(len(row) == 3 for row in rows):
            continue
        if not _is_separator(rows[1][0]) or not _is_separator(rows[1][2]):
            continue
        tables.append(rows)

    target_tables = [
        rows
        for rows in tables
        if any(row[0].startswith("`") and row[0].endswith("`") for row in rows[2:])
    ]
    if not target_tables:
        raise ClaimsError(
            f"{page}: no table lists a package target; "
            "the install guide needs its package support table"
        )
    if len(target_tables) > 1:
        raise ClaimsError(
            f"{page}: more than one table lists package targets; "
            "keep one package support table"
        )

    rows = target_tables[0]
    claims: list[tuple[str, bool]] = []
    seen: set[str] = set()
    for index, row in enumerate(rows[2:], start=2):
        cell = row[0]
        if not (cell.startswith("`") and cell.endswith("`")):
            raise ClaimsError(
                f"{page}: table row {index} does not start with a backticked target"
            )
        target = cell[1:-1].strip()
        if target not in TARGETS:
            raise ClaimsError(
                f"{page}: table row {index} names {target!r}, "
                "which the Package compat workflow does not run"
            )
        if target in seen:
            raise ClaimsError(f"{page}: table row {index} repeats {target}")
        seen.add(target)
        marker = row[2].lower()
        if marker in MARKER_YES:
            verified = True
        elif marker in MARKER_NO:
            verified = False
        else:
            raise ClaimsError(
                f"{page}: table row {index} has marker {row[2]!r}; "
                "write Yes or No"
            )
        claims.append((target, verified))

    missing = [target for target in TARGETS if target not in seen]
    if missing:
        raise ClaimsError(
            f"{page}: the package support table omits {', '.join(missing)}; "
            "either advertise or refuse each target"
        )
    return claims


def command_docs_claims(pages: list[Path], reports_dir: Path) -> int:
    reports, report_problems = collect(reports_dir)
    proven = passing(reports)

    failures: list[str] = []
    announcements: list[str] = []
    for page in pages:
        try:
            claims = advertised_targets(page)
        except ClaimsError as exc:
            failures.append(str(exc))
            continue
        for target, verified in claims:
            if not verified:
                continue
            if target in proven:
                announcements.append(
                    f"{page.name}: {target} verified by {proven[target].path}"
                )
            else:
                failures.append(
                    f"{page.name} advertises {target}, but no passing report "
                    "exists for it"
                )

    for line in announcements:
        print(f"package_compat: {line}")
    if failures:
        for failure in failures:
            print(f"package_compat: {failure}", file=sys.stderr)
        for problem in report_problems:
            print(f"package_compat: report problem: {problem}", file=sys.stderr)
        return 1
    print(
        f"package_compat: every advertised target has a passing report "
        f"({len(announcements)} claims)",
        file=sys.stderr,
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Gate the package compatibility reports and the install "
        "guide's support claims.",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    check = commands.add_parser(
        "check", help="refuse incomplete or cross-compiled reports"
    )
    check.add_argument("--reports", type=Path, required=True)
    check.add_argument(
        "--require-targets",
        default="",
        help="comma-separated targets that must each have a report",
    )
    check.add_argument("--matrix-out", type=Path)

    claims = commands.add_parser(
        "docs-claims", help="refuse an install guide that advertises an unproven target"
    )
    claims.add_argument(
        "--install",
        type=Path,
        action="append",
        required=True,
        help="install guide whose package support table is checked (repeatable)",
    )
    claims.add_argument("--reports", type=Path, required=True)

    args = parser.parse_args(argv)
    if args.command == "check":
        return command_check(
            args.reports, requested_targets(args.require_targets), args.matrix_out
        )
    return command_docs_claims(args.install, args.reports)


if __name__ == "__main__":
    sys.exit(main())
