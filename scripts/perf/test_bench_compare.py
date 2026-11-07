#!/usr/bin/env python3
"""Unit tests for the comparison benchmark's pure logic.

The expensive parts (spawning agents) are exercised by the smoke run, not here;
these tests pin only the arithmetic and report shapes that silently rot:

- "each additional session" is a least-squares slope, and it degrades to None
  rather than inventing a number when only one session count was measured;
- ratios treat horon as 1.0x and never divide by zero;
- the JSON envelope always carries suite/schema/host/started_at/results/skipped;
- install size counts a package directory but not the whole node_modules tree.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_common as bc  # noqa: E402
import bench_compare as bench  # noqa: E402


class StatsTests(unittest.TestCase):
    def test_summarize_empty_is_none(self) -> None:
        self.assertIsNone(bc.summarize([]))

    def test_summarize_reports_nearest_rank_p95(self) -> None:
        summary = bc.summarize([5, 1, 4, 2, 3])
        self.assertEqual(summary["samples"], 5)
        self.assertEqual(summary["median"], 3)
        self.assertEqual(summary["min"], 1)
        self.assertEqual(summary["max"], 5)
        self.assertEqual(summary["p95"], 5)
        self.assertNotIn("p99", summary)

    def test_summarize_ignores_none(self) -> None:
        summary = bc.summarize([None, 2.0, None, 4.0])  # type: ignore[list-item]
        self.assertEqual(summary["samples"], 2)
        self.assertEqual(summary["median"], 3)

    def test_slope_needs_spread(self) -> None:
        self.assertIsNone(bc.linear_slope([1], [10]))
        self.assertIsNone(bc.linear_slope([2, 2], [10, 20]))

    def test_slope_is_per_additional_session(self) -> None:
        # 40 MB at 1 session, 50 MB at 2, 60 MB at 3 -> exactly 10 MB/session.
        slope = bc.linear_slope([1, 2, 3], [40e6, 50e6, 60e6])
        self.assertAlmostEqual(slope, 10e6, delta=1)


class RatioTests(unittest.TestCase):
    def test_same_within_two_percent(self) -> None:
        self.assertEqual(bc.fmt_ratio(1.0), "same")
        self.assertEqual(bc.fmt_ratio(1.01), "same")

    def test_more_and_less(self) -> None:
        self.assertEqual(bc.fmt_ratio(2.5), "2.50× more")
        self.assertEqual(bc.fmt_ratio(0.5), "2.00× less")

    def test_none_and_zero(self) -> None:
        self.assertEqual(bc.fmt_ratio(None), "—")
        self.assertEqual(bc.fmt_ratio(0), "—")

    def test_relative_uses_baseline(self) -> None:
        results = [{"name": "horon", "oneshot": {"ms": 100.0}}, {"name": "codex", "oneshot": {"ms": 250.0}}]
        base = bench.baseline_value(results, ("oneshot", "ms"))
        self.assertEqual(base, 100.0)
        self.assertEqual(bench.relative(250.0, base), "2.50× more")

    def test_relative_handles_missing_baseline(self) -> None:
        results = [{"name": "codex", "oneshot": {"ms": 250.0}}]
        base = bench.baseline_value(results, ("oneshot", "ms"))
        self.assertIsNone(base)
        self.assertEqual(bench.relative(250.0, base), "—")


class SessionAggregateTests(unittest.TestCase):
    def test_first_session_and_slope(self) -> None:
        points = [
            {"sessions": 1, "idle_rss_bytes": 40e6, "post_turn_rss_bytes": 45e6},
            {"sessions": 2, "idle_rss_bytes": 50e6, "post_turn_rss_bytes": 55e6},
            {"sessions": 5, "idle_rss_bytes": 80e6, "post_turn_rss_bytes": 85e6},
        ]
        self.assertEqual(bench.plan_first_session(points, "idle_rss_bytes"), 40e6)
        self.assertAlmostEqual(bench.slope_series(points, "idle_rss_bytes"), 10e6, delta=1e5)

    def test_missing_point_skips_slope_input(self) -> None:
        points = [{"sessions": 1, "idle_rss_bytes": None}, {"sessions": 2, "idle_rss_bytes": 50e6}]
        self.assertIsNone(bench.plan_first_session(points, "idle_rss_bytes"))
        self.assertIsNone(bench.slope_series(points, "idle_rss_bytes"))

    def test_session_population_slopes_include_threads_and_fds(self) -> None:
        points = [
            {"sessions": 1, "idle_rss_bytes": 40e6, "thread_count": 12, "fd_count": 20},
            {"sessions": 10, "idle_rss_bytes": 50e6, "thread_count": 21, "fd_count": 38},
            {"sessions": 100, "idle_rss_bytes": 140e6, "thread_count": 111, "fd_count": 218},
        ]
        summary = bench.summarize_sessions(points, 1)
        self.assertEqual(summary["first_session_threads"], 12)
        self.assertAlmostEqual(summary["each_additional_threads"], 1.0, delta=1e-6)
        self.assertAlmostEqual(summary["each_additional_fds"], 2.0, delta=1e-6)
        self.assertEqual(summary["skipped"], [])

    def test_session_counters_stay_null_with_a_skip_reason(self) -> None:
        # A tool without `server:memory` must still report the RSS slopes; the
        # thread and fd slopes are None with the reason beside them.
        reason = "server:memory unavailable: unknown command"
        points = [
            {"sessions": 1, "idle_rss_bytes": 40e6, "counter_error": reason},
            {"sessions": 2, "idle_rss_bytes": 50e6, "counter_error": reason},
        ]
        summary = bench.summarize_sessions(points, 1)
        self.assertIsNone(summary["each_additional_threads"])
        self.assertIsNone(summary["each_additional_fds"])
        self.assertIsNone(summary["first_session_threads"])
        self.assertEqual(summary["skipped"], [reason])
        self.assertIsNotNone(summary["each_additional_idle_rss_bytes"])

    def test_process_counters_come_from_the_server_memory_payload(self) -> None:
        payload = {"process": {"thread_count": 17, "fd_count": 42, "rss_bytes": 1}}
        counters, reason = bench.read_process_counters(lambda: payload)
        self.assertEqual(counters, {"thread_count": 17, "fd_count": 42})
        self.assertIsNone(reason)

    def test_process_counters_name_the_missing_field(self) -> None:
        counters, reason = bench.read_process_counters(lambda: {"process": {"rss_bytes": 1}})
        self.assertEqual(counters, {})
        self.assertIn("thread_count", reason or "")


class InstallSizeTests(unittest.TestCase):
    def test_package_directory_is_measured(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            package = Path(root) / "node_modules" / "tool"
            (package / "bin").mkdir(parents=True)
            (package / "package.json").write_text("{}")
            binary = package / "bin" / "tool.js"
            binary.write_text("#!/usr/bin/env node\n")
            size, error = bench.measure_install_size(str(binary))
            self.assertIsNone(error)
            self.assertEqual(size, len("{}") + len("#!/usr/bin/env node\n"))

    def test_launcher_counts_installed_optional_platform_package(self) -> None:
        # npm launchers (e.g. @openai/codex) keep the native binary in an
        # optional sibling package; the launcher alone would report a few KB.
        with tempfile.TemporaryDirectory() as root:
            modules = Path(root) / "node_modules"
            launcher = modules / "@scope" / "tool"
            (launcher / "bin").mkdir(parents=True)
            manifest = json.dumps({"optionalDependencies": {"@scope/tool-native": "1", "absent": "1"}})
            (launcher / "package.json").write_text(manifest)
            (launcher / "bin" / "tool.js").write_text("js")
            native = modules / "@scope" / "tool-native"
            native.mkdir(parents=True)
            (native / "package.json").write_text("{}")
            (native / "tool").write_bytes(b"x" * 1000)
            size, error = bench.measure_install_size(str(launcher / "bin" / "tool.js"))
            self.assertIsNone(error)
            self.assertEqual(size, len(manifest) + len("js") + len("{}") + 1000)

    def test_plain_binary_is_measured(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            binary = Path(root) / "tool"
            binary.write_bytes(b"x" * 128)
            size, error = bench.measure_install_size(str(binary))
            self.assertIsNone(error)
            self.assertEqual(size, 128)

    def test_shell_launcher_counts_sibling_payload(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            launcher = Path(root) / "tool-linux"
            launcher.write_text("#!/bin/sh\nexec \"$0.bin\"\n")
            (Path(root) / "tool-linux.bin").write_bytes(b"x" * 1000)
            size, error = bench.measure_install_size(str(launcher))
            self.assertIsNone(error)
            self.assertEqual(size, len("#!/bin/sh\nexec \"$0.bin\"\n") + 1000)


class DaemonArgvTests(unittest.TestCase):
    """The horon daemon argv must match the flags `serve` currently accepts.

    A stale flag makes `serve` exit before binding, and the harness only reports
    the downstream "daemon socket not ready".
    """

    def test_horon_serve_argv_omits_the_removed_debug_socket_flag(self) -> None:
        socket_path = "/tmp/bench/run/bench.sock"
        argv = bench.horon_daemon_argv("/usr/local/bin/horon-agent", socket_path)
        removed = "--" + "debug-socket"
        self.assertNotIn(removed, argv)
        self.assertEqual(argv[0], "/usr/local/bin/horon-agent")
        self.assertIn("serve", argv)
        self.assertEqual(argv[argv.index("--socket") + 1], socket_path)

    def test_debug_socket_is_derived_from_the_main_socket(self) -> None:
        self.assertEqual(
            bench.debug_socket_for("/tmp/bench/run/bench.sock"), "/tmp/bench/run/bench-debug.sock"
        )


class StraceTests(unittest.TestCase):
    """fsync accounting comes from `strace -c` output, so its parsing is pinned."""

    SUMMARY = """\
