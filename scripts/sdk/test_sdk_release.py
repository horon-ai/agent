#!/usr/bin/env python3
"""Tests for the TypeScript SDK release helpers.

Every case is a pure function call or a temporary directory, so the rules
`publish-typescript-sdk.yml` relies on are proved without a runner, a registry
or a network. The failure cases matter most: a mismatched pin or a wrong
version must stop the workflow before `npm publish` is reached.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import sdk_release as helper

CARGO = """[package]
name = "horon-agent"
version = "0.1.0"
edition = "2021"

[workspace]
members = []
"""


def sdk_manifest(version: str = "0.1.0", pins: dict[str, str] | None = None) -> dict:
    return {
        "name": helper.SDK_NAME,
        "version": version,
        "optionalDependencies": pins
        if pins is not None
        else {name: version for name in helper.RUNTIME_NAMES.values()},
    }


class CargoVersionTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)

    def write(self, text: str) -> Path:
        path = self.root / "Cargo.toml"
        path.write_text(text, encoding="utf-8")
        return path

    def test_reads_the_package_version(self) -> None:
        self.assertEqual(helper.read_cargo_version(self.write(CARGO)), "0.1.0")

    def test_ignores_a_workspace_member_version(self) -> None:
        # A `[workspace.members]` table can carry its own version; only the
        # root `[package]` table names the release line.
        text = CARGO.rstrip() + '\n\n[profile.release]\nversion = "9.9.9"\n'
        self.assertEqual(helper.read_cargo_version(self.write(text)), "0.1.0")

    def test_a_manifest_without_a_package_table_fails(self) -> None:
        with self.assertRaises(ValueError):
            helper.read_cargo_version(self.write('[workspace]\nmembers = ["a"]\n'))

    def test_the_repository_manifest_is_parseable(self) -> None:
        # The real file, not a fixture: the command the workflow runs must work
        # on the file this repository actually has.
        self.assertRegex(helper.read_cargo_version(helper.CARGO_MANIFEST), r"^[0-9]+\.[0-9]+\.[0-9]+$")


class CheckCommittedTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        self.cargo = self.root / "Cargo.toml"
        self.cargo.write_text(CARGO, encoding="utf-8")
        self.sdk = self.root / "package.json"

    def write(self, manifest: dict) -> None:
        self.sdk.write_text(json.dumps(manifest), encoding="utf-8")

    def test_an_aligned_manifest_passes(self) -> None:
        self.write(sdk_manifest())
        self.assertEqual(helper.check_committed(self.sdk, self.cargo), [])

    def test_the_repository_tree_is_aligned(self) -> None:
        # The committed files, not a fixture: this is the check CI runs.
        self.assertEqual(helper.check_committed(helper.SDK_MANIFEST, helper.CARGO_MANIFEST), [])

    def test_a_drifting_version_is_named(self) -> None:
        # The pins stay on the Cargo version, so the failure names the SDK
        # version alone.
        self.write(sdk_manifest(version="1.2.0", pins={name: "0.1.0" for name in helper.RUNTIME_NAMES.values()}))
        self.assertEqual(
            helper.check_committed(self.sdk, self.cargo),
            ["sdk/typescript: version is '1.2.0', expected the Cargo version '0.1.0'"],
        )

    def test_a_drifting_pin_is_named(self) -> None:
        pins = {name: "0.1.0" for name in helper.RUNTIME_NAMES.values()}
        pins["@horon-ai/agent-win32-x64"] = "1.2.0"
        self.write(sdk_manifest(pins=pins))
        self.assertEqual(
            helper.check_committed(self.sdk, self.cargo),
            [
                "sdk/typescript: @horon-ai/agent-win32-x64 is pinned to '1.2.0', "
                "expected the Cargo version '0.1.0'"
            ],
        )

    def test_a_missing_pin_is_named(self) -> None:
        pins = {name: "0.1.0" for name in helper.RUNTIME_NAMES.values()}
        del pins["@horon-ai/agent-darwin-arm64"]
        self.write(sdk_manifest(pins=pins))
        self.assertEqual(
            helper.check_committed(self.sdk, self.cargo),
            ["sdk/typescript: optionalDependencies is missing @horon-ai/agent-darwin-arm64@0.1.0"],
        )

    def test_an_unexpected_pin_is_named(self) -> None:
        pins = {name: "0.1.0" for name in helper.RUNTIME_NAMES.values()}
        pins["@horon-ai/agent-freebsd-x64"] = "0.1.0"
        self.write(sdk_manifest(pins=pins))
        self.assertEqual(
            helper.check_committed(self.sdk, self.cargo),
            ["sdk/typescript: optionalDependencies has unexpected @horon-ai/agent-freebsd-x64"],
        )

    def test_a_wrong_name_is_named(self) -> None:
        manifest = sdk_manifest()
        manifest["name"] = "@horon-ai/horon-agent-sdk"
        self.write(manifest)
        self.assertIn(
            "sdk/typescript: name is '@horon-ai/horon-agent-sdk', expected '@horon-ai/agent-sdk'",
            helper.check_committed(self.sdk, self.cargo),
        )

    def test_unreadable_json_is_named(self) -> None:
        self.sdk.write_text("{", encoding="utf-8")
        problems = helper.check_committed(self.sdk, self.cargo)
        self.assertEqual(len(problems), 1)
        self.assertIn("cannot read the SDK manifest", problems[0])


class StampTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.sdk = Path(self._temp.name) / "package.json"
        self.sdk.write_text(json.dumps(sdk_manifest("0.1.0"), indent="\t") + "\n", encoding="utf-8")

    def test_stamp_moves_the_version_and_every_pin(self) -> None:
        helper.stamp(self.sdk, "0.1.6")
        manifest = json.loads(self.sdk.read_text(encoding="utf-8"))
        self.assertEqual(manifest["version"], "0.1.6")
        self.assertEqual(
            set(manifest["optionalDependencies"].values()),
            {"0.1.6"},
        )

    def test_stamp_keeps_the_tracked_shape(self) -> None:
        # The runner rewrites the checkout only; an unchanged version must be
        # byte-identical so `git status` stays clean on the dry-run path.
        before = self.sdk.read_text(encoding="utf-8")
        helper.stamp(self.sdk, "0.1.0")
        self.assertEqual(self.sdk.read_text(encoding="utf-8"), before)

    def test_stamp_refuses_a_prerelease_or_a_tag(self) -> None:
        for version in ("v0.1.0", "0.1", "latest", ""):
            with self.subTest(version=version), self.assertRaises(ValueError):
                helper.stamp(self.sdk, version)

    def test_stamp_leaves_other_fields_alone(self) -> None:
        manifest = json.loads(self.sdk.read_text(encoding="utf-8"))
        manifest["dependencies"] = {"ajv": "^8.20.0"}
        self.sdk.write_text(json.dumps(manifest, indent="\t") + "\n", encoding="utf-8")
        helper.stamp(self.sdk, "0.1.6")
        after = json.loads(self.sdk.read_text(encoding="utf-8"))
        self.assertEqual(after["dependencies"], {"ajv": "^8.20.0"})


class PublishOrderTests(unittest.TestCase):
    def test_the_sdk_is_published_last(self) -> None:
        order = helper.publish_order()
        self.assertEqual(order[-1], helper.SDK_TARGET)
        self.assertEqual(
            order[:-1],
            ["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64", "win32-arm64"],
        )
        self.assertEqual(len(order), 7)

    def test_the_names_match_the_publish_order(self) -> None:
        names = helper.package_names()
        self.assertEqual(len(names), 7)
        self.assertEqual(names[-1], helper.SDK_NAME)
        self.assertEqual(
            names[:-1],
            [f"@horon-ai/agent-{platform}" for platform in helper.publish_order()[:-1]],
        )


class FixtureAndTagTests(unittest.TestCase):
    def test_the_fixture_version_is_publishable(self) -> None:
        # npm 11 refuses a dry run of an already-published version. Every package
        # carries a 0.0.0 placeholder, so the fixture must be a different stable
        # number; a prerelease would additionally need an explicit `--tag`.
        self.assertNotEqual(helper.FIXTURE_VERSION, "0.0.0")
        self.assertRegex(helper.FIXTURE_VERSION, r"^[0-9]+\.[0-9]+\.[0-9]+$")

    def test_the_tag_rule_is_the_npm_one(self) -> None:
        self.assertEqual(helper.tag_version("v0.1.6"), "0.1.6")
        for tag in ("0.1.6", "v0.1", "v0.1.6-rc.1"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                helper.tag_version(tag)


class ModeTests(unittest.TestCase):
    def test_a_pull_request_never_publishes(self) -> None:
        self.assertEqual(helper.resolve_mode("pull_request", "publish", "true"), "dry-run")

    def test_the_variable_is_required_to_publish(self) -> None:
        for npm_publish in ("", "false", "TRUE ", "1", "typo"):
            with self.subTest(npm_publish=npm_publish):
                self.assertEqual(helper.resolve_mode("workflow_run", "publish", npm_publish), "dry-run")
        self.assertEqual(helper.resolve_mode("workflow_run", "publish", "true"), "publish")

    def test_a_dry_run_request_stays_a_dry_run(self) -> None:
        # A dispatch with dry_run=true must not publish just because the
        # variable is set.
        for requested in ("", "dry-run", "false"):
            with self.subTest(requested=requested):
                self.assertEqual(helper.resolve_mode("workflow_dispatch", requested, "true"), "dry-run")

    def test_only_the_release_path_asks_to_publish(self) -> None:
        self.assertEqual(helper.resolve_mode("workflow_run", "publish", "true"), "publish")
        self.assertEqual(helper.resolve_mode("workflow_dispatch", "publish", "true"), "publish")


class CheckPackagesTests(unittest.TestCase):
    VERSION = "0.1.6"

    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.directory = Path(self._temp.name)
        self.sdk = Path(self._temp.name) / "sdk-package.json"
        self.sdk.write_text(
            json.dumps(sdk_manifest(self.VERSION), indent="\t") + "\n", encoding="utf-8"
        )
        self.write_packages()

    def write_packages(self, version: str | None = None) -> None:
        version = version or self.VERSION
        for platform, (os_name, cpu) in helper.MANIFEST_PLATFORM.items():
            package_dir = self.directory / platform / "bin"
            package_dir.mkdir(parents=True)
            binary = "horon-agent.exe" if os_name == "win32" else "horon-agent"
            (package_dir / binary).write_text("", encoding="utf-8")
            manifest = {"name": helper.RUNTIME_NAMES[platform], "version": version, "os": [os_name], "cpu": [cpu]}
            (package_dir.parent / "package.json").write_text(json.dumps(manifest), encoding="utf-8")

    def test_the_seven_packages_agree(self) -> None:
        self.assertEqual(helper.check_packages(self.directory, self.VERSION, self.sdk), [])

    def test_a_runtime_version_is_named(self) -> None:
        manifest_path = self.directory / "darwin-arm64" / "package.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["version"] = "0.1.5"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION, self.sdk),
            ["darwin-arm64: version is '0.1.5', expected '0.1.6'"],
        )

    def test_an_sdk_pin_is_named(self) -> None:
        pins = {name: self.VERSION for name in helper.RUNTIME_NAMES.values()}
        pins["@horon-ai/agent-linux-arm64"] = "0.1.5"
        self.sdk.write_text(json.dumps(sdk_manifest(self.VERSION, pins=pins)), encoding="utf-8")
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION, self.sdk),
            ["sdk/typescript: @horon-ai/agent-linux-arm64 is pinned to '0.1.5', expected '0.1.6'"],
        )

    def test_an_sdk_version_is_named(self) -> None:
        # The dry run must fail on the SDK itself, not only on the runtimes: the
        # SDK is what a consumer installs.
        self.sdk.write_text(json.dumps(sdk_manifest("1.2.0")), encoding="utf-8")
        problems = helper.check_packages(self.directory, self.VERSION, self.sdk)
        self.assertIn("sdk/typescript: version is '1.2.0', expected '0.1.6'", problems)
        self.assertEqual(len(problems), 7)  # the six pins follow the manifest version

    def test_a_missing_package_is_named(self) -> None:
        for path in sorted((self.directory / "linux-arm64").rglob("*"), reverse=True):
            path.unlink() if path.is_file() else path.rmdir()
        (self.directory / "linux-arm64").rmdir()
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION, self.sdk),
            ["linux-arm64: missing package.json"],
        )

    def test_a_missing_binary_is_named(self) -> None:
        (self.directory / "win32-x64" / "bin" / "horon-agent.exe").unlink()
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION, self.sdk),
            ["win32-x64: bin/horon-agent.exe is missing"],
        )

    def test_an_install_script_is_named(self) -> None:
        manifest_path = self.directory / "win32-arm64" / "package.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["scripts"] = {"postinstall": "node install.js"}
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION, self.sdk),
            ["win32-arm64: declares a postinstall script"],
        )

    def test_a_runtime_dependency_is_named(self) -> None:
        manifest_path = self.directory / "linux-x64" / "package.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["dependencies"] = {"left-pad": "1.0.0"}
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION, self.sdk),
            ["linux-x64: declares dependencies"],
        )

    def test_an_unexpected_directory_is_named(self) -> None:
        # A seventh directory would be published or silently ignored depending
        # on the loop; naming it keeps the set exactly seven.
        (self.directory / "freebsd-x64").mkdir()
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION, self.sdk),
            ["freebsd-x64: unexpected package directory"],
        )


if __name__ == "__main__":
    unittest.main()
