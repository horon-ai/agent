#!/usr/bin/env bash
set -euo pipefail

# Generate a human-readable release notes body for a horon-agent release.
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
# to the highest tagged version below this one.
if [[ -z "$PREV_TAG" ]]; then
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        PREV_TAG="$(git describe --tags --abbrev=0 "$TAG^" 2>/dev/null || true)"
    fi
    if [[ -z "$PREV_TAG" ]]; then
        # `--sort=-v:refname` ranks every tag in the repository, and a
        # prerelease series needs semver order rather than its ASCII order, so
        # the base tag is chosen here: the highest release tag that sorts
        # strictly below the one being released (`v0.2.0-alpha` <
        # `v0.2.0-alpha.1` < `v0.2.0`). That keeps the previous entry on this
        # tag's own line and never picks an unrelated newer tag.
        PREV_TAG="$(git tag -l 'v*' | python3 -c '
import re
import sys

pattern = re.compile(r"v([0-9]+)\.([0-9]+)\.([0-9]+)(?:-([0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*))?$")

def key(tag):
    match = pattern.match(tag)
    if match is None:
        return None
    major, minor, patch, prerelease = match.groups()
    if prerelease is None:
        return ((int(major), int(minor), int(patch)), 1, ())
    identifiers = tuple(
        (0, int(part), "") if part.isdigit() else (1, 0, part)
        for part in prerelease.split(".")
    )
    return ((int(major), int(minor), int(patch)), 0, identifiers)

target = key(sys.argv[1])
lower = [(key(tag), tag) for tag in sys.stdin.read().split() if key(tag) is not None]
lower = [(k, tag) for k, tag in lower if target is None or k < target]
print(max(lower)[1] if lower else "")
' "$TAG")"
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
