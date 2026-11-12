#!/usr/bin/env python3
"""Release-time checks for the TypeScript SDK publish workflow.

`publish-typescript-sdk.yml` publishes seven packages at one version: the SDK
plus the six platform runtime packages `scripts/sdk/prepare_runtime_packages.sh`
generates from it. npm answers none of the questions that makes necessary, so
each rule is a pure function here and is proved without a runner, a registry or
a network:

  * the committed SDK manifest agrees with the root `Cargo.toml` release line,
  * a release version can be stamped into the manifest in the runner checkout,
  * all seven prepared packages carry exactly the release version,
  * the six runtime packages are published before the SDK,
  * the `NPM_PUBLISH` master switch, not a dispatch input, decides publishing.

The tag parsing, fixture version and platform table are imported from
`scripts/npm/release_npm_version.py` rather than copied: both workflows publish
the same release line, and two copies of the tag rule could drift apart.

Commands, each one called by the workflow, `ci.yml` or
`scripts/sdk/publish_preflight.sh`:

  tag-version TAG             print the SDK version a release tag names
  check-committed             assert the committed SDK version and pins agree
  stamp VERSION               write VERSION into the SDK manifest (runner only)
  check-packages DIR VERSION  assert the seven prepared packages agree
  publish-order               print the publish targets, runtime packages first
  package-names               print the seven npm package names
  fixture-version             print the version the pull-request run uses
  mode EVENT REQUESTED NPM_PUBLISH
                              print the mode the run is allowed to take

The workflow's `plan` job resolves the version and the mode by calling this
file, so the rule the unit tests cover is the rule a release runs.
Exit status is 0 when the answer is good, 1 when a check fails (every failure
is named) and 2 for a usage error.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "scripts" / "npm"))

from release_npm_version import (  # noqa: E402  (path-dependent import)
    FIXTURE_VERSION,
    MANIFEST_PLATFORM,
    SEMVER,
    tag_version,
)

SDK_NAME = "@horon-ai/agent-sdk"
SDK_DIR = "packages/sdk/typescript"
SDK_MANIFEST = REPO_ROOT / SDK_DIR / "package.json"
CARGO_MANIFEST = REPO_ROOT / "Cargo.toml"

# The prepared platform package directory that carries the SDK itself. It is
# published from the checkout rather than from a generated directory: `prepack`
# builds `dist/` and only `files` lets it into the tarball.
SDK_TARGET = "sdk"

RUNTIME_NAMES = {platform: f"@horon-ai/agent-{platform}" for platform in MANIFEST_PLATFORM}


def read_cargo_version(path: Path) -> str:
    """Read `[package].version` from the root Cargo.toml text."""
    in_package = False
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        match = re.match(r'version\s*=\s*"([^"]+)"', stripped)
        if in_package and match:
            return match.group(1)
    raise ValueError(f"{path} has no [package].version")


def check_committed(manifest_path: Path, cargo_path: Path) -> list[str]:
    """Return every way the committed SDK manifest leaves the release line.

    The SDK is released at the tag version, which is the root Cargo version's
    line, so a manifest that drifts from `Cargo.toml` would publish an SDK that
    does not match the runtime beside it. CI runs this on every change.
    """
    problems: list[str] = []
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return [f"{manifest_path}: cannot read the SDK manifest ({error})"]
    try:
        cargo_version = read_cargo_version(cargo_path)
    except (OSError, ValueError) as error:
        return [f"{cargo_path}: cannot read the release version ({error})"]

    if manifest.get("name") != SDK_NAME:
        problems.append(f"{SDK_DIR}: name is {manifest.get('name')!r}, expected {SDK_NAME!r}")
    if manifest.get("version") != cargo_version:
        problems.append(
            f"{SDK_DIR}: version is {manifest.get('version')!r}, expected the Cargo version {cargo_version!r}"
        )
    pinned = manifest.get("optionalDependencies") or {}
    for platform, name in RUNTIME_NAMES.items():
        if name not in pinned:
            problems.append(f"{SDK_DIR}: optionalDependencies is missing {name}@{cargo_version}")
        elif pinned[name] != cargo_version:
            problems.append(
                f"{SDK_DIR}: {name} is pinned to {pinned[name]!r}, expected the Cargo version {cargo_version!r}"
            )
    for name in pinned:
        if name not in RUNTIME_NAMES.values():
            problems.append(f"{SDK_DIR}: optionalDependencies has unexpected {name}")
    return problems


def stamp(manifest_path: Path, version: str) -> None:
    """Write `version` into the SDK manifest and its six runtime pins.

    Only the runner checkout is ever stamped. The manifest is written back with
    tabs and a trailing newline, which is the shape the tracked file already has,
    so an unchanged version rewrites identical bytes.
    """
    if SEMVER.fullmatch(version) is None:
        raise ValueError(f"version is not MAJOR.MINOR.PATCH[-prerelease]: {version!r}")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["version"] = version
    pins = manifest.get("optionalDependencies") or {}
    # Existing pins keep their position so an unchanged version rewrites the
    # tracked file byte for byte; a name the manifest lacks is added.
    for name in RUNTIME_NAMES.values():
        pins[name] = version
    manifest["optionalDependencies"] = pins
    manifest_path.write_text(json.dumps(manifest, indent="\t") + "\n", encoding="utf-8")


def publish_order() -> list[str]:
    """Return the publish targets in order: the six runtime packages, then the SDK.

    npm resolves the SDK's optional dependencies by name, so an SDK that is
    visible before its platform packages offers an install that cannot launch.
    """
    return list(MANIFEST_PLATFORM) + [SDK_TARGET]


def package_names() -> list[str]:
    """Return the seven package names in publish order."""
    return [RUNTIME_NAMES[platform] for platform in MANIFEST_PLATFORM] + [SDK_NAME]


def resolve_mode(event: str, requested: str, npm_publish: str) -> str:
    """Return the mode this run may take.

    The repository variable is the owner's master switch: a dispatch can ask for
    `publish`, but anything other than `true` (unset, `false`, a typo) leaves it
    a dry run. A pull request can never publish, whatever the variable says.
    """
    if requested != "publish":
        return "dry-run"
    if event == "pull_request":
        return "dry-run"
    if (npm_publish or "").strip() != "true":
        return "dry-run"
    return "publish"


def check_packages(directory: Path, version: str, sdk_manifest: Path) -> list[str]:
    """Return every way the seven packages disagree with `version`."""
    problems: list[str] = []
    if not directory.is_dir():
        return [f"{directory}: package directory does not exist"]
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
        expected_name = RUNTIME_NAMES[platform]
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
        binary = "horon-agent.exe" if os_name == "win32" else "horon-agent"
        if not (package_dir / "bin" / binary).is_file():
            problems.append(f"{platform}: bin/{binary} is missing")
        # The licence travels with the binary it covers.
        if not (package_dir / "LICENSE").is_file():
            problems.append(f"{platform}: LICENSE is missing")

    for extra in sorted(path.name for path in directory.iterdir() if path.is_dir()):
        if extra not in MANIFEST_PLATFORM:
            problems.append(f"{extra}: unexpected package directory")

    try:
        manifest = json.loads(sdk_manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        problems.append(f"{SDK_DIR}: cannot read the SDK manifest ({error})")
        return problems
    if manifest.get("name") != SDK_NAME:
        problems.append(f"{SDK_DIR}: name is {manifest.get('name')!r}, expected {SDK_NAME!r}")
    if manifest.get("version") != version:
        problems.append(f"{SDK_DIR}: version is {manifest.get('version')!r}, expected {version!r}")
    pinned = manifest.get("optionalDependencies") or {}
    for platform, name in RUNTIME_NAMES.items():
        if name not in pinned:
            problems.append(f"{SDK_DIR}: optionalDependencies is missing {name}")
        elif pinned[name] != version:
            problems.append(f"{SDK_DIR}: {name} is pinned to {pinned[name]!r}, expected {version!r}")
    for name in pinned:
        if name not in RUNTIME_NAMES.values():
            problems.append(f"{SDK_DIR}: optionalDependencies has unexpected {name}")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = parser.add_subparsers(dest="command", required=True)

    tag = commands.add_parser("tag-version", help="print the SDK version a release tag names")
    tag.add_argument("tag")

    committed = commands.add_parser("check-committed", help="assert the committed SDK manifest agrees")
    committed.add_argument("--sdk", type=Path, default=SDK_MANIFEST)
    committed.add_argument("--cargo", type=Path, default=CARGO_MANIFEST)

    stamp_command = commands.add_parser("stamp", help="write a release version into the SDK manifest")
    stamp_command.add_argument("version")
    stamp_command.add_argument("--sdk", type=Path, default=SDK_MANIFEST)

    packages = commands.add_parser("check-packages", help="assert the seven packages agree")
    packages.add_argument("directory", type=Path)
    packages.add_argument("version")
    packages.add_argument("--sdk", type=Path, default=SDK_MANIFEST)

    commands.add_parser("publish-order", help="print the publish targets in publishing order")
    commands.add_parser("package-names", help="print the seven package names")
    commands.add_parser("fixture-version", help="print the version the pull-request run uses")

    mode = commands.add_parser("mode", help="print the mode the run may take")
    mode.add_argument("event")
    mode.add_argument("requested")
    mode.add_argument("npm_publish")

    args = parser.parse_args(argv)

    if args.command == "tag-version":
        try:
            print(tag_version(args.tag))
        except ValueError as error:
            print(f"::error::{error}" if _in_actions() else error, file=sys.stderr)
            return 1
        return 0

    if args.command == "check-committed":
        problems = check_committed(args.sdk, args.cargo)
        if problems:
            for problem in problems:
                print(f"SDK version mismatch: {problem}", file=sys.stderr)
            return 1
        print("the committed SDK version and its six pins equal the Cargo version")
        return 0

    if args.command == "stamp":
        try:
            stamp(args.sdk, args.version)
        except (OSError, ValueError) as error:
            print(f"::error::{error}" if _in_actions() else error, file=sys.stderr)
            return 1
        print(f"stamped {args.sdk} at {args.version}")
        return 0

    if args.command == "check-packages":
        problems = check_packages(args.directory, args.version, args.sdk)
        if problems:
            for problem in problems:
                print(f"package mismatch: {problem}", file=sys.stderr)
            return 1
        print(f"7 packages agree on version {args.version} in {args.directory}")
        return 0

    if args.command == "publish-order":
        for target in publish_order():
            print(target)
        return 0

    if args.command == "package-names":
        for name in package_names():
            print(name)
        return 0

    if args.command == "fixture-version":
        print(FIXTURE_VERSION)
        return 0

    print(resolve_mode(args.event, args.requested, args.npm_publish))
    return 0


def _in_actions() -> bool:
    import os

    return os.environ.get("GITHUB_ACTIONS") == "true"


if __name__ == "__main__":
    sys.exit(main())
