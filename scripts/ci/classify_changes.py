#!/usr/bin/env python3
"""Decide which CI gates a change needs, from the files it touches.

Writes `code`, `docs` and `docs_only` (each `true`/`false`) to `$GITHUB_OUTPUT`
and prints the same lines. Every uncertainty resolves to `code=true`, so a
failure here costs time and never skips a gate.

* Only a pull request is classified. A push (`dev`, `main`, a tagged commit) and
  a manual dispatch always run everything: `scripts/release/check_ci.py` accepts
  only a fully successful push run as release evidence.
* `docs` is true when a README, a Markdown file under `docs/` or an asset under
  `docs/` changed. `docs/` and `README.md` are compiled into the binary by
  `crates/app-core/build.rs` and read by `horon_agent_docs`, so a docs change
  still gets the docs gates and the corpus test, but no release build, no
  cross-platform test run and no structural performance run.
* `code` is true when any other file changed. That includes workflows, scripts,
  dependencies and lockfiles; they are never "documentation".

Usage: classify_changes.py [--event NAME] [--base SHA --head SHA]
       classify_changes.py --files-from FILE --event pull_request   (tests)
"""

from __future__ import annotations

import argparse
import fnmatch
import os
import subprocess
import sys

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


def classify(event: str, files: list[str] | None) -> dict[str, bool]:
    if event != "pull_request" or not files:
        # A push, a dispatch, an empty or unreadable diff: full run.
        return {"code": True, "docs": False, "docs_only": False}
    code = any(not matches(path, DOC_PATTERNS) for path in files)
    docs = any(matches(path, EMBEDDED_PATTERNS) for path in files)
    return {"code": code, "docs": docs or not code, "docs_only": not code}


def changed_files(base: str, head: str) -> list[str] | None:
    try:
        out = subprocess.run(
            ["git", "diff", "--name-only", "-z", f"{base}...{head}"],
            check=True, capture_output=True, text=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError, OSError) as exc:
        print(f"::notice::cannot diff {base}...{head} ({type(exc).__name__}); running every gate")
        return None
    return [path for path in out.split("\0") if path]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", ""))
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--files-from")
    args = parser.parse_args()

    if args.files_from:
        with open(args.files_from, encoding="utf-8") as handle:
            files = [line.strip() for line in handle if line.strip()]
    elif args.base and args.head:
        files = changed_files(args.base, args.head)
    else:
        files = None

    result = classify(args.event, files)
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
