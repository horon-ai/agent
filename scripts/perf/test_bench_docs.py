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
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import bench_charts  # noqa: E402
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
            body = bench_docs.render_body(results, "abc1234", now=NOW, lang="ko")
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
            body = bench_docs.render_body(results, "abc1234", now=NOW, lang="ko")
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
            body = bench_docs.render_body(results, "abc1234", now=NOW, lang="ko")
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
                bench_docs.render_body(results, "abc1234", now=NOW, lang="ko")

    def test_headings_nest_under_the_block_and_fences_are_untouched(self) -> None:
        source = "# Comparison\n\n## Memory\n\n```sh\n# not a heading\n```\n"
        out = bench_docs.demote_headings(source)
        self.assertEqual(out.splitlines()[0], "#### Comparison")
        self.assertIn("##### Memory", out)
        self.assertIn("# not a heading", out.splitlines())


    def test_english_is_the_default_and_both_languages_share_the_structure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            job_dir(results, "benchmark-linux-1", "linux", files={"startup.txt": "serve ready 12 ms\n"})
            en = bench_docs.render_body(results, "abc1234", now=NOW)
            ko = bench_docs.render_body(results, "abc1234", now=NOW, lang="ko")
        self.assertIn("#### Startup\n\n```text\nserve ready 12 ms\n```", en)
        self.assertIn("Not measured: this run did not produce `compare.md`.", en)
        self.assertIn("| macOS | `—` | `—` | No result |", en)
        self.assertIn("#### 기동\n\n```text\nserve ready 12 ms\n```", ko)
        headings = lambda body: [line for line in body.splitlines() if line.startswith("#")]
        self.assertEqual(len(headings(en)), len(headings(ko)))
        self.assertEqual(en.count("\n|"), ko.count("\n|"))
        with self.assertRaises(bench_docs.DocError):
            bench_docs.render_body(results, "abc1234", now=NOW, lang="fr")
PINNED_HEADLESS = """\
`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `abc1234`
- Measured at: 2026-11-07 03:04 UTC

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent 0.9.0` | Success |
| macOS | `—` | `—` | No result |
| Windows | `—` | `—` | No result |

### Linux

#### Competitor comparison

Not measured: this run did not produce `compare.md`.

#### User-installed package cost (headless, mock model)

| Build | Scenario | Turn (median) | Tool round trip |
| --- | --- | --- | --- |
| candidate | empty | 120.0 ms | no |
| candidate | installed | 121.0 ms | no |
| candidate | called | 168.0 ms | yes |

#### Startup

```text
serve ready 12 ms
```

#### Session loading (COLD)

Not measured: this run did not produce `loading-cold.txt`.

#### Session loading (WARM)

Not measured: this run did not produce `loading-warm.txt`.

#### Tool latency

| case | ms |
| --- | --- |
| read | 3.0 |

#### Reload storm

Not measured: this run did not produce `reload.md`.

#### Compile loop

Not measured: this run did not produce `compile.md`.

### macOS

This run has no macOS result. The job did not start or did not upload its result files.

### Windows

This run has no Windows result. The job did not start or did not upload its result files."""


def tui_json() -> str:
    def stat(median: float, low: float, high: float, n: int = 5) -> dict:
        return {"median": median, "min": low, "max": high, "samples": n}

    return json.dumps({
        "suite": "tui",
        "schema": 1,
        "started_at": "2026-11-11T02:00:00Z",
        "first_frame_ms": stat(120.0, 110.0, 130.0),
        "direct_first_frame_ms": stat(95.0, 90.0, 100.0),
        "handoff_ms": stat(25.0, 20.0, 30.0),
        "idle_rss_bytes": stat(48e6, 47e6, 49e6),
        "post_turn_rss_bytes": stat(52e6, 51e6, 53e6),
        "input_echo_p95_ms": stat(4.0, 3.0, 5.0),
        "render_p50_ms": stat(0.4, 0.35, 0.45),
        "render_p95_ms": stat(0.9, 0.8, 1.0),
        "install_size_bytes": {"median": 8e6, "min": 8e6, "max": 8e6, "samples": 1},
        "skipped": [],
        "errors": [],
    })


PACKAGES_MD = """\
# User-installed package cost (headless, mock model)

| Build | Scenario | Turn (median) | Tool round trip |
| --- | --- | --- | --- |
| candidate | empty | 120.0 ms | no |
| candidate | installed | 121.0 ms | no |
| candidate | called | 168.0 ms | yes |
"""


def tui_md() -> str:
    return (
        "### TUI (`horon-tui`)\n\n| Metric | Median | Min\u2013max | Samples |\n"
        "| --- | --- | --- | --- |\n| First frame (bare `horon-agent`) | 120 ms | 110\u2013130 ms | 5 |\n"
    )


def headless_of(body: str) -> str:
    """The headless half of a rendered block, without the separator before the TUI."""
    return body.split("\n\n## TUI (`horon-tui`)", 1)[0]


