#!/usr/bin/env python3

"""Run a compiled libtest executable without asking cargo to rebuild it.

Running a per-crate filtered test through cargo reinstalls the whole `-p`
selection set each time, so every cohort step recompiled the workspace crates
(#19). The CI workflow now compiles all cohort packages in one cargo invocation
with `--message-format=json`, and this script runs the reported executable
directly.

cargo runs lib tests with the working directory at the package root; several
tests read `std::env::current_dir()` and treat it as the repository root (for
example `crates/app-core/src/tool/communicate_tests/assignment.rs`). This script
sets the same working directory and the `CARGO_*` variables cargo would set.

Exit status is non-zero when the package or its executable cannot be resolved,
when it is ambiguous, when a test fails, when the timeout expires, or when
fewer than `--min-passed` tests pass.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import select
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Iterable, Sequence

RESULT_RE = re.compile(r"test result: ok\. (\d+) passed")


def _usage_error(message: str) -> int:
    print(f"run_lib_tests: {message}", file=sys.stderr)
    return 2


def package_name_from_id(package_id: str) -> str:
    """Extract the package name from either cargo `package_id` spelling.

    Cargo writes `path+file:///repo/crates/base#horon-agent-base@0.98.1` for a
    workspace member and `horon-agent-base 0.98.1 (path+file:///...)` for
    packages from an older cargo. Both appear in `compiler-artifact` messages.
    """
    if "#" in package_id:
        fragment = package_id.rsplit("#", 1)[1]
        return fragment.split("@", 1)[0]
    head = package_id.split(" ", 1)[0]
    return head


def _load_artifacts(path: Path) -> Iterable[dict]:
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("reason") == "compiler-artifact":
                yield message


def select_executable(
    artifacts: Iterable[dict], package: str, kind: str, target_name: str | None
) -> Path:
    matches: list[Path] = []
    for message in artifacts:
        if package_name_from_id(str(message.get("package_id", ""))) != package:
            continue
        target = message.get("target") or {}
        target_kinds = target.get("kind") or []
        if kind not in target_kinds:
            continue
        if target_name is not None and target.get("name") != target_name:
            continue
        if not (message.get("profile") or {}).get("test"):
            continue
        executable = message.get("executable")
        if not executable:
            continue
        matches.append(Path(executable))

    if not matches:
        raise LookupError(
            f"no {kind} test executable for package {package!r}"
        )
    unique = sorted({str(path): path for path in matches}.values())
    if len(unique) > 1:
        listed = ", ".join(str(path) for path in unique)
        raise LookupError(
            f"ambiguous {kind} test executable for package {package!r}: {listed}"
        )
    return unique[0]


def _package_dir(artifacts: Iterable[dict], package: str) -> Path:
    for message in artifacts:
        if package_name_from_id(str(message.get("package_id", ""))) != package:
            continue
        manifest = message.get("manifest_path")
        if manifest:
            return Path(manifest).parent
    raise LookupError(f"no manifest_path for package {package!r}")


def _cargo_command() -> str:
    configured = os.environ.get("CARGO")
    if configured:
        return configured
    located = shutil.which("cargo")
    if located:
        return located
    return "cargo"


def _package_version(artifacts: Iterable[dict], package: str) -> str:
    for message in artifacts:
        if package_name_from_id(str(message.get("package_id", ""))) != package:
            continue
        fragments = str(message.get("package_id", "")).rsplit("#", 1)
        if len(fragments) == 2 and "@" in fragments[1]:
            return fragments[1].split("@", 1)[1]
        fields = str(message.get("package_id", "")).split(" ")
        if len(fields) >= 2:
            return fields[1]
    return "0.0.0"


def _kill_process_group(proc: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return


def _force_kill_process_group(proc: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


class Reporter:
    """Write each output line to stdout and, when asked, to a log file."""

    def __init__(self, log: Path | None) -> None:
        self._stream = None
        self._lines: list[str] = []
        if log is not None:
            log.parent.mkdir(parents=True, exist_ok=True)
            self._stream = log.open("w", encoding="utf-8")

    def emit(self, text: str) -> None:
        self._lines.append(text)
        sys.stdout.write(text)
        sys.stdout.flush()
        if self._stream is not None:
            self._stream.write(text)
            self._stream.flush()

    def close(self) -> None:
        if self._stream is not None:
            self._stream.close()

    @property
    def text(self) -> str:
        return "".join(self._lines)


def _stream_process(
    proc: subprocess.Popen[bytes], timeout: int, reporter: Reporter
) -> bool:
    """Pump the process output. Returns True when it finished before timeout."""
    assert proc.stdout is not None
    descriptor = proc.stdout.fileno()
    deadline = time.monotonic() + timeout
    buffer = b""
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        ready, _, _ = select.select([descriptor], [], [], remaining)
        if not ready:
            continue
        chunk = os.read(descriptor, 65536)
        if not chunk:
            break
        buffer += chunk
        while b"\n" in buffer:
            line, buffer = buffer.split(b"\n", 1)
            reporter.emit(line.decode("utf-8", errors="replace") + "\n")
    if buffer:
        reporter.emit(buffer.decode("utf-8", errors="replace"))
    return True


def run_executable(
    executable: Path,
    package_dir: Path,
    package: str,
    version: str,
    timeout: int,
    reporter: Reporter,
    test_args: Sequence[str],
) -> int:
    command = [str(executable), *test_args]
    env = dict(os.environ)
    env.setdefault("CARGO_MANIFEST_DIR", str(package_dir))
    env.setdefault("CARGO_PKG_NAME", package)
    env.setdefault("CARGO_PKG_VERSION", version)
    env.setdefault("CARGO", _cargo_command())
    print(f"==> Running {command[0]} in {package_dir} (timeout {timeout}s)")
    sys.stdout.flush()

    proc = subprocess.Popen(
        command,
        cwd=str(package_dir),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    finished = _stream_process(proc, timeout, reporter)
    if not finished:
        print(
            f"::error::Test executable exceeded timeout after {timeout}s: {executable}",
            file=sys.stderr,
        )
        _kill_process_group(proc)
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            _force_kill_process_group(proc)
            proc.wait()
        return 124

    proc.wait()
    if proc.returncode != 0:
        print(
            f"::error::Test executable failed with status {proc.returncode}: {executable}",
            file=sys.stderr,
        )
        return proc.returncode or 1
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Run a compiled libtest executable without recompiling."
    )
    parser.add_argument("--artifacts", required=True, type=Path,
                        help="JSON lines written by cargo with --message-format=json")
    parser.add_argument("--package", required=True, help="package name to run")
    parser.add_argument("--kind", choices=("lib", "test"), default="lib",
                        help="cargo target kind of the executable")
    parser.add_argument("--target-name", default=None,
                        help="cargo target name (required to disambiguate several tests)")
    parser.add_argument("--timeout", required=True, type=int,
                        help="hard wall-clock timeout in seconds")
    parser.add_argument("--min-passed", type=int, default=None,
                        help="fail when fewer tests than this pass")
    parser.add_argument("--log", type=Path, default=None,
                        help="also write the test output to this file")
    parser.add_argument("test_args", nargs=argparse.REMAINDER,
                        help="arguments for the test binary; prefix with --")
    args = parser.parse_args(argv)

    test_args = list(args.test_args)
    if test_args and test_args[0] == "--":
        test_args = test_args[1:]

    if not args.artifacts.is_file():
        return _usage_error(f"artifacts file not found: {args.artifacts}")

    artifacts = list(_load_artifacts(args.artifacts))
    try:
        executable = select_executable(
            artifacts, args.package, args.kind, args.target_name
        )
        package_dir = _package_dir(artifacts, args.package)
    except LookupError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    version = _package_version(artifacts, args.package)

    reporter = Reporter(args.log)
    try:
        status = run_executable(
            executable, package_dir, args.package, version,
            args.timeout, reporter, test_args,
        )
    finally:
        reporter.close()

    if status != 0:
        return status

    passed = sum(int(match.group(1)) for match in RESULT_RE.finditer(reporter.text))
    if args.min_passed is not None and passed < args.min_passed:
        print(
            f"::error::{args.package} ran {passed} passing tests, "
            f"expected at least {args.min_passed}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
