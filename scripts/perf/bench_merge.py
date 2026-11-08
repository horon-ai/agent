#!/usr/bin/env python3
"""Merge the benchmark parts of one OS into the artifact consumers already read.

`benchmark.yml` no longer measures an OS in one job. It builds once, measures each
unit in its own job, and every job uploads its own `bench-part-<os>-<unit>-<run_id>`
artifact. This script turns those parts back into the single
`benchmark-<os>-<run_id>` directory that `benchmark-docs.yml` downloads and
`bench_docs.py` renders, so the consumer never learns that the run was split.

    bench_merge.py --parts DIR --out DIR --os OS --runner R --expect a,b,c

`--parts` holds one sub-directory per downloaded artifact. A part is identified by
its directory name (`bench-part-<os>-<unit>-<run_id>`) and, when that name does not
carry a unit, by the result files it holds, so a renamed or manually downloaded
part still merges. `build` is the part with the commit and the binary version.

Copying is the contract:

* every result file lands in `--out` under its own name;
* the same file name from two parts is an error, because a name collision means
  two measurements would be silently dropped;
* `platform.txt`, `runner.txt`, `commit.txt` and `horon-version.txt` are metadata
  and may repeat, but only with identical bytes;
* `status.txt` is not copied: this script writes the merged one.

`status.txt` is `success` only when the build part and every expected unit part
exist and report `success`. Anything else writes `failure` and prints one line per
missing or failed part, while still copying every file that did arrive, so a
partial run keeps the results it managed to produce. The exit status stays 0 in
both cases: the status file is the signal, not the exit code.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

BUILD = "build"
STATUS = "status.txt"
# Repeated across parts without being a collision, but the bytes must agree.
METADATA = ("platform.txt", "runner.txt", "commit.txt", "horon-version.txt")

# Result files each unit writes. Used to name a part whose directory name does
# not carry the unit, and to keep this list in one place next to the workflow.
UNIT_FILES = {
    "compare": ("compare.json", "compare.md"),
    "startup": ("startup.txt",),
    "loading-cold": ("loading-cold.json", "loading-cold.txt"),
    "loading-warm": ("loading-warm.json", "loading-warm.txt"),
    "tools": ("tools.json", "tools.md"),
    "reload": ("reload.json", "reload.md"),
    "compile": ("compile.json", "compile.md"),
}


def _read(path: Path) -> str | None:
    if not path.is_file():
        return None
    return path.read_text(encoding="utf-8", errors="replace").strip()


def _split_part_name(name: str) -> tuple[str | None, str | None]:
    """`bench-part-<os>-<unit>-<run_id>` -> (os, unit), or (None, None)."""
    prefix = "bench-part-"
    if not name.startswith(prefix):
        return None, None
    rest = name[len(prefix):]
    parts = rest.split("-")
    if len(parts) < 3 or not parts[-1].isdigit():
        return None, None
    return parts[0], "-".join(parts[1:-1])


def identify(directory: Path) -> tuple[str | None, str | None]:
    """The (os, unit) a part directory claims, by name first and contents second."""
    named_os, unit = _split_part_name(directory.name)
    if unit:
        return named_os, unit
    present = {entry.name for entry in directory.iterdir() if entry.is_file()}
    if present & {"commit.txt", "horon-version.txt"}:
        return named_os, BUILD
    for candidate, files in UNIT_FILES.items():
        if present & set(files):
            return named_os, candidate
    return named_os, None


def merge(parts: Path, out: Path, os_key: str, runner: str, expect: list[str]) -> int:
    out.mkdir(parents=True, exist_ok=True)
    reasons: list[str] = []
    seen: dict[str, str] = {}
    owners: dict[str, str] = {}
    statuses: dict[str, str | None] = {}
    directories = sorted(entry for entry in parts.iterdir() if entry.is_dir())

    for directory in directories:
        named_os, unit = identify(directory)
        if named_os is not None and named_os != os_key:
            reasons.append(f"part {directory.name} belongs to {named_os}, not {os_key}")
            continue
        if unit is None:
            reasons.append(f"part {directory.name} holds no known benchmark result")
            continue
        if unit in statuses:
            reasons.append(f"two parts claim {unit}: {owners[unit]}, {directory.name}")
            continue
        owners[unit] = directory.name
        statuses[unit] = _read(directory / STATUS)

        for source in sorted(entry for entry in directory.iterdir() if entry.is_file()):
            if source.name == STATUS:
                continue
            target = out / source.name
            if source.name in METADATA:
                if target.exists() and target.read_bytes() != source.read_bytes():
                    raise SystemExit(
                        f"metadata file {source.name} differs between parts "
                        f"({seen.get(source.name)}, {directory.name})"
                    )
            elif source.name in seen:
                raise SystemExit(
                    f"file name {source.name} appears in two parts "
                    f"({seen[source.name]}, {directory.name})"
                )
            if not target.exists():
                shutil.copyfile(source, target)
            seen.setdefault(source.name, directory.name)

    # The published artifact names its own platform and runner; a part that
    # disagrees with them means the matrix and the merge were fed different
    # values, which would make the report claim the wrong machine.
    for name, expected in (("platform.txt", os_key), ("runner.txt", runner)):
        recorded = _read(out / name)
        if recorded is not None and recorded != expected:
            raise SystemExit(f"{name} says {recorded!r}, expected {expected!r}")
        (out / name).write_text(expected + "\n", encoding="utf-8")

    for unit in [BUILD, *expect]:
        if unit not in statuses:
            reasons.append(f"missing part: {unit}")
            continue
        status = statuses[unit]
        if status != "success":
            reasons.append(f"failed part: {unit} ({status or 'no status.txt'})")

    (out / STATUS).write_text(("failure" if reasons else "success") + "\n", encoding="utf-8")
    for reason in reasons:
        print(reason)
    print(f"merged {len(seen)} files from {len(directories)} parts: {len(reasons)} problem(s)")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Merge the benchmark parts of one OS")
    parser.add_argument("--parts", required=True, help="directory holding one sub-directory per part")
    parser.add_argument("--out", required=True, help="directory to write the merged artifact into")
    parser.add_argument("--os", required=True, help="OS key the merged artifact is published as")
    parser.add_argument("--runner", required=True, help="runner label that measured the parts")
    parser.add_argument("--expect", required=True, help="comma-separated unit parts that must have succeeded")
    args = parser.parse_args(argv[1:])
    expect = [unit.strip() for unit in args.expect.split(",") if unit.strip()]
    return merge(Path(args.parts), Path(args.out), args.os, args.runner, expect)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
