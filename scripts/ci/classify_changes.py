#!/usr/bin/env python3
"""Decide which CI gates a change needs, from the files it touches.

Writes 15 boolean keys (`code`, `docs`, `docs_only`, `rust`, `docs_site`,
`ts_sdk`, `py_sdk`, `npm_pkg`, `install_scripts`, `release_scripts`,
`workflows`, `tui_only`, `platform_windows`, `platform_macos`, `full_matrix`)
to `$GITHUB_OUTPUT` and prints the same lines plus one `::notice::` line.
Every uncertainty resolves to running everything, so a lookup or diff error
costs runner minutes and never skips a gate.

* A pull request is classified by the three-dot diff against its base. A
  `ci:all-os` or `ci:full` label (passed via `PR_LABELS` or `--labels`) forces
  the full Rust and OS matrix.
* A first-attempt push to `dev` is classified by the two-dot diff against the
  head SHA of the last successful push run of the same workflow on `dev`
  (looked up with `gh api`; an explicit non-empty `--base` wins). Any `dev` push
  that carries non-docs changes (`code=true`) keeps `rust`, `release_scripts`,
  `platform_windows`, `platform_macos`, and `full_matrix` enabled so every
  mandatory push job required by `scripts/release/check_ci.py` executes.
* Every other push (`main`, a tagged commit), a manual dispatch, a re-run, an
  empty diff and every lookup or diff failure runs everything.
* `docs` is true when a README, a Markdown file under `docs/` or an asset under
  `docs/` changed.

Usage: classify_changes.py [--event NAME] [--ref REF] [--attempt N]
                           [--workflow FILE] [--repo OWNER/REPO]
                           [--base SHA] [--head SHA] [--files-from FILE]
                           [--labels JSON_OR_CSV]
Defaults come from GITHUB_EVENT_NAME, GITHUB_REF, GITHUB_RUN_ATTEMPT,
GITHUB_REPOSITORY, and PR_LABELS.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys

HEX40 = re.compile(r"^[0-9a-f]{40}$")
DEV_REF = "refs/heads/dev"
FORCE_ALL_OS_LABELS = frozenset({"ci:all-os", "ci:full"})

# Paths whose change cannot alter a build, a test or a binary except through the
# embedded documentation corpus (covered separately by the `docs` gates).
DOC_PATTERNS = (
    "README*.md",
    "PHILOSOPHY*.md",
    "AGENTS*.md",
    "CHANGELOG*.md",
    "LICENSE*",
    "docs/*",
    ".github/ISSUE_TEMPLATE/*",
    ".github/pull_request_template.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
)

# The part of the documentation that reaches the binary or a docs test.
EMBEDDED_PATTERNS = ("README*.md", "docs/*")

DOCS_SITE_PATTERNS = (
    "packages/docs-site/*",
    "scripts/ci/*docs_site*",
    ".github/workflows/docs-site-deploy.yml",
)

TS_SDK_PATTERNS = (
    "Cargo.toml",
    "packages/sdk/typescript/*",
    "crates/harness-api/*",
    "crates/harness-api-server/*",
    "crates/agent-sdk/*",
    "scripts/sdk/*",
)

PY_SDK_PATTERNS = (
    "packages/sdk/python/*",
    "crates/harness-api/*",
    "crates/harness-api-server/*",
    "crates/agent-sdk/*",
    "scripts/sdk/*",
)

NPM_PKG_PATTERNS = (
    "Cargo.toml",
    "packages/npm/*",
    "scripts/npm/*",
)

INSTALL_SCRIPT_PATTERNS = ("scripts/install/*",)

RELEASE_SCRIPT_PATTERNS = (
    "scripts/release/*",
    "tests/*release*.py",
    "tests/test_ci_workflow_guards.py",
    "tests/test_install_justrust_ci.py",
    "tests/test_verify_public_release.py",
    "tests/test_watch_release_run.py",
    "tests/test_public_release_builds.py",
    "scripts/ci/install_justrust.sh",
    "scripts/ci/classify_changes.py",
    "scripts/ci/test_classify_changes.py",
    ".github/workflows/*",
    ".github/actions/*",
    ".github/dependabot.yml",
)

WORKFLOW_PATTERNS = (
    ".github/workflows/*",
    ".github/actions/*",
    ".github/dependabot.yml",
    "tests/test_ci_workflow_guards.py",
)

# Paths outside DOC_PATTERNS whose changes are verified by dedicated non-Rust
# jobs (SDK, docs-site, workflow-lint, release-automation, install-friction)
# and do not compile or alter Rust crates/scripts used inside Build & Test.
NON_RUST_CODE_PATTERNS = (
    "packages/docs-site/*",
    "packages/sdk/typescript/*",
    "packages/sdk/python/*",
    "packages/npm/*",
    "scripts/npm/prepare_*",
    "scripts/npm/release_npm_version.py",
    "scripts/npm/test_*",
    "scripts/sdk/*",
    "scripts/install/*.sh",
    "scripts/install/lib/*",
    "scripts/release/agent_archive.sh",
    "scripts/release/build_linux_compat.sh",
    "scripts/release/check_ci.py",
    "scripts/release/Containerfile",
    "scripts/release/next_version.py",
    "scripts/release/prepare_source.py",
    "scripts/release/release.sh",
    "scripts/release/release_notes.sh",
    "scripts/release/verify_public.py",
    "scripts/release/watch_run.sh",
    "scripts/release/test_*.py",
    "scripts/ci/classify_changes.py",
    "scripts/ci/test_classify_changes.py",
    "scripts/ci/*docs*",
    "scripts/ci/*issue_refs*",
    "scripts/ci/package_compat.py",
    "scripts/ci/test_package_compat.py",
    "scripts/ci/install_justrust.sh",
    "tests/*release*.py",
    "tests/test_ci_workflow_guards.py",
    "tests/test_install_justrust_ci.py",
    "tests/test_public_release_builds.py",
    "tests/test_verify_public_release.py",
    "tests/test_watch_release_run.py",
    ".github/workflows/*",
    ".github/actions/*",
    ".github/dependabot.yml",
)

# Linux source files directly included via `#[path]` in `crates/platform-macos/src/lib.rs:8-26`.
MACOS_SHARED_LINUX_PATTERNS = (
    "crates/platform-linux/src/child_exit.rs",
    "crates/platform-linux/src/unix.rs",
    "crates/platform-linux/src/unix_fs.rs",
    "crates/platform-linux/src/ipc.rs",
    "crates/platform-linux/src/unix_term.rs",
    "crates/platform-linux/src/signal.rs",
)

FULL_MATRIX_PATTERNS = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/*",
    "build.rs",
    "crates/*/build.rs",
    "examples/packages/*",
)

PLATFORM_MACOS_PATTERNS = (
    "crates/platform-macos/*",
    "crates/platform/*",
    "crates/platform-api/*",
    *MACOS_SHARED_LINUX_PATTERNS,
)

PLATFORM_WINDOWS_PATTERNS = (
    "crates/platform-windows/*",
    "crates/platform/*",
    "crates/platform-api/*",
    "scripts/ci/*.ps1",
    "scripts/install/*.ps1",
    "*.ps1",
)

FULL_RUN = {
    "code": True,
    "docs": False,
    "docs_only": False,
    "rust": True,
    "docs_site": True,
    "ts_sdk": True,
    "py_sdk": True,
    "npm_pkg": True,
    "install_scripts": True,
    "release_scripts": True,
    "workflows": True,
    "tui_only": False,
    "platform_windows": True,
    "platform_macos": True,
    "full_matrix": True,
}


def matches(path: str, patterns: tuple[str, ...]) -> bool:
    # fnmatch's `*` crosses `/`, which is what `docs/*` needs for nested pages.
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def is_dev_push(event: str, ref: str, attempt: int) -> bool:
    return event == "push" and ref == DEV_REF and attempt == 1


def parse_labels(raw: str | None) -> set[str]:
    """Parse PR labels from JSON array or comma-separated string."""
    if not raw or not raw.strip():
        return set()
    stripped = raw.strip()
    if stripped.startswith("["):
        try:
            loaded = json.loads(stripped)
            if isinstance(loaded, list):
                return {str(item).strip() for item in loaded if str(item).strip()}
        except json.JSONDecodeError:
            return set()
    return {item.strip() for item in stripped.split(",") if item.strip()}


def classify(
    event: str,
    files: list[str] | None,
    ref: str = "",
    attempt: int = 1,
    labels: set[str] | list[str] | None = None,
) -> dict[str, bool]:
    """Classify *files*; only a pull request or a first-attempt dev push is classified."""
    dev_push = is_dev_push(event, ref, attempt)
    if event != "pull_request" and not dev_push:
        return dict(FULL_RUN)
    if not files:
        return dict(FULL_RUN)

    code = any(not matches(path, DOC_PATTERNS) for path in files)
    docs = any(matches(path, EMBEDDED_PATTERNS) for path in files)
    if not code:
        return {
            "code": False,
            "docs": docs or not code,
            "docs_only": True,
            "rust": False,
            "docs_site": docs,
            "ts_sdk": False,
            "py_sdk": False,
            "npm_pkg": False,
            "install_scripts": False,
            "release_scripts": False,
            "workflows": False,
            "tui_only": False,
            "platform_windows": False,
            "platform_macos": False,
            "full_matrix": False,
        }

    label_set = set(labels or ())
    if event == "pull_request" and (label_set & FORCE_ALL_OS_LABELS):
        forced = dict(FULL_RUN)
        forced["docs"] = docs
        return forced

    docs_site = docs or any(matches(path, DOCS_SITE_PATTERNS) for path in files)
    ts_sdk = any(matches(path, TS_SDK_PATTERNS) for path in files)
    py_sdk = any(matches(path, PY_SDK_PATTERNS) for path in files)
    npm_pkg = any(matches(path, NPM_PKG_PATTERNS) for path in files)
    install_scripts = any(matches(path, INSTALL_SCRIPT_PATTERNS) for path in files)
    release_scripts = any(matches(path, RELEASE_SCRIPT_PATTERNS) for path in files)
    workflows = any(matches(path, WORKFLOW_PATTERNS) for path in files)
    rust = any(
        not matches(path, DOC_PATTERNS) and not matches(path, NON_RUST_CODE_PATTERNS)
        for path in files
    )
    tui_only = rust and all(
        matches(path, ("crates/tui/*", *DOC_PATTERNS, *NON_RUST_CODE_PATTERNS))
        for path in files
    )
    full_matrix = any(matches(path, FULL_MATRIX_PATTERNS) for path in files)
    platform_macos = full_matrix or any(
        matches(path, PLATFORM_MACOS_PATTERNS) for path in files
    )
    platform_windows = full_matrix or any(
        matches(path, PLATFORM_WINDOWS_PATTERNS) for path in files
    )

    if dev_push:
        # `dev` pushes serve as release evidence for `scripts/release/check_ci.py`
        # whenever non-docs files change, so every mandatory job runs cleanly.
        rust = True
        release_scripts = True
        platform_windows = True
        platform_macos = True
        full_matrix = True
        tui_only = False

    return {
        "code": True,
        "docs": docs,
        "docs_only": False,
        "rust": rust,
        "docs_site": docs_site,
        "ts_sdk": ts_sdk,
        "py_sdk": py_sdk,
        "npm_pkg": npm_pkg,
        "install_scripts": install_scripts,
        "release_scripts": release_scripts,
        "workflows": workflows,
        "tui_only": tui_only,
        "platform_windows": platform_windows,
        "platform_macos": platform_macos,
        "full_matrix": full_matrix,
    }


def fetch_runs(repo: str, workflow: str) -> str | None:
    """Return the API text listing the last successful dev push run, or None."""
    url = (f"repos/{repo}/actions/workflows/{workflow}/runs"
           "?branch=dev&event=push&status=success&per_page=1")
    try:
        out = subprocess.run(["gh", "api", url], capture_output=True,
                             text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout if out.returncode == 0 else None


def last_green_sha(repo: str, workflow: str, fetch=fetch_runs) -> str | None:
    """Head SHA of the last successful push run of *workflow* on `dev`, or None.

    *fetch(repo, workflow)* returns the API response text or None. Any failure,
    an empty list, a null or malformed SHA gives None; nothing raises.
    """
    if not repo or not workflow:
        return None
    try:
        runs = json.loads(fetch(repo, workflow))["workflow_runs"]
        sha = runs[0]["head_sha"]
    except Exception:
        return None
    return sha if isinstance(sha, str) and HEX40.fullmatch(sha) else None


def head_in_origin_main(head: str, cwd: str | None = None) -> bool | None:
    """True if *head* is in origin/main, False if not, None if undeterminable."""
    try:
        result = subprocess.run(
            ["git", "merge-base", "--is-ancestor", head, "origin/main"],
            capture_output=True, text=True, timeout=30, cwd=cwd,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return {0: True, 1: False}.get(result.returncode)


def changed_files(base: str, head: str, three_dot: bool = True,
                  cwd: str | None = None) -> list[str] | None:
    """Files changed between *base* and *head*, or None when git cannot diff.

    A pull request uses ``base...head``; a dev push uses ``base..head``. Both
    pass ``--no-renames`` so a code file moved under `docs/` still lists its old
    code path.
    """
    spec = f"{base}{'...' if three_dot else '..'}{head}"
    try:
        out = subprocess.run(
            ["git", "diff", "--name-only", "-z", "--no-renames", spec],
            check=True, capture_output=True, timeout=30, cwd=cwd,
            # git prints raw path bytes; a non-UTF-8 name must not crash the job.
            encoding="utf-8", errors="surrogateescape",
        ).stdout
    except (subprocess.CalledProcessError, OSError, subprocess.TimeoutExpired):
        return None
    return [path for path in out.split("\0") if path]


def parse_attempt(value: str | None) -> int:
    """A positive attempt number; anything else is 0, which counts as a re-run."""
    try:
        number = int(value or "")
    except ValueError:
        return 0
    return number if number > 0 else 0


def resolve(args: argparse.Namespace, attempt: int,
            fetch=fetch_runs, cwd: str | None = None) -> tuple[list[str] | None, str]:
    """Return the changed files (None = run everything) and the notice text."""
    where = f"{args.event or 'unknown event'} on {args.ref or 'unknown ref'}, attempt {attempt}"
    if args.files_from:
        with open(args.files_from, encoding="utf-8") as handle:
            files = [line.strip() for line in handle if line.strip()]
        return files, f"{where}: {len(files)} file(s) from {args.files_from}"
    if args.event == "pull_request":
        if not (args.base and args.head):
            return None, f"{where}: no base or head SHA; running every gate"
        files = changed_files(args.base, args.head, cwd=cwd)
        if files is None:
            return None, f"{where}: cannot diff {args.base}...{args.head}; running every gate"
        return files, f"{where}: {len(files)} changed file(s) in {args.base}...{args.head}"
    if not is_dev_push(args.event, args.ref, attempt):
        return None, f"{where} is never classified; running every gate"
    if not args.head:
        return None, f"{where}: no head SHA; running every gate"
    in_main = head_in_origin_main(args.head, cwd=cwd)
    if in_main is not False:
        state = "is already in origin/main" if in_main else "could not be checked against origin/main"
        return None, f"{where}: {args.head} {state}; running every gate"
    if args.base:
        base, source = args.base, "--base"
    else:
        base = last_green_sha(args.repo, args.workflow, fetch=fetch)
        source = f"the last successful push run of {args.workflow or '(no workflow)'} on dev"
        if base is None:
            return None, (f"{where}: no successful push run of "
                          f"{args.workflow or '(no workflow)'} on dev found; running every gate")
    files = changed_files(base, args.head, three_dot=False, cwd=cwd)
    if files is None:
        return None, f"{where}: cannot diff {base}..{args.head}; running every gate"
    return files, f"{where}: compared with {base} ({source}): {len(files)} changed file(s)"


def parser() -> argparse.ArgumentParser:
    env = os.environ.get
    p = argparse.ArgumentParser()
    p.add_argument("--event", default=env("GITHUB_EVENT_NAME", ""))
    p.add_argument("--ref", default=env("GITHUB_REF", ""))
    p.add_argument("--attempt", default=env("GITHUB_RUN_ATTEMPT", ""))
    p.add_argument("--workflow", default="")
    p.add_argument("--repo", default=env("GITHUB_REPOSITORY", ""))
    p.add_argument("--base", default="")
    p.add_argument("--head", default="")
    p.add_argument("--files-from")
    p.add_argument("--labels", default=env("PR_LABELS", ""))
    return p


def main(argv: list[str] | None = None, fetch=fetch_runs) -> int:
    args = parser().parse_args(argv)
    attempt = parse_attempt(args.attempt)
    labels = parse_labels(args.labels)
    files, notice = resolve(args, attempt, fetch=fetch)
    result = classify(args.event, files, ref=args.ref, attempt=attempt, labels=labels)
    if files is not None and not files:
        notice += "; empty diff, running every gate"
    lines = [f"{key}={'true' if value else 'false'}" for key, value in result.items()]
    print("\n".join(lines))
    print(
        f"::notice::{notice}; code={str(result['code']).lower()} "
        f"docs_only={str(result['docs_only']).lower()} "
        f"rust={str(result['rust']).lower()}"
    )
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
