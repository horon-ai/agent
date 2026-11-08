#!/usr/bin/env python3

"""Contract tests for scripts/ci/run_lib_tests.py.

Every executable here is a shell stub: the helper only has to resolve a cargo
`compiler-artifact` message, run the reported path, and judge its output, so no
cargo build is needed.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/ci/run_lib_tests.py"


class RunLibTestsTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.temp = Path(self._temp.name)
        self.package_dir = self.temp / "crates" / "base"
        self.package_dir.mkdir(parents=True)

    def write_artifacts(self, entries: list[dict]) -> Path:
        path = self.temp / "test-artifacts.jsonl"
        with path.open("w", encoding="utf-8") as handle:
            # Cargo interleaves build-script and rendered diagnostic messages
            # with the artifacts; the helper must skip them.
            handle.write(json.dumps({"reason": "build-script-executed",
                                     "package_id": "ignored"}) + "\n")
            for entry in entries:
                handle.write(json.dumps({"reason": "compiler-artifact", **entry}) + "\n")
        return path

    def artifact(self, executable: Path, package: str = "horon-agent-base",
                 target: str = "horon_agent_base", kind: str = "lib",
                 version: str = "0.1.0") -> dict:
        return {
            "package_id": f"path+file://{self.temp}#{package}@{version}",
            "manifest_path": str(self.package_dir / "Cargo.toml"),
            "target": {"name": target, "kind": [kind]},
            "profile": {"test": True},
            "executable": str(executable),
        }

    def stub(self, body: str, name: str = "stub") -> Path:
        path = self.temp / name
        path.write_text("#!/bin/sh\n" + body, encoding="utf-8")
        path.chmod(0o755)
        return path

    def run_helper(self, artifacts: Path, *extra: str,
                   package: str = "horon-agent-base") -> subprocess.CompletedProcess:
        command = [sys.executable, str(HELPER),
                   "--artifacts", str(artifacts),
                   "--package", package,
                   "--timeout", "30", *extra]
        return subprocess.run(command, capture_output=True, text=True)

    def test_finds_the_executable_and_runs_it(self):
        executable = self.stub(
            'echo "running one test"\n'
            "echo 'test result: ok. 3 passed; 0 failed; 0 ignored'\n"
        )
        artifacts = self.write_artifacts([self.artifact(executable)])

        result = self.run_helper(artifacts, "--", "--nocapture")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("running one test", result.stdout)
        self.assertIn("test result: ok. 3 passed", result.stdout)

    def test_runs_with_the_package_directory_as_cwd(self):
        marker = self.temp / "cwd.txt"
        executable = self.stub(
            f'pwd > "{marker}"\n'
            "echo 'test result: ok. 1 passed; 0 failed; 0 ignored'\n"
        )
        artifacts = self.write_artifacts([self.artifact(executable)])

        result = self.run_helper(artifacts)

        self.assertEqual(result.returncode, 0, result.stderr)
        # macOS reports /private/var for the temp dir; compare resolved paths.
        self.assertEqual(Path(marker.read_text().strip()).resolve(),
                         self.package_dir.resolve())

    def test_cargo_variables_are_set(self):
        executable = self.stub(
            'echo "manifest=$CARGO_MANIFEST_DIR name=$CARGO_PKG_NAME version=$CARGO_PKG_VERSION"\n'
            "echo 'test result: ok. 1 passed; 0 failed; 0 ignored'\n"
        )
        artifacts = self.write_artifacts([self.artifact(executable)])

        result = self.run_helper(artifacts)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"manifest={self.package_dir}", result.stdout)
        self.assertIn("name=horon-agent-base", result.stdout)
        self.assertIn("version=0.1.0", result.stdout)

    def test_legacy_package_id_format_is_understood(self):
        executable = self.stub("echo 'test result: ok. 1 passed; 0 failed'\n")
        entry = self.artifact(executable)
        # Older cargo wrote `<name> <version> (<path>)` instead of the
        # `path+file://...#<name>@<version>` form.
        entry["package_id"] = f"horon-agent-base 0.1.0 (path+file://{self.temp})"
        artifacts = self.write_artifacts([entry])

        result = self.run_helper(artifacts)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1 passed", result.stdout)

    def test_non_json_noise_is_ignored(self):
        # `run_with_timeout.py` prints its own banner to stdout before starting
        # the command, so the artifacts file can begin with a plain line.
        executable = self.stub("echo 'test result: ok. 2 passed; 0 failed'\n")
        path = self.temp / "noisy-artifacts.jsonl"
        path.write_text(
            "==> Running with timeout 1800s: cargo test\n"
            + json.dumps({"reason": "compiler-artifact", **self.artifact(executable)}) + "\n",
            encoding="utf-8",
        )

        result = self.run_helper(path)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2 passed", result.stdout)

    def test_missing_package_fails(self):
        executable = self.stub("echo 'test result: ok. 1 passed; 0 failed'\n")
        artifacts = self.write_artifacts([self.artifact(executable)])

        result = self.run_helper(artifacts, package="horon-agent-absent")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no lib test executable", result.stderr)

    def test_zero_passed_below_minimum_fails(self):
        executable = self.stub(
            "echo 'test result: ok. 0 passed; 0 failed; 0 filtered out'\n"
        )
        artifacts = self.write_artifacts([self.artifact(executable)])

        result = self.run_helper(artifacts, "--min-passed", "1")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected at least 1", result.stderr)

    def test_failing_test_exits_non_zero(self):
        executable = self.stub(
            "echo 'test result: FAILED. 2 passed; 1 failed; 0 ignored'\n"
            "exit 101\n"
        )
        artifacts = self.write_artifacts([self.artifact(executable)])

        result = self.run_helper(artifacts)

        self.assertEqual(result.returncode, 101, result.stderr)

    def test_timeout_exits_non_zero(self):
        executable = self.stub("sleep 30\n")
        artifacts = self.write_artifacts([self.artifact(executable)])
        command = [sys.executable, str(HELPER),
                   "--artifacts", str(artifacts),
                   "--package", "horon-agent-base",
                   "--timeout", "1"]

        result = subprocess.run(command, capture_output=True, text=True, timeout=90)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exceeded timeout after 1s", result.stderr)

    def test_log_file_receives_the_output(self):
        executable = self.stub("echo 'test result: ok. 1 passed; 0 failed; 0 ignored'\n")
        artifacts = self.write_artifacts([self.artifact(executable)])
        log = self.temp / "cohort.log"

        result = self.run_helper(artifacts, "--log", str(log))

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("test result: ok. 1 passed", log.read_text())

    def test_test_target_is_selected_by_name(self):
        lib = self.stub("echo 'test result: ok. 1 passed; 0 failed'\n", name="lib")
        integration = self.stub("echo 'test result: ok. 5 passed; 0 failed'\n",
                                name="e2e")
        artifacts = self.write_artifacts([
            self.artifact(lib, package="horon-agent"),
            self.artifact(integration, package="horon-agent", target="e2e", kind="test"),
        ])

        result = self.run_helper(artifacts, "--kind", "test",
                                 "--target-name", "e2e", package="horon-agent")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("5 passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
