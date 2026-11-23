#!/usr/bin/env python3
"""Tests for the package compatibility gate.

Each case writes report files into a temporary directory, or a small install
guide with the package support table, and drives the module's `main` the way the
workflow does. The decisions under test are the rejections: cross-compiled
evidence, a missing step for either kind, a failed step, an unknown target or
step, two reports for one target, a required target with no report, and an
install guide that advertises a target without one.
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

import package_compat as gate


def report_steps(*, plugin_resource: bool = True) -> list[dict]:
    steps: list[dict] = []
    for kind in gate.KINDS:
        for step in gate.STEP_ORDER:
            if step not in gate.REQUIRED[kind]:
                continue
            ok = True
            if kind == "plugin" and step == "resource_call" and not plugin_resource:
                ok = False
            steps.append({"kind": kind, "step": step, "ok": ok})
    return steps


def report(target: str, **overrides: object) -> dict:
    body: dict = {
        "schema": 1,
        "host_triple": target,
        "os": target.split("-")[1],
        "arch": "aarch64" if target.startswith("aarch64") else "x86_64",
        "cross_compiled": False,
        "steps": report_steps(),
    }
    body.update(overrides)
    return body


GUIDE = """# Release installation

| Platform | Agent asset |
| --- | --- |
| Linux x86_64 | `horon-agent-linux-x86_64.tar.gz` |

## Package support

