#!/usr/bin/env bash
set -euo pipefail

# Generate a human-readable release notes body for a horon-agent release (issue #435).
#
# Usage:
#   scripts/release/release_notes.sh <tag> [previous-tag]
#
# Behavior: group commit subjects between the previous tag and <tag> by
# conventional-commit prefix (feat/fix/perf/docs/other), then append the
# GitHub compare link.

TAG="${1:?Usage: scripts/release/release_notes.sh <tag> [previous-tag]}"
PREV_TAG="${2:-}"

cd "$(git rev-parse --show-toplevel)"

# Determine the owner/repo slug for the compare link.
repo_slug() {
    if [[ -n "${GITHUB_REPOSITORY:-}" ]]; then
        echo "$GITHUB_REPOSITORY"
        return
    fi
    local url
    url="$(git remote get-url origin 2>/dev/null || true)"
    if [[ "$url" =~ github\.com[:/]+([^/]+/[^/ ]+) ]]; then
        echo "${BASH_REMATCH[1]%.git}"
        return
    fi
    echo "horon-ai/agent"
}

REPO="$(repo_slug)"

# Without an explicit previous tag, use the nearest ancestor tag, falling back
# to the next lower version tag in the tag list.
if [[ -z "$PREV_TAG" ]]; then
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        PREV_TAG="$(git describe --tags --abbrev=0 "$TAG^" 2>/dev/null || true)"
    fi
    if [[ -z "$PREV_TAG" ]]; then
        PREV_TAG="$(git tag -l 'v*' --sort=-v:refname \
            | grep -A1 -Fx "$TAG" | tail -n +2 | head -1 || true)"
    fi
fi

# Group commit subjects since the previous tag by conventional-commit prefix.
render_commits() {
    local range
    local end_ref="$TAG"
    # Support pre-tag dry runs where the tag does not exist yet.
    if ! git rev-parse -q --verify "$TAG^{commit}" >/dev/null; then
        end_ref="HEAD"
    fi
    if [[ -n "$PREV_TAG" ]]; then
        range="$PREV_TAG..$end_ref"
    else
        range="$end_ref"
    fi

    python3 - "$range" << 'PY'
import re
import subprocess
import sys

subjects = subprocess.run(
    ["git", "log", "--no-merges", "--pretty=format:%s", sys.argv[1]],
    check=True,
    capture_output=True,
    text=True,
).stdout.splitlines()

groups = {"feat": [], "fix": [], "perf": [], "docs": [], "other": []}
pattern = re.compile(r"^(feat|fix|perf|docs)(\([^)]*\))?!?:\s*(.+)$", re.IGNORECASE)
for subject in subjects:
    subject = subject.strip()
    if not subject:
        continue
    match = pattern.match(subject)
    if match:
        groups[match.group(1).lower()].append(match.group(3).strip())
    else:
        groups["other"].append(subject)

out = []
for key, heading in (
    ("feat", "Features"),
    ("fix", "Fixes"),
    ("perf", "Performance"),
    ("docs", "Documentation"),
    ("other", "Other changes"),
):
    items = groups[key]
    if not items:
        continue
    out.append(f"### {heading}")
    out.append("")
    out.extend(f"- {item}" for item in items)
    out.append("")

print("\n".join(out).rstrip())
PY
}

BODY="$(render_commits)"

if [[ -n "$BODY" ]]; then
    printf '%s\n\n' "$BODY"
fi

if [[ -n "$PREV_TAG" ]]; then
    printf '**Full changelog**: https://github.com/%s/compare/%s...%s\n' \
        "$REPO" "$PREV_TAG" "$TAG"
else
    printf '**Full changelog**: https://github.com/%s/commits/%s\n' "$REPO" "$TAG"
fi
