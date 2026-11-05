#!/usr/bin/env python3
"""Compile-loop benchmark: how long an edit-loop rebuild takes.

Port of jcode's `bench_compile.sh` plus the checkpoints of
`bench_selfdev_checkpoints.sh` / `bench_selfdev_build.sh`, mapped onto the parts
of the build that horon-agent actually has:

  warm_noop        rebuild with nothing changed (the cost of asking cargo)
  leaf_touch       one touched file in the root bin crate (`src/main.rs`)
  core_touch       one touched file in the mid-stack crate (`crates/app-core`)
  build_meta_touch one touched file in `crates/build-meta`, which other crates
                   depend on, so it recompiles the stack above it

jcode's `tui_touch` and `commit blast` checkpoints have no horon equivalent:
horon-agent is headless (no TUI crate), and its git hash is baked into build-meta
exactly like the build_meta_touch checkpoint exercises.

Touch method
------------
`--touch` in jcode appended a comment and restored it. Here the file's mtime is
bumped past the newest artifact and then restored, so the source bytes never
change: a benchmark must not leave a checkout that differs from `git status`.
Cargo's fingerprint compares mtimes, so a bump is enough to mark the unit dirty.
`--dry-run` performs the touch/restore cycle without invoking cargo, which is how
the release binary smoke-verifies this script without a 10-minute rebuild.

Nothing here is a gate. A slow rebuild is a measurement, not a failure.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_common as common  # noqa: E402

SUITE = "compile"

# (mode, description, freshly-touched path relative to the repo root)
MODES: list[tuple[str, str, str | None]] = [
    ("warm_noop", "warm rebuild, nothing changed", None),
    ("leaf_touch", "touch root bin crate (src/main.rs)", "src/main.rs"),
    ("core_touch", "touch mid-stack crate (crates/app-core/src/lib.rs)", "crates/app-core/src/lib.rs"),
    ("build_meta_touch", "touch build metadata crate (crates/build-meta/src/lib.rs)", "crates/build-meta/src/lib.rs"),
]

DEFAULT_COMMAND = ["cargo", "check", "--quiet"]
BUMP_SECONDS = 5.0


class TouchRestorer:
    """Bump a file's mtime, then put the original timestamp back."""

    def __init__(self, path: Path):
        self.path = path
        self.original_ns: int | None = None

    def bump(self) -> None:
        stat = self.path.stat()
        self.original_ns = stat.st_mtime_ns
        target = time.time() + BUMP_SECONDS
        os.utime(self.path, (target, target))

    def restore(self) -> None:
        if self.original_ns is None:
            return
        os.utime(self.path, ns=(self.original_ns, self.original_ns))
        self.original_ns = None


def run_command(command: list[str], repo: Path, timeout: float, env: dict[str, str]) -> tuple[float | None, str | None]:
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            command,
            cwd=str(repo),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return None, "timed out after %.0fs" % timeout
    except OSError as error:
        return None, "%s: %s" % (type(error).__name__, error)
    elapsed = time.perf_counter() - started
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip().splitlines()
        tail = detail[-1] if detail else "no stderr"
        return None, "exit %d: %s" % (completed.returncode, tail[:300])
    return elapsed, None


def measure_mode(
    mode: str,
    path: Path | None,
    command: list[str],
    repo: Path,
    runs: int,
    timeout: float,
    env: dict[str, str],
    dry_run: bool,
) -> dict:
    restorer = TouchRestorer(path) if path else None
    durations: list[float] = []
    errors: list[str] = []
    try:
        for index in range(runs):
            if restorer is not None:
                restorer.bump()
            if dry_run:
                # Prove the touch cycle without paying for a rebuild.
                durations.append(0.0)
                common.eprint("bench_compile: %s run %d/%d dry-run touch=%s" % (mode, index + 1, runs, path))
                continue
            elapsed, error = run_command(command, repo, timeout, env)
            if error is not None:
                errors.append(error)
                common.eprint("bench_compile: %s run %d/%d FAILED %s" % (mode, index + 1, runs, error))
                continue
            durations.append(elapsed)
            common.eprint("bench_compile: %s run %d/%d %.3fs" % (mode, index + 1, runs, elapsed))
    finally:
        if restorer is not None:
            restorer.restore()

    stats = None if dry_run else common.summarize(durations)
    return {
        "name": mode,
        "touch": str(path.relative_to(repo)) if path else None,
        "command": " ".join(command),
        "seconds": stats,
        "dry_run": dry_run,
        "errors": errors,
        "error": None if (stats is not None or dry_run) else (errors[0] if errors else "no successful run"),
    }


def warmup(command: list[str], repo: Path, timeout: float, env: dict[str, str]) -> str | None:
    elapsed, error = run_command(command, repo, timeout, env)
    if error is None:
        common.eprint("bench_compile: warmup %.3fs" % elapsed)
    return error


