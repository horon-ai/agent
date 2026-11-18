#!/usr/bin/env python3
"""Tests for the CPU pass of the tools benchmark, limited to what can mislead.

A CPU figure is only worth reporting when it cannot read lower than the truth:

* the `/proc` reader must find the time fields behind a process name that
  itself holds spaces and parentheses;
* a thread that exits inside a window takes its clock with it, so that window
  must fall back to the process counters instead of reporting the smaller sum;
* a system without `/proc`, or a call that failed inside a window, must report
  no figure and a reason, never a zero;
* two series whose ranges overlap are a tie, whichever median is lower.

No sockets, no binaries, no cargo: everything below runs pure or on tmp files.
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import bench_common as common  # noqa: E402
import bench_tools  # noqa: E402


def stat_line(comm: str, utime: int, stime: int, cutime: int, cstime: int) -> str:
    # pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt utime stime cutime cstime ...
    return "4242 (%s) S 1 4242 4242 0 -1 4194304 100 0 0 0 %d %d %d %d 20 0 3 0 100\n" % (
        comm,
        utime,
        stime,
        cutime,
        cstime,
    )


def write_thread(proc_root: Path, tid: str, cpu_ns: int, voluntary: int, involuntary: int) -> None:
    task = proc_root / "4242" / "task" / tid
    task.mkdir(parents=True, exist_ok=True)
    (task / "schedstat").write_text("%d 500 7\n" % cpu_ns, encoding="utf-8")
    (task / "status").write_text(
        "Name:\tworker\nvoluntary_ctxt_switches:\t%d\nnonvoluntary_ctxt_switches:\t%d\n"
        % (voluntary, involuntary),
        encoding="utf-8",
    )


def snapshot(process_ms: float, children_ms: float, threads: dict[str, tuple[int, int, int]]) -> dict:
    return {
        "process_ms": process_ms,
        "children_ms": children_ms,
        "threads": {
            tid: {"cpu_ns": cpu_ns, "voluntary": voluntary, "involuntary": involuntary}
            for tid, (cpu_ns, voluntary, involuntary) in threads.items()
        },
    }


class ProcessCpuSnapshot(unittest.TestCase):
    def _snapshot(self, proc_root: Path) -> dict | None:
        # 100 ticks per second, so one tick is 10 ms on every host that runs the test.
        with mock.patch.object(common.os, "sysconf", return_value=100, create=True):
            return common.process_cpu_snapshot(4242, proc_root=str(proc_root))

    def test_reads_times_behind_a_name_with_spaces_and_parentheses(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "4242").mkdir()
            (root / "4242" / "stat").write_text(stat_line("horon (agent) 1 2", 30, 12, 5, 2), encoding="utf-8")
            write_thread(root, "4242", 1_500_000, 10, 1)
            write_thread(root, "4250", 2_500_000, 4, 0)
            result = self._snapshot(root)
        self.assertEqual(result["process_ms"], 420.0)
        self.assertEqual(result["children_ms"], 70.0)
        self.assertEqual(
            result["threads"],
            {
                "4242": {"cpu_ns": 1_500_000, "voluntary": 10, "involuntary": 1},
                "4250": {"cpu_ns": 2_500_000, "voluntary": 4, "involuntary": 0},
            },
        )

    def test_missing_process_is_none(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(self._snapshot(Path(tmp)))

    def test_truncated_stat_line_is_none(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "4242").mkdir()
            (root / "4242" / "stat").write_text("4242 (horon) S 1 4242\n", encoding="utf-8")
            self.assertIsNone(self._snapshot(root))

    def test_thread_without_switch_counters_is_left_out(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "4242").mkdir()
            (root / "4242" / "stat").write_text(stat_line("horon", 1, 1, 0, 0), encoding="utf-8")
            write_thread(root, "4242", 1_000, 1, 0)
            half = root / "4242" / "task" / "4251"
            half.mkdir(parents=True)
            (half / "schedstat").write_text("9000 0 1\n", encoding="utf-8")
            (half / "status").write_text("Name:\tworker\n", encoding="utf-8")
            result = self._snapshot(root)
        self.assertEqual(sorted(result["threads"]), ["4242"])


class CpuUsageBetween(unittest.TestCase):
    def test_live_threads_use_the_nanosecond_clocks(self) -> None:
        before = snapshot(100.0, 20.0, {"1": (1_000_000, 10, 2), "2": (4_000_000, 5, 0)})
        after = snapshot(110.0, 50.0, {"1": (3_500_000, 14, 2), "2": (4_250_000, 6, 1)})
        usage = common.cpu_usage_between(before, after)
        self.assertEqual(usage["source"], "schedstat")
        self.assertAlmostEqual(usage["cpu_ms"], 2.75)
        self.assertEqual(usage["children_cpu_ms"], 30.0)
        self.assertEqual(usage["voluntary_switches"], 5)
        self.assertEqual(usage["involuntary_switches"], 1)
        self.assertEqual(usage["threads_exited"], 0)

    def test_a_thread_born_inside_the_window_counts_in_full(self) -> None:
        before = snapshot(100.0, 0.0, {"1": (1_000_000, 10, 0)})
        after = snapshot(110.0, 0.0, {"1": (2_000_000, 12, 0), "9": (3_000_000, 7, 1)})
        usage = common.cpu_usage_between(before, after)
        self.assertEqual(usage["source"], "schedstat")
        self.assertAlmostEqual(usage["cpu_ms"], 4.0)
        self.assertEqual(usage["voluntary_switches"], 9)
        self.assertEqual(usage["involuntary_switches"], 1)

    def test_an_exited_thread_never_reads_as_a_saving(self) -> None:
        before = snapshot(100.0, 0.0, {"1": (1_000_000, 10, 0), "2": (50_000_000, 5, 0)})
        after = snapshot(160.0, 0.0, {"1": (2_000_000, 12, 0)})
        usage = common.cpu_usage_between(before, after)
        # The surviving thread alone would say 1 ms; the process counters say 60 ms.
        self.assertEqual(usage["source"], "stat")
        self.assertEqual(usage["cpu_ms"], 60.0)
        self.assertIsNone(usage["voluntary_switches"])
        self.assertIsNone(usage["involuntary_switches"])
        self.assertEqual(usage["threads_exited"], 1)


class FakeClient:
    def __init__(self, payloads: list[dict]):
        self.payloads = payloads
        self.calls = 0

    def call(self, _command: str) -> tuple[float, int, dict]:
        payload = self.payloads[min(self.calls, len(self.payloads) - 1)]
        self.calls += 1
        return 1.0, 10, payload


def target_with(client: FakeClient) -> bench_tools.Target:
    target = bench_tools.Target("candidate", "/h")
    target.client = client
    target.process = mock.Mock(pid=4242)
    return target


CASE = {"label": "read", "tool": "read", "payload": lambda: {"file_path": "a"}}
OK = {"type": "debug_response", "ok": True, "output": "{}"}


class CpuWindow(unittest.TestCase):
    def test_no_proc_reports_a_reason_and_makes_no_call(self) -> None:
        client = FakeClient([OK])
        with mock.patch.object(common, "process_cpu_snapshot", return_value=None):
            usage, reason = bench_tools.cpu_window(target_with(client), CASE, 4)
        self.assertIsNone(usage)
        self.assertIn("/proc", reason)
        self.assertEqual(client.calls, 0)

    def test_a_failed_call_voids_the_window(self) -> None:
        client = FakeClient([OK, {"type": "debug_response", "ok": False, "output": "read refused"}])
        snap = snapshot(0.0, 0.0, {"1": (0, 0, 0)})
        with mock.patch.object(common, "process_cpu_snapshot", return_value=snap):
            usage, reason = bench_tools.cpu_window(target_with(client), CASE, 4)
        self.assertIsNone(usage)
        self.assertIn("read refused", reason)

    def test_an_expected_error_case_is_a_valid_window(self) -> None:
        client = FakeClient([{"type": "debug_response", "ok": False, "output": "Unknown tool"}])
        case = dict(CASE, expect_error=True)
        snaps = [snapshot(0.0, 0.0, {"1": (0, 0, 0)}), snapshot(10.0, 0.0, {"1": (6_000_000, 3, 0)})]
        with mock.patch.object(common, "process_cpu_snapshot", side_effect=snaps):
            usage, reason = bench_tools.cpu_window(target_with(client), case, 3)
        self.assertIsNone(reason)
        self.assertAlmostEqual(usage["cpu_ms"], 6.0)
        self.assertEqual(client.calls, 3)

    def test_a_window_that_lost_a_thread_is_taken_again(self) -> None:
        client = FakeClient([OK])
        snaps = [
            snapshot(0.0, 0.0, {"1": (0, 0, 0), "2": (0, 0, 0)}),
            snapshot(10.0, 0.0, {"1": (1_000_000, 1, 0)}),
            snapshot(10.0, 0.0, {"1": (1_000_000, 1, 0)}),
            snapshot(20.0, 0.0, {"1": (3_000_000, 5, 0)}),
        ]
        with mock.patch.object(common, "process_cpu_snapshot", side_effect=snaps):
            usage, reason = bench_tools.cpu_window(target_with(client), CASE, 2)
        self.assertIsNone(reason)
        self.assertEqual(usage["source"], "schedstat")
        self.assertAlmostEqual(usage["cpu_ms"], 2.0)
        self.assertEqual(client.calls, 4)


def window(cpu_ms: float, children: float = 0.0, voluntary: int | None = 0, source: str = "schedstat") -> dict:
    return {
        "cpu_ms": cpu_ms,
        "children_cpu_ms": children,
        "voluntary_switches": voluntary,
        "involuntary_switches": None if voluntary is None else 0,
        "source": source,
        "threads_exited": 0 if source == "schedstat" else 1,
    }


class CpuSummary(unittest.TestCase):
    def test_figures_are_per_call(self) -> None:
        summary = bench_tools.summarize_cpu([window(10.0, 40.0, 20), window(30.0, 80.0, 60)], 10, None)
        self.assertIsNone(summary["cpu_error"])
        cpu = summary["cpu"]
        self.assertEqual(cpu["cpu_ms_per_call"]["min"], 1.0)
        self.assertEqual(cpu["cpu_ms_per_call"]["max"], 3.0)
        self.assertEqual(cpu["cpu_ms_per_call"]["samples"], 2)
        self.assertEqual(cpu["children_cpu_ms_per_call"]["median"], 6.0)
        self.assertEqual(cpu["voluntary_switches_per_call"]["median"], 4.0)
        self.assertEqual(cpu["sources"], ["schedstat"])

    def test_an_error_leaves_no_figure(self) -> None:
        summary = bench_tools.summarize_cpu([window(10.0)], 10, "call failed inside the CPU window: boom")
        self.assertIsNone(summary["cpu"])
        self.assertEqual(summary["cpu_error"], "call failed inside the CPU window: boom")

    def test_no_window_leaves_no_figure(self) -> None:
        summary = bench_tools.summarize_cpu([], 10, None)
        self.assertIsNone(summary["cpu"])
        self.assertTrue(summary["cpu_error"])

    def test_a_tick_window_drops_the_switch_counts(self) -> None:
        windows = [window(10.0, voluntary=20), window(20.0, voluntary=None, source="stat")]
        cpu = bench_tools.summarize_cpu(windows, 10, None)["cpu"]
        self.assertIsNone(cpu["voluntary_switches_per_call"])
        self.assertIsNone(cpu["involuntary_switches_per_call"])
        self.assertEqual(cpu["sources"], ["schedstat", "stat"])


def row(name: str, cpu_windows: list[dict] | None, error: str | None = None) -> dict:
    result = bench_tools.summarize_case(name, name, [1.0], [10], 20, [])
    if cpu_windows is not None or error:
        result.update(bench_tools.summarize_cpu(cpu_windows or [], 10, error))
    return result


class CpuMarkdown(unittest.TestCase):
    def test_a_latency_only_run_has_no_cpu_section(self) -> None:
        text = bench_tools.markdown([row("read", None)], [])
        self.assertNotIn("Tool CPU per call", text)

    def test_an_unmeasured_case_is_a_dash_with_its_reason(self) -> None:
        rows = [
            row("read", [window(5.0, voluntary=30)]),
            row("edit", None, "call failed inside the CPU window: edit refused"),
        ]
        text = bench_tools.markdown(rows, [])
        self.assertIn("### Tool CPU per call", text)
        self.assertIn("| read | 0.500 ms | 0.500 ms | 0.500 ms | 0.000 ms | 3.00 | 0.00 | 1 × 10 | schedstat |", text)
        self.assertIn("| edit | — | — | — | — | — | — | 0 | — |", text)
        self.assertIn("CPU unmeasured: call failed inside the CPU window: edit refused", text)
        self.assertNotIn("candidate against baseline", text)

    def test_overlapping_ranges_are_a_tie(self) -> None:
        ours = row("read", [window(4.0), window(6.0)])
        ours["baseline"] = bench_tools.summarize_cpu([window(5.0), window(9.0)], 10, None)
        lower = row("write", [window(2.0), window(3.0)])
        lower["baseline"] = bench_tools.summarize_cpu([window(4.0), window(6.0)], 10, None)
        higher = row("ls", [window(8.0), window(9.0)])
        higher["baseline"] = bench_tools.summarize_cpu([window(4.0), window(6.0)], 10, None)
        text = bench_tools.markdown([ours, lower, higher], [])
        self.assertIn("### Tool CPU per call: candidate against baseline", text)
        self.assertIn("| read | 0.500 ms | 0.400 ms – 0.600 ms | 0.700 ms | 0.500 ms – 0.900 ms | -28.6% | tie |", text)
        self.assertIn("| write | 0.250 ms | 0.200 ms – 0.300 ms | 0.500 ms | 0.400 ms – 0.600 ms | -50.0% | lower |", text)
        self.assertIn("| ls | 0.850 ms | 0.800 ms – 0.900 ms | 0.500 ms | 0.400 ms – 0.600 ms | +70.0% | higher |", text)

    def test_a_baseline_without_a_figure_is_not_compared(self) -> None:
        ours = row("read", [window(4.0)])
        ours["baseline"] = bench_tools.summarize_cpu([], 10, "baseline call failed")
        text = bench_tools.markdown([ours], [])
        self.assertNotIn("candidate against baseline", text)


class Arguments(unittest.TestCase):
    def _rejects(self, *argv: str) -> None:
        with mock.patch.object(sys, "stderr", open(os.devnull, "w", encoding="utf-8")) as sink:
            try:
                with self.assertRaises(SystemExit):
                    bench_tools.parse_args(list(argv))
            finally:
                sink.close()

    def test_the_cpu_pass_is_off_unless_asked_for(self) -> None:
        args = bench_tools.parse_args([])
        self.assertEqual(args.cpu_iterations, 0)
        self.assertIsNone(args.baseline_binary)

    def test_negative_cpu_iterations_are_refused(self) -> None:
        self._rejects("--cpu-iterations", "-1")

    def test_zero_cpu_rounds_are_refused(self) -> None:
        self._rejects("--cpu-rounds", "0")


class MissingBaseline(unittest.TestCase):
    def test_a_missing_baseline_binary_is_a_reason_not_a_daemon(self) -> None:
        with mock.patch.object(bench_tools.subprocess, "Popen") as popen:
            target, reason = bench_tools.start_baseline("/no/such/horon-agent", Path("."), mock.Mock())
        self.assertIsNone(target)
        self.assertIn("/no/such/horon-agent", reason)
        popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
