#!/usr/bin/env python3
"""Tests for bench_docs.py, the writer of RESULTS.md's generated benchmark block.

The script commits straight to `main` and `dev` from CI, so the contracts pinned
here are the ones whose failure would damage the document or hide a gap:

* the hand-written text around the block survives byte-for-byte, and a second
  run replaces the block instead of growing it;
* a missing or doubled marker is an error, never a silent append;
* the recorded commit round-trips, because the next run decides whether to
  measure at all from it;
* a suite that produced nothing shows up as unmeasured, not as an empty section;
* fragment headings nest under the block without touching code fences.
"""

from __future__ import annotations

import datetime as dt
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import bench_docs  # noqa: E402

DOC = (
    "# 성능\n\n손으로 쓴 앞부분.\n\n## 자동 측정\n\n"
    "<!-- bench-auto:start -->\n\n아직 없음.\n\n<!-- bench-auto:end -->\n\n## 뒷부분\n\n손으로 쓴 뒷부분.\n"
)
NOW = dt.datetime(2026, 11, 7, 3, 4, tzinfo=dt.timezone.utc)


class BlockReplacement(unittest.TestCase):
    def test_text_outside_the_block_is_preserved_and_reruns_do_not_grow(self) -> None:
        once = bench_docs.replace_block(DOC, "abc1234", "첫 측정")
        twice = bench_docs.replace_block(once, "def5678", "둘째 측정")
        head, tail = DOC.split("<!-- bench-auto:start -->")[0], DOC.split("<!-- bench-auto:end -->")[1]
        for text in (once, twice):
            self.assertTrue(text.startswith(head))
            self.assertTrue(text.endswith("<!-- bench-auto:end -->" + tail))
        self.assertNotIn("첫 측정", twice)
        self.assertIn("둘째 측정", twice)
        self.assertEqual(twice.count("bench-auto:start"), 1)

    def test_recorded_commit_round_trips(self) -> None:
        self.assertIsNone(bench_docs.last_commit(DOC))
        sha = "0123456789abcdef0123456789abcdef01234567"
        self.assertEqual(bench_docs.last_commit(bench_docs.replace_block(DOC, sha, "x")), sha)

    def test_missing_or_doubled_markers_are_errors(self) -> None:
        cases = {
            "no markers": "# 성능\n",
            "no end": "<!-- bench-auto:start -->\n",
            "two starts": DOC + "<!-- bench-auto:start -->\n",
            "end before start": "<!-- bench-auto:end -->\n<!-- bench-auto:start -->\n",
        }
        for name, text in cases.items():
            with self.subTest(case=name), self.assertRaises(bench_docs.DocError):
                bench_docs.replace_block(text, "abc1234", "x")

    def test_non_sha_commit_is_rejected(self) -> None:
        with self.assertRaises(bench_docs.DocError):
            bench_docs.replace_block(DOC, "main --> <!-- x", "x")


def job_dir(results: Path, name: str, platform: str, status: str = "success", files: dict | None = None) -> Path:
    directory = results / name
    directory.mkdir()
    (directory / "platform.txt").write_text(platform + "\n", encoding="utf-8")
    (directory / "status.txt").write_text(status + "\n", encoding="utf-8")
    for file_name, text in (files or {}).items():
        (directory / file_name).write_text(text, encoding="utf-8")
    return directory


class Rendering(unittest.TestCase):
    def test_missing_suites_are_listed_as_unmeasured(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            job_dir(
                results,
                "benchmark-linux-1",
                "linux",
                files={
                    "tools.md": "### Tool latency\n\n| a |\n",
                    "startup.txt": "serve ready 12 ms\n",
                    "reload.md": "   \n",
                },
            )
            body = bench_docs.render_body(results, "abc1234", now=NOW)
        self.assertIn("#### Tool latency", body)
        self.assertIn("#### 기동\n\n```text\nserve ready 12 ms\n```", body)
        linux = body.split("### Linux", 1)[1].split("### macOS", 1)[0]
        for name in ("compare.md", "loading-cold.txt", "loading-warm.txt", "reload.md", "compile.md"):
            with self.subTest(fragment=name):
                self.assertIn(f"측정 안 됨: 이 실행에서 `{name}` 파일이", linux)
        self.assertIn("2026-11-07 03:04 UTC", body)

    def test_every_os_gets_its_section_in_fixed_order_even_without_results(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            # Directory names do not decide the OS or the order; platform.txt does.
            job_dir(results, "a-windows", "windows", files={"tools.md": "### W\n"})
            job_dir(results, "b-linux", "linux", files={"tools.md": "### L\n"})
            body = bench_docs.render_body(results, "abc1234", now=NOW)
        positions = [body.index(f"\n### {title}\n") for title in ("Linux", "macOS", "Windows")]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("| macOS | `—` | `—` | 결과 없음 |", body)
        self.assertIn("이 실행에서 macOS 결과가 없습니다", body)
        self.assertLess(body.index("#### L"), body.index("#### W"))

    def test_a_failed_job_is_marked_and_its_partial_results_kept(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            job_dir(
                results,
                "benchmark-windows-1",
                "windows",
                status="failure",
                files={"runner.txt": "windows-latest\n", "horon-version.txt": "horon-agent v1\n", "tools.md": "### W\n"},
            )
            body = bench_docs.render_body(results, "abc1234", now=NOW)
        self.assertIn("| Windows | `windows-latest` | `horon-agent v1` | 일부 실패 (`failure`) |", body)
        windows = body.split("### Windows", 1)[1]
        self.assertIn("끝까지 성공하지 못했습니다", windows)
        self.assertIn("#### W", windows)

    def test_two_jobs_claiming_one_os_are_an_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            job_dir(results, "one", "linux")
            job_dir(results, "two", "linux")
            with self.assertRaises(bench_docs.DocError):
                bench_docs.render_body(results, "abc1234", now=NOW)

    def test_headings_nest_under_the_block_and_fences_are_untouched(self) -> None:
        source = "# Comparison\n\n## Memory\n\n```sh\n# not a heading\n```\n"
        out = bench_docs.demote_headings(source)
        self.assertEqual(out.splitlines()[0], "#### Comparison")
        self.assertIn("##### Memory", out)
        self.assertIn("# not a heading", out.splitlines())


class CommandLine(unittest.TestCase):
    def test_render_then_last_commit(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            doc = Path(tmp) / "RESULTS.md"
            doc.write_text(DOC, encoding="utf-8")
            results = Path(tmp) / "bench-results"
            results.mkdir()
            code = bench_docs.main(
                ["render", "--doc", str(doc), "--results", str(results), "--commit", "abc1234"]
            )
            self.assertEqual(code, 0)
            self.assertEqual(bench_docs.last_commit(doc.read_text(encoding="utf-8")), "abc1234")

    def test_render_fails_without_markers_and_leaves_the_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            doc = Path(tmp) / "RESULTS.md"
            doc.write_text("# 성능\n", encoding="utf-8")
            code = bench_docs.main(
                ["render", "--doc", str(doc), "--results", tmp, "--commit", "abc1234"]
            )
            self.assertEqual(code, 1)
            self.assertEqual(doc.read_text(encoding="utf-8"), "# 성능\n")


if __name__ == "__main__":
    unittest.main()
