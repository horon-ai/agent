#!/usr/bin/env python3
"""Prepare the tagged release checkout and record its source provenance.

A release builds from the exact tagged commit. This helper pins the release-only
tooling to the reviewed workflow commit instead of a branch tip and writes
`release-provenance.json`, so every published asset can be traced back to a tag
commit, a workflow commit and a workflow run.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

RELEASE_HELPERS = ("scripts/ci/install_justrust.sh", "scripts/release/build_linux_compat.sh")


def git(*args):
    return subprocess.check_output(["git", *args]).decode().strip()


def tag_fits_version(tag, version):
    """True when `tag` may release the source whose Cargo version is `version`.

    The Cargo version names the release line and its lowest patch; later
    patches on the same MAJOR.MINOR line are numbered by tags (see
    next_version.py), so the tag must share MAJOR.MINOR and not go below the
    Cargo patch.
    """
    parts = re.fullmatch(r"([0-9]+)\.([0-9]+)\.([0-9]+)", version)
    if parts is None:
        return False
    wanted = tuple(map(int, tag[1:].split(".")))
    have = tuple(map(int, parts.groups()))
    return wanted[:2] == have[:2] and wanted[2] >= have[2]


def prepare(tag, workflow_sha):
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag):
        raise ValueError("Release tag must be vMAJOR.MINOR.PATCH")
    if not re.fullmatch(r"[0-9a-f]{40}", workflow_sha):
        raise ValueError("Workflow commit must be a full SHA")
    commit = git("rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}")
    if git("rev-parse", "HEAD") != commit:
        raise ValueError("Checkout must remain at the release tag commit")
    if git("status", "--porcelain", "--untracked-files=no"):
        raise ValueError("Tracked checkout must be clean before preparing the release")
    manifest = subprocess.check_output(["git", "show", "HEAD:Cargo.toml"]).decode()
    package = manifest.split("[package]", 1)[1].split("[", 1)[0]
    version = re.search(r'^version\s*=\s*"([^"\n]+)"', package, re.MULTILINE)
    if version is None or not tag_fits_version(tag, version.group(1)):
        raise ValueError("Tag does not fit the package version line (same MAJOR.MINOR, patch at or above Cargo.toml)")
    provenance = {"tag": tag, "tag_commit": commit, "workflow_commit": workflow_sha,
                  "run_url": os.environ.get("RELEASE_RUN_URL", "")}
    provenance["eligibility_policy"] = "exact-sha-main-ci"
    # Release-only tooling comes from the reviewed workflow commit, not a branch
    # tip. Older tags predate this helper. It is not Rust application source.
    provenance["release_helpers"] = {}
    for helper in RELEASE_HELPERS:
        content = subprocess.check_output(["git", "show", f"{workflow_sha}:{helper}"])
        Path(helper).write_bytes(content)
        Path(helper).chmod(0o755)
        provenance["release_helpers"][helper] = hashlib.sha256(content).hexdigest()
    Path(os.environ.get("RELEASE_PROVENANCE_PATH", "release-provenance.json")).write_text(json.dumps(provenance, indent=2) + "\n")
    return provenance


def check_public_state(tag):
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag):
        raise ValueError("Invalid release tag")
    draft = subprocess.check_output(["gh", "release", "view", tag, "--json", "isDraft", "--jq", ".isDraft"]).decode().strip()
    if draft != "true":
        raise ValueError("Re-running a release requires an existing draft release")
    releases = json.loads(subprocess.check_output(["gh", "release", "list", "--exclude-drafts", "--limit", "1000", "--json", "tagName"]))
    def version(name):
        return tuple(map(int, name[1:].split(".")))
    if any(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", item["tagName"]) and version(item["tagName"]) > version(tag) for item in releases):
        raise ValueError("Refusing a release that would downgrade a newer public release")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("workflow_sha", nargs="?")
    parser.add_argument("--check-public-state", action="store_true")
    args = parser.parse_args()
    if args.check_public_state:
        check_public_state(args.tag)
    elif args.workflow_sha is None:
        parser.error("workflow_sha is required to prepare release source")
    else:
        prepare(args.tag, args.workflow_sha)
