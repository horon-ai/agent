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

# A release tag is `vMAJOR.MINOR.PATCH` with an optional semver prerelease
# suffix (`v0.2.0-alpha.1`). Build metadata (`v0.1.0+build`) is refused: the tag
# names one release, and one release names one artifact set.
RELEASE_TAG = re.compile(
    r"v(?P<major>[0-9]+)\.(?P<minor>[0-9]+)\.(?P<patch>[0-9]+)"
    r"(?:-(?P<prerelease>[0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*))?"
)

CORE_VERSION = re.compile(r"([0-9]+)\.([0-9]+)\.([0-9]+)")


def git(*args):
    return subprocess.check_output(["git", *args]).decode().strip()


def parse_release_tag(tag):
    """`(major, minor, patch, prerelease)` for a release tag, or None.

    `prerelease` is the suffix without its `-`, or None for a stable tag.
    """
    match = RELEASE_TAG.fullmatch(tag)
    if match is None:
        return None
    return (int(match["major"]), int(match["minor"]), int(match["patch"]), match["prerelease"])


def semver_key(tag):
    """Total order over release tags, per semver §11.

    A prerelease sorts below its own release, numeric identifiers compare
    numerically, alphanumeric ones by ASCII, and a shorter identifier list is
    lower when it is a prefix of the longer one: `v0.2.0-alpha` <
    `v0.2.0-alpha.1` < `v0.2.0-alpha.2` < `v0.2.0`. Raises ValueError for a name
    that is not a release tag.
    """
    parsed = parse_release_tag(tag)
    if parsed is None:
        raise ValueError(f"Not a release tag: {tag!r}")
    major, minor, patch, prerelease = parsed
    if prerelease is None:
        return ((major, minor, patch), 1, ())
    identifiers = tuple(
        (0, int(identifier), "") if identifier.isdigit() else (1, 0, identifier)
        for identifier in prerelease.split(".")
    )
    return ((major, minor, patch), 0, identifiers)


def tag_fits_version(tag, version):
    """True when `tag` may release the source whose Cargo version is `version`.

    A stable tag keeps the line rule: the Cargo version names the release line
    and its lowest patch; later patches on the same MAJOR.MINOR line are
    numbered by tags (see next_version.py), so the tag must share MAJOR.MINOR
    and not go below the Cargo patch.

    A prerelease may additionally open the next minor line of the same major at
    patch 0 (`v0.2.0-alpha` over Cargo 0.1.x), because the prerelease announces
    the line that Cargo.toml has not taken yet. Any other minor or major
    change still needs the root version bumped first.
    """
    parsed = parse_release_tag(tag)
    have = CORE_VERSION.fullmatch(version)
    if parsed is None or have is None:
        return False
    major, minor, patch, prerelease = parsed
    cargo = tuple(map(int, have.groups()))
    if prerelease is None:
        return (major, minor) == cargo[:2] and patch >= cargo[2]
    if (major, minor) == cargo[:2]:
        return patch >= cargo[2]
    return major == cargo[0] and minor == cargo[1] + 1 and patch == 0


def prepare(tag, workflow_sha):
    if parse_release_tag(tag) is None:
        raise ValueError("Release tag must be vMAJOR.MINOR.PATCH[-PRERELEASE]")
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
    if parse_release_tag(tag) is None:
        raise ValueError("Invalid release tag")
    draft = subprocess.check_output(["gh", "release", "view", tag, "--json", "isDraft", "--jq", ".isDraft"]).decode().strip()
    if draft != "true":
        raise ValueError("Re-running a release requires an existing draft release")
    releases = json.loads(subprocess.check_output(["gh", "release", "list", "--exclude-drafts", "--limit", "1000", "--json", "tagName"]))
    # Prereleases are public releases in this list, so the guard has to order
    # them: an older alpha that crawled in after a newer release is a downgrade.
    # A stable tag is compared with stable releases only, so an alpha that
    # opened the next line (`v0.2.0-alpha`) never blocks a later patch of the
    # current line (`v0.1.25`); `Latest` only ever moves between stable tags.
    stable = parse_release_tag(tag)[3] is None
    published = [semver_key(item["tagName"]) for item in releases
                 if (parsed := parse_release_tag(item["tagName"])) is not None
                 and (not stable or parsed[3] is None)]
    if any(key > semver_key(tag) for key in published):
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
