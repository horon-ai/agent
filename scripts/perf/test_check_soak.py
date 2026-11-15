#!/usr/bin/env python3
"""Contract tests for scripts/perf/check_soak.py.

Run with `python3 -m unittest scripts/perf/test_check_soak.py`.

The scheduled soak workflow's whole value is that a retention regression lands as
a readable report and a red job, so these tests pin the parse of the test's own
output, the failure verdict, and the "the run never started" case that must not
be mistaken for a clean run.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent.parent
CHECKER = SCRIPTS / "check_soak.py"

CLEAN_METRICS = {
    "cycles": 1000,
    "window": 100,
    "windows": 10,
    "rotating_sessions": 8,
    "turn_rounds": 1000,
    "failed_rounds": 0,
    "quiescence_misses": 0,
    "samples": 2000,
    "budgets": {
        "fd_retention": 8.0,
        "thread_retention": 4.0,
        "gauge": 0.5,
        "sustained_rule": "fails when a counter retains above budget, or rises in at least half the windows",
    },
    "shared_context_grew": False,
    "non_returning": [],
}

RETAINED_METRICS = dict(
    CLEAN_METRICS,
    non_returning=[
        "fd: retained 12 above budget 8",
        "threads: rose in 7 of 9 windows",
    ],
)


def soak_log(metrics: dict, *, table: bool = True, counters: bool = True) -> str:
    """A log shaped like the soak test's own stderr."""
    parts = [
        "running 1 test",
        "STRUCTURAL_PERF section=soak cycles=1000 fd_retention=0 thread_retention=0 non_returning=0",
    ]
    if table:
        parts += [
            "soak_attach_detach_table",
            "| counter | baseline | final | slope |",
            "| --- | --- | --- | --- |",
            "| fd | 12 | 12 | 0 |",
        ]
    if counters:
        parts += [
            "soak_attach_detach_counter=fd: baseline 12, final 12, slope +0.00 per 100 cycles",
            "soak_attach_detach_counter=threads: baseline 22, final 22, slope +0.00 per 100 cycles",
        ]
    parts.append("soak_attach_detach_metrics=" + json.dumps(metrics, indent=2))
    parts.append("test result: ok. 1 passed; 0 failed")
    return "\n".join(parts) + "\n"


class CheckSoakTests(unittest.TestCase):
    def run_checker(self, log_text: str, *extra: str) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / "soak.log"
            log.write_text(log_text, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(CHECKER), str(log), *extra],
                capture_output=True,
                text=True,
            )

    def test_a_clean_run_passes_and_reports_every_counter(self):
        with tempfile.TemporaryDirectory() as temp:
            report = Path(temp) / "report.md"
            raw = Path(temp) / "report.json"
            log = Path(temp) / "soak.log"
            log.write_text(soak_log(CLEAN_METRICS), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(CHECKER), str(log), "--output", str(report), "--json", str(raw)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            body = report.read_text(encoding="utf-8")
            self.assertIn("**Clean.**", body)
            self.assertIn("1000 cycles", body)
            self.assertIn("fd_retention", body)
            # The per-counter lines and the table are carried into the report so a
            # reader sees the slope, not just the verdict.
            self.assertIn("baseline 12, final 12", body)
            self.assertIn("| counter | baseline | final | slope |", body)
            self.assertEqual(json.loads(raw.read_text(encoding="utf-8"))["cycles"], 1000)

    def test_a_retained_counter_fails_the_check_and_names_itself(self):
        result = self.run_checker(soak_log(RETAINED_METRICS), "--output", str(Path(tempfile.mkdtemp()) / "r.md"), "--require-metrics")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("retained 2 counter(s)", result.stderr)
        self.assertIn("fd: retained 12 above budget 8", result.stderr)

    def test_a_retained_counter_also_fails_without_asking_for_a_file(self):
        result = self.run_checker(soak_log(RETAINED_METRICS))
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_a_log_without_metrics_is_not_a_clean_run(self):
        # The dangerous shape: the test binary crashed before printing anything,
        # which must not read as "no retention".
        result = self.run_checker("error: test failed to build\n", "--require-metrics")
        self.assertEqual(result.returncode, 1)
        self.assertIn("did not reach its report", result.stderr)

    def test_metrics_without_table_or_counters_still_report(self):
        result = self.run_checker(soak_log(CLEAN_METRICS, table=False, counters=False))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("**Clean.**", result.stdout)

    def test_shared_context_growth_is_reported_not_failed(self):
        metrics = dict(CLEAN_METRICS, shared_context_grew=True)
        result = self.run_checker(soak_log(metrics))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("`shared_context` grew", result.stdout)

    def test_windows_zero_is_reported_as_absent_rather_than_a_division(self):
        metrics = dict(CLEAN_METRICS, windows=0)
        result = self.run_checker(soak_log(metrics))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Cycles: 1000", result.stdout)

    def test_the_workflow_runs_the_documented_test_and_pins_its_timeout(self):
        workflow = (REPO_ROOT / ".github/workflows/soak.yml").read_text(encoding="utf-8")
        self.assertIn("soak_attach_detach::soak_attach_detach_returns_every_counter_to_baseline", workflow)
        self.assertIn("timeout-minutes: 180", workflow)
        self.assertIn("cron: '0 6 * * 1'", workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("scripts/perf/check_soak.py", workflow)
        self.assertIn("actions/upload-artifact@", workflow)
        # The 1,000 cycles the documented baseline names, not the 200-cycle gate.
        self.assertIn("default: \"1000\"", workflow)

    def test_the_tracking_issue_write_permission_is_scoped_to_the_soak_job(self):
        workflow = (REPO_ROOT / ".github/workflows/soak.yml").read_text(encoding="utf-8")
        self.assertEqual(workflow.count("issues: write"), 1, "only the soak job may write issues")
        top = workflow.split("\njobs:", 1)[0]
        self.assertNotIn("issues: write", top, "the workflow default must stay read-only")
        self.assertIn("permissions:\n  contents: read\n", top)
        # The workflow must not be able to write anything else.
        job = workflow.split("\njobs:", 1)[1]
        self.assertNotIn("pull-requests: write", job)
        self.assertNotIn("contents: write", job)

    def test_the_tracking_issue_is_reused_not_reopened_each_week(self):
        workflow = (REPO_ROOT / ".github/workflows/soak.yml").read_text(encoding="utf-8")
        self.assertIn("gh issue list --state open", workflow)
        self.assertIn("gh issue comment", workflow)
        self.assertIn("gh issue create", workflow)
        # Only a scheduled failure reports, so a manual diagnosis run never
        # creates an issue.
        self.assertIn("github.event_name == 'schedule'", workflow)


if __name__ == "__main__":
    unittest.main()
