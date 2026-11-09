#!/usr/bin/env python3
"""List every warning in the logs of one GitHub Actions run.

AGENTS.md ("CI warnings") makes a CI run with any warning an unfinished task.
This is the check: it reads the log of every job of a run and prints each
distinct warning with the job and step that produced it, plus the run's
warning-level annotations. It exits 0 only when there are none.

    scripts/ci/run_warnings.py <run-id> [--repo OWNER/NAME]

Needs the `gh` CLI, signed in. A run still in progress has incomplete logs; wait
for it to finish first.

What counts as a warning, matched at the start of the message (timestamp and ANSI
colour codes removed) so a flag such as `--warning=no-unknown-keyword` or a test
named `..._warns_...` is not one:

  warning: ...        rustc, cargo, clippy, rustup (`warn:`), PowerShell scripts
  warning[...]: ...   rustc lints with a code
  Warning: ...        cargo-audit, ssh
  ##[warning]...      GitHub Actions annotations written to the log
  cargo:warning=...   build-script output echoed by cargo
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict

# `gh run view --log` writes the escape character as the two characters "^[".
ANSI = re.compile(r"(?:\x1b|\^\[)\[[0-9;]*[A-Za-z]")
# `gh run view --log` lines: "<job>\t<step>\t<timestamp>Z <message>".
LOG_LINE = re.compile(r"^(?P<job>[^\t]*)\t(?P<step>[^\t]*)\t\ufeff?\d{4}-\d\d-\d\dT[\d:.]+Z ?(?P<msg>.*)$")
WARNING = re.compile(
    r"^\s*(?:warning(?:\[[^\]]*\])?:|warn:|Warning:|##\[warning\]|cargo:warning=)", re.IGNORECASE
)
# cargo's per-crate summary repeats warnings that were already printed above it.
SUMMARY = re.compile(r"^warning: `[^`]+` \(.*\) generated \d+ warnings?")


def gh(*args: str) -> str:
    result = subprocess.run(["gh", *args], check=True, text=True, stdout=subprocess.PIPE)
    return result.stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("run_id")
    parser.add_argument("--repo", default="horon-ai/agent")
    args = parser.parse_args()

    run = json.loads(gh("run", "view", args.run_id, "--repo", args.repo, "--json", "status,jobs"))
    if run["status"] != "completed":
        print(f"run {args.run_id} is {run['status']}; its logs are incomplete", file=sys.stderr)
        return 2

    found: dict[tuple[str, str], list[str]] = defaultdict(list)
    for job in run["jobs"]:
        log = gh("run", "view", "--repo", args.repo, "--job", str(job["databaseId"]), "--log")
        for line in log.splitlines():
            match = LOG_LINE.match(ANSI.sub("", line))
            if not match:
                continue
            message = match["msg"]
            if WARNING.match(message) and not SUMMARY.match(message):
                key = (job["name"], match["step"])
                text = message.strip()[:200]
                if text not in found[key]:
                    found[key].append(text)

        annotations = json.loads(
            gh("api", f"repos/{args.repo}/check-runs/{job['databaseId']}/annotations", "--paginate")
        )
        for annotation in annotations:
            if annotation["annotation_level"] in ("warning", "failure"):
                key = (job["name"], "annotation")
                text = f"{annotation['annotation_level']}: {annotation['message'].strip()[:200]}"
                if text not in found[key]:
                    found[key].append(text)

    total = sum(len(messages) for messages in found.values())
    for (job, step), messages in sorted(found.items()):
        print(f"== {job} / {step}")
        for message in messages:
            print(f"   {message}")
    print(f"{total} distinct warning(s) in {len(run['jobs'])} jobs of run {args.run_id}")
    return 1 if total else 0


if __name__ == "__main__":
    raise SystemExit(main())
