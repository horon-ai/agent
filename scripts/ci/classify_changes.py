#!/usr/bin/env python3
"""Decide which CI gates a change needs, from the files it touches.

Writes `code`, `docs` and `docs_only` (each `true`/`false`) to `$GITHUB_OUTPUT`
and prints the same lines. Every uncertainty resolves to `code=true`, so a
failure here costs time and never skips a gate.

* A pull request is classified. A push to `dev` is also classified (first
  attempt only), compared with the head SHA of the last successful push run of
  the same workflow on `dev`. Every other push (`main`, a tagged commit), a
  manual dispatch, a re-run and every lookup or diff failure runs everything:
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
                           [--base SHA --head SHA]
       classify_changes.py --files-from FILE --event pull_request   (tests)
       classify_changes.py --files-from FILE --event push --ref refs/heads/dev --attempt 1
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


def matches(path: str, patterns: tuple[str, ...]) -> bool:
    # fnmatch's `*` crosses `/`, which is what `docs/*` needs for nested pages.
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def _default_fetch(repo: str, workflow: str) -> str | None:
    """Fetch the head_sha of the last successful push run of *workflow* on *dev*.

    Uses `gh api` (requires GH_TOKEN in env). Returns the raw response text
    or None on any failure (timeout 30s, missing gh, network error).
    """
    try:
        out = subprocess.run(
            [
                "gh", "api",
                f"repos/{repo}/actions/workflows/{workflow}/runs",
                "?branch=dev&event=push&status=success&per_page=1",
            ],
            capture_output=True, text=True, timeout=30,
        )
    except Exception:
        return None
    if out.returncode != 0:
        return None
    return out.stdout


def last_green_sha(repo: str, workflow: str, fetch=None) -> str | None:
    """Return the head_sha of the last successful push run of *workflow* on *dev*.

    *fetch* defaults to _default_fetch. It may be injected (callable returning
    text, or a plain value). Returns None unless the parsed head_sha is a string
    of exactly 40 hex characters. API failure, timeout, missing gh, invalid JSON,
    empty list, null, wrong type: all return None, never an exception. The entire
    body is wrapped in one try/except Exception -> None.
    """
    try:
        if fetch is None:
            raw = _default_fetch(repo, workflow)
        else:
            raw = fetch(repo, workflow) if callable(fetch) else fetch
        if raw is None:
            return None
        # Parse JSON or check if already a 40-hex SHA.
        head_sha = None
        try:
            data = json.loads(raw)
        except (json.JSONDecodeError, ValueError):
            data = None
        if isinstance(data, dict) and isinstance(data.get("workflow_runs"), list):
            runs = data["workflow_runs"]
            if runs:
                rs = runs[0]
                if isinstance(rs, dict):
                    head_sha = rs.get("head_sha")
        if head_sha is None and isinstance(raw, str) and HEX40.fullmatch(raw):
            head_sha = raw
        if isinstance(head_sha, str) and HEX40.fullmatch(head_sha):
            return head_sha
        return None
    except Exception:
        return None


def head_in_origin_main(head: str) -> bool | None:
    """Return True if *head* is contained in origin/main.

    Returns True (exit 0), False (exit 1), or None when it cannot be determined
    (any other exit code, missing git, or exception).
    """
    if not head:
        return None
    try:
        result = subprocess.run(
            ["git", "merge-base", "--is-ancestor", head, "origin/main"],
            capture_output=True, text=True, timeout=30,
        )
    except Exception:
        return None
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    return None


def classify(event: str, files: list[str] | None,
             ref: str = "", attempt: int = 1) -> dict[str, bool]:
    """Classify *files* for *event*.

    Only a pull request, or a first-attempt push to refs/heads/dev, is
    classified. Everything else returns a full run (code=true).
    """
    is_pr = event == "pull_request"
    is_dev_push = event == "push" and ref == "refs/heads/dev" and attempt == 1
    if not is_pr and not is_dev_push:
        return {"code": True, "docs": False, "docs_only": False}
    if not files:
        return {"code": True, "docs": False, "docs_only": False}
    code = any(not matches(path, DOC_PATTERNS) for path in files)
    docs = any(matches(path, EMBEDDED_PATTERNS) for path in files)
    return {"code": code, "docs": docs or not code, "docs_only": not code}


def changed_files(base: str, head: str, three_dot: bool = True,
                  cwd: str | None = None) -> list[str] | None:
    """List files changed between base and head.

    three_dot=True (default, PR): uses ``base...head`` (merge-base diff).
    three_dot=False (dev push): uses ``base..head`` (two-dot tree diff).
    Both forms pass ``--no-renames`` so a code file moved into a doc path
    still lists its old code path.
    *cwd* is passed to the git subprocess; when None the caller's cwd is used.
    """
    sep = "..." if three_dot else ".."
    try:
        out = subprocess.run(
            ["git", "diff", "--name-only", "-z", "--no-renames",
             f"{base}{sep}{head}"],
            check=True, capture_output=True, text=True, timeout=30,
            cwd=cwd,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError, OSError,
            subprocess.TimeoutExpired) as exc:
        print(f"::notice::cannot diff {base}{sep}{head} "
              f"({type(exc).__name__}); running every gate")
        return None
    return [path for path in out.split("\0") if path]


def _parse_positive_int(val: str | None) -> int | None:
    """Return val as int if it is a positive integer, else None."""
    if not val:
        return None
    try:
        n = int(val)
        return n if n > 0 else None
    except ValueError:
        return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", ""))
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--files-from")
    parser.add_argument("--workflow", default=os.environ.get("GITHUB_WORKFLOW", ""))
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
    parser.add_argument("--ref", default=os.environ.get("GITHUB_REF", ""))
    parser.add_argument("--attempt", default=os.environ.get("GITHUB_RUN_ATTEMPT", ""))
    args = parser.parse_args()

    attempt = _parse_positive_int(args.attempt)

    if args.files_from:
        with open(args.files_from, encoding="utf-8") as handle:
            files = [line.strip() for line in handle if line.strip()]
    elif args.base and args.head:
        files = changed_files(args.base, args.head)
    else:
        # For a dev push (first attempt), derive base from last green SHA.
        # When --base is absent, no base → full run.
        if not args.base and args.event == "push" and args.ref == "refs/heads/dev" and attempt == 1:
            base_sha = last_green_sha(args.repo, args.workflow)
            if base_sha is None:
                # No green SHA available → full run, but still output result.
                files = None
            else:
                # We have a base but no head: full run if cannot diff.
                # The caller should have passed --head or use GITHUB_SHA.
                files = None
        else:
            files = None

    result = classify(args.event, files, ref=args.ref, attempt=attempt)
    lines = [f"{key}={'true' if value else 'false'}" for key, value in result.items()]
    print("\n".join(lines))
    if files is not None:
        print(f"::notice::{len(files)} changed file(s); code={result['code']} docs={result['docs']}")
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
