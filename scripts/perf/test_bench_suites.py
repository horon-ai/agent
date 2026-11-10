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
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import bench_compile  # noqa: E402
import bench_select  # noqa: E402
import bench_reload_storm  # noqa: E402
import bench_tools  # noqa: E402
import bench_merge  # noqa: E402
import bench_smoke  # noqa: E402


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


class PlatformSelection(unittest.TestCase):
    """The workflow's OS matrix comes from bench_select.platforms."""

    def test_all_or_empty_selects_every_os_in_fixed_order(self) -> None:
        for raw in ("all", "", "Windows,linux,macos"):
            with self.subTest(raw=raw):
                self.assertEqual(
                    [entry["os"] for entry in bench_select.platforms(raw)], ["linux", "macos", "windows"]
                )

    def test_one_os_runs_alone_on_its_hosted_runner(self) -> None:
        self.assertEqual(bench_select.platforms("windows"), [{"os": "windows", "runner": "windows-latest"}])

    def test_unknown_os_fails_instead_of_measuring_nothing(self) -> None:
        with self.assertRaises(SystemExit):
            bench_select.platforms("linux,freebsd")


class JobSelection(unittest.TestCase):
    """The measure, compile, and merge matrices come from bench_select."""

    def test_all_splits_loading_and_leaves_compile_to_its_own_job(self) -> None:
        units = {entry["unit"] for entry in bench_select.jobs("linux", "all")}
        self.assertEqual(units, {"compare", "startup", "loading-cold", "loading-warm", "tools", "reload"})
        self.assertEqual(bench_select.compile_jobs("linux", "all"), [{"os": "linux", "runner": "ubuntu-24.04"}])

    def test_subset_measures_only_the_chosen_units_on_every_chosen_os(self) -> None:
        entries = bench_select.jobs("linux,windows", "tools")
        self.assertEqual([(e["os"], e["unit"]) for e in entries], [("linux", "tools"), ("windows", "tools")])
        self.assertEqual(bench_select.compile_jobs("linux,windows", "tools"), [])

    def test_compile_only_has_no_measure_job(self) -> None:
        self.assertEqual(bench_select.jobs("all", "compile"), [])
        self.assertEqual(len(bench_select.compile_jobs("all", "compile")), 3)

    def test_merge_expects_every_unit_with_compile_last(self) -> None:
        self.assertEqual(
            bench_select.expected_units("loading,compile"), ["loading-cold", "loading-warm", "compile"]
        )


def _part(root: Path, name: str, status: str | None, files: dict[str, str]) -> None:
    directory = root / name
    directory.mkdir()
    for file_name, text in files.items():
        (directory / file_name).write_text(text, encoding="utf-8")
    if status is not None:
        (directory / "status.txt").write_text(status + "\n", encoding="utf-8")