% time     seconds  usecs/call     calls    errors syscall
------ ----------- ----------- --------- --------- ----------------
 62.50    0.000412          82         5           fsync
 36.67    0.000247          61         4           fdatasync
  0.83    0.000007           7         1           openat
------ ----------- --------- --------- ----------------
100.00    0.000666                    10           total
"""

    def test_strace_summary_parser_counts_fsync_and_fdatasync(self) -> None:
        parsed = bench.parse_strace_summary(self.SUMMARY)
        self.assertEqual(parsed, {"total": 9, "fsync": 5, "fdatasync": 4})

    def test_strace_summary_parser_handles_the_errors_column(self) -> None:
        # With failed calls strace fills the errors column, which must not be
        # mistaken for the call count.
        text = "100.00 0.000001 1 3 2 fsync\n100.00 0.000001 1 3 fdatasync"
        parsed = bench.parse_strace_summary(text)
        self.assertEqual(parsed, {"total": 6, "fsync": 3, "fdatasync": 3})

    def test_strace_summary_parser_returns_none_without_matching_rows(self) -> None:
        self.assertIsNone(bench.parse_strace_summary("strace: nothing matched\n"))
        self.assertIsNone(bench.parse_strace_summary(""))

    def test_strace_argv_keeps_the_measured_command(self) -> None:
        argv = bench.strace_argv(["/bin/tool", "run", "hi"], "/tmp/summary.txt")
        self.assertEqual(argv[0], "strace")
        self.assertIn("-f", argv)
        self.assertEqual(argv[argv.index("-e") + 1], "trace=fsync,fdatasync")
        self.assertEqual(argv[argv.index("-o") + 1], "/tmp/summary.txt")
        self.assertEqual(argv[-3:], ["/bin/tool", "run", "hi"])

    def test_summarize_oneshot_reports_the_median_calls(self) -> None:
        runs = [
            {"ms": 10.0, "fsync_calls": 40, "fsync_breakdown": {"total": 40, "fsync": 40, "fdatasync": 0}},
            {"ms": 11.0, "fsync_calls": 44, "fsync_breakdown": {"total": 44, "fsync": 44, "fdatasync": 0}},
            {"ms": 12.0, "fsync_calls": 42, "fsync_breakdown": {"total": 42, "fsync": 42, "fdatasync": 0}},
        ]
        summary = bench.summarize_oneshot(runs)
        self.assertEqual(summary["fsync_calls"], 42)
        self.assertEqual(summary["fsync_calls_runs"], [40, 44, 42])

    def test_summarize_oneshot_without_syscalls_stays_empty(self) -> None:
        summary = bench.summarize_oneshot([{"ms": 10.0, "fsync_calls": None}])
        self.assertIsNone(summary["fsync_calls"])
        self.assertEqual(summary["fsync_calls_runs"], [])

    def test_markdown_shows_the_fsync_column_when_measured(self) -> None:
        result = {
            "name": "horon",
            "label": "horon",
            "oneshot": {"ms": 100.0, "peak_rss_bytes": 40e6, "tool_roundtrip": True, "fsync_calls": 41},
            "sessions_memory": {},
        }
        report = bc.build_report("compare", [result], [])
        markdown = bench.render_markdown(report, [1], 1)
        self.assertIn("fsync calls", markdown)
        self.assertIn("| 41 |", markdown)


class ReportShapeTests(unittest.TestCase):
    def test_markdown_survives_a_failed_version_probe(self) -> None:
        # A binary that cannot execute has version_ms.summary == None; the
        # report must still render the row instead of raising.
        result = {
            "name": "horon",
            "label": "horon",
            "version": None,
            "version_ms": {"summary": None, "error": "Exec format error"},
            "oneshot": {"ms": None, "summary": None, "peak_rss_bytes": None, "tool_roundtrip": None,
                        "errors": ["exit 126: Exec format error"], "error": "exit 126: Exec format error"},
            "sessions_memory": {},
        }
        report = bc.build_report("compare", [result], [])
        markdown = bench.render_markdown(report, [1], 1)
        self.assertIn("| horon |", markdown)
        self.assertIn("Exec format error", markdown)


    def test_envelope_contract(self) -> None:
        report = bc.build_report("compare", [{"name": "horon"}], [{"name": "pi", "reason": "missing"}])
        for key in ("suite", "schema", "host", "started_at", "results", "skipped"):
            self.assertIn(key, report)
        self.assertEqual(report["schema"], 1)
        self.assertEqual(report["suite"], "compare")
        for key in ("os", "arch", "cpu_count", "python"):
            self.assertIn(key, report["host"])

    def test_write_json_roundtrip(self) -> None:
        report = bc.build_report("compare", [], [])
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "nested", "out.json")
            bc.write_json(path, report)
            with open(path, encoding="utf-8") as fh:
                loaded = json.load(fh)
            self.assertEqual(loaded["suite"], "compare")

    def test_markdown_renders_baseline_and_skips(self) -> None:
        results = [
            {
                "name": "horon",
                "label": "horon",
                "version": "v1",
                "version_ms": {"summary": {"median": 10.0}},
                "oneshot": {"ms": 100.0, "peak_rss_bytes": 40e6, "tool_roundtrip": True},
                "sessions_memory": {
                    "points": [{"sessions": 1, "post_turn_rss_bytes": 45e6}],
                    "first_session_idle_rss_bytes": 40e6,
                    "each_additional_idle_rss_bytes": 3e6,
                    "first_session_post_turn_rss_bytes": 45e6,
                    "each_additional_post_turn_rss_bytes": 4e6,
                },
                "install_size_bytes": 1024,
            },
            {
                "name": "codex",
                "label": "Codex CLI",
                "version": "v2",
                "version_ms": {"summary": {"median": 150.0}},
                "oneshot": {"ms": 250.0, "peak_rss_bytes": 120e6, "tool_roundtrip": True},
                "sessions_memory": {
                    "points": [{"sessions": 1, "post_turn_rss_bytes": 200e6}],
                    "first_session_idle_rss_bytes": 130e6,
                    "each_additional_idle_rss_bytes": 60e6,
                    "first_session_post_turn_rss_bytes": 200e6,
                    "each_additional_post_turn_rss_bytes": 70e6,
                },
                "install_size_bytes": 2048,
            },
        ]
        report = bc.build_report("compare", results, [{"name": "pi", "reason": "missing"}])
        markdown = bench.render_markdown(report, [1], 1)
        self.assertIn("baseline", markdown)
        self.assertIn("2.50× more", markdown)
        self.assertIn("pi (missing)", markdown)
        self.assertIn("| horon |", markdown)


if __name__ == "__main__":
    unittest.main()
