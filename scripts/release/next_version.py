#!/usr/bin/env python3
"""Decide the release tag for a commit that passed CI on main.

The root `Cargo.toml` `[package].version` names the release line
(MAJOR.MINOR) and the lowest patch it may publish. Patch numbers come from
tags, not from commits: the next release is the highest existing tag on the
same MAJOR.MINOR line plus one patch, or the Cargo version itself when the line
has no tag yet. No version-bump commit is written, so a release can never
trigger another release.

Prints `tag=<vX.Y.Z>` or `skip=<reason>` lines in `$GITHUB_OUTPUT` format and
exits nonzero only when the inputs are inconsistent (for example a Cargo
version line older than the newest release).
"""
import argparse
import re
import subprocess
import sys

SEMVER = re.compile(r"v?([0-9]+)\.([0-9]+)\.([0-9]+)")


def parse(value):
    match = SEMVER.fullmatch(value.strip())
    if match is None:
        raise ValueError(f"not a MAJOR.MINOR.PATCH version: {value!r}")
    return tuple(int(part) for part in match.groups())


def cargo_version(manifest):
    """Read `[package].version` from the text of the root Cargo.toml."""
    in_package = False
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        match = re.match(r'version\s*=\s*"([^"]+)"', stripped)
        if in_package and match:
            return parse(match.group(1))
    raise ValueError("root Cargo.toml has no [package].version")


def decide(cargo, tags, head, is_ancestor):
    """Return ("tag", "vX.Y.Z") or ("skip", reason).

    `tags` maps release tag names to the commit they point at; names that are
    not vMAJOR.MINOR.PATCH are ignored. `is_ancestor(a, b)` is true when commit
    `a` is reachable from commit `b` (equal commits included).
    """
    releases = {}
    for name, commit in tags.items():
        if re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", name):
            releases[parse(name)] = commit
    for version, commit in releases.items():
        if commit == head:
            return "skip", f"{head} is already released as v{'.'.join(map(str, version))}"
    if releases:
        latest = max(releases)
        if is_ancestor(head, releases[latest]):
            return "skip", f"{head} is older than the latest release v{'.'.join(map(str, latest))}"
    line = [version for version in releases if version[:2] == cargo[:2]]
    patch = max(cargo[2], max(version[2] for version in line) + 1) if line else cargo[2]
    candidate = (cargo[0], cargo[1], patch)
    if releases and candidate <= max(releases):
        newest = "v" + ".".join(map(str, max(releases)))
        raise ValueError(
            f"Cargo.toml version line {cargo[0]}.{cargo[1]} is older than the latest release {newest}; "
            "raise MAJOR or MINOR in Cargo.toml")
    return "tag", "v" + ".".join(map(str, candidate))


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sha", help="full commit SHA that passed CI")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.sha):
        parser.error("sha must be a full lowercase commit SHA")
    cargo = cargo_version(git("show", f"{args.sha}:Cargo.toml"))
    tags = {}
    for line in git("for-each-ref", "--format=%(refname:short) %(*objectname) %(objectname)", "refs/tags").splitlines():
        name, *objects = line.split()
        tags[name] = objects[0] if len(objects) == 2 else objects[-1]

    def is_ancestor(commit, descendant):
        return subprocess.run(["git", "merge-base", "--is-ancestor", commit, descendant]).returncode == 0

    try:
        kind, value = decide(cargo, tags, args.sha, is_ancestor)
    except ValueError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    print(f"{kind}={value}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
