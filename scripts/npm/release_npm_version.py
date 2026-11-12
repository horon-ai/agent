#!/usr/bin/env python3
"""Release-time checks for the npm publish workflow.

`publish-npm.yml` needs three things npm does not answer itself: the version a
release tag names, validation of a manual dispatch before anything is built,
and a statement that the seven generated packages agree on one version. Each
rule is a pure function here so it can be proved without a runner, a registry
or a network.

Commands:

  tag-version TAG             print the npm version a release tag names
  fixture-version             print the unpublished version the pull-request run uses
  dispatch                    validate dispatch inputs, print source/version/mode
  missing-assets DIR          name every required release asset DIR lacks
  check-packages DIR VERSION  assert the seven prepared packages agree
  publish-order               print the package directories in publishing order

Exit status is 0 when the answer is good, 1 when a check fails (every failure
is named) and 2 for a usage error.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

# The npm package version is the release tag without its `v`. A prerelease or a
# build suffix is not published from a tag: the tag names a stable patch.
TAG = re.compile(r"v([0-9]+\.[0-9]+\.[0-9]+)")
SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?")

# Platform package -> release archive suffix, in publish order.
SUFFIXES = {
    "linux-x64": "linux-x86_64",
    "linux-arm64": "linux-aarch64",
    "darwin-x64": "macos-x86_64",
    "darwin-arm64": "macos-aarch64",
    "win32-x64": "windows-x86_64",
    "win32-arm64": "windows-aarch64",
}

# The npm `os`/`cpu` a platform package declares.
MANIFEST_PLATFORM = {
    "linux-x64": ("linux", "x64"),
    "linux-arm64": ("linux", "arm64"),
    "darwin-x64": ("darwin", "x64"),
    "darwin-arm64": ("darwin", "arm64"),
    "win32-x64": ("win32", "x64"),
    "win32-arm64": ("win32", "arm64"),
}

# Both stems for every platform, plus the checksum file the prepare script
# verifies before it unpacks anything.
ARCHIVES = tuple(
    f"{stem}-{suffix}.tar.gz" for suffix in SUFFIXES.values() for stem in ("horon-agent", "horon-tui")
)
REQUIRED = ARCHIVES + ("SHA256SUMS",)

WRAPPER = "@horon-ai/horon"
WRAPPER_DIR = "horon"

# A version that is not on the registry, so `npm publish --dry-run` on the
# pull-request path still exercises publishing: npm 11.5.1 refuses a dry run of
# a version that is already published, and every package already carries a
# 0.0.0 placeholder. A stable number rather than a prerelease, because npm
# requires an explicit `--tag` for a prerelease.
FIXTURE_VERSION = "0.0.1"


def tag_version(tag: str) -> str:
    """Return the npm version a `vMAJOR.MINOR.PATCH` release tag names."""
    match = TAG.fullmatch(tag.strip())
    if match is None:
        raise ValueError(f"not a vMAJOR.MINOR.PATCH release tag: {tag!r}")
    return match.group(1)


def dispatch_plan(
    release_tag: str, run_id: str, version: str, dry_run: bool
) -> dict[str, str]:
    """Validate a manual dispatch and say what it resolved to.

    Raises ValueError with one sentence naming the bad input; the workflow turns
    that into a failed step before anything is downloaded or published.
    """
    release_tag = (release_tag or "").strip()
    run_id = (run_id or "").strip()
    version = (version or "").strip()

    if bool(release_tag) == bool(run_id):
        raise ValueError("exactly one of release_tag and run_id is required")
    if run_id:
        if not run_id.isdigit():
            raise ValueError(f"run_id must be a numeric workflow run id: {run_id!r}")
        if not version:
            raise ValueError("version is required with run_id")
        resolved = version
    else:
        if version:
            raise ValueError("version is derived from release_tag; drop the version input")
        resolved = tag_version(release_tag)
    if SEMVER.fullmatch(resolved) is None:
        raise ValueError(f"version is not MAJOR.MINOR.PATCH[-prerelease]: {resolved!r}")
    if run_id and not dry_run:
        raise ValueError("run_id dispatches are dry-run only: a workflow artifact is not a published release")
    return {
        "source": "release_tag" if release_tag else "run_id",
        "version": resolved,
        "mode": "dry-run" if dry_run else "publish",
    }


def publish_order() -> list[str]:
    """Return the prepared package directories in publish order.

    The six platform packages must exist before the wrapper: npm resolves the
    wrapper's optional dependencies by name, and a consumer that installs while
    the wrapper is visible but a platform package is not gets an install that
    cannot launch.
    """
    return list(SUFFIXES) + [WRAPPER_DIR]


def missing_assets(directory: Path) -> list[str]:
    """Name every required asset the directory lacks or does not checksum."""
    missing = [name for name in REQUIRED if not (directory / name).is_file()]
    sums = directory / "SHA256SUMS"
    if sums.is_file():
        recorded = {line.split()[-1] for line in sums.read_text(encoding="utf-8").splitlines() if line.split()}
        for name in ARCHIVES:
            if name in missing:
                continue
            if (directory / name).is_file() and name not in recorded:
                missing.append(f"{name} (no SHA256SUMS line)")
    return missing


def check_packages(directory: Path, version: str) -> list[str]:
    """Return every way the prepared packages disagree with `version`."""
    problems: list[str] = []
    for platform, (os_name, cpu) in MANIFEST_PLATFORM.items():
        package_dir = directory / platform
        manifest_path = package_dir / "package.json"
        if not manifest_path.is_file():
            problems.append(f"{platform}: missing package.json")
            continue
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            problems.append(f"{platform}: package.json is not JSON ({error})")
            continue
        expected_name = f"{WRAPPER}-{platform}"
        if manifest.get("name") != expected_name:
            problems.append(f"{platform}: name is {manifest.get('name')!r}, expected {expected_name!r}")
        if manifest.get("version") != version:
            problems.append(f"{platform}: version is {manifest.get('version')!r}, expected {version!r}")
        if manifest.get("os") != [os_name]:
            problems.append(f"{platform}: os is {manifest.get('os')!r}, expected {[os_name]!r}")
        if manifest.get("cpu") != [cpu]:
            problems.append(f"{platform}: cpu is {manifest.get('cpu')!r}, expected {[cpu]!r}")
        if manifest.get("dependencies"):
            problems.append(f"{platform}: declares dependencies")
        scripts = manifest.get("scripts") or {}
        for hook in ("preinstall", "install", "postinstall"):
            if scripts.get(hook):
                problems.append(f"{platform}: declares a {hook} script")
        entry = "horon-agent.exe" if os_name == "win32" else "horon-agent"
        if not (package_dir / "bin" / entry).is_file():
            problems.append(f"{platform}: bin/{entry} is missing")
        companion = "horon-tui.exe" if os_name == "win32" else "horon-tui"
        if not (package_dir / "bin" / companion).is_file():
            problems.append(f"{platform}: bin/{companion} is missing")

    wrapper_dir = directory / WRAPPER_DIR
    manifest_path = wrapper_dir / "package.json"
    if not manifest_path.is_file():
        problems.append(f"{WRAPPER_DIR}: missing package.json")
        return problems
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        problems.append(f"{WRAPPER_DIR}: package.json is not JSON ({error})")
        return problems
    if manifest.get("name") != WRAPPER:
        problems.append(f"{WRAPPER_DIR}: name is {manifest.get('name')!r}, expected {WRAPPER!r}")
    if manifest.get("version") != version:
        problems.append(f"{WRAPPER_DIR}: version is {manifest.get('version')!r}, expected {version!r}")
    pinned = manifest.get("optionalDependencies") or {}
    expected_pins = {f"{WRAPPER}-{platform}": version for platform in MANIFEST_PLATFORM}
    if pinned != expected_pins:
        for name, expected in expected_pins.items():
            if name not in pinned:
                problems.append(f"{WRAPPER_DIR}: optionalDependencies is missing {name}")
            elif pinned[name] != expected:
                problems.append(f"{WRAPPER_DIR}: {name} is pinned to {pinned[name]!r}, expected {expected!r}")
        for name in pinned:
            if name not in expected_pins:
                problems.append(f"{WRAPPER_DIR}: optionalDependencies has unexpected {name}")
    if not (wrapper_dir / "bin" / "horon.js").is_file():
        problems.append(f"{WRAPPER_DIR}: bin/horon.js is missing")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    tag = commands.add_parser("tag-version", help="print the npm version a release tag names")
    tag.add_argument("tag")

    commands.add_parser("fixture-version", help="print the version the pull-request run publishes")

    dispatch = commands.add_parser("dispatch", help="validate a manual dispatch")
    dispatch.add_argument("--release-tag", default="")
    dispatch.add_argument("--run-id", default="")
    dispatch.add_argument("--version", default="")
    dispatch.add_argument("--dry-run", default="true")

    assets = commands.add_parser("missing-assets", help="name the release assets a directory lacks")
    assets.add_argument("directory", type=Path)

    packages = commands.add_parser("check-packages", help="assert the seven packages agree")
    packages.add_argument("directory", type=Path)
    packages.add_argument("version")

    commands.add_parser(
        "publish-order", help="print the package directories in publishing order"
    )

    args = parser.parse_args(argv)

    if args.command == "tag-version":
        try:
            print(tag_version(args.tag))
        except ValueError as error:
            print(f"::error::{error}" if _in_actions() else error, file=sys.stderr)
            return 1
        return 0

    if args.command == "fixture-version":
        print(FIXTURE_VERSION)
        return 0

    if args.command == "dispatch":
        dry_run = args.dry_run.strip().lower() not in ("false", "0", "")
        try:
            plan = dispatch_plan(args.release_tag, args.run_id, args.version, dry_run)
        except ValueError as error:
            print(f"::error::{error}" if _in_actions() else error, file=sys.stderr)
            return 1
        for key, value in plan.items():
            print(f"{key}={value}")
        return 0

    if args.command == "publish-order":
        for directory in publish_order():
            print(directory)
        return 0

    if args.command == "missing-assets":
        missing = missing_assets(args.directory)
        if missing:
            for name in missing:
                print(f"missing release asset: {name}", file=sys.stderr)
            return 1
        print(f"all {len(REQUIRED)} release assets present in {args.directory}")
        return 0

    problems = check_packages(args.directory, args.version)
    if problems:
        for problem in problems:
            print(f"package mismatch: {problem}", file=sys.stderr)
        return 1
    print(f"7 packages agree on version {args.version} in {args.directory}")
    return 0


def _in_actions() -> bool:
    import os

    return os.environ.get("GITHUB_ACTIONS") == "true"


if __name__ == "__main__":
    sys.exit(main())
