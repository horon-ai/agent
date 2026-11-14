#!/usr/bin/env python3
"""Every `#NNN` written in `.github/` and `scripts/` must name a real issue or PR.

A comment that points at a number nobody can open is worse than no comment: the
reader follows it, finds nothing, and cannot tell whether the reasoning was lost
or the number was never real. Eleven such references had accumulated (they were
written against another repository or before a history rewrite), so the sweep
that removes them needs something that keeps them from coming back.

The check reads the numbers out of the tree and resolves them against this
repository with `gh api`, which the workflows already require. Only the refs in
`.github/` and `scripts/` are checked: those are the files a maintainer opens
while debugging CI, and they are the ones that rotted.

Two deliberate allowances, both narrow:

* `.github/workflows/require-issue.yml` documents the syntax it accepts, so its
  numbers are examples, not references.
* An API rate limit is transient and shared with every other session on the
  account, so that one condition exits zero with a `::notice::` line instead of
  turning a healthy pull request red. Every other failure — a 404, a network
  error, a missing `gh`, a missing credential — fails the gate, because "could
  not check" must never read as "checked and fine". The workflow therefore
  passes `GH_TOKEN` to the step; without it `gh` exits refusing to run at all.

Usage: check_issue_refs.py [--repo OWNER/REPO] [--root DIR]
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

SEARCHED = (".github", "scripts")
REF = re.compile(r"#(?P<number>[0-9]{1,5})\b")
ILLUSTRATIVE = {".github/workflows/require-issue.yml"}


def references(root: Path) -> dict[int, list[str]]:
    """Map each referenced number to the `path:line` sites that mention it."""
    found: dict[int, list[str]] = {}
    for directory in SEARCHED:
        for path in sorted((root / directory).rglob("*")):
            if not path.is_file():
                continue
            relative = path.relative_to(root).as_posix()
            if relative in ILLUSTRATIVE:
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            for position, line in enumerate(text.splitlines(), start=1):
                for match in REF.finditer(line):
                    found.setdefault(int(match.group("number")), []).append(f"{relative}:{position}")
    return found


def existing_numbers(repo: str) -> set[int]:
    """Every issue and pull request number in `repo`; issues and PRs share the numbering."""
    try:
        output = subprocess.run(
            ["gh", "api", "--paginate", f"repos/{repo}/issues?state=all&per_page=100", "--jq", ".[].number"],
            check=True,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        raise SystemExit("gh is not installed, so issue references cannot be verified")
    except subprocess.CalledProcessError as error:
        message = (error.stderr or "").strip()
        if "rate limit" in message.lower():
            print("::notice::issue reference check skipped: GitHub API rate limit reached")
            return set()
        if "GH_TOKEN" in message:
            # `gh` refuses to make any request from a workflow without it. The
            # fix is a workflow change, so name it instead of quoting the hint.
            raise SystemExit(
                "gh has no credential here; the job must pass GH_TOKEN (see the "
                "'Issue references resolve' step in .github/workflows/ci.yml)"
            )
        raise SystemExit(f"gh api failed while listing issues: {message}")
    return {int(line) for line in output.stdout.split()}


def problems(found: dict[int, list[str]], existing: set[int]) -> list[str]:
    return [
        f"#{number} does not exist in this repository: {', '.join(sites)}"
        for number, sites in sorted(found.items())
        if number not in existing
    ]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", default="horon-ai/agent")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args(argv)

    found = references(args.root)
    if not found:
        print("no issue references found")
        return 0
    existing = existing_numbers(args.repo)
    if not existing:
        return 0
    failures = problems(found, existing)
    if failures:
        print("\n".join(failures), file=sys.stderr)
        print(f"{len(failures)} unresolvable reference(s) in {', '.join(SEARCHED)}", file=sys.stderr)
        return 1
    print(f"{len(found)} distinct issue reference(s) in {', '.join(SEARCHED)} all resolve")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