def markdown(results: list[dict], skipped: list[dict]) -> str:
    lines = ["### Compile loop", ""]
    if skipped:
        lines.append("Skipped: " + "; ".join("%s (%s)" % (s["name"], s["reason"]) for s in skipped))
        lines.append("")
    rows = []
    for row in results:
        stats = row["seconds"]
        rows.append(
            [
                row["name"],
                row["touch"] or "—",
                common.fmt_ms(stats["median"] * 1000) if stats else "—",
                common.fmt_ms(stats["min"] * 1000) if stats else "—",
                common.fmt_ms(stats["max"] * 1000) if stats else "—",
                str(stats["samples"]) if stats else "0",
            ]
        )
    lines.append(common.markdown_table(["Checkpoint", "Touched", "median", "min", "max", "samples"], rows))
    if any(row["dry_run"] for row in results):
        lines.append("")
        lines.append("Dry run: touches were exercised, cargo was not invoked.")
    failed = [row["name"] for row in results if row["error"]]
    if failed:
        lines.append("")
        lines.append("Failed checkpoints: " + ", ".join(failed))
    return "\n".join(lines)


def finish(args: argparse.Namespace, results: list[dict], skipped: list[dict], started_at: str) -> int:
    report = common.build_report(SUITE, results, skipped, started_at)
    if args.json_out:
        common.write_json(args.json_out, report)
    if args.markdown_out:
        common.write_markdown(args.markdown_out, markdown(results, skipped))
    print(json.dumps({k: report[k] for k in ("suite", "schema", "host", "started_at")}, indent=2))
    if skipped and not results:
        common.eprint("bench_compile: skipped — %s" % skipped[0]["reason"])
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", default=None, help="checkout to measure (default: this repository)")
    parser.add_argument("--runs", type=int, default=1, help="timed runs per checkpoint")
    parser.add_argument("--timeout", type=float, default=1800.0, help="per-run cargo deadline in seconds")
    parser.add_argument(
        "--modes",
        default=None,
        help="comma-separated subset of %s (default: all)" % ",".join(mode for mode, _, _ in MODES),
    )
    parser.add_argument("--command", default=None, help="cargo command prefix (default: cargo check --quiet)")
    parser.add_argument("--quick", action="store_true", help="warm no-op only, one run")
    parser.add_argument("--dry-run", action="store_true", help="exercise the touch cycle without cargo")
    parser.add_argument("--no-warmup", action="store_true", help="skip the untimed warmup run")
    parser.add_argument("--json-out", default=None)
    parser.add_argument("--markdown-out", default=None)
    args = parser.parse_args(argv)
    if args.runs < 1:
        parser.error("--runs must be positive")
    return args


def selected_modes(raw: str | None, quick: bool) -> list[tuple[str, str, str | None]]:
    if quick:
        return [MODES[0]]
    if raw is None:
        return list(MODES)
    wanted = [part.strip() for part in raw.split(",") if part.strip()]
    known = {mode for mode, _, _ in MODES}
    unknown = [name for name in wanted if name not in known]
    if unknown:
        raise SystemExit("unknown mode(s): %s; known: %s" % (", ".join(unknown), ", ".join(sorted(known))))
    return [entry for entry in MODES if entry[0] in wanted]


def run(args: argparse.Namespace) -> int:
    started_at = common.utc_now_iso()
    results: list[dict] = []
    skipped: list[dict] = []

    repo = Path(args.repo).resolve() if args.repo else Path(__file__).resolve().parents[2]
    if not (repo / "Cargo.toml").is_file():
        skipped.append({"name": SUITE, "reason": "no Cargo.toml at %s" % repo})
        return finish(args, results, skipped, started_at)

    command = (args.command.split() if args.command else list(DEFAULT_COMMAND))
    env = dict(os.environ)
    env.setdefault("CARGO_TERM_COLOR", "never")

    if not args.dry_run and shutil.which(command[0], path=env.get("PATH")) is None:
        skipped.append({"name": SUITE, "reason": "%s not found on PATH" % command[0]})
        return finish(args, results, skipped, started_at)

    modes = []
    for mode, description, relative in selected_modes(args.modes, args.quick):
        if relative is not None and not (repo / relative).is_file():
            skipped.append({"name": mode, "reason": "touch path is missing: %s" % relative})
            continue
        modes.append((mode, description, relative))

    if not args.dry_run and not args.no_warmup and modes:
        error = warmup(command, repo, args.timeout, env)
        if error is not None:
            skipped.append({"name": SUITE, "reason": "warmup build failed: %s" % error})
            return finish(args, results, skipped, started_at)

    for mode, _description, relative in modes:
        path = repo / relative if relative else None
        results.append(
            measure_mode(mode, path, command, repo, args.runs, args.timeout, env, args.dry_run)
        )

    return finish(args, results, skipped, started_at)


def main(argv: list[str]) -> int:
    return run(parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
