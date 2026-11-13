"""Tests for the lib test cohort coverage ratchet (#226).

Each case builds a fake workflow and a fake test executable, so the gate runs
without a workspace, cargo, or a compiled test binary. The executable is a shell
script that answers `--list` the way libtest does: one `name: test` line per
selected test, filtered by the argument.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_lib_test_coverage as gate

# A fake libtest binary: `--list [filter]` prints the names containing <filter>,
# which is how libtest selects tests for a cohort.
LISTER = '''#!/usr/bin/env python3
import sys
names = {names!r}
arguments = [argument for argument in sys.argv[1:] if argument != "--list"]
selected = [
    name for name in names
    if not arguments or any(argument in name for argument in arguments)
]
for name in selected:
    print(f"{{name}}: test")
'''


class CoverageGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)

    def write_lister(self, tests: list[str]) -> Path:
        """A fake test executable whose `--list <filter>` filters like libtest."""
        path = self.root / "fake-tests"
        path.write_text(LISTER.format(names=tests), encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IEXEC)
        return path

    def write_artifacts(self, package: str, executable: Path) -> Path:
        path = self.root / "artifacts.jsonl"
        message = {
            "reason": "compiler-artifact",
            "package_id": f"path+file:///repo#{package}@0.1.0",
            "target": {"kind": ["lib"], "name": package},
            "profile": {"test": True},
            "executable": str(executable),
        }
        path.write_text(json.dumps(message) + "\n", encoding="utf-8")
        return path

    def write_workflow(self, cohorts: list[tuple[str, str]]) -> Path:
        lines = ["jobs:", "  test:", "    steps:"]
        for package, filt in cohorts:
            lines.append("      - run: |")
            lines.append("          python3 scripts/ci/run_lib_tests.py \\")
            lines.append(f'            --artifacts "$TEST_ARTIFACTS" --package {package} \\')
            lines.append(f"            --timeout 600 -- {filt}")
        path = self.root / "ci.yml"
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return path

    def write_baseline(self, packages: dict[str, list[str]]) -> Path:
        path = self.root / "baseline.json"
        path.write_text(
            json.dumps({"version": 1, "packages": packages}) + "\n", encoding="utf-8"
        )
        return path

    def run_gate(self, *extra: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gate.main(list(extra))
        return code, out.getvalue(), err.getvalue()

    def test_uncovered_module_absent_from_baseline_fails(self):
        executable = self.write_lister(["a::b::covered", "x::y::orphan"])
        artifacts = self.write_artifacts("pkg", executable)
        workflow = self.write_workflow([("pkg", "a::b::")])
        baseline = self.write_baseline({"pkg": []})

        code, out, err = self.run_gate(
            "--artifacts", str(artifacts), "--workflow", str(workflow), "--baseline", str(baseline)
        )

        self.assertEqual(code, 1)
        self.assertIn("x::y holds lib tests", out)
        self.assertIn("never runs", out)

    def test_uncovered_module_in_baseline_passes(self):
        executable = self.write_lister(["a::b::covered", "x::y::orphan"])
        artifacts = self.write_artifacts("pkg", executable)
        workflow = self.write_workflow([("pkg", "a::b::")])
        baseline = self.write_baseline({"pkg": ["x::y"]})

        code, out, err = self.run_gate(
            "--artifacts", str(artifacts), "--workflow", str(workflow), "--baseline", str(baseline)
        )

        self.assertEqual(code, 0, err)
        self.assertIn("Lib test cohort coverage OK", out)

    def test_baseline_module_covered_now_is_a_drop_not_a_failure(self):
        executable = self.write_lister(["a::b::covered"])
        artifacts = self.write_artifacts("pkg", executable)
        workflow = self.write_workflow([("pkg", "a::b::")])
        baseline = self.write_baseline({"pkg": ["x::y"]})

        code, out, _err = self.run_gate(
            "--artifacts", str(artifacts), "--workflow", str(workflow), "--baseline", str(baseline)
        )

        self.assertEqual(code, 0)
        self.assertIn("covered now: pkg x::y", out)

    def test_whole_crate_cohort_covers_every_module(self):
        executable = self.write_lister(["a::b::one", "c::d::two"])
        artifacts = self.write_artifacts("pkg", executable)
        # An empty filter list is a whole-crate run: `--list` with no argument.
        workflow = self.write_workflow([("pkg", "")])
        baseline = self.write_baseline({})

        code, _out, err = self.run_gate(
            "--artifacts", str(artifacts), "--workflow", str(workflow), "--baseline", str(baseline)
        )

        self.assertEqual(code, 0, err)

    def test_compiled_package_without_any_cohort_fails(self):
        """A package compiled with `--lib` and named by no cohort is invisible.

        `measure` walks cohorts, so before this case such a package produced no
        row at all and the gate passed with every one of its tests unstarted.
        """
        executable = self.write_lister(["a::b::one"])
        artifacts = self.write_artifacts("pkg", executable)
        workflow = self.write_workflow([("other", "")])
        baseline = self.write_baseline({})

        code, out, _err = self.run_gate(
            "--artifacts", str(artifacts), "--workflow", str(workflow), "--baseline", str(baseline)
        )

        self.assertEqual(code, 1)
        self.assertIn("no ci.yml cohort names it", out)

    def test_workflow_without_a_cohort_fails(self):
        executable = self.write_lister(["a::b::one"])
        artifacts = self.write_artifacts("pkg", executable)
        workflow = self.root / "ci.yml"
        workflow.write_text("jobs: {}\n", encoding="utf-8")
        baseline = self.write_baseline({})

        code, _out, err = self.run_gate(
            "--artifacts", str(artifacts), "--workflow", str(workflow), "--baseline", str(baseline)
        )

        self.assertEqual(code, 1)
        self.assertIn("no lib test runs at all", err)

    def test_missing_artifacts_file_is_a_usage_error(self):
        code, _out, err = self.run_gate("--artifacts", str(self.root / "absent.jsonl"))
        self.assertEqual(code, 2)
        self.assertIn("artifacts file not found", err)

    def test_update_writes_the_measured_baseline(self):
        executable = self.write_lister(["a::b::covered", "x::y::orphan", "x::z::also"])
        artifacts = self.write_artifacts("pkg", executable)
        workflow = self.write_workflow([("pkg", "a::b::")])
        baseline = self.root / "baseline.json"

        code, out, err = self.run_gate(
            "--artifacts", str(artifacts), "--workflow", str(workflow),
            "--baseline", str(baseline), "--update",
        )

        self.assertEqual(code, 0, err)
        self.assertIn("Wrote 2 module entries", out)
        written = json.loads(baseline.read_text(encoding="utf-8"))
        self.assertEqual(written["packages"], {"pkg": ["x::y", "x::z"]})


if __name__ == "__main__":
    unittest.main()