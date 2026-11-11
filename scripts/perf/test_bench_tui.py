#!/usr/bin/env python3
"""Unit tests for `bench_tui.py`, run without either binary.

Everything timing a process is exercised by the workflow on three operating
systems; what is covered here is the logic that decides what a report says --
percentiles, the paired handoff difference, skip handling, the required keys,
screen scraping and the markdown table.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_tui  # noqa: E402


class PercentileTest(unittest.TestCase):
    def test_single_value_is_its_own_percentile(self):
        self.assertEqual(bench_tui.percentile([7.0], 0.95), 7.0)

    def test_empty_series_is_none(self):
        self.assertIsNone(bench_tui.percentile([], 0.95))

    def test_nearest_rank_never_invents_a_value(self):
        values = [1.0, 2.0, 3.0, 4.0, 5.0]
        self.assertEqual(bench_tui.percentile(values, 0.5), 3.0)
        self.assertEqual(bench_tui.percentile(values, 0.95), 5.0)

    def test_matches_the_shared_p95_helper(self):
        import bench_common as common

        values = [12.0, 3.0, 9.0, 44.0, 5.0, 6.0]
        self.assertEqual(bench_tui.percentile(values, 0.95), common.p95(values))

    def test_unsorted_and_missing_values_are_tolerated(self):
        self.assertEqual(bench_tui.percentile([5.0, None, 1.0, 3.0], 0.5), 3.0)


class SummaryTest(unittest.TestCase):
    def test_empty_summary_is_none(self):
        self.assertIsNone(bench_tui.summary([]))

    def test_summary_carries_the_shared_shape(self):
        stats = bench_tui.summary([4.0, 2.0, 6.0])
        self.assertEqual(stats["median"], 4.0)
        self.assertEqual(stats["min"], 2.0)
        self.assertEqual(stats["max"], 6.0)
        self.assertEqual(stats["samples"], 3)
        self.assertIn("p95", stats)

    def test_echo_summary_exposes_the_p95_it_is_named_for(self):
        stats = bench_tui.echo_summary([1.0, 2.0, 3.0, 4.0, 5.0])
        self.assertEqual(stats["p95"], 5.0)
        self.assertEqual(stats["median"], 3.0)
        self.assertEqual(stats["samples"], 5)

    def test_echo_summary_of_nothing_is_none(self):
        self.assertIsNone(bench_tui.echo_summary([]))


class HandoffTest(unittest.TestCase):
    def test_pairs_runs_by_index(self):
        self.assertEqual(
            bench_tui.paired_differences([10.0, 20.0], [4.0, 5.0]), [6.0, 15.0]
        )

    def test_stops_at_the_shorter_series(self):
        self.assertEqual(bench_tui.paired_differences([10.0, 20.0, 30.0], [4.0]), [6.0])

    def test_negative_difference_is_kept(self):
        self.assertEqual(bench_tui.paired_differences([1.0], [3.0]), [-2.0])

    def test_no_pairs_is_an_empty_series(self):
        self.assertEqual(bench_tui.paired_differences([], [1.0]), [])


class SkipTest(unittest.TestCase):
    def test_windows_skips_the_pty_metrics_with_the_exact_reason(self):
        reason = bench_tui.pty_skip_reason("win32", "/b/horon-agent.exe", "/b/horon-tui.exe")
        self.assertEqual(reason, "pty unavailable on Windows")
        self.assertEqual(bench_tui.WINDOWS_PTY_SKIP, "pty unavailable on Windows")

    def test_missing_tui_is_reported_as_its_own_skip(self):
        reason = bench_tui.pty_skip_reason("linux", "/b/horon-agent", None)
        self.assertIn("horon-tui", reason)

    def test_posix_with_both_binaries_runs(self):
        self.assertIsNone(bench_tui.pty_skip_reason("darwin", "/b/a", "/b/t"))
        self.assertIsNone(bench_tui.pty_skip_reason("linux", "/b/a", "/b/t"))

    def test_missing_render_bench_is_skipped(self):
        self.assertIsNone(bench_tui.render_skip_reason("/b/render_bench"))
        self.assertIn("render", bench_tui.render_skip_reason(None))


class ReportTest(unittest.TestCase):
    def test_every_metric_key_exists_even_when_unmeasured(self):
        document = bench_tui.report()
        for key, _label, _unit, _scalar in bench_tui.ROWS:
            self.assertIn(key, document)
            self.assertIsNone(document[key])

    def test_measured_values_replace_the_placeholders(self):
        document = bench_tui.report(install_size_bytes=1024)
        self.assertEqual(document["install_size_bytes"], 1024)
        self.assertIsNone(document["render_p50_ms"])

    def test_report_names_its_suite_and_documents_the_handoff(self):
        document = bench_tui.report()
        self.assertEqual(document["suite"], "tui")
        self.assertEqual(document["handoff_definition"], bench_tui.HANDOFF_DEFINITION)
        self.assertIn("bare `horon-agent`", bench_tui.HANDOFF_DEFINITION)
        self.assertIn("direct `horon-tui`", bench_tui.HANDOFF_DEFINITION)

    def test_report_is_json_serialisable(self):
        document = bench_tui.report(first_frame_ms=bench_tui.summary([1.0, 2.0]),
                                    skipped=[{"name": "pty", "reason": "x"}])
        self.assertEqual(json.loads(json.dumps(document))["suite"], "tui")
        self.assertEqual(document["skipped"], [{"name": "pty", "reason": "x"}])


class ScreenTextTest(unittest.TestCase):
    def test_escape_sequences_become_word_breaks(self):
        raw = "\x1b[2J\x1b[1;1HSend\x1b[3;4Ha\x1b[0m message"
        self.assertEqual(bench_tui.screen_text(raw).strip(), "Send a message")

    def test_a_full_screen_redraw_does_not_glue_words_together(self):
        raw = "Send\x1b[10;2Ha\x1b[10;4Hmessage"
        self.assertIn("Send a message", bench_tui.screen_text(raw))

    def test_cursor_forward_preserves_the_space_it_stands_for(self):
        self.assertEqual(bench_tui.screen_text("Send\x1b[1Ca message"), "Send a message")

    def test_osc_titles_are_dropped(self):
        raw = "\x1b]0;horon\x07Send a message"
        self.assertEqual(bench_tui.screen_text(raw).strip(), "Send a message")


class MarkdownTest(unittest.TestCase):
    def report(self) -> dict:
        return bench_tui.report(
            first_frame_ms=bench_tui.summary([10.0, 12.0, 14.0]),
            direct_first_frame_ms=bench_tui.summary([5.0, 6.0, 7.0]),
            handoff_ms=bench_tui.summary([5.0, 6.0, 7.0]),
            input_echo_p95_ms=bench_tui.echo_summary([1.0, 2.0]),
            idle_rss_bytes=bench_tui.summary([30_000_000.0]),
            post_turn_rss_bytes=bench_tui.summary([40_000_000.0]),
            render_p50_ms=bench_tui.summary([0.2, 0.3]),
            render_p95_ms=bench_tui.summary([0.5, 0.6]),
            install_size_bytes=8_000_000,
            render={"frames": 1000, "width": 120, "height": 40, "runs": 5},
        )

    def test_heading_names_the_tui_version(self):
        self.assertIn("### TUI (`horon-tui`)", bench_tui.markdown(self.report()))

    def test_every_metric_has_a_row(self):
        text = bench_tui.markdown(self.report())
        for _key, label, _unit, _scalar in bench_tui.ROWS:
            self.assertIn(f"| {label} |", text)

    def test_unmeasured_table_uses_the_missing_marker(self):
        text = bench_tui.markdown(bench_tui.report())
        self.assertIn("| First frame (bare `horon-agent`) | — | — | — | 0 |", text)

    def test_install_size_is_a_single_value_row(self):
        self.assertIn("| Install size (`horon-tui`) | 7.6 MB | — | 1 |", bench_tui.markdown(self.report()))

    def test_skip_reason_appears_in_the_table(self):
        document = self.report()
        document["skipped"] = [{"name": "pty", "reason": "pty unavailable on Windows"}]
        self.assertIn("pty unavailable on Windows", bench_tui.markdown(document))

    def test_error_appears_in_the_table(self):
        document = self.report()
        document["errors"] = [{"step": "turn", "error": "timeout"}]
        self.assertIn("turn: timeout", bench_tui.markdown(document))

    def test_render_detail_names_the_terminal_size(self):
        self.assertIn("1000 frames on a 120×40 terminal", bench_tui.markdown(self.report()))

    def test_headless_and_tui_numbers_stay_in_separate_tables(self):
        text = bench_tui.markdown(self.report())
        head, _, rest = text.partition("### TUI (`horon-tui`)")
        self.assertEqual(head.strip(), "")
        table = rest.split("| Metric |", 1)[1]
        # The TUI table holds only TUI rows: no headless suite metric is copied in.
        for suite in ("Compare", "Startup", "Loading", "Tools", "Reload", "Compile"):
            self.assertNotIn(f"| {suite}", table)
        self.assertEqual(text.count("| Metric |"), 1)


class FilesTest(unittest.TestCase):
    def test_report_round_trips_through_the_writer(self):
        document = bench_tui.report(install_size_bytes=4096)
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "tui.json")
            bench_tui.common.write_json(path, document)
            with open(path, encoding="utf-8") as handle:
                self.assertEqual(json.load(handle)["install_size_bytes"], 4096)

    def test_markdown_writer_appends_a_newline(self):
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "tui.md")
            bench_tui.common.write_markdown(path, "body")
            with open(path, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "body\n")


if __name__ == "__main__":
    unittest.main()
