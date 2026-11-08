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

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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


class ExtraHoronRowTests(unittest.TestCase):
    """A second build of horon is measured as its own row.

    The before/after pair has to come from one run: two workflow runs on one
    shared runner are two machines as far as wall-clock is concerned.
    """

    def test_horon_kind_covers_the_candidate_and_extra_builds(self) -> None:
        self.assertTrue(bench.horon_kind("horon"))
        self.assertTrue(bench.horon_kind("horon-base"))
        self.assertFalse(bench.horon_kind("jcode"))
        self.assertFalse(bench.horon_kind("horonx"))

    def test_extra_build_uses_the_horon_runner(self) -> None:
        self.assertIs(bench.runner_class("horon-base"), bench.HoronRunner)
        self.assertIs(bench.runner_class("horon"), bench.HoronRunner)
        self.assertIs(bench.runner_class("jcode"), bench.JcodeRunner)
        self.assertIsNone(bench.runner_class("nope"))

    def test_extra_build_is_measured_right_after_the_candidate(self) -> None:
        overrides = {"horon": "/c", "horon-base": "/b", "jcode": "/j"}
        ordered = bench.order_tools(["jcode", "horon"], overrides)
        # The caller's order for the other tools is untouched.
        self.assertEqual(ordered, ["jcode", "horon", "horon-base"])

    def test_default_order_puts_the_baseline_build_second(self) -> None:
        overrides = {"horon": "/c", "horon-base": "/b", "jcode": "/j"}
        ordered = bench.order_tools(["horon", "jcode"], overrides)
        self.assertEqual(ordered, ["horon", "horon-base", "jcode"])

    def test_candidate_row_is_inserted_and_extras_follow_it(self) -> None:
        ordered = bench.order_tools(["codex"], {"horon-base": "/b", "codex": "/c"})
        self.assertEqual(ordered, ["horon", "horon-base", "codex"])

    def test_extra_build_row_keeps_its_own_label(self) -> None:
        self.assertEqual(bench.row_label("horon-base"), "horon-base")
        self.assertIsNone(bench.row_label("horon"))
        self.assertIsNone(bench.row_label("codex"))

    def test_extra_build_keeps_name_label_and_binary(self) -> None:
        runner = bench.HoronRunner("/b", name="horon-base", label=bench.row_label("horon-base"))
        self.assertEqual(runner.name, "horon-base")
        self.assertEqual(runner.label, "horon-base")
        self.assertEqual(runner.binary, "/b")

    def test_report_renders_both_horon_rows_with_threads_and_fsync(self) -> None:
        def row(name: str, ms: float, threads: float, fsync: int) -> dict:
            return {
                "name": name,
                "label": name,
                "version": "v1",
                "version_ms": {"summary": {"median": 10.0}},
                "oneshot": {"ms": ms, "peak_rss_bytes": 40e6, "tool_roundtrip": True,
                            "fsync_calls": fsync, "summary": {"samples": 1}},
                "sessions_memory": {
                    "points": [{"sessions": 1, "post_turn_rss_bytes": 45e6}],
                    "first_session_idle_rss_bytes": 40e6,
                    "each_additional_idle_rss_bytes": 3e6,
                    "each_additional_threads": threads,
                    "each_additional_fds": 0.5,
                    "first_session_post_turn_rss_bytes": 45e6,
                    "each_additional_post_turn_rss_bytes": 4e6,
                    "skipped": [],
                },
                "install_size_bytes": 1024,
            }

        results = [row("horon", 100.0, 0.02, 20), row("horon-base", 200.0, 1.0, 40)]
        report = bc.build_report("compare", results, [])
        markdown = bench.render_markdown(report, [1], 1)
        # Both rows are present and the baseline is the candidate, so the
        # before/after ratio is exact rather than two runs apart.
        self.assertIn("| horon |", markdown)
        self.assertIn("| horon-base |", markdown)
        self.assertIn("Extra horon builds (`horon-base`)", markdown)
        self.assertIn("2.00× more", markdown)
        self.assertIn("| 40 |", markdown)
        self.assertIn("0.02", markdown)
        self.assertIn("1.00", markdown)

    def test_each_horon_row_carries_its_own_threads_fds_and_fsync(self) -> None:
        # The report has to answer "did the candidate stop growing a thread per
        # session, and did the fsync count drop" for both builds; a row that
        # dropped either counter would silently read as a win.
        class FakeRunner:
            def __init__(self, name: str, threads: int, fsync: int):
                self.name = name
                self.label = name
                self.binary = f"/{name}"
                self.threads = threads
                self.fsync = fsync

            def env_for_version(self, root: str) -> dict:
                return {}

            def oneshot(self, mock, root, cwd, timeout, syscalls=False) -> dict:
                return {"ms": 1.0, "peak_rss_bytes": 1, "tool_roundtrip": True,
                        "fsync_calls": self.fsync}

            def sessions(self, mock, root, cwd, count, turns, timeout) -> dict:
                return {"samples": [{"phase": "idle", "rss_bytes": 1},
                                    {"phase": "turn1", "rss_bytes": 1,
                                     "thread_count": self.threads * count,
                                     "fd_count": 2 * count}],
                        "errors": [], "sessions_created": count, "counter_error": None}

        class FakeMock:
            def reset(self) -> None:
                pass

        saved = (bench.tool_version, bench.measure_version, bench.measure_install_size)
        bench.tool_version = lambda binary, env: "v1"
        bench.measure_version = lambda argv, env, runs: {"summary": {"median": 1.0}}
        bench.measure_install_size = lambda binary: (1, None)
        try:
            results = bench.measure_tools(
                [FakeRunner("horon", 3, 20), FakeRunner("horon-base", 1, 40)],
                FakeMock(), [1, 10], 1, 1, 1, 30.0,
            )
        finally:
            bench.tool_version, bench.measure_version, bench.measure_install_size = saved

        candidate, baseline = results
        self.assertEqual(candidate["oneshot"]["fsync_calls"], 20)
        self.assertEqual(baseline["oneshot"]["fsync_calls"], 40)
        self.assertAlmostEqual(candidate["sessions_memory"]["each_additional_threads"], 3.0)
        self.assertAlmostEqual(baseline["sessions_memory"]["each_additional_threads"], 1.0)
        self.assertAlmostEqual(candidate["sessions_memory"]["each_additional_fds"], 2.0)

        report = bc.build_report("compare", results, [])
        markdown = bench.render_markdown(report, [1, 10], 1)
        self.assertIn("| 20 |", markdown)
        self.assertIn("| 40 |", markdown)

    def test_one_unpreparable_row_does_not_drop_the_other(self) -> None:
        # A row whose version probe or temp root fails must record its error and
        # stay out of the shared phases; the other row still reports numbers.
        class BrokenRunner:
            name = "horon-base"
            label = "horon-base"
            binary = "/broken"

            def env_for_version(self, root: str) -> dict:
                raise OSError("no such binary")

        class GoodRunner:
            name = "horon"
            label = "horon"
            binary = "/good"

            def env_for_version(self, root: str) -> dict:
                return {}

            def oneshot(self, mock, root, cwd, timeout, syscalls=False) -> dict:
                return {"ms": 2.0, "peak_rss_bytes": 1, "tool_roundtrip": True}

            def sessions(self, mock, root, cwd, count, turns, timeout) -> dict:
                return {"samples": [{"phase": "idle", "rss_bytes": 1},
                                    {"phase": "turn1", "rss_bytes": 1}],
                        "errors": [], "sessions_created": count, "counter_error": None}

        class FakeMock:
            def reset(self) -> None:
                pass

        saved = (bench.tool_version, bench.measure_version, bench.measure_install_size)
        bench.tool_version = lambda binary, env: "v1"
        bench.measure_version = lambda argv, env, runs: {"summary": {"median": 1.0}}
        bench.measure_install_size = lambda binary: (1, None)
        try:
            results = bench.measure_tools(
                [GoodRunner(), BrokenRunner()], FakeMock(), [1], 1, 1, 1, 30.0,
            )
        finally:
            bench.tool_version, bench.measure_version, bench.measure_install_size = saved

        good, broken = results
        self.assertIn("no such binary", broken["error"])
        self.assertIsNone(broken["oneshot"])
        self.assertEqual(good["oneshot"]["ms"], 2.0)

    def test_bin_override_flags_place_the_baseline_row(self) -> None:
        # The workflow passes `--bin horon=... --bin horon-base=...`; parsing and
        # ordering have to agree on the row names or the second build is never
        # measured.
        args = bench.parse_args([
            "--bin", "horon=/c",
            "--bin", "horon-base=/b",
            "--sessions", "1", "10", "100",
        ])
        overrides = bench.parse_bin_overrides(args.bin)
        self.assertEqual(sorted(overrides), ["horon", "horon-base"])
        self.assertEqual(bench.order_tools(args.tools, overrides)[:2], ["horon", "horon-base"])
        self.assertEqual(args.sessions, [1, 10, 100])

    def test_only_horon_builds_share_a_group(self) -> None:
        runners = [
            bench.HoronRunner("/c", name="horon"),
            bench.HoronRunner("/b", name="horon-base"),
            bench.JcodeRunner("/j"),
        ]
        groups = bench.row_groups(runners)
        self.assertEqual([[r.name for r in group] for group in groups],
                         [["horon", "horon-base"], ["jcode"]])

    def test_a_single_horon_build_is_a_group_of_one(self) -> None:
        groups = bench.row_groups([bench.HoronRunner("/c")])
        self.assertEqual([[r.name for r in group] for group in groups], [["horon"]])

    def test_two_builds_alternate_every_phase(self) -> None:
        # A before/after pair is only comparable when both rows see the same
        # machine state; alternating makes a load spike land on both sides
        # instead of on whichever build happened to run second.
        order: list[str] = []

        class FakeRunner:
            def __init__(self, name: str):
                self.name = name
                self.label = name
                self.binary = f"/{name}"

            def env_for_version(self, root: str) -> dict:
                return {}

            def oneshot(self, mock, root, cwd, timeout, syscalls=False) -> dict:
                order.append(f"oneshot:{self.name}")
                return {"ms": 1.0, "peak_rss_bytes": 1, "tool_roundtrip": True}

            def sessions(self, mock, root, cwd, count, turns, timeout) -> dict:
                order.append(f"sessions{count}:{self.name}")
                return {"samples": [{"phase": "idle", "rss_bytes": 1},
                                    {"phase": "turn1", "rss_bytes": 1}],
                        "errors": [], "sessions_created": count, "counter_error": None}

        class FakeMock:
            def reset(self) -> None:
                order.append("reset")

        saved = (bench.tool_version, bench.measure_version, bench.measure_install_size)
        bench.tool_version = lambda binary, env: "v1"
        bench.measure_version = lambda argv, env, runs: {"summary": {"median": 1.0}}
        bench.measure_install_size = lambda binary: (1, None)
        try:
            results = bench.measure_tools(
                [FakeRunner("horon"), FakeRunner("horon-base")],
                FakeMock(), [1, 10], 1, 1, 2, 30.0,
            )
        finally:
            bench.tool_version, bench.measure_version, bench.measure_install_size = saved

        self.assertEqual([row["name"] for row in results], ["horon", "horon-base"])
        self.assertEqual(order, [
            "oneshot:horon", "oneshot:horon-base",
            "oneshot:horon", "oneshot:horon-base",
            "reset", "sessions1:horon", "reset", "sessions1:horon-base",
            "reset", "sessions10:horon", "reset", "sessions10:horon-base",
        ])
        for row in results:
            self.assertEqual(row["oneshot"]["ms"], 1.0)
            self.assertIsNotNone(row["sessions_memory"])


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