| Target | Platform | Native execution verified |
| --- | --- | --- |
{rows}
"""


def guide(rows: list[tuple[str, str]]) -> str:
    return GUIDE.format(
        rows="\n".join(f"| `{target}` | a platform | {marker} |" for target, marker in rows)
    )


ALL_YES = [(target, "Yes") for target in gate.TARGETS]


class PackageCompatTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        self.reports = self.root / "reports"
        self.reports.mkdir()

    def write_report(self, name: str, body: dict) -> Path:
        path = self.reports / name
        path.write_text(json.dumps(body), encoding="utf-8")
        return path

    def write_all(self) -> None:
        for target in gate.TARGETS:
            self.write_report(f"{target}.json", report(target))

    def write_guide(self, text: str) -> Path:
        path = self.root / "INSTALL.md"
        path.write_text(text, encoding="utf-8")
        return path

    def run_gate(self, argv: list[str]) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gate.main(argv)
        return code, out.getvalue(), err.getvalue()

    def check(self, *extra: str) -> tuple[int, str, str]:
        return self.run_gate(["check", "--reports", str(self.reports), *extra])

    # -- check ---------------------------------------------------------------

    def test_six_complete_reports_pass_and_print_the_matrix(self) -> None:
        self.write_all()
        code, out, err = self.check("--require-targets", ",".join(gate.TARGETS))
        self.assertEqual(code, 0)
        self.assertIn("| extension | install |", out)
        self.assertIn("| plugin | resource_call |", out)
        self.assertNotIn(gate.CELL_FAILED, out)
        self.assertNotIn(gate.CELL_NO_REPORT, out)
        self.assertIn("6 native target reports passed", err)

    def test_the_extension_resource_call_cell_is_not_applicable(self) -> None:
        self.write_all()
        _, out, _ = self.check()
        row = next(line for line in out.splitlines() if "| extension | resource_call |" in line)
        self.assertEqual(row.count(gate.CELL_NOT_APPLICABLE), len(gate.TARGETS))

    def test_a_cross_compiled_report_is_refused(self) -> None:
        self.write_report(
            "cross.json", report("aarch64-apple-darwin", cross_compiled=True)
        )
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("cross_compiled must be false", err)

    def test_a_missing_extension_step_is_refused(self) -> None:
        steps = [step for step in report_steps() if step["step"] != "descendants_reaped"]
        self.write_report("mac.json", report("aarch64-apple-darwin", steps=steps))
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("extension has no descendants_reaped step", err)

    def test_a_missing_plugin_resource_call_is_refused(self) -> None:
        steps = [step for step in report_steps() if step["step"] != "resource_call"]
        self.write_report("mac.json", report("aarch64-apple-darwin", steps=steps))
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("plugin has no resource_call step", err)

    def test_a_failed_step_is_refused_and_shown_in_the_matrix(self) -> None:
        self.write_report(
            "mac.json",
            report("aarch64-apple-darwin", steps=report_steps(plugin_resource=False)),
        )
        code, out, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("plugin resource_call failed", err)
        row = next(line for line in out.splitlines() if "| plugin | resource_call |" in line)
        self.assertIn(gate.CELL_FAILED, row)

    def test_an_unknown_host_triple_is_refused(self) -> None:
        self.write_report(
            "musl.json", report("x86_64-unknown-linux-musl", host_triple="x86_64-unknown-linux-musl")
        )
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("is not one of the six native targets", err)

    def test_an_unknown_step_name_is_refused(self) -> None:
        steps = report_steps() + [{"kind": "extension", "step": "instal", "ok": True}]
        self.write_report("mac.json", report("aarch64-apple-darwin", steps=steps))
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("which is not a extension step", err)

    def test_a_report_that_is_not_json_is_refused(self) -> None:
        (self.reports / "broken.json").write_text("not json", encoding="utf-8")
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("not JSON", err)

    def test_two_reports_for_one_target_are_refused(self) -> None:
        self.write_report("a.json", report("aarch64-apple-darwin"))
        self.write_report("b.json", report("aarch64-apple-darwin"))
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("a second report for aarch64-apple-darwin", err)

    def test_a_required_target_without_a_report_is_refused(self) -> None:
        self.write_report("mac.json", report("aarch64-apple-darwin"))
        code, _, err = self.check("--require-targets", ",".join(gate.TARGETS))
        self.assertEqual(code, 1)
        self.assertIn("no report for the required target x86_64-pc-windows-msvc", err)

    def test_a_target_without_a_report_is_shown_as_missing_not_failed(self) -> None:
        self.write_report("mac.json", report("aarch64-apple-darwin"))
        code, out, _ = self.check()
        self.assertEqual(code, 0)
        self.assertIn("Reports: 1 of 6 targets.", out)
        self.assertIn(gate.CELL_NO_REPORT, out)

    def test_an_empty_report_directory_is_refused(self) -> None:
        code, _, err = self.check()
        self.assertEqual(code, 1)
        self.assertIn("no *.json reports", err)

    def test_an_unknown_required_target_is_refused(self) -> None:
        self.write_all()
        with self.assertRaises(SystemExit) as raised:
            self.check("--require-targets", "x86_64-unknown-linux-musl")
        self.assertIn("which the workflow does not run", str(raised.exception))

    def test_matrix_out_writes_the_printed_table(self) -> None:
        self.write_all()
        matrix = self.root / "docs" / "matrix.md"
        code, out, _ = self.check("--matrix-out", str(matrix))
        self.assertEqual(code, 0)
        self.assertEqual(matrix.read_text(encoding="utf-8"), out)

    # -- docs-claims ---------------------------------------------------------

    def claims(self, *pages: Path) -> tuple[int, str, str]:
        argv = ["docs-claims", "--reports", str(self.reports)]
        for page in pages:
            argv += ["--install", str(page)]
        return self.run_gate(argv)

    def test_a_verified_row_with_a_passing_report_passes(self) -> None:
        self.write_all()
        page = self.write_guide(guide(ALL_YES))
        code, out, _ = self.claims(page)
        self.assertEqual(code, 0)
        self.assertIn("aarch64-apple-darwin verified by aarch64-apple-darwin.json", out)

    def test_a_verified_row_without_a_report_is_refused(self) -> None:
        self.write_report("mac.json", report("aarch64-apple-darwin"))
        page = self.write_guide(guide(ALL_YES))
        code, _, err = self.claims(page)
        self.assertEqual(code, 1)
        self.assertIn("advertises x86_64-pc-windows-msvc, but no passing report", err)

    def test_a_refused_row_without_a_report_passes(self) -> None:
        rows = [
            (target, "Yes" if target == "aarch64-apple-darwin" else "No")
            for target in gate.TARGETS
        ]
        self.write_report("mac.json", report("aarch64-apple-darwin"))
        page = self.write_guide(guide(rows))
        code, _, _ = self.claims(page)
        self.assertEqual(code, 0)

    def test_a_failed_step_does_not_prove_the_advertised_target(self) -> None:
        self.write_report(
            "mac.json",
            report("aarch64-apple-darwin", steps=report_steps(plugin_resource=False)),
        )
        rows = [
            (target, "Yes" if target == "aarch64-apple-darwin" else "No")
            for target in gate.TARGETS
        ]
        page = self.write_guide(guide(rows))
        code, _, err = self.claims(page)
        self.assertEqual(code, 1)
        self.assertIn("advertises aarch64-apple-darwin", err)
        self.assertIn("plugin resource_call failed", err)

    def test_a_target_outside_the_six_is_refused(self) -> None:
        self.write_all()
        rows = ALL_YES[:-1] + [("x86_64-unknown-linux-musl", "Yes")]
        page = self.write_guide(guide(rows))
        code, _, err = self.claims(page)
        self.assertEqual(code, 1)
        self.assertIn("which the Package compat workflow does not run", err)

    def test_a_table_that_omits_a_target_is_refused(self) -> None:
        self.write_all()
        page = self.write_guide(guide(ALL_YES[:-1]))
        code, _, err = self.claims(page)
        self.assertEqual(code, 1)
        self.assertIn("the package support table omits x86_64-pc-windows-msvc", err)

    def test_an_unreadable_marker_is_refused(self) -> None:
        self.write_all()
        rows = ALL_YES[:-1] + [("x86_64-pc-windows-msvc", "Supported")]
        page = self.write_guide(guide(rows))
        code, _, err = self.claims(page)
        self.assertEqual(code, 1)
        self.assertIn("write Yes or No", err)

    def test_a_guide_without_a_target_table_is_refused(self) -> None:
        self.write_all()
        page = self.write_guide("# Release installation\n\nNo package table.\n")
        code, _, err = self.claims(page)
        self.assertEqual(code, 1)
        self.assertIn("no table lists a package target", err)

    def test_the_korean_marker_is_read(self) -> None:
        self.write_all()
        rows = [(target, "예") for target in gate.TARGETS]
        page = self.write_guide(guide(rows))
        code, _, _ = self.claims(page)
        self.assertEqual(code, 0)

    def test_the_korean_refusal_marker_is_read(self) -> None:
        self.write_report("mac.json", report("aarch64-apple-darwin"))
        rows = [
            (target, "예" if target == "aarch64-apple-darwin" else "아니오")
            for target in gate.TARGETS
        ]
        page = self.write_guide(guide(rows))
        code, _, _ = self.claims(page)
        self.assertEqual(code, 0)


if __name__ == "__main__":
    unittest.main()