class MergeParts(unittest.TestCase):
    """bench_merge rebuilds the one artifact bench_docs.py reads."""

    def _run(self, add_parts) -> tuple[Path, str]:
        holder = tempfile.TemporaryDirectory()
        self.addCleanup(holder.cleanup)
        root = Path(holder.name)
        parts = root / "parts"
        parts.mkdir()
        _part(parts, "bench-part-linux-build-7", "success", {"commit.txt": "abc\n", "horon-version.txt": "v1\n"})
        add_parts(parts)
        out = root / "out"
        bench_merge.merge(parts, out, "linux", "ubuntu-24.04", ["tools", "startup"])
        return out, (out / "status.txt").read_text(encoding="utf-8").strip()

    def test_every_part_succeeded(self) -> None:
        def add(parts: Path) -> None:
            _part(parts, "bench-part-linux-tools-7", "success", {"tools.json": "{}", "tools.md": "t"})
            _part(parts, "bench-part-linux-startup-7", "success", {"startup.txt": "s"})

        out, status = self._run(add)
        self.assertEqual(status, "success")
        self.assertEqual(
            sorted(p.name for p in out.iterdir()),
            ["commit.txt", "horon-version.txt", "platform.txt", "runner.txt", "startup.txt",
             "status.txt", "tools.json", "tools.md"],
        )

    def test_missing_part_fails_but_keeps_the_others(self) -> None:
        def add(parts: Path) -> None:
            _part(parts, "bench-part-linux-tools-7", "success", {"tools.md": "t"})

        out, status = self._run(add)
        self.assertEqual(status, "failure")
        self.assertTrue((out / "tools.md").is_file())

    def test_failed_part_fails(self) -> None:
        def add(parts: Path) -> None:
            _part(parts, "bench-part-linux-tools-7", "failure", {"tools.md": "t"})
            _part(parts, "bench-part-linux-startup-7", "success", {"startup.txt": "s"})

        self.assertEqual(self._run(add)[1], "failure")

    def test_duplicate_result_name_is_an_error(self) -> None:
        def add(parts: Path) -> None:
            _part(parts, "bench-part-linux-tools-7", "success", {"tools.md": "t"})
            _part(parts, "bench-part-linux-startup-7", "success", {"tools.md": "other"})

        with self.assertRaises(SystemExit):
            self._run(add)


class DaemonHomeEnv(unittest.TestCase):
    """Both suites start the daemon on a throwaway HORON_HOME.

    The binary reads its home from HORON_HOME only. Under any other name it
    falls back to `$HOME/.horon`, away from the directory the suite prepared.
    """

    def _start(self, module, root: Path) -> tuple[tuple, dict]:
        with mock.patch.object(module.subprocess, "Popen") as popen:
            result = module.start_daemon("/h", root, root, root / "daemon.log")
        return result, popen.call_args.kwargs["env"]

    def test_tools_daemon_home_is_under_the_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _, spawned = self._start(bench_tools, root)
            self.assertEqual(Path(spawned["HORON_HOME"]), root / "home")
            self.assertTrue((root / "home" / "sessions").is_dir())

    def test_reload_daemon_home_is_under_the_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (_, _, home), spawned = self._start(bench_reload_storm, root)
            self.assertEqual(Path(spawned["HORON_HOME"]), root / "home")
            self.assertEqual(home, spawned["HORON_HOME"])
            self.assertTrue((root / "home" / "sessions").is_dir())


class SmokeVerdict(unittest.TestCase):
    """The smoke reads the report, because every suite exits 0 on a failed run."""

    def test_measured_report_passes(self) -> None:
        row = {"name": "read", "latency_ms": {"median": 1.0}, "errors": [], "error": None}
        report = {"results": [row], "skipped": []}
        self.assertEqual(bench_smoke.problems("tools", report), [])

    def test_skipped_suite_fails(self) -> None:
        report = {"results": [], "skipped": [{"name": "tools", "reason": "binary missing"}]}
        self.assertEqual(bench_smoke.problems("tools", report), ["skipped: binary missing", "no results"])

    def test_daemon_error_inside_a_session_point_fails(self) -> None:
        row = {
            "name": "horon",
            "error": None,
            "oneshot": {"ms": 1.0, "error": None, "errors": []},
            "sessions_memory": {"points": [{"sessions": 1, "error": "Unknown provider profile 'mock'", "errors": []}]},
        }
        self.assertEqual(
            bench_smoke.problems("compare", {"results": [row], "skipped": []}),
            ["results[0].sessions_memory.points[0].error: Unknown provider profile 'mock'"],
        )

    def test_compare_scenario_that_did_not_run_fails(self) -> None:
        row = {"name": "horon", "error": None, "oneshot": None, "sessions_memory": {"points": []}}
        self.assertEqual(
            bench_smoke.problems("compare", {"results": [row], "skipped": []}),
            ["horon: oneshot did not run"],
        )



if __name__ == "__main__":
    unittest.main()