class ToolRegistryTests(unittest.TestCase):
    def test_omp_and_omo_runners_are_registered(self) -> None:
        self.assertIs(bench.RUNNERS.get("omp"), bench.OmpRunner)
        self.assertIs(bench.RUNNERS.get("omo"), bench.OmoRunner)
        self.assertEqual(bench.OmpRunner.name, "omp")
        self.assertEqual(bench.OmpRunner.label, "oh-my-pi (omp)")
        self.assertEqual(bench.OmoRunner.name, "omo")
        self.assertEqual(bench.OmoRunner.label, "oh-my-openagent (omo)")

    def test_tools_flag_accepts_omp_and_omo(self) -> None:
        args = bench.parse_args(["--tools", "omp", "omo"])
        self.assertEqual(args.tools, ["omp", "omo"])
        for name in args.tools:
            self.assertIn(name, bench.RUNNERS, f"{name} would be reported as an unknown tool")

    def test_tools_help_lists_every_runner(self) -> None:
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured), self.assertRaises(SystemExit):
            bench.parse_args(["--help"])
        text = captured.getvalue()
        for name in bench.RUNNERS:
            self.assertIn(name, text)

    def test_resolve_bin_finds_omp_and_omo(self) -> None:
        if os.name == "nt":
            self.skipTest("the executable bit does not decide lookup on Windows")
        with tempfile.TemporaryDirectory() as root:
            for name in ("omp", "omo"):
                path = os.path.join(root, name)
                with open(path, "w", encoding="utf-8") as fh:
                    fh.write("#!/bin/sh\n")
                os.chmod(path, 0o755)
                found = bench.resolve_bin(name, None, {"PATH": root})
                self.assertEqual(os.path.realpath(found or ""), os.path.realpath(path))
            self.assertIsNone(bench.resolve_bin("omp", None, {"PATH": ""}))


