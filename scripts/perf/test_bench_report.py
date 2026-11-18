#!/usr/bin/env python3
"""Contract tests for bench_report.py: what a reader of the report is told about a missing value."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench_report  # noqa: E402


def result(name: str, oneshot_ms: float | None, **extra) -> dict:
    scenario = {"ms": oneshot_ms, "summary": {"median": oneshot_ms, "min": oneshot_ms, "max": oneshot_ms, "samples": 5},
                "peak_rss_bytes": 1024, "tool_roundtrip": True}
    scenario.update(extra)
    return {"name": name, "label": name, "version": "1", "oneshot": scenario, "install_size_bytes": 10}


class CollectTest(unittest.TestCase):
    def collect(self, results: list[dict], skipped: list[dict] | None = None) -> dict:
        run = {"results": results, "skipped": skipped or [], "host": {}, "parameters": {}, "started_at": "2026-11-15T00:00:00Z"}
        return bench_report.collect({"linux": run})["os"]["linux"]["metrics"]["03-oneshot-time"]

    def test_rows_sorted_fastest_first_with_spread(self) -> None:
        block = self.collect([result("horon", 60.0), result("jcode", 300.0), result("codex", 120.0)])
        self.assertEqual([r["name"] for r in block["rows"]], ["horon", "codex", "jcode"])
        self.assertEqual(block["rows"][0]["samples"], 5)

    def test_failed_round_trip_is_unmeasured_with_a_reason_not_zero(self) -> None:
        block = self.collect([result("horon", 60.0), result("codex", 90.0, tool_roundtrip=False)])
        self.assertEqual([r["name"] for r in block["rows"]], ["horon"])
        self.assertEqual(block["missing"], [{"tool": "codex", "reason": "도구 왕복 확인 실패"}])

    def test_tool_that_did_not_install_keeps_its_recorded_reason(self) -> None:
        run = {"results": [result("horon", 60.0)], "skipped": [{"name": "jcode", "reason": "install failed: 404"}],
               "host": {}, "parameters": {}, "started_at": "2026-11-15T00:00:00Z"}
        data = bench_report.collect({"linux": run, "macos": {**run, "results": [result("horon", 60.0), result("jcode", 300.0)]}})
        missing = data["os"]["linux"]["metrics"]["03-oneshot-time"]["missing"]
        self.assertEqual(missing, [{"tool": "jcode", "reason": "install failed: 404"}])


class ScopeIdsTest(unittest.TestCase):
    def test_ids_and_references_get_the_prefix_together(self) -> None:
        svg = '<clipPath id="a"/><g clip-path="url(#a)"/><use href="#a"/><clipPath id="ab"/>'
        scoped = bench_report._scope_ids(svg, "p-")
        self.assertEqual(scoped, '<clipPath id="p-a"/><g clip-path="url(#p-a)"/><use href="#p-a"/><clipPath id="p-ab"/>')


if __name__ == "__main__":
    unittest.main()
