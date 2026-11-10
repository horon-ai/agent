#!/usr/bin/env python3
"""Enforce the growth ratchets: every ceiling in `scripts/ci/*.json` holds.

A growth ratchet is a number that may fall but not rise. Nothing in this
repository stopped `Cargo.lock` from growing or internal `legacy` leftovers
from multiplying, and a cleanup that is not finished cannot be prevented from
coming back one commit at a time. This gate records each count on the current
tree and fails on any increase.

Naming rule: **a growth ratchet is any `scripts/ci/*.json` file that either
ends in `.ratchet.json` or has a top-level object declaring `counter` or
`ceiling`.** The gate discovers them itself, so a later ratchet (the
`Result<_, String>` count of #156, model-name branches, a future count) adds
one JSON file and no workflow or `check_all.sh` edit. Both spellings work: the
`.ratchet.json` suffix is the recommended name, and a sibling-shaped file such
as `result_string_budget.json` is recognised by its keys. The per-gate budgets
that predate this gate (`panic_budget.json`, `code_size_budget.json`, ...) use
neither spelling and stay owned by their own scripts. Every discovered file is
validated and evaluated; a malformed one is a failure, never a skip - including
a `.ratchet.json` that lost its `counter`, which would otherwise read as "no
ratchet declared".

File format:

    {
      "version": 1,
      "name": "internal-legacy-markers",
      "description": "Why this count is ratcheted and what it measures.",
      "counter": { ...see below... },
      "ceiling": 998
    }

`counter.kind` selects the measurement. Two kinds exist, and between them they
cover the ratchets planned for this tree:

- `cargo_lock_packages` - `[[package]]` entries in `Cargo.lock`. Each entry is
  one resolved dependency (the same crate at two versions counts twice), so the
  number is the install footprint the lockfile pins.
- `regex_lines` - Rust source lines matching a regular expression, summed over
  the given roots. Parameters: `pattern` (required, POSIX-ERE-style regex),
  `case_insensitive` (default true), `roots` (default `["src", "crates"]`),
  `extensions` (default `[".rs"]`), and `exclude` (default `[]`).

An `exclude` entry is `{"path": "<prefix relative to the repository root>",
"reason": "<why this subtree is external contract>"}`. Excluded subtrees are
not counted, so the ratchet freezes only the internal ones. The reason is
required and printed by `--list`; an exclude whose prefix matches no scanned
file fails, so a stale entry cannot silently widen the hole.

Counting rules that are deliberate, not incidental:

- `regex_lines` counts every matching line, including test modules and
  comments, because a marker in either still names a legacy path and still
  multiplies. This matches the `git grep -c` evidence an issue quotes.
- Growth fails; a drop does not. The ceiling is an upper bound that only an
  explicit `--update` lowers, which is the same rule the four budgets beside
  this one follow. A cleanup in an unrelated change must not have to edit this
  file, and the gate is still unable to loosen itself: a lower count with the
  old ceiling still fails the moment anything grows back.
- `Cargo.lock` is read as text, never with cargo, so the gate costs
  milliseconds and cannot refresh the lockfile as a side effect. Adding a
  dependency legitimately raises the count: bump the ceiling in the same
  commit, with the review that accompanies a new dependency.

`--update` re-pins every ceiling to the current count. It is the only way to
raise one, and it is meant to be a visible commit of its own (a new dependency,
or a bound raised after a delivered cleanup), never a way to hide a regression.
Dropping a ceiling is optional: the gate reports the drop and still passes.

Usage:
  python3 scripts/ci/check_growth_ratchets.py
  python3 scripts/ci/check_growth_ratchets.py --update
  python3 scripts/ci/check_growth_ratchets.py --list
  python3 scripts/ci/check_growth_ratchets.py --root /tmp/synthetic-repo
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

RATCHET_DIR = Path("scripts") / "ci"
RATCHET_SUFFIX = ".ratchet.json"
RATCHET_KEYS = ("counter", "ceiling")
LOCKFILE_COUNTER = "cargo_lock_packages"
REGEX_COUNTER = "regex_lines"
COUNTER_KINDS = (LOCKFILE_COUNTER, REGEX_COUNTER)
DEFAULT_ROOTS = ("src", "crates")
DEFAULT_EXTENSIONS = (".rs",)


class RatchetError(Exception):
    """A ratchet file the gate cannot evaluate, reported as a failure."""


def declares_ratchet(path: Path, data: Any) -> bool:
    """Whether this file is a growth ratchet.

    Two spellings are accepted, so a later ratchet is discovered without an
    edit here: the `.ratchet.json` suffix, or a top-level object declaring
    `counter` or `ceiling` (which is how `result_string_budget.json` and
    friends are recognised). A `.ratchet.json` that lost its keys is still
    discovered, and then fails validation, because silently skipping it would
    turn a typo into "no ratchet declared".
    """
    if path.name.endswith(RATCHET_SUFFIX):
        return True
    return isinstance(data, dict) and any(key in data for key in RATCHET_KEYS)


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RatchetError(f"{path.as_posix()}: cannot read JSON: {error}") from error


def ratchet_files(root: Path) -> list[Path]:
    """Every growth ratchet file under `scripts/ci`, in stable order."""
    directory = root / RATCHET_DIR
    if not directory.is_dir():
        raise RatchetError(f"ratchet directory missing: {directory}")
    found: list[Path] = []
    for path in sorted(directory.glob("*.json")):
        data = read_json(path)
        if declares_ratchet(path, data):
            found.append(path)
    return found


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RatchetError(message)


def parse_exclude(raw: Any, where: str) -> dict[str, str]:
    require(isinstance(raw, dict), f"{where}: each exclude entry must be an object")
    path = raw.get("path")
    reason = raw.get("reason")
    require(isinstance(path, str) and path, f"{where}: exclude entry needs a non-empty path")
    require(isinstance(reason, str) and reason.strip(), f"{where}: exclude {path!r} needs a reason")
    require(".." not in Path(path).parts, f"{where}: exclude path must stay inside the root: {path}")
    return {"path": path, "reason": reason}


def parse_counter(raw: Any, where: str) -> dict[str, Any]:
    require(isinstance(raw, dict), f"{where}: counter must be an object")
    kind = raw.get("kind")
    require(
        kind in COUNTER_KINDS,
        f"{where}: unknown counter kind {kind!r} (known: {', '.join(COUNTER_KINDS)})",
    )
    if kind == LOCKFILE_COUNTER:
        unexpected = set(raw) - {"kind"}
        require(not unexpected, f"{where}: {LOCKFILE_COUNTER} takes no parameters, got {sorted(unexpected)}")
        return {"kind": kind}
    pattern = raw.get("pattern")
    require(isinstance(pattern, str) and pattern, f"{where}: {REGEX_COUNTER} needs a pattern")
    try:
        compiled = re.compile(pattern, re.IGNORECASE if raw.get("case_insensitive", True) else 0)
    except re.error as error:
        raise RatchetError(f"{where}: pattern does not compile: {error}") from error
    roots = raw.get("roots", list(DEFAULT_ROOTS))
    require(
        isinstance(roots, list) and roots and all(isinstance(item, str) and item for item in roots),
        f"{where}: roots must be a non-empty list of paths",
    )
    for item in roots:
        require(".." not in Path(item).parts, f"{where}: root must stay inside the repository: {item}")
    extensions = raw.get("extensions", list(DEFAULT_EXTENSIONS))
    require(
        isinstance(extensions, list) and extensions
        and all(isinstance(item, str) and item.startswith(".") for item in extensions),
        f"{where}: extensions must be a non-empty list such as ['.rs']",
    )
    excludes = raw.get("exclude", [])
    require(isinstance(excludes, list), f"{where}: exclude must be a list")
    parsed = [parse_exclude(entry, where) for entry in excludes]
    paths = [entry["path"] for entry in parsed]
    require(len(paths) == len(set(paths)), f"{where}: duplicate exclude path")
    return {
        "kind": kind,
        "pattern": pattern,
        "regex": compiled,
        "case_insensitive": raw.get("case_insensitive", True),
        "roots": roots,
        "extensions": extensions,
        "exclude": parsed,
    }


def load_ratchet(path: Path) -> dict[str, Any]:
    where = path.as_posix()
    data = read_json(path)
    require(isinstance(data, dict), f"{where}: ratchet file must be a JSON object")
    require(data.get("version") == 1, f"{where}: unsupported version {data.get('version')!r}")
    name = data.get("name")
    require(isinstance(name, str) and name.strip(), f"{where}: name must be a non-empty string")
    description = data.get("description")
    require(
        isinstance(description, str) and description.strip(),
        f"{where}: description must say what this ratchet protects",
    )
    ceiling = data.get("ceiling")
    require(
        isinstance(ceiling, int) and not isinstance(ceiling, bool) and ceiling >= 0,
        f"{where}: ceiling must be a non-negative integer",
    )
    return {
        "path": path,
        "name": name,
        "description": description,
        "ceiling": ceiling,
        "counter": parse_counter(data.get("counter"), where),
        "raw": data,
    }


def load_ratchets(root: Path) -> list[dict[str, Any]]:
    files = ratchet_files(root)
    require(
        bool(files),
        f"no growth ratchet file under {root / RATCHET_DIR} "
        f"(a *{RATCHET_SUFFIX} file, or an object declaring {' or '.join(RATCHET_KEYS)})",
    )
    ratchets = [load_ratchet(path) for path in files]
    names = [ratchet["name"] for ratchet in ratchets]
    duplicates = sorted({name for name in names if names.count(name) > 1})
    require(not duplicates, f"duplicate ratchet name(s): {', '.join(duplicates)}")
    return ratchets


def excluded(relative: str, excludes: list[dict[str, str]]) -> bool:
    return any(relative == entry["path"] or relative.startswith(entry["path"].rstrip("/") + "/")
               for entry in excludes)


def count_lockfile_packages(root: Path) -> tuple[int, dict[str, int]]:
    lockfile = root / "Cargo.lock"
    if not lockfile.is_file():
        raise RatchetError(f"Cargo.lock missing: {lockfile}")
    count = sum(1 for line in lockfile.read_text(encoding="utf-8").splitlines()
                if line.strip() == "[[package]]")
    return count, {"Cargo.lock": count}


def matching_files(root: Path, roots: list[str], extensions: list[str],
                   excludes: list[dict[str, str]]) -> list[tuple[Path, str]]:
    files: list[tuple[Path, str]] = []
    for relative_root in roots:
        directory = root / relative_root
        if not directory.is_dir():
            raise RatchetError(f"scan root missing: {directory}")
        for path in sorted(directory.rglob("*")):
            if path.suffix not in extensions or not path.is_file():
                continue
            relative = path.relative_to(root).as_posix()
            if excluded(relative, excludes):
                continue
            files.append((path, relative))
    return files


def count_regex_lines(root: Path, counter: dict[str, Any]) -> tuple[int, dict[str, int]]:
    stale = [entry["path"] for entry in counter["exclude"] if not (root / entry["path"]).exists()]
    if stale:
        raise RatchetError(
            "exclude path does not exist, so it cannot describe external contract: "
            + ", ".join(stale)
        )
    files = matching_files(root, counter["roots"], counter["extensions"], counter["exclude"])
    counts: dict[str, int] = {}
    for path, relative in files:
        matched = sum(
            1
            for line in path.read_text(encoding="utf-8", errors="ignore").splitlines()
            if counter["regex"].search(line)
        )
        if matched:
            counts[relative] = matched
    return sum(counts.values()), counts


def measure(root: Path, counter: dict[str, Any]) -> tuple[int, dict[str, int]]:
    if counter["kind"] == LOCKFILE_COUNTER:
        return count_lockfile_packages(root)
    if counter["kind"] == REGEX_COUNTER:
        return count_regex_lines(root, counter)
    raise RatchetError(f"unhandled counter kind: {counter['kind']}")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2],
                        help="repository root (default: this checkout)")
    parser.add_argument("--update", action="store_true", help="re-pin every ceiling to the current count")
    parser.add_argument("--list", action="store_true", help="print the discovered ratchets and exit")
    return parser.parse_args(argv)


def write_ceiling(ratchet: dict[str, Any], ceiling: int) -> None:
    raw = dict(ratchet["raw"])
    raw["ceiling"] = ceiling
    ratchet["path"].write_text(json.dumps(raw, indent=2) + "\n", encoding="utf-8")


def report_top(counts: dict[str, int], limit: int = 10) -> None:
    ranked = sorted(counts.items(), key=lambda item: (-item[1], item[0]))[:limit]
    if not ranked:
        return
    print("    largest contributing files:", file=sys.stderr)
    for relative, count in ranked:
        print(f"      {count:>5}  {relative}", file=sys.stderr)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    root = args.root.resolve()
    try:
        ratchets = load_ratchets(root)
    except RatchetError as error:
        print(f"Growth ratchet error: {error}", file=sys.stderr)
        return 1

    if args.list:
        for ratchet in ratchets:
            print(f"{ratchet['name']}  ceiling={ratchet['ceiling']}  {ratchet['path'].name}")
            print(f"  {ratchet['description']}")
            for entry in ratchet["counter"].get("exclude", []):
                print(f"  exclude {entry['path']}: {entry['reason']}")
        return 0

    failures: list[str] = []
    improvements: list[str] = []
    for ratchet in ratchets:
        try:
            current, counts = measure(root, ratchet["counter"])
        except (RatchetError, OSError) as error:
            print(f"Growth ratchet error in {ratchet['path'].name}: {error}", file=sys.stderr)
            return 1

        if args.update:
            if current != ratchet["ceiling"]:
                write_ceiling(ratchet, current)
                print(f"Updated {ratchet['name']}: ceiling {ratchet['ceiling']} -> {current}")
            else:
                print(f"Unchanged {ratchet['name']}: ceiling {current}")
            continue

        ceiling = ratchet["ceiling"]
        if current > ceiling:
            failures.append(
                f"{ratchet['name']}: {current} > ceiling {ceiling} (+{current - ceiling})"
            )
            report_top(counts)
        elif current < ceiling:
            improvements.append(f"{ratchet['name']}: {current} < ceiling {ceiling}")
        else:
            print(f"{ratchet['name']} OK: {current}/{ceiling}")

    if args.update:
        return 0

    if failures:
        print("Growth ratchet exceeded:", file=sys.stderr)
        for entry in failures:
            print(f"  - {entry}", file=sys.stderr)
        print(
            "A new dependency re-pins its ceiling in the reviewed commit that adds it; "
            "an internal marker is removed, not re-pinned.",
            file=sys.stderr,
        )
        return 1

    if improvements:
        print("Growth ratchet improved:")
        for entry in improvements:
            print(f"  - {entry}")
        print("Consider running: scripts/ci/check_growth_ratchets.py --update")
        return 0

    print(f"All {len(ratchets)} growth ratchets hold.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
