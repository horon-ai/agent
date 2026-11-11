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
        "direct_tui_first_frame_ms": stat(95.0, 90.0, 100.0),
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
        self.assertIn("tui first frame", latency)
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



if __name__ == "__main__":
    unittest.main()