class ToolEnvIsolationTests(unittest.TestCase):
    """omp and omo must never see the parent's config, home, or credentials."""

    PARENT_LEAKS = {
        "OMP_PROFILE": "leak",
        "OMP_PLUGIN_ROOT": "/leak",
        "OMP_CODING_AGENT_DIR": "/leak",
        "OMP_DAEMON_RUNTIME_DIR": "/leak",
        "OMO_CODING_AGENT_DIR": "/leak",
        "OMO_PACKAGE_DIR": "/leak",
        "OMO_PLUGIN_ROOT": "/leak",
        "OMO_DEBUG": "1",
        # Without this scrub omo opts back into telemetry even under automation.
        "OMO_TELEMETRY_ALLOW_AUTOMATION": "1",
        "SENPI_CODING_AGENT_DIR": "/leak",
        "PI_CODING_AGENT_DIR": "/leak",
        "PI_CONFIG_DIR": "/leak",
        "PI_PACKAGE_DIR": "/leak",
        "ANTHROPIC_API_KEY": "leak",
        "OPENAI_API_KEY": "leak",
        "OPENAI_BASE_URL": "http://leak/v1",
    }

    # These are written by the runner itself, so they must point into the temp
    # root instead of being absent.
    AGENT_DIR_KEYS = ("PI_CODING_AGENT_DIR", "OMO_CODING_AGENT_DIR", "SENPI_CODING_AGENT_DIR")

    def _assert_isolated(self, env: dict, root: str) -> None:
        for key in self.PARENT_LEAKS:
            if key in self.AGENT_DIR_KEYS:
                if key in env:
                    self.assertTrue(env[key].startswith(root), f"{key}={env[key]}")
                continue
            self.assertNotIn(key, env, key)
        self.assertTrue(env["HOME"].startswith(root), env["HOME"])
        self.assertNotEqual(os.path.realpath(env["HOME"]), os.path.realpath(os.path.expanduser("~")))

    def test_omp_env_drops_parent_configuration(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            root = os.path.realpath(root)
            with mock.patch.dict(os.environ, self.PARENT_LEAKS, clear=False):
                env = bench.omp_env(root, "http://127.0.0.1:9")
            self._assert_isolated(env, root)

    def test_omo_env_drops_parent_configuration(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            root = os.path.realpath(root)
            with mock.patch.dict(os.environ, self.PARENT_LEAKS, clear=False):
                env = bench.omo_env(root, "http://127.0.0.1:9")
            self._assert_isolated(env, root)
            agent_dirs = {env[key] for key in self.AGENT_DIR_KEYS}
            self.assertEqual(len(agent_dirs), 1)

    def test_mock_models_json_points_at_the_mock_server(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            env = bench.omp_env(root, "http://127.0.0.1:9")
            with open(os.path.join(env["PI_CODING_AGENT_DIR"], "models.json"), encoding="utf-8") as fh:
                models = json.load(fh)
        provider = models["providers"]["mock"]
        self.assertEqual(provider["baseUrl"], "http://127.0.0.1:9/v1")
        self.assertEqual(provider["api"], "openai-completions")
        self.assertEqual([model["id"] for model in provider["models"]], ["mock-model"])

    def test_omp_and_omo_turn_off_telemetry_and_updates(self) -> None:
        for builder in (bench.omp_env, bench.omo_env):
            with self.subTest(tool=builder.__name__), tempfile.TemporaryDirectory() as root:
                env = builder(root, "http://127.0.0.1:9")
                self.assertEqual(env["PI_OFFLINE"], "1")
                self.assertEqual(env["PI_SKIP_VERSION_CHECK"], "1")
                self.assertEqual(env["PI_TELEMETRY"], "0")
                self.assertEqual(env["DISABLE_TELEMETRY"], "1")
                self.assertEqual(env["DO_NOT_TRACK"], "1")

    def test_each_measurement_gets_a_fresh_agent_dir(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            first = bench.omp_env(root, "http://127.0.0.1:9")
            second = bench.omp_env(root, "http://127.0.0.1:9")
        # omp caches the provider base URL inside the agent dir, so a reused dir
        # would keep calling an older endpoint.
        self.assertNotEqual(first["PI_CODING_AGENT_DIR"], second["PI_CODING_AGENT_DIR"])

    def test_oneshot_and_session_argv_carry_the_required_flags(self) -> None:
        omp = bench.omp_oneshot_argv("omp")
        self.assertIn("-p", omp)
        self.assertIn("--no-session", omp)
        self.assertIn("--no-title", omp)
        self.assertIn("--no-lsp", omp)
        self.assertEqual(omp[omp.index("--model") + 1], "mock/mock-model")
        self.assertEqual(omp[-1], bench.TURN_PROMPT)
        omo = bench.omo_oneshot_argv("omo")
        self.assertIn("-p", omo)
        self.assertIn("--no-session", omo)
        self.assertEqual(omo[omo.index("--tools") + 1], "bash,read")
        self.assertEqual(omo[omo.index("--model") + 1], "mock/mock-model")
        for argv in (bench.omp_session_argv("omp"), bench.omo_session_argv("omo")):
            self.assertIn("--mode", argv)
            self.assertEqual(argv[argv.index("--mode") + 1], "rpc")


class ToolListTests(unittest.TestCase):
    def test_horon_is_first_and_appears_once(self) -> None:
        self.assertEqual(bench.normalize_tools(["omp", "omo"]), ["horon", "omp", "omo"])
        self.assertEqual(
            bench.normalize_tools(["horon", "omp", "omo", "omp"]), ["horon", "omp", "omo"]
        )
        self.assertEqual(bench.normalize_tools(["jcode", "horon"]), ["horon", "jcode"])


class MissingToolWarningTests(unittest.TestCase):
    REPORT = {
        "results": [],
        "skipped": [
            {"name": "syscalls", "reason": "strace not available"},
            {"name": "omp", "reason": "install failed: bun is required"},
        ],
    }

    def test_feature_skip_is_not_a_missing_tool(self) -> None:
        self.assertEqual([s["name"] for s in bench.missing_tools(self.REPORT)], ["omp"])
        self.assertEqual(bench.missing_tools({"skipped": [{"name": "syscalls", "reason": "x"}]}), [])

    def test_markdown_warns_before_the_tables(self) -> None:
        markdown = bench.render_markdown(bc.build_report("compare", [], self.REPORT["skipped"], "t"), [1], 1)
        warning = next(line for line in markdown.splitlines() if line.startswith("> **Warning"))
        self.assertIn("1 tool(s) were not measured", warning)
        self.assertIn("omp (install failed: bun is required)", warning)
        self.assertNotIn("syscalls", warning)

    def test_ci_run_gets_an_annotation_and_a_summary_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.md"
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                bench.announce_missing_tools(
                    bench.missing_tools(self.REPORT),
                    {"GITHUB_ACTIONS": "true", "GITHUB_STEP_SUMMARY": str(summary)},
                )
            self.assertEqual(
                out.getvalue().strip(),
                "::warning title=compare: omp not measured::omp: install failed: bun is required",
            )
            self.assertIn("`omp`: install failed: bun is required", summary.read_text())

    def test_nothing_is_announced_outside_ci_or_without_missing_tools(self) -> None:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            bench.announce_missing_tools(bench.missing_tools(self.REPORT), {})
            bench.announce_missing_tools([], {"GITHUB_ACTIONS": "true"})
        self.assertEqual(out.getvalue(), "")

    def test_annotation_text_cannot_start_a_new_command(self) -> None:
        line = bench.github_warnings([{"name": "x", "reason": "a\n::error::b 100%"}])[0]
        self.assertNotIn("\n", line)
        self.assertIn("a%0A::error::b 100%25", line)


class FakeMockServer:
    """Enough of the mock server for the scenario wiring, with no process."""

    base_url = "http://127.0.0.1:9"

    def reset(self) -> None:
        pass


class ScenarioSummarizeTests(unittest.TestCase):
    def test_round_trip_needs_the_sentinel_in_every_run(self) -> None:
        seen = [{"ms": 1.0, "sentinel_seen": True}, {"ms": 2.0, "sentinel_seen": True}]
        self.assertTrue(bench.summarize_scenario(seen)["tool_round_trip"])

    def test_mock_ok_without_the_sentinel_is_not_a_round_trip(self) -> None:
        # The agent answered, but nothing proves it reached the fixture.
        summary = bench.summarize_scenario([{"ms": 1.0, "sentinel_seen": False}])
        self.assertFalse(summary["tool_round_trip"])
        self.assertEqual(summary["ms"], 1.0)

    def test_an_empty_series_is_not_a_round_trip(self) -> None:
        summary = bench.summarize_scenario([])
        self.assertFalse(summary["tool_round_trip"])
        self.assertIsNone(summary["ms"])

    def test_ready_time_is_the_median_of_the_runs_that_saw_it(self) -> None:
        runs = [
            {"ms": 100.0, "sentinel_seen": True, "mcp_ready_ms": 40.0},
            {"ms": 120.0, "sentinel_seen": True, "mcp_ready_ms": 60.0},
            {"ms": 90.0, "sentinel_seen": False, "mcp_ready_ms": None},
        ]
        summary = bench.summarize_scenario(runs)
        self.assertEqual(summary["mcp_ready_ms"], 50.0)
        self.assertFalse(summary["tool_round_trip"])

    def test_via_comes_from_the_runs(self) -> None:
        summary = bench.summarize_scenario([{"ms": 1.0, "sentinel_seen": True, "via": "file-read"}])
        self.assertEqual(summary["via"], "file-read")


class ScenarioSelectionTests(unittest.TestCase):
    def test_default_is_all_four_scenarios(self) -> None:
        args = bench.parse_args([])
        self.assertEqual(args.scenarios, ["oneshot", "sessions", "skill", "mcp"])
        self.assertEqual(
            bench.parse_scenarios(args.scenarios), ["oneshot", "sessions", "skill", "mcp"]
        )

    def test_a_comma_list_and_a_repeated_name_normalize(self) -> None:
        self.assertEqual(bench.parse_scenarios(["skill,mcp", "skill"]), ["skill", "mcp"])

    def test_an_unknown_scenario_fails_instead_of_being_dropped(self) -> None:
        with self.assertRaises(SystemExit):
            bench.parse_scenarios(["skills"])
        with self.assertRaises(SystemExit):
            bench.parse_scenarios([""])

    def _patched(self):
        saved = (bench.tool_version, bench.measure_version, bench.measure_install_size)
        bench.tool_version = lambda binary, env: "v1"
        bench.measure_version = lambda argv, env, runs: {"summary": {"median": 1.0}}
        bench.measure_install_size = lambda binary: (1, None)
        return saved

    def test_a_subset_really_skips_the_missing_phases(self) -> None:
        order: list[str] = []

        class FakeRunner:
            name = "horon"
            label = "horon"
            binary = "/b"

            def env_for_version(self, root: str) -> dict:
                return {}

            def oneshot(self, mock, root, cwd, timeout, syscalls=False) -> dict:
                order.append("oneshot")
                return {"ms": 1.0, "peak_rss_bytes": 1, "tool_roundtrip": True}

            def sessions(self, mock, root, cwd, count, turns, timeout) -> dict:
                order.append("sessions")
                return {"samples": [], "errors": [], "sessions_created": count}

            def skill(self, mock, root, cwd, timeout) -> dict:
                order.append("skill")
                return {"ms": 1.0, "sentinel_seen": True, "via": "file-read"}

            def mcp(self, mock, root, cwd, timeout) -> dict:
                order.append("mcp")
                return {"ms": 1.0, "sentinel_seen": True, "via": "mcp-tool"}

        runner = FakeRunner()
        saved = self._patched()
        try:
            only_oneshot = bench.measure_tools(
                [runner], FakeMockServer(), [1], 1, 1, 1, 30.0, scenarios=["oneshot"]
            )[0]
        finally:
            bench.tool_version, bench.measure_version, bench.measure_install_size = saved
        self.assertEqual(order, ["oneshot"])
        self.assertIsNone(only_oneshot["sessions_memory"])
        self.assertIsNone(only_oneshot["skill"])
        self.assertIsNone(only_oneshot["mcp"])

        order.clear()
        saved = self._patched()
        try:
            both = bench.measure_tools(
                [runner], FakeMockServer(), [1], 1, 1, 1, 30.0, scenarios=["skill", "mcp"]
            )[0]
        finally:
            bench.tool_version, bench.measure_version, bench.measure_install_size = saved
        self.assertEqual(order, ["skill", "mcp"])
        self.assertIsNone(both["oneshot"])
        self.assertIsNone(both["sessions_memory"])
        self.assertEqual(both["skill"]["via"], "file-read")
        self.assertTrue(both["mcp"]["tool_round_trip"])

    def test_an_unsupported_scenario_is_decided_once(self) -> None:
        calls: list[str] = []

        class FakeRunner:
            name = "horon"
            label = "horon"
            binary = "/b"

            def env_for_version(self, root: str) -> dict:
                return {}

            def oneshot(self, mock, root, cwd, timeout, syscalls=False) -> dict:
                return {"ms": 1.0, "peak_rss_bytes": 1, "tool_roundtrip": True}

            def sessions(self, mock, root, cwd, count, turns, timeout) -> dict:
                return {"samples": [], "errors": [], "sessions_created": count}

            def mcp(self, mock, root, cwd, timeout) -> dict:
                calls.append("mcp")
                return bench.unsupported("no MCP client")

        saved = self._patched()
        try:
            result = bench.measure_tools(
                [FakeRunner()], FakeMockServer(), [1], 1, 1, 3, 30.0, scenarios=["mcp"]
            )[0]
        finally:
            bench.tool_version, bench.measure_version, bench.measure_install_size = saved
        self.assertEqual(calls, ["mcp"])
        self.assertEqual(result["mcp"], {"skipped": "no MCP client"})


class ScenarioRouteTests(unittest.TestCase):
    def test_pi_mcp_is_unsupported_with_a_reason(self) -> None:
        result = bench.PiRunner("/pi").mcp(FakeMockServer(), "/tmp/root", "/tmp/root/ws", 1.0)
        self.assertIn("skipped", result)
        self.assertIn("MCP", result["skipped"])

    def test_horon_argv_allow_the_tool_each_scenario_needs(self) -> None:
        skill = bench.horon_turn_argv("/h", bench.skill_prompt("/w/SKILL.md"), "read")
        self.assertEqual(skill[skill.index("--tools") + 1], "read")
        self.assertTrue(skill[-1].startswith("[bench:skill]"))
        mcp = bench.horon_turn_argv("/h", bench.mcp_prompt(), "mcp__bench__bench_echo")
        self.assertEqual(mcp[mcp.index("--tools") + 1], "mcp__bench__bench_echo")
        self.assertTrue(mcp[-1].startswith("[bench:mcp]"))

    def test_horon_keeps_mcp_off_outside_the_new_scenarios(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            # The one-shot and the session daemon both build their env here.
            self.assertEqual(bench.horon_env(root, "/h")["HORON_AGENT_RUN_MCP"], "0")

    def test_prompt_markers_select_the_mock_plan(self) -> None:
        self.assertTrue(bench.skill_prompt("/w/SKILL.md").startswith("[bench:skill]"))
        self.assertTrue(bench.mcp_prompt().startswith("[bench:mcp]"))
        self.assertFalse(bench.TURN_PROMPT.startswith("[bench:"))


class ScenarioEnvIsolationTests(unittest.TestCase):
    """Every scenario env keeps its home and config under the scenario root."""

    PARENT_LEAKS = ToolEnvIsolationTests.PARENT_LEAKS

    REQUIRED_KEYS = {
        "HoronRunner": ("HOME", "XDG_CONFIG_HOME", "HORON_AGENT_HOME"),
        "JcodeRunner": ("HOME", "XDG_CONFIG_HOME", "JCODE_HOME"),
        "ClaudeRunner": ("HOME", "XDG_CONFIG_HOME", "CLAUDE_CONFIG_DIR"),
        "CodexRunner": ("HOME", "XDG_CONFIG_HOME", "CODEX_HOME"),
        "OpencodeRunner": ("HOME", "XDG_CONFIG_HOME"),
        "PiRunner": ("HOME", "XDG_CONFIG_HOME", "PI_CODING_AGENT_DIR"),
        "OmpRunner": ("HOME", "XDG_CONFIG_HOME", "PI_CODING_AGENT_DIR"),
        "OmoRunner": ("HOME", "XDG_CONFIG_HOME", "PI_CODING_AGENT_DIR",
                      "OMO_CODING_AGENT_DIR", "SENPI_CODING_AGENT_DIR"),
    }
    GUARDED_KEYS = ("HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME", "XDG_CONFIG_HOME", "JCODE_HOME",
                    "HORON_AGENT_HOME", "PI_CODING_AGENT_DIR", "OMO_CODING_AGENT_DIR",
                    "SENPI_CODING_AGENT_DIR")
    RUNNERS = (bench.HoronRunner, bench.JcodeRunner, bench.ClaudeRunner, bench.CodexRunner,
               bench.OpencodeRunner, bench.PiRunner, bench.OmpRunner, bench.OmoRunner)

    def _capture(self, runner, scenario: str, root: str):
        captured: list[dict] = []
        saved = bench.measure_oneshot

        def fake(argv, cwd, env, mock, timeout, syscalls=False):
            captured.append({"argv": argv, "cwd": cwd, "env": env})
            return {"ms": 1.0, "peak_rss_bytes": 1, "sentinel_seen": True, "via": "x",
                    "error": None}

        bench.measure_oneshot = fake
        try:
            with mock.patch.dict(os.environ, self.PARENT_LEAKS, clear=False):
                result = getattr(runner, scenario)(
                    FakeMockServer(), os.path.join(root, scenario),
                    os.path.join(root, scenario, "ws"), 1.0,
                )
        finally:
            bench.measure_oneshot = saved
        return result, captured

    def test_every_tool_keeps_skill_and_mcp_state_under_the_scenario_root(self) -> None:
        leak_values = set(self.PARENT_LEAKS.values())
        for runner_cls in self.RUNNERS:
            for scenario in ("skill", "mcp"):
                with self.subTest(tool=runner_cls.__name__, scenario=scenario), \
                        tempfile.TemporaryDirectory() as tmp:
                    root = os.path.realpath(tmp)
                    result, captured = self._capture(runner_cls("/bin/tool"), scenario, root)
                    if "skipped" in result:
                        # pi is the one tool without an MCP client.
                        self.assertEqual((runner_cls, scenario), (bench.PiRunner, "mcp"))
                        self.assertEqual(captured, [])
                        continue
                    self.assertEqual(len(captured), 1)
                    env = captured[0]["env"]
                    for key in self.GUARDED_KEYS:
                        if key in env:
                            self.assertTrue(
                                os.path.realpath(env[key]).startswith(root),
                                f"{runner_cls.__name__} {scenario}: {key}={env[key]}",
                            )
                            self.assertNotIn(env[key], leak_values, key)
                    for key in self.REQUIRED_KEYS[runner_cls.__name__]:
                        self.assertIn(key, env, f"{runner_cls.__name__} {scenario}")

    def test_fixtures_land_in_each_tools_own_skill_path(self) -> None:
        for runner_cls in self.RUNNERS:
            with self.subTest(tool=runner_cls.__name__), tempfile.TemporaryDirectory() as tmp:
                root = os.path.realpath(tmp)
                self._capture(runner_cls("/bin/tool"), "skill", root)
                found = [
                    os.path.join(dirpath, name)
                    for dirpath, _dirs, names in os.walk(root)
                    for name in names
                    if name == "SKILL.md"
                ]
                self.assertEqual(len(found), 1, found)
                with open(found[0], encoding="utf-8") as fh:
                    self.assertIn("BENCH-SKILL-SENTINEL", fh.read())

    def test_horon_registers_the_fixture_mcp_server_in_the_global_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = os.path.realpath(tmp)
            _result, captured = self._capture(bench.HoronRunner("/bin/horon"), "mcp", root)
            env = captured[0]["env"]
            self.assertEqual(env["HORON_AGENT_RUN_MCP"], "1")
            path = os.path.join(env["HORON_AGENT_HOME"], "mcp.json")
            with open(path, encoding="utf-8") as fh:
                config = json.load(fh)
        self.assertIn("bench", config["servers"])
        self.assertTrue(config["servers"]["bench"]["args"][0].endswith("bench_mcp_server.py"))
        self.assertIn("mcp__bench__bench_echo", captured[0]["argv"])

    def test_horon_skill_scenario_also_enables_mcp(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            _result, captured = self._capture(
                bench.HoronRunner("/bin/horon"), "skill", os.path.realpath(tmp)
            )
        self.assertEqual(captured[0]["env"]["HORON_AGENT_RUN_MCP"], "1")


class ScenarioReportTests(unittest.TestCase):
    def _result(self, name: str, label: str, skill=None, mcp=None, error=None) -> dict:
        return {
            "name": name,
            "label": label,
            "version": "v1",
            "version_ms": {"summary": {"median": 1.0}},
            "oneshot": {"ms": 100.0, "peak_rss_bytes": 40e6, "tool_roundtrip": True},
            "sessions_memory": {
                "points": [{"sessions": 1, "post_turn_rss_bytes": 45e6}],
                "first_session_idle_rss_bytes": 40e6,
                "each_additional_idle_rss_bytes": 3e6,
                "first_session_post_turn_rss_bytes": 45e6,
                "each_additional_post_turn_rss_bytes": 4e6,
                "skipped": [],
            },
            "skill": skill,
            "mcp": mcp,
            "install_size_bytes": 1024,
            "error": error,
        }

    def test_new_tables_carry_times_routes_and_reasons(self) -> None:
        results = [
            self._result("horon", "horon",
                         skill={"ms": 100.0, "peak_rss_bytes": 40e6, "tool_round_trip": True,
                                "via": "file-read", "summary": {"samples": 1}},
                         mcp={"ms": 200.0, "peak_rss_bytes": 50e6, "tool_round_trip": True,
                              "via": "mcp-tool", "mcp_ready_ms": 80.0,
                              "summary": {"samples": 1}}),
            self._result("pi", "pi",
                         skill={"ms": 250.0, "peak_rss_bytes": 60e6, "tool_round_trip": True,
                                "via": "file-read", "summary": {"samples": 1}},
                         mcp={"skipped": "pi has no MCP client"}),
        ]
        report = bc.build_report("compare", results, [])
        report["parameters"] = {"scenarios": ["oneshot", "sessions", "skill", "mcp"]}
        markdown = bench.render_markdown(report, [1], 1)
        self.assertIn("**Skill invocation**", markdown)
        self.assertIn("**MCP tool call**", markdown)
        self.assertIn("| Via |", markdown)
        self.assertIn("file-read", markdown)
        self.assertIn("mcp-tool", markdown)
        self.assertIn("2.50× more", markdown)
        self.assertIn("skipped: pi has no MCP client", markdown)
        self.assertIn("80.0 ms", markdown)

    def test_a_not_selected_scenario_renders_no_table(self) -> None:
        results = [self._result("horon", "horon")]
        report = bc.build_report("compare", results, [])
        report["parameters"] = {"scenarios": ["oneshot"]}
        markdown = bench.render_markdown(report, [1], 1)
        self.assertNotIn("**Skill invocation**", markdown)
        self.assertNotIn("**MCP tool call**", markdown)
        self.assertIn("**One-shot headless prompt**", markdown)

    def test_a_broken_row_still_gets_a_row_with_its_error(self) -> None:
        results = [self._result("horon", "horon", error="no such binary")]
        report = bc.build_report("compare", results, [])
        report["parameters"] = {"scenarios": ["skill"]}
        markdown = bench.render_markdown(report, [1], 1)
        self.assertIn("no such binary", markdown)

    def test_markdown_cell_does_not_break_the_table(self) -> None:
        self.assertEqual(bench.scenario_cell("a | b"), "a / b")


class McpFixtureTests(unittest.TestCase):
    def test_fixture_answers_initialize_list_call_and_rejects_unknown_methods(self) -> None:
        proc = subprocess.Popen(
            [sys.executable, str(bench.MCP_FIXTURE)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize",
             "params": {"protocolVersion": "2024-11-05", "capabilities": {}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
             "params": {"name": "bench_echo", "arguments": {"text": "bench-nonce-7"}}},
            {"jsonrpc": "2.0", "id": 4, "method": "tools/call",
             "params": {"name": "not_here", "arguments": {}}},
            {"jsonrpc": "2.0", "id": 5, "method": "resources/list"},
        ]
        out, _err = proc.communicate(
            "\n".join(json.dumps(item) for item in requests) + "\n", timeout=30
        )
        self.assertEqual(proc.returncode, 0)
        responses = [json.loads(line) for line in out.splitlines() if line.strip()]
        self.assertEqual([item["id"] for item in responses], [1, 2, 3, 4, 5])
        self.assertEqual(responses[0]["result"]["protocolVersion"], "2024-11-05")
        self.assertEqual(responses[1]["result"]["tools"][0]["name"], "bench_echo")
        self.assertEqual(responses[2]["result"]["content"][0]["text"], "bench-nonce-7")
        self.assertFalse(responses[2]["result"]["isError"])
        self.assertEqual(responses[3]["error"]["code"], -32602)
        self.assertEqual(responses[4]["error"]["code"], -32601)

    def test_fixture_exits_zero_on_eof(self) -> None:
        proc = subprocess.Popen(
            [sys.executable, str(bench.MCP_FIXTURE)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        out, _err = proc.communicate("", timeout=30)
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(out.strip(), "")


class SkillFixtureTests(unittest.TestCase):
    def test_fixture_has_frontmatter_and_the_sentinel_body(self) -> None:
        text = bench.SKILL_FIXTURE.read_text(encoding="utf-8")
        self.assertTrue(text.startswith("---\n"))
        self.assertIn("name: bench-skill", text)
        self.assertIn("description:", text)
        self.assertIn("BENCH-SKILL-SENTINEL", text)

    def test_install_skill_copies_it_under_the_requested_root(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            path = bench.install_skill(root)
            self.assertEqual(os.path.dirname(path), os.path.join(root, "bench-skill"))
            with open(path, encoding="utf-8") as fh:
                self.assertIn("BENCH-SKILL-SENTINEL", fh.read())


if __name__ == "__main__":
    unittest.main()
