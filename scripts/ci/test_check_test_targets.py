#!/usr/bin/env python3
"""Tests for the integration test target guard (#226).

Each case builds a fake workflow plus fake manifest/allowlist documents, so the
gate runs without a workspace. `cargo metadata` is stubbed through the module's
`metadata_test_targets` seam rather than executed.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_test_targets as gate


class IntegrationTargetGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        self._targets: list[dict] = []

    def write_workflow(self, body: str) -> Path:
        path = self.root / "ci.yml"
        path.write_text(body, encoding="utf-8")
        return path

    def write_manifest(self, rows: list[dict]) -> Path:
        path = self.root / "targets.json"
        path.write_text(json.dumps({"version": 1, "targets": rows}) + "\n", encoding="utf-8")
        return path

    def write_allowlist(self, entries: list[dict]) -> Path:
        path = self.root / "allowlist.json"
        path.write_text(json.dumps({"version": 1, "entries": entries}) + "\n", encoding="utf-8")
        return path

    def run_gate(
        self,
        workflow: Path,
        manifest: Path,
        allowlist: Path,
        targets: list[dict],
        *extra: str,
    ):
        self._targets = targets
        original = gate.metadata_test_targets
        gate.metadata_test_targets = lambda: self._targets
        try:
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = gate.main(
                    [
                        "--workflow",
                        str(workflow),
                        "--manifest",
                        str(manifest),
                        "--allowlist",
                        str(allowlist),
                        *extra,
                    ]
                )
            return code, out.getvalue(), err.getvalue()
        finally:
            gate.metadata_test_targets = original

    COMPILE = (
        "run: |\n"
        "  packages=(-p horon-agent -p horon-agent-base)\n"
        '  cargo test --locked "${packages[@]}" --lib --bins --tests --no-run\n'
        "  python3 scripts/ci/run_integration_targets.py --artifacts x.jsonl\n"
    )

    def target(self, package: str, name: str) -> dict:
        return {"package": package, "target": name, "path": f"tests/{name}.rs"}

    def row(self, package: str, name: str, min_passed: int = 1) -> dict:
        return {"package": package, "target": name, "min_passed": min_passed}

    def test_skip_with_a_reason_is_accepted(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        row = self.row("horon-agent", "acp_infra", 30)
        row["skipped"] = ["v5_real_daemon_cancel_note_then_prompt"]
        row["skip_reason"] = "the outcome follows runner load"
        manifest = self.write_manifest([row])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "acp_infra")]
        )
        self.assertEqual(code, 0, out)

    def test_skip_without_a_reason_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        row = self.row("horon-agent", "acp_infra", 30)
        row["skipped"] = ["v5_real_daemon_cancel_note_then_prompt"]
        manifest = self.write_manifest([row])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "acp_infra")]
        )
        self.assertEqual(code, 1)
        self.assertIn("skip_reason", out)

    def test_skip_list_of_non_names_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        row = self.row("horon-agent", "acp_infra", 30)
        row["skipped"] = [""]
        row["skip_reason"] = "a reason"
        manifest = self.write_manifest([row])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "acp_infra")]
        )
        self.assertEqual(code, 1)
        self.assertIn("not made of test names", out)

    def test_manifest_row_counts_as_run(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([self.row("horon-agent", "e2e", 76)])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "e2e")]
        )
        self.assertEqual(code, 0, out)
        self.assertIn("1 run by targets.json", out)

    def test_target_absent_from_both_files_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "orphan")]
        )
        self.assertEqual(code, 1)
        self.assertIn("never run", out)
        self.assertIn("orphan", out)

    def test_allowlisted_target_passes(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist(
            [
                {
                    "package": "horon-agent",
                    "target": "probe",
                    "path": "tests/probe.rs",
                    "reason": "measurement harness, not a pass/fail assertion",
                }
            ]
        )
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "probe")]
        )
        self.assertEqual(code, 0, out)

    def test_stale_manifest_row_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([self.row("horon-agent", "gone")])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(workflow, manifest, allowlist, [])
        self.assertEqual(code, 1)
        self.assertIn("names no integration target", out)

    def test_stale_allowlist_entry_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist(
            [{"package": "horon-agent", "target": "gone", "path": "x", "reason": "gone"}]
        )
        code, out, _ = self.run_gate(workflow, manifest, allowlist, [])
        self.assertEqual(code, 1)
        self.assertIn("names no integration target", out)

    def test_manifest_row_without_positive_min_passed_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([self.row("horon-agent", "e2e", 0)])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "e2e")]
        )
        self.assertEqual(code, 1)
        self.assertIn("needs a positive", out)

    def test_manifest_row_for_uncompiled_package_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([self.row("horon-agent-logging", "probe")])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent-logging", "probe")]
        )
        self.assertEqual(code, 1)
        self.assertIn("does not build", out)

    def test_manifest_without_runner_step_fails(self) -> None:
        workflow = self.write_workflow(
            "run: |\n"
            "  packages=(-p horon-agent)\n"
            '  cargo test --locked "${packages[@]}" --lib --bins --no-run\n'
        )
        manifest = self.write_manifest([self.row("horon-agent", "e2e")])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "e2e")]
        )
        self.assertEqual(code, 1)
        self.assertIn("never runs run_integration_targets.py", out)

    def test_target_in_both_manifest_and_allowlist_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([self.row("horon-agent", "e2e")])
        allowlist = self.write_allowlist(
            [
                {
                    "package": "horon-agent",
                    "target": "e2e",
                    "path": "tests/e2e.rs",
                    "reason": "duplicate decision",
                }
            ]
        )
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "e2e")]
        )
        self.assertEqual(code, 1)
        self.assertIn("both run and allowlisted", out)

    def test_allowlist_entry_without_reason_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist(
            [{"package": "horon-agent", "target": "probe", "path": "x", "reason": ""}]
        )
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "probe")]
        )
        self.assertEqual(code, 1)
        self.assertIn("has no reason", out)

    def test_todo_reason_fails(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist(
            [
                {
                    "package": "horon-agent",
                    "target": "probe",
                    "path": "x",
                    "reason": "TODO: why this target does not run in ci.yml",
                }
            ]
        )
        code, out, _ = self.run_gate(
            workflow, manifest, allowlist, [self.target("horon-agent", "probe")]
        )
        self.assertEqual(code, 1)
        self.assertIn("has no reason", out)

    def test_run_lib_tests_package_must_be_compiled(self) -> None:
        workflow = self.write_workflow(
            "run: |\n"
            "  packages=(-p horon-agent)\n"
            '  cargo test --locked "${packages[@]}" --lib --bins --no-run\n'
            "  python3 scripts/ci/run_lib_tests.py --artifacts x --package horon-agent-base --timeout 60\n"
            "  python3 scripts/ci/run_integration_targets.py --artifacts x.jsonl\n"
        )
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(workflow, manifest, allowlist, [])
        self.assertEqual(code, 1)
        self.assertIn("absent from the compile step", out)

    def test_missing_compile_step_fails(self) -> None:
        workflow = self.write_workflow("run: echo nothing\n")
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(workflow, manifest, allowlist, [])
        self.assertEqual(code, 1)
        self.assertIn("has no compile step", out)

    def test_update_writes_todo_entries_for_unrun_targets(self) -> None:
        workflow = self.write_workflow(self.COMPILE)
        manifest = self.write_manifest([])
        allowlist = self.write_allowlist([])
        code, out, _ = self.run_gate(
            workflow,
            manifest,
            allowlist,
            [self.target("horon-agent", "orphan")],
            "--update",
        )
        self.assertEqual(code, 0, out)
        document = json.loads(allowlist.read_text(encoding="utf-8"))
        self.assertEqual([entry["target"] for entry in document["entries"]], ["orphan"])
        self.assertTrue(document["entries"][0]["reason"].startswith("TODO"))


if __name__ == "__main__":
    unittest.main()
