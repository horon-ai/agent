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

Supports `--jobs N` to execute stateless targets concurrently with per-worker
isolated `0700` `HORON_HOME` and `HORON_AGENT_RUNTIME_DIR` directories while
keeping shared-state and timing-sensitive targets strictly serial.

Usage:
  python3 scripts/ci/run_integration_targets.py --artifacts "$TEST_ARTIFACTS"
  python3 scripts/ci/run_integration_targets.py --artifacts "$TEST_ARTIFACTS" --jobs 4
  python3 scripts/ci/run_integration_targets.py --artifacts f.jsonl --only sdk
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "scripts/ci/integration_targets.json"
DEFAULT_RUNNER = ROOT / "scripts/ci/run_lib_tests.py"

# Targets that own exclusive runner resources (loopback btrfs mounts, heavy soak
# loops, spend-cap/perf bounds, live daemon signal/socket timing) run strictly
# sequentially after the parallel pool completes.
SERIAL_TARGET_SUBSTRINGS: tuple[str, ...] = (
    "spend_cap",
    "tool_dispatch_perf",
    "loading_probe_contract",
    "scratch_live_working_dir",
    "e2e",
    "acp_infra",
    "acp_conformance",
    "acp_lifecycle",
    "server_signal_shutdown",
    "monitor_restart_qa",
    "mcp_stdio_collision_integration",
    "on_demand_server_spawn",
)


def is_serial_target(target: dict) -> bool:
    name = str(target.get("target", ""))
    return bool(target.get("serial")) or any(sub in name for sub in SERIAL_TARGET_SUBSTRINGS)


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


def run_one_target(
    target: dict,
    *,
    artifacts: Path,
    runner: Path,
    timeout: int,
    capture_output: bool,
) -> tuple[str, int, str]:
    label = f"{target['package']}::{target['target']}"
    worker_root = Path(tempfile.mkdtemp(prefix="horon-it-"))
    os.chmod(worker_root, 0o700)
    worker_home = worker_root / "home"
    worker_runtime = worker_root / "runtime"
    worker_home.mkdir(mode=0o700)
    worker_runtime.mkdir(mode=0o700)
    os.chmod(worker_home, 0o700)
    os.chmod(worker_runtime, 0o700)

    env = os.environ.copy()
    env["HORON_HOME"] = str(worker_home)
    env["HORON_AGENT_RUNTIME_DIR"] = str(worker_runtime)

    command = [
        sys.executable,
        str(runner),
        "--artifacts",
        str(artifacts),
        "--kind",
        "test",
        "--target-name",
        target["target"],
        "--package",
        target["package"],
        "--min-passed",
        str(target["min_passed"]),
        "--timeout",
        str(timeout),
        "--",
        *skips(target),
    ]
    try:
        if capture_output:
            completed = subprocess.run(
                command,
                cwd=ROOT,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
            return label, completed.returncode, completed.stdout or ""
        completed = subprocess.run(command, cwd=ROOT, env=env)
        return label, completed.returncode, ""
    finally:
        shutil.rmtree(worker_root, ignore_errors=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--artifacts", required=True, type=Path)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--runner", type=Path, default=DEFAULT_RUNNER)
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument(
        "--jobs",
        type=int,
        default=1,
        help="number of parallel workers for non-serial targets (default: 1)",
    )
    parser.add_argument(
        "--only",
        action="append",
        default=[],
        help="restrict to a package name; repeatable",
    )
    args = parser.parse_args(argv)

    if args.jobs < 1:
        print("::error::--jobs must be >= 1", file=sys.stderr)
        return 2

    if not args.artifacts.is_file():
        print(f"::error::artifacts file not found: {args.artifacts}", file=sys.stderr)
        return 2

    targets = load_targets(args.manifest)
    if args.only:
        targets = [target for target in targets if target["package"] in args.only]

    scoped_raw = os.environ.get("HORON_CI_SCOPED_PACKAGES", "").strip()
    if scoped_raw:
        scoped_set = {item.strip() for item in scoped_raw.split(",") if item.strip()}
        targets = [target for target in targets if target["package"] in scoped_set]
        if not targets:
            print("Integration contracts skipped (0 targets in HORON_CI_SCOPED_PACKAGES)")
            return 0

    if not targets:
        print("::error::no integration targets selected", file=sys.stderr)
        return 1

    parallel_targets = [t for t in targets if args.jobs > 1 and not is_serial_target(t)]
    serial_targets = [t for t in targets if args.jobs == 1 or is_serial_target(t)]

    failures: list[str] = []
    output_lock = threading.Lock()

    if parallel_targets:
        with ThreadPoolExecutor(max_workers=min(args.jobs, len(parallel_targets))) as pool:
            futures = {
                pool.submit(
                    run_one_target,
                    target,
                    artifacts=args.artifacts,
                    runner=args.runner,
                    timeout=args.timeout,
                    capture_output=True,
                ): target
                for target in parallel_targets
            }
            for future in as_completed(futures):
                label, returncode, output = future.result()
                with output_lock:
                    print(f"::group::integration {label}", flush=True)
                    if output:
                        print(output, end="" if output.endswith("\n") else "\n", flush=True)
                    print("::endgroup::", flush=True)
                if returncode != 0:
                    failures.append(label)

    for target in serial_targets:
        label = f"{target['package']}::{target['target']}"
        print(f"::group::integration {label}", flush=True)
        _, returncode, _ = run_one_target(
            target,
            artifacts=args.artifacts,
            runner=args.runner,
            timeout=args.timeout,
            capture_output=False,
        )
        print("::endgroup::", flush=True)
        if returncode != 0:
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
