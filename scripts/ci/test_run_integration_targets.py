#!/usr/bin/env python3
"""Contract tests for the per-test `skipped` rows in the integration manifest.

The runner is a stub that prints its arguments, so these check only that a row's
`skipped` names reach the test binary as `--skip`, that a row without them adds
nothing, and that a failing target still fails the step.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/ci/run_integration_targets.py"


class IntegrationTargetRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.temp = Path(self._temp.name)
        self.artifacts = self.temp / "artifacts.jsonl"
        self.artifacts.write_text("", encoding="utf-8")

    def stub_runner(self, exit_code: int = 0) -> Path:
        """A Python stand-in for run_lib_tests.py, which the runner starts with
        `sys.executable`; it records the arguments it was given."""
        path = self.temp / "runner.py"
        path.write_text(
            "import sys\n"
            "from pathlib import Path\n"
            f"Path({str(self.temp / 'args.txt')!r}).write_text(\n"
            "    '\\n'.join(sys.argv[1:]) + '\\n', encoding='utf-8')\n"
            f"raise SystemExit({exit_code})\n",
            encoding="utf-8",
        )
        return path

    def write_manifest(self, rows: list[dict]) -> Path:
        path = self.temp / "targets.json"
        path.write_text(json.dumps({"version": 1, "targets": rows}) + "\n", encoding="utf-8")
        return path

    def run_helper(self, manifest: Path, runner: Path) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                sys.executable,
                str(HELPER),
                "--artifacts", str(self.artifacts),
                "--manifest", str(manifest),
                "--runner", str(runner),
            ],
            capture_output=True,
            text=True,
        )

    def arguments(self) -> list[str]:
        return (self.temp / "args.txt").read_text(encoding="utf-8").splitlines()

    def test_a_skip_reaches_the_test_binary(self) -> None:
        manifest = self.write_manifest([
            {
                "package": "horon-agent",
                "target": "acp_infra",
                "min_passed": 30,
                "skipped": ["v5_real_daemon_cancel_note_then_prompt"],
                "skip_reason": "the outcome follows runner load",
            }
        ])

        result = self.run_helper(manifest, self.stub_runner())

        self.assertEqual(result.returncode, 0, result.stderr)
        arguments = self.arguments()
        self.assertIn("--skip", arguments)
        self.assertEqual(
            arguments[arguments.index("--skip") + 1],
            "v5_real_daemon_cancel_note_then_prompt",
        )
        self.assertLess(
            arguments.index("--"),
            arguments.index("--skip"),
            "the skip must come after the `--` that ends the runner's own flags",
        )

    def test_a_row_never_introduces_a_skip_flag_on_its_own(self) -> None:
        manifest = self.write_manifest([
            {"package": "horon-agent", "target": "e2e", "min_passed": 76}
        ])

        result = self.run_helper(manifest, self.stub_runner())

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip", self.arguments())

    def test_a_failing_target_still_fails_the_step(self) -> None:
        manifest = self.write_manifest([
            {"package": "horon-agent", "target": "e2e", "min_passed": 76}
        ])

        result = self.run_helper(manifest, self.stub_runner(exit_code=1))

        self.assertEqual(result.returncode, 1)
        self.assertIn("1 of 1 integration targets failed", result.stderr)


if __name__ == "__main__":
    unittest.main()
