#!/usr/bin/env python3
"""Decide which CI gates a change needs, from the files it touches.

Writes `code`, `docs` and `docs_only` (each `true`/`false`) to `$GITHUB_OUTPUT`
and prints the same lines plus exactly one `::notice::` line. Every uncertainty
resolves to `code=true`, so a failure here costs time and never skips a gate.

* A pull request is classified by the three-dot diff against its base.
* A first-attempt push to `dev` is classified by the two-dot diff against the
  head SHA of the last successful push run of the same workflow on `dev`
  (looked up with `gh api`; an explicit non-empty `--base` wins). A head that is
  already in `origin/main` runs everything: it may be the commit a release tags.
* Every other push (`main`, a tagged commit), a manual dispatch, a re-run, an
  empty diff and every lookup or diff failure runs everything:
  `scripts/release/check_ci.py` accepts only a fully successful push run as
  release evidence.
* `docs` is true when a README, a Markdown file under `docs/` or an asset under
  `docs/` changed. `docs/` and `README.md` are compiled into the binary by
  `crates/app-core/build.rs` and read by `horon_agent_docs`, so a docs change
  still gets the docs gates and the corpus test, but no release build, no
  cross-platform test run and no structural performance run.
* `code` is true when any other file changed. That includes workflows, scripts,
  dependencies and lockfiles; they are never "documentation".

Usage: classify_changes.py [--event NAME] [--ref REF] [--attempt N]
                           [--workflow FILE] [--repo OWNER/REPO]
                           [--base SHA] [--head SHA] [--files-from FILE]
Defaults come from GITHUB_EVENT_NAME, GITHUB_REF, GITHUB_RUN_ATTEMPT and
GITHUB_REPOSITORY.
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

# Paths whose change cannot alter a build, a test or a binary except through the
# embedded documentation corpus (covered separately by the `docs` gates).
DOC_PATTERNS = (
    "README*.md",
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

FULL_RUN = {"code": True, "docs": False, "docs_only": False}


def matches(path: str, patterns: tuple[str, ...]) -> bool:
    # fnmatch's `*` crosses `/`, which is what `docs/*` needs for nested pages.
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def is_dev_push(event: str, ref: str, attempt: int) -> bool:
    return event == "push" and ref == DEV_REF and attempt == 1


def classify(event: str, files: list[str] | None,
             ref: str = "", attempt: int = 1) -> dict[str, bool]:
    """Classify *files*; only a pull request or a first-attempt dev push is classified."""
    if event != "pull_request" and not is_dev_push(event, ref, attempt):
        return dict(FULL_RUN)
    if not files:
        return dict(FULL_RUN)
    code = any(not matches(path, DOC_PATTERNS) for path in files)
    docs = any(matches(path, EMBEDDED_PATTERNS) for path in files)
    return {"code": code, "docs": docs or not code, "docs_only": not code}


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
    return p


def main(argv: list[str] | None = None, fetch=fetch_runs) -> int:
    args = parser().parse_args(argv)
    attempt = parse_attempt(args.attempt)
    files, notice = resolve(args, attempt, fetch=fetch)
    result = classify(args.event, files, ref=args.ref, attempt=attempt)
    if files is not None and not files:
        notice += "; empty diff, running every gate"
    lines = [f"{key}={'true' if value else 'false'}" for key, value in result.items()]
    print("\n".join(lines))
    print(f"::notice::{notice}; code={str(result['code']).lower()} "
          f"docs_only={str(result['docs_only']).lower()}")
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
