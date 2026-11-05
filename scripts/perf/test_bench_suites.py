#!/usr/bin/env python3
"""Tests for the ported benchmark suites, limited to behaviour that can be wrong.

Three consumer-visible contracts are pinned here, each of which has already
produced a misleading report shape elsewhere in this directory:

* a tool case that never succeeded must report a null latency and an error, not
  a fast zero (the memory gate makes the same promise);
* the rendered table must show that gap as an em dash and list it as unmeasured,
  so a reader cannot mistake a refusal for a win;
* the compile benchmark's touch cycle must leave the checkout byte- and
  timestamp-identical, because a benchmark that edits sources corrupts the next
  build.

No sockets, no binaries, no cargo: everything below runs pure or on tmp files.
"""

from __future__ import annotations

import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import bench_compile  # noqa: E402
import bench_reload_storm  # noqa: E402
import bench_tools  # noqa: E402


class ToolCaseReporting(unittest.TestCase):
    def test_failed_case_reports_null_and_error(self) -> None:
        row = bench_tools.summarize_case(
            "read (tiny file)", "read", [], [], 40, ["Tool 'read' is not allowed"]
        )
        self.assertIsNone(row["latency_ms"])
        self.assertEqual(row["error"], "Tool 'read' is not allowed")
        self.assertFalse(row["error"] == "")

    def test_successful_case_reports_stats(self) -> None:
        row = bench_tools.summarize_case("bash (true)", "bash", [1.0, 3.0, 2.0], [10, 10, 10], 32, [])
        self.assertIsNone(row["error"])
        self.assertEqual(row["latency_ms"]["samples"], 3)
        self.assertEqual(row["latency_ms"]["median"], 2.0)
        self.assertEqual(row["latency_ms"]["min"], 1.0)
        self.assertEqual(row["latency_ms"]["max"], 3.0)

    def test_markdown_renders_a_failed_case_as_unmeasured(self) -> None:
        rows = [
            bench_tools.summarize_case("read", "read", [0.5, 0.7], [5, 5], 20, []),
            bench_tools.summarize_case("edit", "edit", [], [], 20, ["edit refused"]),
        ]
        text = bench_tools.markdown(rows, [])
        self.assertIn("| read |", text)
        self.assertIn("| edit | — | — | — | — | 0 |", text)
        self.assertIn("Unmeasured: edit", text)

    def test_markdown_reports_skipped_suites(self) -> None:
        text = bench_tools.markdown([], [{"name": "tools", "reason": "binary missing"}])
        self.assertIn("Skipped: tools (binary missing)", text)


class ReloadStormReporting(unittest.TestCase):
    def test_refusal_is_a_failure_not_a_fast_resume(self) -> None:
        outcome = {
            "sessions": 2,
            "history_ms": None,
            "wall_ms": 12.0,
            "failures": [{"session": "bench-storm-000", "error": "Failed to restore session"}],
            "error": "Failed to restore session",
        }
        text = bench_reload_storm.markdown([outcome], [], 1)
        self.assertIn("| 2 | — | — | — | 12.0 ms | 1 |", text)
        self.assertIn("Unmeasured: 2", text)

    def test_parse_counts_rejects_zero_and_words(self) -> None:
        self.assertEqual(bench_reload_storm.parse_counts("1, 10,40"), [1, 10, 40])
        with self.assertRaises(SystemExit):
            bench_reload_storm.parse_counts("0")
        with self.assertRaises(SystemExit):
            bench_reload_storm.parse_counts("ten")


class CompileTouchCycle(unittest.TestCase):
    def test_touch_restores_bytes_and_mtime(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "source.rs"
            path.write_text("fn main() {}\n", encoding="utf-8")
            original = path.stat().st_mtime_ns
            restorer = bench_compile.TouchRestorer(path)
            restorer.bump()
            self.assertGreater(path.stat().st_mtime_ns, original)
            self.assertEqual(path.read_text(encoding="utf-8"), "fn main() {}\n")
            restorer.restore()
            self.assertEqual(path.stat().st_mtime_ns, original)
            self.assertEqual(path.read_text(encoding="utf-8"), "fn main() {}\n")

    def test_restore_without_bump_is_a_noop(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "source.rs"
            path.write_text("x\n", encoding="utf-8")
            before = path.stat().st_mtime_ns
            bench_compile.TouchRestorer(path).restore()
            self.assertEqual(path.stat().st_mtime_ns, before)

    def test_quick_selects_only_the_noop_checkpoint(self) -> None:
        modes = bench_compile.selected_modes(None, quick=True)
        self.assertEqual([mode for mode, _, _ in modes], ["warm_noop"])

    def test_unknown_mode_is_an_error(self) -> None:
        with self.assertRaises(SystemExit):
            bench_compile.selected_modes("warm_noop,tui_touch", quick=False)

    def test_markdown_separates_dry_run_from_a_measurement(self) -> None:
        results = [
            {
                "name": "warm_noop",
                "touch": None,
                "command": "cargo check --quiet",
                "seconds": {"median": 0.7, "p95": 0.7, "min": 0.7, "max": 0.7, "samples": 1},
                "dry_run": True,
                "errors": [],
                "error": None,
            }
        ]
        text = bench_compile.markdown(results, [])
        self.assertIn("Dry run: touches were exercised, cargo was not invoked.", text)
        self.assertIn("| warm_noop | — | 700.0 ms |", text)


if __name__ == "__main__":
    unittest.main()
