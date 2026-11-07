#!/usr/bin/env python3
"""Enforce that OS-specific cfg lines live behind the platform crates.

The platform crates (`platform`, `platform-api`, `platform-macos`,
`platform-linux`, `platform-windows`) own every OS primitive. The rest of the
tree must be free of `unix`/`windows`/`target_os`/`target_family` cfg lines
except for features that genuinely exist on only one OS.

The budget is a ratchet, like `check_code_size.py`:

- Every OS cfg line outside the platform crates is tracked in
  `platform_cfg_allowlist.json` by file, with the exact count and a reason
  explaining why the line is product code rather than an unextracted primitive.
- A new line fails the gate and is reported as `path:line: text`.
- A count that dropped also fails until `--update` lowers it.
- `--update` rewrites counts only and never invents a reason: a file it adds
  starts with the reason "TODO: classify", so the gate keeps failing until
  somebody classifies the line.

A line counts when it carries one of the cfg markers `#[cfg(`, `#[cfg_attr(`,
`#![cfg(`, `#![cfg_attr(`, `cfg!(` and mentions one of the OS tokens, regardless
of whether it sits in a test module. Inner attributes count because
`#![cfg(unix)]` gates a whole file, which is the strongest form of the OS
dependency this gate removes. Commented-out cfg lines count too, so a stale
comment needs a reason as well; delete it instead of allowlisting it.

`--report <path>` writes the remaining lines as `path<TAB>count<TAB>reason`,
the evidence file for the platform-adapter work.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

from _rust_scan import REPO_ROOT, rust_files

DEFAULT_ALLOWLIST = Path(__file__).resolve().parent / "platform_cfg_allowlist.json"
SCAN_ROOTS = ("src", "crates")
PLATFORM_CRATES = frozenset(
    {
        "platform",
        "platform-api",
        "platform-macos",
        "platform-linux",
        "platform-windows",
    }
)
CFG_MARKER = re.compile(r"#\s*!?\s*\[\s*cfg(?:_attr)?\s*\(|\bcfg!\s*\(")
OS_TOKEN = re.compile(r"\b(?:unix|windows|target_os|target_family)\b")
TODO_REASON = "TODO: classify"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update",
        action="store_true",
        help="refresh allowlist counts from the current tree (never fills reasons)",
    )
    parser.add_argument(
        "--report",
        metavar="PATH",
        help="write the remaining lines as path, count and reason",
    )
    parser.add_argument(
        "--root",
        default=str(REPO_ROOT),
        help="repository root to scan (default: this checkout)",
    )
    parser.add_argument(
        "--allowlist",
        default=str(DEFAULT_ALLOWLIST),
        help="allowlist file to check or rewrite (default: scripts/ci/platform_cfg_allowlist.json)",
    )
    return parser.parse_args(argv)


def load_allowlist(path: Path) -> dict[str, dict[str, Any]]:
    if not path.exists():
        raise SystemExit(f"error: missing allowlist file: {path}")
    data = json.loads(path.read_text(encoding="utf-8"))
    files = data.get("files") if isinstance(data, dict) else None
    if not isinstance(files, dict):
        raise SystemExit(f"error: invalid allowlist format: {path}")
    tracked: dict[str, dict[str, Any]] = {}
    for name, entry in files.items():
        if (
            not isinstance(name, str)
            or not isinstance(entry, dict)
            or not isinstance(entry.get("count"), int)
            or isinstance(entry.get("count"), bool)
            or entry["count"] <= 0
        ):
            raise SystemExit(f"error: invalid allowlist entry for {name!r} in {path}")
        reason = entry.get("reason", "")
        tracked[name] = {
            "count": entry["count"],
            "reason": reason if isinstance(reason, str) else "",
        }
    return tracked


def write_allowlist(path: Path, tracked: dict[str, dict[str, Any]]) -> None:
    payload = {"version": 1, "files": tracked}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def os_cfg_lines(path: Path) -> list[tuple[int, str]]:
    """Every line of `path` that is an OS cfg line, as (line number, text)."""
    text = path.read_text(encoding="utf-8", errors="ignore")
    return [
        (number, line.strip())
        for number, line in enumerate(text.splitlines(), 1)
        if CFG_MARKER.search(line) and OS_TOKEN.search(line)
    ]


def scan(root: Path) -> dict[str, list[tuple[int, str]]]:
    """OS cfg lines outside the platform crates, keyed by root-relative path."""
    found: dict[str, list[tuple[int, str]]] = {}
    roots = tuple(root / name for name in SCAN_ROOTS)
    for path in rust_files(roots):
        relative = path.relative_to(root).as_posix()
        parts = relative.split("/")
        if parts[0] == "crates" and len(parts) > 1 and parts[1] in PLATFORM_CRATES:
            continue
        hits = os_cfg_lines(path)
        if hits:
            found[relative] = hits
    return found


def reason_is_missing(reason: str) -> bool:
    stripped = reason.strip()
    return not stripped or stripped.upper().startswith("TODO")


def verify(
    tracked: dict[str, dict[str, Any]],
    found: dict[str, list[tuple[int, str]]],
) -> list[str]:
    """Every way the tree violates the allowlist, as printable lines."""
    violations: list[str] = []
    for path in sorted(set(tracked) | set(found)):
        hits = found.get(path, [])
        entry = tracked.get(path)
        if entry is None:
            for line, text in hits:
                violations.append(f"unaccounted OS cfg line: {path}:{line}: {text}")
            continue
        if len(hits) > entry["count"]:
            violations.append(
                f"allowlist tracks {entry['count']} OS cfg lines but {path} has "
                f"{len(hits)}:"
            )
            violations.extend(f"  {path}:{line}: {text}" for line, text in hits)
        elif len(hits) < entry["count"]:
            violations.append(
                f"allowlist overstates {path}: {entry['count']} -> {len(hits)} "
                "OS cfg lines; run scripts/ci/check_platform_cfg.py --update"
            )
        if reason_is_missing(entry["reason"]):
            violations.append(
                f"missing reason for {path}: classify its {len(hits)} OS cfg "
                "line(s) and replace \"TODO: classify\""
            )
    return violations


def to_tracked(
    found: dict[str, list[tuple[int, str]]],
    previous: dict[str, dict[str, Any]],
) -> dict[str, dict[str, Any]]:
    """Fresh allowlist: current counts, existing reasons kept, no invented ones."""
    tracked: dict[str, dict[str, Any]] = {}
    for path in sorted(found):
        entry = previous.get(path)
        reason = entry["reason"] if entry is not None else TODO_REASON
        tracked[path] = {"count": len(found[path]), "reason": reason}
    return tracked


def write_report(
    path: Path,
    found: dict[str, list[tuple[int, str]]],
    tracked: dict[str, dict[str, Any]],
) -> None:
    rows = [
        "# OS cfg lines outside the platform crates (product classifications)",
        "# path\tcount\treason",
    ]
    for name in sorted(found):
        entry = tracked.get(name)
        reason = entry["reason"].strip() if entry is not None else TODO_REASON
        rows.append(f"{name}\t{len(found[name])}\t{reason}")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(rows) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    root = Path(args.root).resolve()
    allowlist_path = Path(args.allowlist).resolve()
    tracked = load_allowlist(allowlist_path)
    found = scan(root)

    if args.update:
        tracked = to_tracked(found, tracked)
        write_allowlist(allowlist_path, tracked)
        print(
            "Updated platform cfg allowlist: "
            f"{sum(entry['count'] for entry in tracked.values())} lines in "
            f"{len(tracked)} files"
        )

    if args.report:
        report_path = Path(args.report).resolve()
        write_report(report_path, found, tracked)
        print(f"Wrote platform cfg report: {report_path}")

    violations = verify(tracked, found)
    if violations:
        print(
            "OS cfg budget exceeded. OS primitives belong behind "
            "horon_agent_platform; only features that exist on one OS may "
            "keep a cfg line, and each one needs a reason in "
            "scripts/ci/platform_cfg_allowlist.json:",
            file=sys.stderr,
        )
        for entry in violations:
            print(f"  - {entry}", file=sys.stderr)
        if not args.update:
            print(
                "Run scripts/ci/check_platform_cfg.py --update to lower counts, "
                "then classify every file listed above.",
                file=sys.stderr,
            )
        return 1

    print(
        "Platform cfg budget OK: "
        f"{sum(entry['count'] for entry in tracked.values())} product OS cfg "
        f"lines in {len(tracked)} files outside the platform crates"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
