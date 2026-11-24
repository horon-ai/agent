#!/usr/bin/env python3
"""Contract tests for scripts/ci/workspace_impact.py."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import workspace_impact as impact  # noqa: E402


class WorkspaceImpactTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.metadata = impact.load_workspace_metadata()

    def run_pr(self, *files: str, runner_os: str = "Linux", labels=None) -> dict:
        return impact.compute_impact(
            event="pull_request",
            files=list(files),
            runner_os=runner_os,
            labels=labels,
            metadata=self.metadata,
        )

    def test_tui_only_change_selects_only_horon_tui(self) -> None:
        res = self.run_pr("crates/tui/src/lib.rs")
        self.assertTrue(res["scoped"])
        self.assertEqual(res["packages"], ["horon-tui"])
        self.assertFalse(res["run_package_example"])

    def test_extension_sdk_change_selects_extension_sdk_and_package_example(self) -> None:
        res = self.run_pr("crates/extension-sdk/src/lib.rs")
        self.assertTrue(res["scoped"])
        self.assertEqual(res["packages"], ["horon-agent-extension-sdk"])
        self.assertIn("horon-package-example", res["impacted"])
        self.assertTrue(res["run_package_example"])

    def test_shared_platform_linux_unix_propagates_to_platform_macos_and_dependents(self) -> None:
        res = self.run_pr("crates/platform-linux/src/unix.rs")
        self.assertTrue(res["scoped"])
        self.assertIn("horon-agent-platform-macos", res["impacted"])
        self.assertIn("horon-agent-platform", res["packages"])
        self.assertIn("horon-agent-base", res["packages"])
        self.assertIn("horon-agent", res["packages"])
        self.assertNotIn("horon-agent-token-estimate", res["packages"])

    def test_hidden_couplings_docs_sdk_parity_and_acp_credentials(self) -> None:
        doc_res = self.run_pr("docs/runtime/TOOLS.md")
        self.assertTrue(doc_res["scoped"])
        self.assertIn("horon-agent-app-core", doc_res["packages"])
        self.assertIn("horon-agent", doc_res["packages"])

        ko_doc = self.run_pr("docs/runtime/TOOLS.ko.md")
        self.assertTrue(ko_doc["scoped"])
        self.assertEqual(ko_doc["packages"], [])

        ts_sdk = self.run_pr("packages/sdk/typescript/src/client.ts")
        self.assertTrue(ts_sdk["scoped"])
        self.assertIn("horon-agent-sdk", ts_sdk["packages"])
        self.assertIn("horon-tui", ts_sdk["packages"])
        self.assertIn("horon-agent", ts_sdk["packages"])

        macos_inc = self.run_pr("crates/platform-macos/src/lib.rs")
        self.assertTrue(macos_inc["scoped"])
        self.assertIn("horon-agent", macos_inc["packages"])

    def test_full_fallback_on_shared_files_push_labels_or_metadata_failure(self) -> None:
        full_linux = impact.full_packages_for_os("Linux")
        full_macos = impact.full_packages_for_os("macOS")
        self.assertEqual(len(full_linux), 26)
        self.assertEqual(len(full_macos), 25)

        for path in (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "examples/packages/example/src/main.rs",
            "scripts/ci/run_lib_tests.py",
            "tests/support/acp_credentials.rs",
        ):
            res = self.run_pr(path)
            if path == "tests/support/acp_credentials.rs":
                # Root `tests/` maps directly to horon-agent.
                self.assertIn("horon-agent", res["packages"])
                continue
            self.assertFalse(res["scoped"], path)
            self.assertEqual(res["packages"], full_linux, path)
            self.assertTrue(res["run_package_example"], path)

        for reason, res in (
            ("empty", self.run_pr()),
            ("none", impact.compute_impact(event="pull_request", files=None, metadata=self.metadata)),
            ("push", impact.compute_impact(event="push", files=["crates/tui/src/lib.rs"], metadata=self.metadata)),
            ("label", self.run_pr("crates/tui/src/lib.rs", labels={"ci:all-os"})),
            ("macos", impact.compute_impact(event="push", files=["crates/tui/src/lib.rs"], runner_os="macOS", metadata=self.metadata)),
        ):
            self.assertFalse(res["scoped"], reason)
            expected = full_macos if reason == "macos" else full_linux
            self.assertEqual(res["packages"], expected, reason)

        with mock.patch.object(impact, "load_workspace_metadata", side_effect=OSError("no cargo")):
            fallback = impact.compute_impact(event="pull_request", files=["crates/tui/src/lib.rs"])
        self.assertFalse(fallback["scoped"])
        self.assertEqual(fallback["packages"], full_linux)

    def test_cli_env_output(self) -> None:
        with tempfile.NamedTemporaryFile("w", suffix=".txt", encoding="utf-8") as files_txt:
            files_txt.write("crates/tui/src/lib.rs\n")
            files_txt.flush()
            out = subprocess.check_output(
                [
                    sys.executable,
                    str(Path(__file__).resolve().with_name("workspace_impact.py")),
                    "--event", "pull_request",
                    "--files-from", files_txt.name,
                    "--runner-os", "Linux",
                    "--format", "env",
                ],
                text=True,
            )
        self.assertIn("SCOPED=true", out)
        self.assertIn("PACKAGES_FLAGS=-p horon-tui", out)
        self.assertIn("SCOPED_PACKAGES_CSV=horon-tui", out)
        self.assertIn("RUN_PACKAGE_EXAMPLE=false", out)


if __name__ == "__main__":
    unittest.main()