class TwoVersionSections(unittest.TestCase):
    """The block carries the headless suites and the TUI side by side.

    The headless half must stay byte-identical to the renderer that existed
    before the TUI suite, so an unchanged input keeps an unchanged document; the
    TUI half is appended and never shares a table with the headless numbers.
    """

    def fixture(self, results: Path, with_tui: bool) -> None:
        linux = job_dir(results, "benchmark-linux-1", "linux", files={
            "runner.txt": "ubuntu-24.04\n",
            "horon-version.txt": "horon-agent 0.9.0\n",
            "packages.md": PACKAGES_MD,
            "startup.txt": "serve ready 12 ms\n",
            "tools.md": "### Tool latency\n\n| case | ms |\n| --- | --- |\n| read | 3.0 |\n",
            "reload.md": "   \n",
        })
        if with_tui:
            (linux / "tui.json").write_text(tui_json(), encoding="utf-8")
            (linux / "tui.md").write_text(tui_md(), encoding="utf-8")

    def test_headless_body_matches_the_pinned_renderer_output(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.fixture(results, with_tui=True)
            body = bench_docs.headless_body(
                bench_docs.find_platform_dirs(results), "abc1234", None, NOW, "en"
            )
        self.assertEqual(body, PINNED_HEADLESS)

    def test_tui_results_add_a_second_version_section(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.fixture(results, with_tui=True)
            body = bench_docs.render_body(results, "abc1234", now=NOW)
        headless_at = body.index("## Headless (`horon-agent`)")
        tui_at = body.index("## TUI (`horon-tui`)")
        self.assertLess(headless_at, tui_at)
        # The headless half is exactly the pinned bytes, wrapped in its heading.
        self.assertEqual(headless_of(body), "## Headless (`horon-agent`)\n\n" + PINNED_HEADLESS)
        tui = body[tui_at:]
        self.assertIn("#### TUI (`horon-tui`)", tui)
        self.assertIn("| First frame (bare `horon-agent`) | 120 ms |", tui)
        # No headless label leaks into the TUI section.
        for label in ("Compile loop", "Reload storm", "Session loading"):
            self.assertNotIn(label, tui)

    def test_a_missing_tui_result_still_renders_every_headless_fragment(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.fixture(results, with_tui=False)
            body = bench_docs.render_body(results, "abc1234", now=NOW)
        self.assertEqual(headless_of(body), "## Headless (`horon-agent`)\n\n" + PINNED_HEADLESS)
        for heading in ("#### Startup", "#### Tool latency", "#### Compile loop"):
            self.assertIn(heading, body.split("## TUI (`horon-tui`)", 1)[0])
        # The missing TUI is stated once, not fatal.
        self.assertIn("This run did not produce `tui.md` for Linux", body)
        self.assertIn("The job did not start or did not upload its result files.", body.split("## TUI")[1])

    def test_korean_names_both_versions(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.fixture(results, with_tui=True)
            body = bench_docs.render_body(results, "abc1234", now=NOW, lang="ko")
        self.assertIn("## 헤드리스 (`horon-agent`)", body)
        self.assertIn("## TUI (`horon-tui`)", body)

    def test_tui_charts_come_only_from_tui_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.fixture(results, with_tui=True)
            dirs = bench_docs.find_platform_dirs(results)
            files = bench_charts.render_tui_charts(dirs, "abc1234", "en", "42")
        self.assertEqual(sorted(files), ["tui-01-latency.svg", "tui-02-memory.svg"])
        latency = files["tui-01-latency.svg"]
        self.assertIn("abc1234", latency)
        # The chart reads the same key the suite writes; a mismatch shows up as
        # "not measured" bars even though the number is in tui.json.
        self.assertIn("tui first frame", latency)
        self.assertNotIn("not measured", latency)
        # The comparison numbers of the other suites never enter these charts.
        self.assertNotIn("Tool latency", latency)
        memory = files["tui-02-memory.svg"]
        self.assertIn("(MB, lower is better)", memory)
        self.assertIn("idle RSS", memory)
        self.assertIn("install size", memory)

    def test_no_tui_json_means_no_tui_charts_and_no_tui_links(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.fixture(results, with_tui=False)
            dirs = bench_docs.find_platform_dirs(results)
            self.assertEqual(bench_charts.render_tui_charts(dirs, "abc1234", "en"), {})
            body = bench_docs.render_body(results, "abc1234", now=NOW)
        self.assertNotIn("tui-01-latency.svg", body)


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


def compare_result(name: str, label: str, ms: float | None, rss: float | None, roundtrip: bool = True,
                   fsync: float | None = None) -> dict:
    oneshot = None if ms is None else {"ms": ms, "peak_rss_bytes": rss, "tool_roundtrip": roundtrip,
                                       "fsync_calls": fsync}
    return {"name": name, "label": label, "version_ms": {"summary": {"median": 3.0}},
            "oneshot": oneshot, "install_size_bytes": 50 * 1024 * 1024, "error": None,
            "sessions_memory": {"points": [{"sessions": 1, "post_turn_rss_bytes": 40e6},
                                           {"sessions": 10, "post_turn_rss_bytes": 45e6}],
                                "first_session_post_turn_rss_bytes": 40e6,
                                "each_additional_post_turn_rss_bytes": 400e3}}


def compare_json(*results: dict, skipped: list | None = None) -> str:
    return json.dumps({"suite": "compare", "started_at": "2026-11-10T00:03:56Z",
                       "results": list(results), "skipped": skipped or []})


CHART_DOC = (
    "# 성능\n\n## 차트\n\n<!-- bench-charts:start -->\n<!-- bench-charts:end -->\n\n## 자동 측정\n\n"
    "<!-- bench-auto:start -->\n\n아직 없음.\n\n<!-- bench-auto:end -->\n"
)


class Charts(unittest.TestCase):
    """Charts are committed by CI, so the same input must give the same bytes,
    and a value the table blanks must never become a zero-length bar."""

    def make_results(self, results: Path) -> None:
        job_dir(results, "l", "linux", files={"compare.json": compare_json(
            compare_result("jcode", "jcode", 150.0, 38e6, fsync=5),
            compare_result("horon", "horon", 60.0, 37e6, fsync=14),
            compare_result("pi", "pi", 400.0, 90e6, roundtrip=False),
        )})
        job_dir(results, "m", "macos", files={"compare.json": compare_json(
            compare_result("horon", "horon", 200.0, 30e6),
            compare_result("jcode", "jcode", 100.0, 31e6),
            skipped=[{"name": "syscalls", "reason": "strace not available"}],
        )})

    def test_same_input_gives_identical_svg(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.make_results(results)
            dirs = bench_docs.find_platform_dirs(results)
            first = bench_charts.render_charts(dirs, "abc1234", "en", "42")
            second = bench_charts.render_charts(dirs, "abc1234", "en", "42")
        self.assertEqual(first, second)
        self.assertEqual(len(first), len(bench_charts.METRICS) + 1)
        for name, svg in first.items():
            with self.subTest(chart=name):
                self.assertTrue(svg.startswith("<svg "))
                self.assertIn("abc1234", svg)

    def test_unmeasured_values_are_labelled_not_drawn_as_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            self.make_results(results)
            files = bench_charts.render_charts(bench_docs.find_platform_dirs(results), "abc1234", "en")
        oneshot = files["03-oneshot-time.svg"]
        # pi's round trip failed: no bar value, an explicit label instead.
        self.assertIn("not measured", oneshot)
        self.assertNotIn(">400", oneshot)
        # Windows produced nothing at all.
        self.assertIn("This run has no comparison result for Windows.", oneshot)
        fsync = files["09-fsync-calls.svg"]
        self.assertIn("(strace is not available on this runner.)", fsync)
        # horon is drawn first and in blue on every chart.
        self.assertIn(bench_charts.BLUE, oneshot)
        summary = files["00-summary.svg"]
        self.assertIn("60.0 ms · #1 of 2", summary)  # pi has no comparable value, so it is not ranked
        self.assertIn("200 ms · #2 of 2", summary)

    def test_render_writes_charts_only_when_the_document_has_the_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            results = root / "bench-results"
            results.mkdir()
            self.make_results(results)
            with_block = root / "a" / "RESULTS.ko.md"
            without = root / "b" / "RESULTS.md"
            for doc, text in ((with_block, CHART_DOC), (without, DOC)):
                doc.parent.mkdir()
                doc.write_text(text, encoding="utf-8")
            for doc, lang in ((with_block, "ko"), (without, "en")):
                code = bench_docs.main(["render", "--doc", str(doc), "--results", str(results),
                                        "--commit", "abc1234", "--lang", lang])
                self.assertEqual(code, 0)
            text = with_block.read_text(encoding="utf-8")
            self.assertIn("![벤치마크 요약](charts/00-summary.ko.svg)", text)
            self.assertTrue((with_block.parent / "charts" / "03-oneshot-time.ko.svg").is_file())
            self.assertFalse((without.parent / "charts").exists())
            # A second run rewrites the block instead of growing it.
            bench_docs.main(["render", "--doc", str(with_block), "--results", str(results),
                             "--commit", "abc1234", "--lang", "ko"])
            self.assertEqual(with_block.read_text(encoding="utf-8"), text)

    def test_no_comparison_gives_a_note_and_no_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            job_dir(root, "l", "linux")
            doc = root / "RESULTS.md"
            out = bench_docs.write_charts(CHART_DOC, doc, root, "abc1234")
        self.assertIn("has no competitor comparison", out)
        self.assertFalse((root / "charts").exists())

    def test_doubled_chart_markers_are_errors(self) -> None:
        with self.assertRaises(bench_docs.DocError):
            bench_docs.charts_span(CHART_DOC + "<!-- bench-charts:start -->\n")



def summary_result(name: str, version: str, oneshot: float, rss: float, skill: float, mcp: float,
                   per_session: float, install: float, body: int = 3699, tools: int = 2358) -> dict:
    def stat(value: float) -> dict:
        return {"median": value, "min": value - 1, "max": value + 1, "samples": 5}
    return {
        "name": name, "label": name, "version": version, "error": None,
        "version_ms": {"summary": stat(3.3)},
        "oneshot": {"ms": oneshot, "peak_rss_bytes": rss, "tool_roundtrip": True, "summary": stat(oneshot),
                    "runs": [{"request_bytes": [{"body_bytes": body, "tools_bytes": tools, "tool_count": 1}]}]},
        "skill": {"ms": skill, "summary": stat(skill)},
        "mcp": {"ms": mcp, "summary": stat(mcp)},
        "install_size_bytes": install,
        "sessions_memory": {"points": [{"sessions": 1, "post_turn_rss_bytes": 40e6},
                                       {"sessions": 10, "post_turn_rss_bytes": 45e6}],
                            "each_additional_post_turn_rss_bytes": per_session},
    }


RELEASE = {
    "tool": "jcode", "selection": "latest", "tag": "v1.0.1", "version": "1.0.1",
    "commit": "10114429c", "asset": "jcode-linux-x86_64.tar.gz",
    "sha256": "ab" * 32, "download_url": "https://github.com/1jehuang/jcode/releases/download/v1.0.1/jcode-linux-x86_64.tar.gz",
    "served_url": "https://github.com/1jehuang/jcode/releases/download/v1.0.1/jcode-linux-x86_64.tar.gz",
    "served_by": "release-download", "version_output": "jcode v1.0.1 (10114429c)",
}


def summary_results(results: Path, with_release: bool = True) -> None:
    job_dir(results, "benchmark-linux-1", "linux", files={
        "runner.txt": "ubuntu-24.04\n",
        "compare.json": json.dumps({
            "suite": "compare", "started_at": "2026-11-10T00:03:56Z",
            "results": [
                summary_result("jcode", "jcode v1.0.1 (10114429c)", 150.0, 38.6e6, 57.0, 179.0, 160.0e3, 163.4e6,
                               body=3245, tools=1433),
                summary_result("horon", "horon-agent v0.1.0-dev (50d24d7)", 60.0, 38.4e6, 36.0, 62.0, 80.0e3, 112.1e6),
            ],
            "skipped": [],
        }),
    })
    if with_release:
        (results / "benchmark-linux-1" / "jcode-release.json").write_text(json.dumps(RELEASE), encoding="utf-8")


README_DOC = (
    "# Numbers\n\nHand-written headline stays.\n\n"
    "## Benchmarks\n\n<!-- bench-summary:start -->\n\nold summary\n\n<!-- bench-summary:end -->\n\n"
    "Hand-written tail stays.\n"
)


class MeasuredSummary(unittest.TestCase):
    """README and RESULTS blocks are generated from the run's own compare.json.

    The contract is that no number is carried over from an earlier measurement:
    the values in the table are the values in the artifact, the version label is
    the binary that run installed, and a missing artifact produces a stated gap.
    """

    def test_summary_table_comes_from_compare_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            summary_results(results)
            dirs = bench_docs.find_platform_dirs(results)
            summary = bench_docs.summary_body(dirs, "abc1234")
        self.assertIn("| Linux (`ubuntu-24.04`) | horon | jcode |", summary)
        self.assertIn("| One-shot prompt time | 60.0 ms | 150.0 ms |", summary)
        self.assertIn("| Skill invocation time | 36.0 ms | 57.0 ms |", summary)
        self.assertIn("| Memory per additional session | 80.0 KB | 160.0 KB |", summary)
        self.assertIn("| Install size | 112.1 MB | 163.4 MB |", summary)
        self.assertIn("| First model request, body / tool catalog bytes | 3,699 / 2,358 | 3,245 / 1,433 |", summary)
        # The release label is the installed binary and its recorded identity.
        self.assertIn("jcode v1.0.1 (release `v1.0.1`, commit `10114429c`", summary)
        # Only measured values appear: nothing is derived that the artifact does not hold.
        self.assertNotIn("42.5", summary)
        self.assertNotIn("138.0", summary)

    def test_versions_table_records_the_measured_release_identity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            summary_results(results)
            body = bench_docs.versions_body(bench_docs.find_platform_dirs(results), "abc1234")
        self.assertIn("| jcode | `jcode v1.0.1 (10114429c)` |", body)
        self.assertIn("| horon | `horon-agent v0.1.0-dev (50d24d7)` |", body)
        self.assertIn(f"| Linux | `v1.0.1` | `{RELEASE['commit']}` | `jcode-linux-x86_64.tar.gz` |", body)
        self.assertIn("`" + "ab" * 32 + "`", body)
        self.assertIn("release-download", body)
        self.assertRegex(body, r"\| macOS \| — \|")

    def test_no_artifact_is_stated_not_invented(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            job_dir(results, "benchmark-linux-1", "linux")
            dirs = bench_docs.find_platform_dirs(results)
            self.assertIn("No competitor comparison artifact", bench_docs.summary_body(dirs, "abc1234"))
            self.assertIn("No comparison artifact", bench_docs.versions_body(dirs, "abc1234"))

    def test_readme_blocks_are_rewritten_and_the_tail_is_untouched(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            results = root / "bench-results"
            results.mkdir()
            summary_results(results)
            doc = root / "README.md"
            doc.write_text(README_DOC, encoding="utf-8")
            for _ in range(2):
                code = bench_docs.main(["render", "--doc", str(doc), "--results", str(results),
                                        "--commit", "abc1234"])
                self.assertEqual(code, 0)
            text = doc.read_text(encoding="utf-8")
        self.assertNotIn("old summary", text)
        self.assertIn("Hand-written headline stays.", text)
        self.assertIn("Hand-written tail stays.", text)
        self.assertIn("| One-shot prompt time | 60.0 ms | 150.0 ms |", text)
        self.assertEqual(text.count("bench-summary:start"), 1)


class PartialRuns(unittest.TestCase):
    """A compare-only round must not overwrite the recorded full-run block."""

    def all_fragments(self, directory: Path) -> None:
        for name in ("compare.md", "packages.md", "startup.txt", "loading-cold.txt", "loading-warm.txt",
                     "tools.md", "reload.md", "compile.md", "tui.md"):
            (directory / name).write_text("# measured\n", encoding="utf-8")

    def test_compare_only_refuses_to_replace_the_recorded_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            results = root / "bench-results"
            results.mkdir()
            directory = job_dir(results, "benchmark-linux-1", "linux", files={"compare.md": "# c\n"})
            (directory / "compare.json").write_text(compare_json(
                compare_result("horon", "horon", 60.0, 37e6), compare_result("jcode", "jcode", 150.0, 38e6)),
                encoding="utf-8")
            doc = root / "RESULTS.md"
            doc.write_text(DOC, encoding="utf-8")
            code = bench_docs.main(["render", "--doc", str(doc), "--results", str(results),
                                    "--commit", "abc1234", "--require-complete"])
            self.assertEqual(code, 1)
            self.assertEqual(doc.read_text(encoding="utf-8"), DOC)
            self.assertEqual(bench_docs.missing_suite_groups(bench_docs.find_platform_dirs(results)),
                             ["packages", "startup", "loading", "tools", "reload", "compile", "tui"])

    def test_compare_only_refreshes_versions_without_touching_the_full_record(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            results = root / "bench-results"
            results.mkdir()
            summary_results(results)
            doc = root / "RESULTS.md"
            doc.write_text("<!-- bench-versions:start -->\nold\n<!-- bench-versions:end -->\n\n" + DOC, encoding="utf-8")
            code = bench_docs.main(["render", "--doc", str(doc), "--results", str(results),
                                    "--commit", "abc1234", "--comparison-only"])
            self.assertEqual(code, 0)
            text = doc.read_text(encoding="utf-8")
        self.assertIn("| jcode | `jcode v1.0.1 (10114429c)` |", text)
        self.assertIn(DOC, text)

    def test_a_full_run_replaces_the_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            results = root / "bench-results"
            results.mkdir()
            directory = job_dir(results, "benchmark-linux-1", "linux")
            self.all_fragments(directory)
            doc = root / "RESULTS.md"
            doc.write_text(DOC, encoding="utf-8")
            code = bench_docs.main(["render", "--doc", str(doc), "--results", str(results),
                                    "--commit", "abc1234", "--require-complete"])
            self.assertEqual(code, 0)
            self.assertEqual(bench_docs.last_commit(doc.read_text(encoding="utf-8")), "abc1234")


def eight_tool_results() -> list[dict]:
    specs = [
        ("horon", "horon", "horon-agent v0.1.0-dev (abc1234)", 45.0, 41e6, 37.0, 64.0, 380e3, 117e6),
        ("horon-base", "horon-base", "horon-agent v0.1.0-dev (0001111)", 48.0, 42e6, 39.0, 66.0, 390e3, 118e6),
        ("jcode", "jcode", "jcode v1.0.1 (10114429c)", 148.0, 80e6, 54.0, 165.0, 173e3, 191e6),
        ("claude_code", "Claude Code", "2.1.317 (Claude Code)", 500.0, 200e6, 520.0, 650.0, 50e6, 230e6),
        ("codex", "Codex CLI", "codex-cli 0.166.0", 290.0, 90e6, 310.0, 410.0, 45e6, 98e6),
        ("opencode", "OpenCode", "1.19.5", 450.0, 150e6, 470.0, 580.0, 800e3, 165e6),
        ("pi", "pi", "1.5.0", 510.0, 130e6, 530.0, 610.0, 95e6, 35e6),
        ("omp", "oh-my-pi (omp)", "18.18.0", 620.0, 160e6, 650.0, 740.0, 110e6, 120e6),
        ("omo", "oh-my-openagent (omo)", "5.1.64", 710.0, 220e6, 730.0, 820.0, 150e6, 250e6),
    ]
    rows = []
    for name, label, version, oneshot, rss, skill, mcp, slope, install in specs:
        entry = summary_result(name, version, oneshot, rss, skill, mcp, slope, install)
        entry["label"] = label
        rows.append(entry)
    return rows


class AllCompetitorGuardAndSummary(unittest.TestCase):
    """Summaries, versions, charts and guards cover all 7 competitors and omit A/B rows."""

    def test_summary_and_versions_include_all_competitors_without_horon_base(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            job_dir(results, "benchmark-linux-1", "linux", files={
                "runner.txt": "ubuntu-24.04\n",
                "compare.json": compare_json(*eight_tool_results()),
                "jcode-release.json": json.dumps(RELEASE),
            })
            dirs = bench_docs.find_platform_dirs(results)
            summary = bench_docs.summary_body(dirs, "abc1234", "en")
            versions = bench_docs.versions_body(dirs, "abc1234", "en")
            charts = bench_charts.render_charts(dirs, "abc1234", "en")

        expected_header = (
            "| Linux (`ubuntu-24.04`) | horon | jcode | Claude Code | Codex CLI | "
            "OpenCode | pi | oh-my-pi (omp) | oh-my-openagent (omo) |"
        )
        self.assertIn(expected_header, summary)
        self.assertNotIn("horon-base", summary)
        for label in ("horon", "jcode", "Claude Code", "Codex CLI", "OpenCode", "pi", "oh-my-pi (omp)", "oh-my-openagent (omo)"):
            self.assertIn(f"| {label} |", versions)
        self.assertNotIn("horon-base", versions)
        self.assertIn("horon against 7 tools", charts["00-summary.svg"])

    def test_require_all_competitors_rejects_partial_run_and_accepts_full_run(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            partial_dir = root / "partial"
            partial_dir.mkdir()
            summary_results(partial_dir)
            doc = root / "README.md"
            doc.write_text(README_DOC, encoding="utf-8")

            code = bench_docs.main([
                "render", "--doc", str(doc), "--results", str(partial_dir),
                "--commit", "abc1234", "--require-all-competitors",
            ])
            self.assertEqual(code, 1)
            self.assertEqual(doc.read_text(encoding="utf-8"), README_DOC)

            full_dir = root / "full"
            full_dir.mkdir()
            for idx, key in enumerate(("linux", "macos", "windows"), start=1):
                runner = {"linux": "ubuntu-24.04\n", "macos": "macos-latest\n", "windows": "windows-latest\n"}[key]
                asset = {
                    "linux": "jcode-linux-x86_64.tar.gz",
                    "macos": "jcode-macos-aarch64.tar.gz",
                    "windows": "jcode-windows-x86_64.exe",
                }[key]
                job_dir(full_dir, f"benchmark-{key}-{idx}", key, files={
                    "runner.txt": runner,
                    "compare.json": compare_json(*eight_tool_results()),
                    "jcode-release.json": json.dumps({**RELEASE, "asset": asset}),
                })
            code = bench_docs.main([
                "render", "--doc", str(doc), "--results", str(full_dir),
                "--commit", "abc1234", "--require-all-competitors",
            ])
            self.assertEqual(code, 0)
            self.assertIn("oh-my-openagent (omo)", doc.read_text(encoding="utf-8"))

    def test_require_all_competitors_rejects_missing_or_failed_os_comparison(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            partial_os = root / "partial-os"
            partial_os.mkdir()
            job_dir(partial_os, "benchmark-linux-1", "linux", files={
                "runner.txt": "ubuntu-24.04\n",
                "compare.json": compare_json(*eight_tool_results()),
                "jcode-release.json": json.dumps(RELEASE),
            })
            job_dir(partial_os, "benchmark-macos-2", "macos", status="failure", files={
                "runner.txt": "macos-latest\n",
            })
            readme = root / "README.md"
            readme.write_text(README_DOC, encoding="utf-8")
            results_doc = root / "RESULTS.md"
            results_doc.write_text(CHART_DOC, encoding="utf-8")

            for doc in (readme, results_doc):
                original = doc.read_text(encoding="utf-8")
                code = bench_docs.main([
                    "render", "--doc", str(doc), "--results", str(partial_os),
                    "--commit", "abc1234", "--require-all-competitors",
                ])
                self.assertEqual(code, 1)
                self.assertEqual(doc.read_text(encoding="utf-8"), original)


class PerOsReleaseAndMainCommitGuardTests(unittest.TestCase):
    """Per-OS jcode releases, npm provenance tables, and --require-main-commit guard."""

    def test_per_os_jcode_and_npm_provenance_render_cleanly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            npm_prov = {
                "tools": {
                    "claude_code": {
                        "label": "Claude Code",
                        "package": "@anthropic-ai/claude-code",
                        "installed_version": "2.1.331",
                        "selection": "npm dist-tag latest",
                        "git_head": None,
                        "integrity": "sha512-claude331==",
                    },
                    "omo": {
                        "label": "oh-my-openagent (omo)",
                        "package": "omo-ai",
                        "installed_version": "5.1.83",
                        "selection": "npm dist-tag latest",
                        "git_head": "69a1863a457da06fb64ae788e335bf272f1df406",
                        "integrity": "sha512-omo5183==",
                    },
                }
            }
            linux_rows = [
                summary_result("horon", "horon-agent v0.1.23-dev (4100da7)", 45.0, 41e6, 37.0, 64.0, 380e3, 117e6),
                summary_result("jcode", "jcode v1.1.2 (3090ca4)", 140.0, 75e6, 52.0, 160.0, 170e3, 190e6),
            ]
            other_rows = [
                summary_result("horon", "horon-agent v0.1.23-dev (4100da7)", 90.0, 38e6, 70.0, 95.0, 400e3, 105e6),
                summary_result("jcode", "jcode v1.0.3 (a0ae514c4)", 180.0, 32e6, 80.0, 200.0, 250e3, 110e6),
            ]
            job_dir(results, "benchmark-linux-1", "linux", files={
                "runner.txt": "ubuntu-24.04\n",
                "compare.json": compare_json(*linux_rows),
                "jcode-release.json": json.dumps({**RELEASE, "tag": "v1.1.2", "version": "1.1.2", "commit": "b" * 40}),
                "competitor-provenance.json": json.dumps(npm_prov),
            })
            for key, runner, asset in (("macos", "macos-latest", "jcode-macos-aarch64.tar.gz"),
                                       ("windows", "windows-latest", "jcode-windows-x86_64.exe")):
                job_dir(results, f"benchmark-{key}-1", key, files={
                    "runner.txt": f"{runner}\n",
                    "compare.json": compare_json(*other_rows),
                    "jcode-release.json": json.dumps({
                        **RELEASE, "tag": "v1.0.3", "version": "1.0.3", "commit": "c" * 40, "asset": asset
                    }),
                    "competitor-provenance.json": json.dumps(npm_prov),
                })
            dirs = bench_docs.find_platform_dirs(results)
            summary = bench_docs.summary_body(dirs, "4100da7", "en")
            versions_en = bench_docs.versions_body(dirs, "4100da7", "en")
            versions_ko = bench_docs.versions_body(dirs, "4100da7", "ko")

        self.assertIn("v1.1.2 (Linux: release `v1.1.2`", summary)
        self.assertIn("v1.0.3 (macOS, Windows: release `v1.0.3`", summary)
        self.assertEqual(versions_en.count("| jcode |"), 1)
        self.assertIn("`jcode v1.1.2 (3090ca4)` (Linux), `jcode v1.0.3 (a0ae514c4)` (macOS, Windows)", versions_en)
        self.assertIn("| Claude Code | `@anthropic-ai/claude-code` | `2.1.331` | npm dist-tag latest |", versions_en)
        self.assertIn("`sha512-omo5183==`", versions_ko)
        self.assertIn("`69a1863a457da06fb64ae788e335bf272f1df406`", versions_ko)

    def test_require_main_commit_rejects_dev_commit_and_v010_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            env = {
                **os.environ,
                "GIT_AUTHOR_NAME": "Test",
                "GIT_AUTHOR_EMAIL": "t@example.com",
                "GIT_COMMITTER_NAME": "Test",
                "GIT_COMMITTER_EMAIL": "t@example.com",
            }
            def git(*args: str) -> str:
                return subprocess.check_output(["git", *args], cwd=repo, env=env, text=True).strip()

            git("init", "-b", "main")
            doc = repo / "RESULTS.md"
            doc.write_text(DOC, encoding="utf-8")
            git("add", "RESULTS.md")
            git("commit", "-m", "main commit")
            main_sha = git("rev-parse", "HEAD")
            git("checkout", "-q", "-b", "dev")
            (repo / "extra.txt").write_text("dev\n", encoding="utf-8")
            git("add", "extra.txt")
            git("commit", "-m", "dev commit")
            dev_sha = git("rev-parse", "HEAD")

            results = repo / "results"
            results.mkdir()
            linux_dir = job_dir(results, "benchmark-linux-1", "linux", files={
                "commit.txt": f"{dev_sha}\n",
                "horon-version.txt": f"horon-agent v0.1.23-dev ({dev_sha[:7]})\n",
                "startup.txt": "ok\n",
            })

            # Commit outside `main` must fail and leave DOC intact.
            code_dev = bench_docs.main([
                "render", "--doc", str(doc), "--results", str(results),
                "--commit", dev_sha, "--require-main-commit", "--main-ref", "main",
            ])
            self.assertEqual(code_dev, 1)
            self.assertEqual(doc.read_text(encoding="utf-8"), DOC)

            # A `main` commit carrying fallback v0.1.0-dev must fail.
            (linux_dir / "commit.txt").write_text(f"{main_sha}\n", encoding="utf-8")
            (linux_dir / "horon-version.txt").write_text(f"horon-agent v0.1.0-dev ({main_sha[:7]})\n", encoding="utf-8")
            code_v010 = bench_docs.main([
                "render", "--doc", str(doc), "--results", str(results),
                "--commit", main_sha, "--require-main-commit", "--main-ref", "main",
            ])
            self.assertEqual(code_v010, 1)

            # A genuine `main` commit with real semver passes.
            (linux_dir / "horon-version.txt").write_text(f"horon-agent v0.1.23-dev ({main_sha[:7]})\n", encoding="utf-8")
            code_ok = bench_docs.main([
                "render", "--doc", str(doc), "--results", str(results),
                "--commit", main_sha, "--require-main-commit", "--main-ref", "main",
            ])
            self.assertEqual(code_ok, 0)
            self.assertEqual(bench_docs.last_commit(doc.read_text(encoding="utf-8")), main_sha)


class CommittedBenchmarkDocsTests(unittest.TestCase):
    """Checked-in summary, versions and summary charts must cover all 3 OSes and 8 tools."""

    REPO = SCRIPTS.parents[1]
    TOOL_LABELS = ("horon", "jcode", "Claude Code", "Codex CLI", "OpenCode", "pi",
                   "oh-my-pi (omp)", "oh-my-openagent (omo)")

    @staticmethod
    def _between(text: str, start: str, end: str) -> str:
        return text.split(start, 1)[1].split(end, 1)[0]

    def test_readme_summaries_cover_all_three_oses_and_eight_tools(self) -> None:
        for name in ("README.md", "README.ko.md"):
            with self.subTest(doc=name):
                text = (self.REPO / name).read_text(encoding="utf-8")
                block = self._between(text, bench_docs.SUMMARY_START, bench_docs.SUMMARY_END)
                for os_heading in ("| Linux (`", "| macOS (`", "| Windows (`"):
                    self.assertIn(os_heading, block)
                for label in self.TOOL_LABELS:
                    self.assertIn(label, block)
                self.assertNotIn("horon-base", block)
                self.assertNotIn("v0.1.0-dev", block)
                self.assertNotIn("| — | — | — |", block)
                self.assertIn("actions/runs/", block)

    def test_results_versions_and_summary_charts_cover_all_eight_tools(self) -> None:
        commits: dict[str, str | None] = {}
        for name, missing_marker, npm_marker in (
            ("docs/performance/RESULTS.md", "not recorded", "npm dist-tag latest"),
            ("docs/performance/RESULTS.ko.md", "기록 없음", "npm dist-tag latest"),
        ):
            with self.subTest(doc=name):
                text = (self.REPO / name).read_text(encoding="utf-8")
                commit = bench_docs.last_commit(text)
                self.assertIsNotNone(commit)
                assert commit is not None
                self.assertEqual(len(commit), 40)
                commits[name] = commit
                versions = self._between(text, bench_docs.VERSIONS_START, bench_docs.VERSIONS_END)
                for label in self.TOOL_LABELS:
                    self.assertIn(f"| {label} |", versions)
                self.assertIn(f"({commit[:7]})", versions)
                self.assertNotIn("v0.1.0-dev", versions)
                self.assertNotIn("horon-base", versions)
                self.assertIn(npm_marker, versions)
                for os_row in ("| Linux | `v", "| macOS | `v", "| Windows | `v"):
                    self.assertIn(os_row, versions)
                self.assertNotIn(missing_marker, versions)

        self.assertEqual(
            commits["docs/performance/RESULTS.md"],
            commits["docs/performance/RESULTS.ko.md"],
        )
        measured_commit = commits["docs/performance/RESULTS.md"]
        assert measured_commit is not None
        has_origin_main = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", "origin/main"],
            cwd=self.REPO,
            capture_output=True,
            text=True,
            check=False,
        ).returncode == 0
        if has_origin_main:
            ancestry = subprocess.run(
                ["git", "merge-base", "--is-ancestor", measured_commit, "origin/main"],
                cwd=self.REPO,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(
                ancestry.returncode,
                0,
                f"measured RESULTS commit {measured_commit} must be an ancestor of origin/main",
            )

        charts_dir = self.REPO / "docs" / "performance" / "charts"
        en_svg = (charts_dir / "00-summary.svg").read_text(encoding="utf-8")
        ko_svg = (charts_dir / "00-summary.ko.svg").read_text(encoding="utf-8")
        self.assertIn("horon against 7 tools", en_svg)
        self.assertIn("horon 대 도구 7개", ko_svg)
        self.assertNotIn("not measured", en_svg.replace("not measured", "", 2))
        self.assertNotIn("측정 불가", ko_svg.replace("측정 불가", "", 2))


if __name__ == "__main__":
    unittest.main()
