#!/usr/bin/env python3
"""Tests for the npm release helpers.

Every case is a pure function call or a temporary directory, so the rules
`publish-npm.yml` relies on are proved without a runner, a registry or a
network. The tests also pin the fixture version away from `0.0.0` on purpose:
npm refuses a dry run of a version that is already published.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import release_npm_version as helper


class TagVersionTests(unittest.TestCase):
    def test_strips_the_leading_v(self) -> None:
        self.assertEqual(helper.tag_version("v1.2.3"), "1.2.3")
        self.assertEqual(helper.tag_version("v0.14.6"), "0.14.6")
        self.assertEqual(helper.tag_version(" v10.0.1 "), "10.0.1")

    def test_rejects_a_version_without_the_v(self) -> None:
        with self.assertRaises(ValueError):
            helper.tag_version("1.2.3")

    def test_rejects_a_partial_version(self) -> None:
        for tag in ("v1.2", "v1", "v1.2.x", "release-1.2.3"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                helper.tag_version(tag)

    def test_rejects_a_prerelease_tag(self) -> None:
        # A stable npm version comes from a stable patch tag; a prerelease tag
        # would publish a different version than the release names.
        for tag in ("v1.2.3-rc.1", "v1.2.3+build.5"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                helper.tag_version(tag)


class FixtureVersionTests(unittest.TestCase):
    def test_fixture_version_is_not_the_placeholder(self) -> None:
        # `npm publish --dry-run` fails for 0.0.0 because every package already
        # has that version on the registry, so the fixture must differ from it.
        self.assertNotEqual(helper.FIXTURE_VERSION, "0.0.0")
        # Stable, not a prerelease: npm demands an explicit `--tag` for those
        # and would otherwise fail the run for a reason unrelated to packaging.
        self.assertRegex(helper.FIXTURE_VERSION, r"^[0-9]+\.[0-9]+\.[0-9]+$")


class DispatchPlanTests(unittest.TestCase):
    def test_release_tag_derives_the_version(self) -> None:
        self.assertEqual(
            helper.dispatch_plan("v1.2.3", "", "", True),
            {"source": "release_tag", "version": "1.2.3", "mode": "dry-run"},
        )
        self.assertEqual(
            helper.dispatch_plan("v1.2.3", "", "", False),
            {"source": "release_tag", "version": "1.2.3", "mode": "publish"},
        )

    def test_run_id_requires_a_version(self) -> None:
        self.assertEqual(
            helper.dispatch_plan("", "4815162", "1.2.3", True),
            {"source": "run_id", "version": "1.2.3", "mode": "dry-run"},
        )
        with self.assertRaises(ValueError) as caught:
            helper.dispatch_plan("", "4815162", "", True)
        self.assertIn("version is required with run_id", str(caught.exception))

    def test_run_id_must_be_numeric(self) -> None:
        with self.assertRaises(ValueError) as caught:
            helper.dispatch_plan("", "run-4815162", "1.2.3", True)
        self.assertIn("numeric workflow run id", str(caught.exception))

    def test_run_id_can_never_publish(self) -> None:
        # A workflow artifact is not a public release: its provenance would name
        # a run, not the tag the release published.
        with self.assertRaises(ValueError) as caught:
            helper.dispatch_plan("", "4815162", "1.2.3", False)
        self.assertIn("dry-run only", str(caught.exception))

    def test_release_tag_and_run_id_are_exclusive(self) -> None:
        for tag, run_id in (("v1.2.3", "4815162"), ("", "")):
            with self.subTest(tag=tag, run_id=run_id), self.assertRaises(ValueError) as caught:
                helper.dispatch_plan(tag, run_id, "", True)
            self.assertIn("exactly one of release_tag and run_id", str(caught.exception))

    def test_version_with_a_release_tag_is_refused(self) -> None:
        with self.assertRaises(ValueError) as caught:
            helper.dispatch_plan("v1.2.3", "", "1.2.4", True)
        self.assertIn("derived from release_tag", str(caught.exception))

    def test_version_must_be_semver(self) -> None:
        for version in ("1.2", "v1.2.3", "latest", ""):
            with self.subTest(version=version), self.assertRaises(ValueError):
                helper.dispatch_plan("", "4815162", version, True)


class MissingAssetsTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.directory = Path(self._temp.name)

    def write_all_assets(self) -> None:
        for name in helper.ARCHIVES:
            (self.directory / name).write_bytes(b"archive")
        sums = "\n".join(f"{'0' * 64}  {name}" for name in helper.ARCHIVES)
        (self.directory / "SHA256SUMS").write_text(sums + "\n", encoding="utf-8")

    def test_every_asset_present(self) -> None:
        self.write_all_assets()
        self.assertEqual(helper.missing_assets(self.directory), [])
        self.assertEqual(len(helper.REQUIRED), 13)

    def test_a_missing_archive_is_named(self) -> None:
        self.write_all_assets()
        (self.directory / "horon-tui-windows-aarch64.tar.gz").unlink()
        (self.directory / "horon-agent-macos-x86_64.tar.gz").unlink()
        self.assertEqual(
            helper.missing_assets(self.directory),
            ["horon-agent-macos-x86_64.tar.gz", "horon-tui-windows-aarch64.tar.gz"],
        )

    def test_a_missing_checksum_line_is_named(self) -> None:
        self.write_all_assets()
        sums = (self.directory / "SHA256SUMS").read_text(encoding="utf-8")
        (self.directory / "SHA256SUMS").write_text(
            sums.replace("  horon-tui-linux-aarch64.tar.gz\n", ""), encoding="utf-8"
        )
        self.assertEqual(
            helper.missing_assets(self.directory),
            ["horon-tui-linux-aarch64.tar.gz (no SHA256SUMS line)"],
        )

    def test_a_missing_checksum_file_is_named(self) -> None:
        self.write_all_assets()
        (self.directory / "SHA256SUMS").unlink()
        self.assertEqual(helper.missing_assets(self.directory), ["SHA256SUMS"])


class PublishOrderTests(unittest.TestCase):
    def test_the_wrapper_is_published_last(self) -> None:
        # The wrapper's optional dependencies resolve by name, so publishing it
        # before the platform packages would offer an install that cannot run.
        order = helper.publish_order()
        self.assertEqual(order[-1], "horon")
        self.assertEqual(
            order[:-1],
            [
                "linux-x64",
                "linux-arm64",
                "darwin-x64",
                "darwin-arm64",
                "win32-x64",
                "win32-arm64",
            ],
        )
        self.assertEqual(len(order), 7)


class CheckPackagesTests(unittest.TestCase):
    VERSION = "1.2.3"

    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.directory = Path(self._temp.name)
        self.write_packages()

    def write_packages(self, version: str | None = None) -> None:
        version = version or self.VERSION
        for platform, (os_name, cpu) in helper.MANIFEST_PLATFORM.items():
            package_dir = self.directory / platform / "bin"
            package_dir.mkdir(parents=True)
            entry = "horon-agent.exe" if os_name == "win32" else "horon-agent"
            companion = "horon-tui.exe" if os_name == "win32" else "horon-tui"
            (package_dir / entry).write_text("", encoding="utf-8")
            (package_dir / companion).write_text("", encoding="utf-8")
            (package_dir.parent / "LICENSE").write_text("MIT License\n", encoding="utf-8")
            manifest = {
                "name": f"{helper.WRAPPER}-{platform}",
                "version": version,
                "os": [os_name],
                "cpu": [cpu],
            }
            (package_dir.parent / "package.json").write_text(json.dumps(manifest), encoding="utf-8")
        wrapper_dir = self.directory / helper.WRAPPER_DIR
        (wrapper_dir / "bin").mkdir(parents=True)
        (wrapper_dir / "bin" / "horon.js").write_text("", encoding="utf-8")
        (wrapper_dir / "LICENSE").write_text("MIT License\n", encoding="utf-8")
        wrapper = {
            "name": helper.WRAPPER,
            "version": version,
            "optionalDependencies": {
                f"{helper.WRAPPER}-{platform}": version for platform in helper.MANIFEST_PLATFORM
            },
        }
        (wrapper_dir / "package.json").write_text(json.dumps(wrapper), encoding="utf-8")

    def test_the_seven_packages_agree(self) -> None:
        self.assertEqual(helper.check_packages(self.directory, self.VERSION), [])

    def test_a_platform_version_is_named(self) -> None:
        manifest_path = self.directory / "darwin-arm64" / "package.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["version"] = "1.2.2"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION),
            ["darwin-arm64: version is '1.2.2', expected '1.2.3'"],
        )

    def test_a_wrapper_pin_is_named(self) -> None:
        manifest_path = self.directory / helper.WRAPPER_DIR / "package.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["optionalDependencies"][f"{helper.WRAPPER}-win32-x64"] = "0.0.0"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION),
            ["horon: @horon-ai/horon-win32-x64 is pinned to '0.0.0', expected '1.2.3'"],
        )

    def test_a_missing_package_is_named(self) -> None:
        for path in sorted((self.directory / "linux-arm64").rglob("*"), reverse=True):
            path.unlink() if path.is_file() else path.rmdir()
        (self.directory / "linux-arm64").rmdir()
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION),
            ["linux-arm64: missing package.json"],
        )

    def test_a_missing_tui_binary_is_named(self) -> None:
        # The platform package must ship the terminal client beside the agent;
        # without it the wrapper launches an agent whose sibling is missing.
        (self.directory / "linux-x64" / "bin" / "horon-tui").unlink()
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION),
            ["linux-x64: bin/horon-tui is missing"],
        )

    def test_a_missing_license_is_named(self) -> None:
        # A package without the licence ships binaries without their terms.
        (self.directory / "linux-x64" / "LICENSE").unlink()
        (self.directory / helper.WRAPPER_DIR / "LICENSE").unlink()
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION),
            ["linux-x64: LICENSE is missing", "horon: LICENSE is missing"],
        )

    def test_an_install_script_is_named(self) -> None:
        manifest_path = self.directory / "win32-arm64" / "package.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["scripts"] = {"postinstall": "node install.js"}
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assertEqual(
            helper.check_packages(self.directory, self.VERSION),
            ["win32-arm64: declares a postinstall script"],
        )


if __name__ == "__main__":
    unittest.main()
