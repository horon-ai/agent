#!/usr/bin/env python3
"""Write a benchmark run into the generated block of docs/performance/RESULTS.md.

`.github/workflows/benchmark-docs.yml` runs every benchmark suite on Linux, macOS
and Windows after a push to `main` passes CI, then calls this script once per
branch (`main`, `dev`) to replace the block between the two markers below.
Everything outside the block is left byte-for-byte as it was: the rest of
RESULTS.md is a hand-written record.

    <!-- bench-auto:start commit=<sha> -->
    ...generated...
    <!-- bench-auto:end -->

The start marker carries the measured commit, so the next run can ask "did the
agent code change since the last measurement?" (`last-commit`).

`--results` is a directory holding one sub-directory per benchmark job (one
downloaded artifact each). A job directory names its OS in `platform.txt`
(`linux`, `macos`, `windows`) and its job outcome in `status.txt`; the OS
sections appear in that fixed order, and an OS with no directory is shown as
not measured rather than left out.

Subcommands:

    render      --doc PATH --results DIR --commit SHA [--run-url URL] [--lang en|ko]
    last-commit --doc PATH     prints the recorded commit, or nothing

RESULTS.md (English, `--lang en`, the default) and RESULTS.ko.md (`--lang ko`)
carry the same block in their own language; the workflow renders both. Table
contents written by the benchmark suites are language-neutral and shared.

A document may also carry a second, optional block near its top:

    <!-- bench-charts:start -->
    ...generated...
    <!-- bench-charts:end -->

When it is present, `render` draws the per-OS `compare.json` files as SVG charts
(`bench_charts.py`) into a `charts/` folder beside the document and rewrites the
block with the summary chart and links to the per-metric charts. A document
without that block gets no charts, so an older branch keeps working unchanged.
The workflow copies `bench_charts.py` next to this script.

Two more blocks are optional and are written only when the document has them:

    <!-- bench-summary:start -->  ... <!-- bench-summary:end -->  (README.md)
    <!-- bench-versions:start --> ... <!-- bench-versions:end --> (RESULTS.md)

`bench-summary` builds the headline comparison table from each OS's
`compare.json`: the version label is the binary that run installed, and the
medians come from the same metric readers the charts use. `bench-versions` lists
the measured tool versions and each runner's `jcode-release.json` identity (tag,
peeled tag commit, asset, SHA-256, served by). Nothing is carried over from an
earlier measurement: a run without a `compare.json` writes an explicit "not
available" line.

`render --require-complete` refuses to replace the recorded `bench-auto` block
unless the results hold at least one artifact from every suite group. A
compare-only run still updates the README blocks, but cannot overwrite the
startup, loading, tools, reload, compile and TUI record with "not measured".
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import re
import sys
from pathlib import Path

import bench_charts
import bench_request_bytes as brb

START_RE = re.compile(r"^<!-- bench-auto:start(?: commit=([0-9a-f]{7,40}))? -->$", re.MULTILINE)
END = "<!-- bench-auto:end -->"
CHARTS_START = "<!-- bench-charts:start -->"
CHARTS_END = "<!-- bench-charts:end -->"
CHARTS_DIR = "charts"
FENCE_RE = re.compile(r"^\s*(```|~~~)")
HEADING_RE = re.compile(r"^(#{1,6}) ")

# The OS sections, in the order they appear. (key in platform.txt, title)
PLATFORMS = (("linux", "Linux"), ("macos", "macOS"), ("windows", "Windows"))

# Fragments written by benchmark.yml, in the order they appear in an OS section.
# (file name, title key used when the fragment is plain text or missing)
FRAGMENTS = (
    ("compare.md", "compare"),
    ("packages.md", "packages"),
    ("startup.txt", "startup"),
    ("loading-cold.txt", "loading_cold"),
    ("loading-warm.txt", "loading_warm"),
    ("tools.md", "tools"),
    ("reload.md", "reload"),
    ("compile.md", "compile"),
)

LANGS = ("en", "ko")

# The block holds two version sections. The headless one is the whole body the
# renderer produced before the TUI existed, byte for byte: the six suites and the
# run header stay exactly where they were, so an unchanged input still produces
# an unchanged document. The TUI section is appended after it and is the only
# place `horon-tui` numbers appear, so the two binaries never share a table.
HEADLESS_LEVEL = 2
TUI_FRAGMENT = "tui.md"

# Optional generated blocks. A document carries whichever it wants: RESULTS.md
# has the charts block, README.md has the summary block, and RESULTS has the
# version table. A missing block is not an error, so an older branch or a
# document that does not want a summary keeps working unchanged.
SUMMARY_START = "<!-- bench-summary:start -->"
SUMMARY_END = "<!-- bench-summary:end -->"
VERSIONS_START = "<!-- bench-versions:start -->"
VERSIONS_END = "<!-- bench-versions:end -->"

# The README rows. The order is the order a reader compares them in, and every
# value comes from the run's compare.json through bench_charts' metric readers.
SUMMARY_METRICS = ("03-oneshot-time", "05-skill-time", "06-mcp-time",
                   "04-oneshot-memory", "08-per-session-memory", "02-install-size")
EXPECTED_COMPETITORS = ("jcode", "claude_code", "codex", "opencode", "pi", "omp", "omo")
# Groups that must all have at least one artifact before RESULTS.md's recorded
# block may be replaced: a compare-only run must not overwrite startup, loading
# and TUI records from an earlier full run with "not measured".
REQUIRED_SUITES = (("compare", ("compare.md",)),
                   ("packages", ("packages.md",)),
                   ("startup", ("startup.txt",)),
                   ("loading", ("loading-cold.txt", "loading-warm.txt")),
                   ("tools", ("tools.md",)),
                   ("reload", ("reload.md",)),
                   ("compile", ("compile.md",)),
                   ("tui", ("tui.md",)))

TEXT = {
    "en": {
        "compare": "Competitor comparison",
        "packages": "User-installed package cost",
        "startup": "Startup",
        "loading_cold": "Session loading (COLD)",
        "loading_warm": "Session loading (WARM)",
        "tools": "Tool latency",
        "reload": "Reload storm",
        "compile": "Compile loop",
        "missing": "Not measured: this run did not produce `{name}`.",
        "no_result": "No result",
        "unknown": "Unknown status",
        "partial": "Partial failure (`{status}`)",
        "success": "Success",
        "no_platform": "This run has no {title} result. The job did not start or did not upload its result files.",
        "job_failed": "The measurement job on this OS did not finish successfully. Any suite below without results is missing for that reason.",
        "intro": "`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.",
        "commit": "Commit",
        "measured_at": "Measured at",
        "run": "Run",
        "run_note": "(raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)",
        "runners": "Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.",
        "header": "| OS | Runner | Binary | Status |",
        "headless": "Headless (`horon-agent`)",
        "tui": "TUI (`horon-tui`)",
        "tui_intro": "The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.",
        "tui_missing": "This run did not produce `tui.md` for {title}, so the TUI was not measured on this OS.",
        "tui_no_platform": "This run has no {title} TUI result. The job did not start or did not upload its result files.",
        "summary_intro": "`.github/workflows/benchmark-docs.yml` rewrites this summary from the run's own `compare.json`; the version label is the binary each runner installed, not a hand-written constant.",
        "summary_none": "No competitor comparison artifact was available for this run, so this summary is left empty rather than filled from an earlier measurement.",
        "summary_measured": "Measured commit `{commit}`, {when} UTC. Each OS runs on a different machine, so compare inside a row.",
        "summary_release": "jcode {version} (release `{tag}`, commit `{commit}`).",
        "summary_version_only": "jcode {version} (its release identity was not recorded for this run).",
        "summary_release_unknown": "jcode's release identity was not recorded for this run.",
        "request_bytes": "First model request, body / tool catalog bytes",
        "versions_intro": "Versions recorded by the comparison of `{commit}` at {when} UTC. Each version is listed once across the runner artifacts.",
        "versions_none": "No comparison artifact was available, so no measured version table is written.",
        "version_tool": "Tool",
        "version_value": "Version",
        "release_intro": "jcode release identity per runner, from the `jcode-release.json` each job downloaded and verified.",
        "release_os": "OS",
        "release_tag": "Tag",
        "release_commit": "Commit",
        "release_asset": "Asset",
        "release_sha": "SHA-256",
        "release_served": "Served by",
        "release_missing": "not recorded",
    },
    "ko": {
        "compare": "경쟁 도구 비교",
        "packages": "사용자 설치 패키지 비용",
        "startup": "기동",
        "loading_cold": "세션 로딩 (COLD)",
        "loading_warm": "세션 로딩 (WARM)",
        "tools": "도구 지연",
        "reload": "리로드 폭주",
        "compile": "컴파일 루프",
        "missing": "측정 안 됨: 이 실행에서 `{name}` 파일이 만들어지지 않았습니다.",
        "no_result": "결과 없음",
        "unknown": "상태 모름",
        "partial": "일부 실패 (`{status}`)",
        "success": "성공",
        "no_platform": "이 실행에서 {title} 결과가 없습니다. 작업이 시작되지 못했거나 결과 파일을 올리지 못했습니다.",
        "job_failed": "이 OS의 측정 작업이 끝까지 성공하지 못했습니다. 아래에서 결과가 없는 묶음은 그 때문입니다.",
        "intro": "이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.",
        "commit": "커밋",
        "measured_at": "측정 시각",
        "run": "실행",
        "run_note": "(원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)",
        "runners": "OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.",
        "header": "| OS | 러너 | 바이너리 | 상태 |",
        "headless": "헤드리스 (`horon-agent`)",
        "tui": "TUI (`horon-tui`)",
        "tui_intro": "대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.",
        "tui_missing": "이 실행에서 {title} 의 `tui.md` 가 만들어지지 않아 그 OS의 TUI를 재지 못했습니다.",
        "tui_no_platform": "이 실행에서 {title} TUI 결과가 없습니다. 작업이 시작되지 못했거나 결과 파일을 올리지 못했습니다.",
        "summary_intro": "`.github/workflows/benchmark-docs.yml`이 이 요약을 그 실행의 `compare.json`에서 다시 씁니다. 버전 표기는 러너가 실제로 설치한 실행 파일에서 읽은 값이지 손으로 적은 상수가 아닙니다.",
        "summary_none": "이 실행에는 경쟁 도구 비교 산출물이 없어서, 이전 측정값으로 채우지 않고 요약을 비워 둡니다.",
        "summary_measured": "측정 커밋 `{commit}`, {when} UTC. OS마다 다른 기계이므로 같은 줄 안에서만 비교합니다.",
        "summary_release": "jcode {version}(릴리스 `{tag}`, 커밋 `{commit}`).",
        "summary_version_only": "jcode {version}(이 실행에서는 릴리스 신원을 기록하지 못했습니다).",
        "summary_release_unknown": "이 실행에서 jcode 릴리스 신원을 기록하지 못했습니다.",
        "request_bytes": "첫 모델 요청 본문 / 도구 목록 바이트",
        "versions_intro": "`{commit}`의 비교 측정을 {when} UTC에 실행해 기록한 버전입니다. 여러 러너에서 같은 버전을 기록했으면 한 번만 표시합니다.",
        "versions_none": "비교 산출물이 없어 측정 버전 표를 쓰지 않습니다.",
        "version_tool": "도구",
        "version_value": "버전",
        "release_intro": "러너별 jcode 릴리스 신원입니다. 각 작업이 내려받아 검증한 `jcode-release.json`에서 읽습니다.",
        "release_os": "OS",
        "release_tag": "태그",
        "release_commit": "커밋",
        "release_asset": "자산",
        "release_sha": "SHA-256",
        "release_served": "제공 경로",
        "release_missing": "기록 없음",
    },
}

# The block sits under a `##` section of RESULTS.md, each OS gets `###`, and
# fragment headings are shifted so the shallowest one becomes this level.
PLATFORM_LEVEL = 3
FRAGMENT_LEVEL = 4


class DocError(Exception):
    pass


def _block_span(text: str) -> tuple[re.Match[str], int]:
    starts = list(START_RE.finditer(text))
    if len(starts) != 1:
        raise DocError(f"expected exactly one bench-auto start marker, found {len(starts)}")
    ends = [m.start() for m in re.finditer(re.escape(END), text)]
    if len(ends) != 1:
        raise DocError(f"expected exactly one bench-auto end marker, found {len(ends)}")
    if ends[0] < starts[0].end():
        raise DocError("bench-auto end marker comes before the start marker")
    return starts[0], ends[0]


def last_commit(text: str) -> str | None:
    start, _ = _block_span(text)
    return start.group(1)


def replace_block(text: str, commit: str, body: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{7,40}", commit):
        raise DocError(f"commit must be a lowercase hex SHA, got {commit!r}")
    start, end = _block_span(text)
    marker = f"<!-- bench-auto:start commit={commit} -->"
    return text[: start.start()] + marker + "\n\n" + body.strip("\n") + "\n\n" + text[end:]


def optional_span(text: str, start: str, end: str, label: str) -> tuple[int, int] | None:
    """Body span of an optional block, or None when the document has none."""
    starts = [match.start() for match in re.finditer(re.escape(start), text)]
    ends = [match.start() for match in re.finditer(re.escape(end), text)]
    if not starts and not ends:
        return None
    if len(starts) != 1 or len(ends) != 1:
        raise DocError(f"expected one {label} start and end marker, found {len(starts)} and {len(ends)}")
    if ends[0] < starts[0]:
        raise DocError(f"{label} end marker comes before the start marker")
    return starts[0], ends[0]


def replace_optional(text: str, start: str, end: str, body: str) -> tuple[str, bool]:
    """Replace an optional block in place; return (text, whether it existed)."""
    span = optional_span(text, start, end, start)
    if span is None:
        return text, False
    return text[: span[0]] + start + "\n\n" + body.strip("\n") + "\n\n" + text[span[1]:], True


def charts_span(text: str) -> tuple[int, int] | None:
    """Body span of the optional charts block, or None when the document has none."""
    starts = [m.end() for m in re.finditer(re.escape(CHARTS_START), text)]
    ends = [m.start() for m in re.finditer(re.escape(CHARTS_END), text)]
    if not starts and not ends:
        return None
    if len(starts) != 1 or len(ends) != 1:
        raise DocError(f"expected one bench-charts start and end marker, found {len(starts)} and {len(ends)}")
    if ends[0] < starts[0]:
        raise DocError("bench-charts end marker comes before the start marker")
    return starts[0], ends[0]


def _run_id(run_url: str | None) -> str | None:
    match = re.search(r"/runs/(\d+)", run_url or "")
    return match.group(1) if match else None


def write_charts(text: str, doc: Path, results: Path, commit: str, run_url: str | None = None,
                 lang: str = "en") -> str:
    """Draw the charts beside `doc` and rewrite the charts block; no block, no charts."""
    span = charts_span(text)
    if span is None:
        return text
    dirs = find_platform_dirs(results)
    files = bench_charts.render_charts(dirs, commit, lang, _run_id(run_url))
    tui_files = bench_charts.render_tui_charts(dirs, commit, lang, _run_id(run_url))
    folder = doc.parent / CHARTS_DIR
    if files:
        folder.mkdir(parents=True, exist_ok=True)
        for name, svg in {**files, **tui_files}.items():
            (folder / name).write_text(svg, encoding="utf-8", newline="\n")
    body = bench_charts.markdown(lang, bool(files), CHARTS_DIR, bool(tui_files))
    return text[: span[0]] + "\n\n" + body + "\n\n" + text[span[1]:]


def demote_headings(markdown: str, level: int = FRAGMENT_LEVEL) -> str:
    """Shift headings so the shallowest becomes `level`; code fences are untouched."""
    lines = markdown.splitlines()
    in_fence = False
    found: list[int] = []
    for line in lines:
        if FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        match = None if in_fence else HEADING_RE.match(line)
        if match:
            found.append(len(match.group(1)))
    if not found:
        return markdown
    shift = level - min(found)
    out = []
    in_fence = False
    for line in lines:
        if FENCE_RE.match(line):
            in_fence = not in_fence
            out.append(line)
            continue
        match = None if in_fence else HEADING_RE.match(line)
        if match:
            depth = min(6, max(1, len(match.group(1)) + shift))
            line = "#" * depth + line[len(match.group(1)) :]
        out.append(line)
    return "\n".join(out)


def _read(path: Path) -> str | None:
    if not path.is_file():
        return None
    text = path.read_text(encoding="utf-8").strip()
    return text or None


def find_platform_dirs(results: Path) -> dict[str, Path]:
    """Map each OS key to the job directory that names it in platform.txt.

    `results` itself counts as a job directory too, because a download of a
    single artifact may land flat instead of in a sub-directory.
    """
    found: dict[str, Path] = {}
    known = {key for key, _ in PLATFORMS}
    for directory in [results, *sorted(p for p in results.iterdir() if p.is_dir())]:
        key = (_read(directory / "platform.txt") or "").lower()
        if key not in known:
            continue
        if key in found:
            raise DocError(f"two result directories claim {key}: {found[key].name}, {directory.name}")
        found[key] = directory
    return found


def missing_suite_groups(dirs: dict[str, Path]) -> list[str]:
    """Suite groups with no artifact in any platform directory.

    RESULTS.md's recorded block mixes every suite; replacing it needs at least
    one artifact from each group, or a compare-only run would overwrite the
    startup, loading and TUI history with "not measured".
    """
    missing = []
    for name, files in REQUIRED_SUITES:
        if not any((directory / file_name).is_file()
                   for directory in dirs.values() for file_name in files):
            missing.append(name)
    return missing


def missing_comparison_platforms(dirs: dict[str, Path]) -> list[str]:
    """OS platforms missing a readable compare.json."""
    compare = bench_charts.load_compare(dirs)
    return [key for key, _ in PLATFORMS if key not in compare]


def missing_competitors(dirs: dict[str, Path]) -> list[str]:
    """Competitors absent from an OS comparison or unmeasured on every OS."""
    compare = bench_charts.load_compare(dirs)
    if not compare or any(key not in compare for key in dirs):
        return list(EXPECTED_COMPETITORS)
    measured_anywhere = {
        result.get("name")
        for run in compare.values()
        for result in run.get("results", [])
        if isinstance(result, dict) and not result.get("error")
    }
    missing: list[str] = []
    for name in EXPECTED_COMPETITORS:
        in_every_os = all(
            name in (
                {r.get("name") for r in run.get("results", []) if isinstance(r, dict)}
                | {s.get("name") for s in run.get("skipped", []) if isinstance(s, dict)}
            )
            for run in compare.values()
        )
        if not in_every_os or name not in measured_anywhere:
            missing.append(name)
    return missing


def _tool_result(run: dict | None, name: str) -> dict | None:
    for result in (run or {}).get("results", []):
        if isinstance(result, dict) and result.get("name") == name:
            return result
    return None


def _fmt(metric: bench_charts.Metric, value: float | None, lang: str) -> str:
    """One decimal for times and sizes, matching the numbers RESULTS.md prints."""
    if value is None:
        return "—"
    if metric.unit == "ms":
        return f"{value:.1f} ms" if value < 1000 else f"{value / 1000:.2f} s"
    if metric.unit == "bytes":
        for limit, unit in ((1_000_000_000, "GB"), (1_000_000, "MB"), (1_000, "KB")):
            if value >= limit:
                return f"{value / limit:.1f} {unit}"
        return f"{value:.0f} B"
    return f"{value:,.0f} " + ("회" if lang == "ko" else "calls")


def _cell(value: str) -> str:
    return value.replace("|", "&#124;")


def _semver(text: str | None) -> str | None:
    match = re.search(r"(\d+\.\d+\.\d+)", text or "")
    return f"v{match.group(1)}" if match else None


def _release_record(directory: Path | None) -> dict | None:
    """The `jcode-release.json` the compare job kept, or None when it is absent."""
    if directory is None:
        return None
    content = _read(directory / "jcode-release.json")
    if content is None:
        return None
    try:
        data = json.loads(content)
    except ValueError:
        return None
    return data if isinstance(data, dict) else None


def _release_sentence(dirs: dict[str, Path], compare: dict[str, dict], lang: str) -> str:
    text = TEXT[lang]
    for key, _ in PLATFORMS:
        version = _semver((_tool_result(compare.get(key), "jcode") or {}).get("version"))
        record = _release_record(dirs.get(key))
        if version and record and record.get("tag") == version:
            return text["summary_release"].format(version=version, tag=record["tag"],
                                                  commit=record.get("commit") or "—")
    for run in compare.values():
        version_text = (_tool_result(run, "jcode") or {}).get("version")
        if _semver(version_text):
            return text["summary_version_only"].format(version=_semver(version_text))
    return text["summary_release_unknown"]


def _request_bytes_cells(run: dict | None, tools: list[tuple[str, str]]) -> list[str] | None:
    cells: list[str] = []
    any_value = False
    for name, _ in tools:
        result = _tool_result(run, name)
        entry = brb.first_request((result or {}).get("oneshot"))
        if not entry:
            cells.append("—")
            continue
        body, tool_bytes = entry.get("body_bytes"), entry.get("tools_bytes")
        if body is None and tool_bytes is None:
            cells.append("—")
        elif body is None or tool_bytes is None:
            any_value = True
            cells.append("—")
        else:
            any_value = True
            cells.append(f"{body:,} / {tool_bytes:,}")
    return cells if any_value else None


def summary_body(dirs: dict[str, Path], commit: str, lang: str = "en") -> str:
    """The README benchmark summary: a table and its source, nothing inferred."""
    text = TEXT[lang]
    compare = bench_charts.load_compare(dirs)
    if not compare:
        return text["summary_none"]
    tools = bench_charts._tools(compare) or [("horon", "horon"), ("jcode", "jcode")]
    metrics = {metric.stem: metric for metric in bench_charts.METRICS}
    lines = [
        text["summary_intro"],
        "",
        text["summary_measured"].format(commit=commit, when=bench_charts._measured_at(compare)),
        _release_sentence(dirs, compare, lang),
        "",
    ]
    header_tail = " | ".join(_cell(label) for _, label in tools)
    sep_row = "| " + " | ".join("---" for _ in range(len(tools) + 1)) + " |"
    for key, title in PLATFORMS:
        run = compare.get(key)
        directory = dirs.get(key)
        runner = (_read(directory / "runner.txt") if directory else None) or "—"
        lines.append(f"| {title} (`{runner}`) | {header_tail} |")
        lines.append(sep_row)
        if run is None:
            lines.append("| " + " | ".join("—" for _ in range(len(tools) + 1)) + " |")
            lines.append("")
            continue
        for stem in SUMMARY_METRICS:
            metric = metrics[stem]
            values = bench_charts._values(run, metric, tools)
            formatted = " | ".join(_fmt(metric, value, lang) for value in values)
            lines.append(f"| {metric.title[lang]} | {formatted} |")
        req_cells = _request_bytes_cells(run, tools)
        if req_cells is not None:
            lines.append(f"| {text['request_bytes']} | " + " | ".join(req_cells) + " |")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def versions_body(dirs: dict[str, Path], commit: str, lang: str = "en") -> str:
    """The measured version table, plus each runner's jcode release identity."""
    text = TEXT[lang]
    compare = bench_charts.load_compare(dirs)
    if not compare:
        return text["versions_none"]
    lines = [text["versions_intro"].format(commit=commit, when=bench_charts._measured_at(compare)), "",
             f"| {text['version_tool']} | {text['version_value']} |", "| --- | --- |"]
    seen: set[tuple[str, str]] = set()
    rows: list[tuple[str, str, str]] = []
    for key, _ in PLATFORMS:
        for result in (compare.get(key) or {}).get("results", []):
            if not isinstance(result, dict):
                continue
            name = str(result.get("name") or "")
            if name != "horon" and name.startswith("horon-"):
                continue
            label = str(result.get("label") or name or "")
            version = str(result.get("version") or "—")
            if label and (label, version) not in seen:
                seen.add((label, version))
                rows.append((name, label, version))
    rows.sort(key=lambda item: item[0] != "horon")
    for _, label, version in rows:
        lines.append(f"| {_cell(label)} | `{_cell(version)}` |")
    lines += ["", text["release_intro"], "",
              f"| {text['release_os']} | {text['release_tag']} | {text['release_commit']} | "
              f"{text['release_asset']} | {text['release_sha']} | {text['release_served']} |",
              "| --- | --- | --- | --- | --- | --- |"]
    for key, title in PLATFORMS:
        record = _release_record(dirs.get(key))
        if not record:
            lines.append(f"| {title} | — | — | — | — | {text['release_missing']} |")
            continue
        lines.append("| " + " | ".join([
            title,
            f"`{_cell(str(record.get('tag') or '—'))}`",
            f"`{_cell(str(record.get('commit') or '—'))}`",
            f"`{_cell(str(record.get('asset') or '—'))}`",
            f"`{_cell(str(record.get('sha256') or '—'))}`",
            _cell(str(record.get("served_by") or "—")),
        ]) + " |")
    return "\n".join(lines)


def _fragment(results: Path, name: str, title: str, lang: str = "en") -> str:
    heading = "#" * FRAGMENT_LEVEL + " " + title
    content = _read(results / name)
    if content is None:
        return f"{heading}\n\n{TEXT[lang]['missing'].format(name=name)}"
    if name.endswith(".md"):
        return demote_headings(content)
    return f"{heading}\n\n```text\n{content}\n```"


def _status_label(directory: Path | None, lang: str = "en") -> str:
    text = TEXT[lang]
    if directory is None:
        return text["no_result"]
    status = _read(directory / "status.txt")
    if status == "success":
        return text["success"]
    if status is None:
        return text["unknown"]
    return text["partial"].format(status=status)


def _platform_section(title: str, directory: Path | None, lang: str = "en") -> str:
    text = TEXT[lang]
    heading = "#" * PLATFORM_LEVEL + " " + title
    if directory is None:
        return f"{heading}\n\n{text['no_platform'].format(title=title)}"
    parts = [heading]
    if _read(directory / "status.txt") not in ("success", None):
        parts.append(text["job_failed"])
    parts += [_fragment(directory, name, text[key], lang) for name, key in FRAGMENTS]
    return "\n\n".join(parts)


def headless_body(
    dirs: dict[str, Path],
    commit: str,
    run_url: str | None,
    now: _dt.datetime,
    lang: str,
) -> str:
    """The whole block body this renderer produced before the TUI suite existed.

    Nothing in here may change when the TUI is added: the six headless suites,
    the run header and the per-OS sections keep their bytes so an unchanged
    input still produces an unchanged document.
    """
    text = TEXT[lang]
    lines = [
        text["intro"],
        "",
        f"- {text['commit']}: `{commit}`",
        f"- {text['measured_at']}: {now.strftime('%Y-%m-%d %H:%M')} UTC",
    ]
    if run_url:
        lines.append(f"- {text['run']}: <{run_url}> {text['run_note']}")
    lines += [
        "",
        text["runners"],
        "",
        text["header"],
        "| --- | --- | --- | --- |",
    ]
    for key, title in PLATFORMS:
        directory = dirs.get(key)
        runner = (_read(directory / "runner.txt") if directory else None) or "—"
        version = (_read(directory / "horon-version.txt") if directory else None) or "—"
        version = version.splitlines()[0]
        lines.append(f"| {title} | `{runner}` | `{version}` | {_status_label(directory, lang)} |")
    parts = ["\n".join(lines)]
    parts += [_platform_section(title, dirs.get(key), lang) for key, title in PLATFORMS]
    return "\n\n".join(parts)


def _tui_section(dirs: dict[str, Path], lang: str) -> str:
    """The TUI version section: one table per OS, from that OS's `tui.md`."""
    text = TEXT[lang]
    parts = ["#" * HEADLESS_LEVEL + " " + text["tui"], text["tui_intro"]]
    if bench_charts.load_tui(dirs):
        parts += [f"![{text['tui']}]({CHARTS_DIR}/{name})"
                  for name in bench_charts.tui_chart_names(lang)]
    for key, title in PLATFORMS:
        directory = dirs.get(key)
        heading = "#" * PLATFORM_LEVEL + " " + title
        if directory is None:
            parts += [heading, text["tui_no_platform"].format(title=title)]
            continue
        content = _read(directory / TUI_FRAGMENT)
        if content is None:
            parts += [heading, text["tui_missing"].format(title=title)]
            continue
        parts += [heading, demote_headings(content)]
    return "\n\n".join(parts)


def render_body(
    results: Path,
    commit: str,
    run_url: str | None = None,
    now: _dt.datetime | None = None,
    lang: str = "en",
) -> str:
    if lang not in LANGS:
        raise DocError(f"unknown language {lang!r}, expected one of {', '.join(LANGS)}")
    text = TEXT[lang]
    now = now or _dt.datetime.now(_dt.timezone.utc)
    dirs = find_platform_dirs(results)
    headless = headless_body(dirs, commit, run_url, now, lang)
    return "\n\n".join([
        "#" * HEADLESS_LEVEL + " " + text["headless"],
        headless,
        _tui_section(dirs, lang),
    ])


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    render = sub.add_parser("render", help="rewrite the generated blocks in place")
    render.add_argument("--doc", required=True, type=Path)
    render.add_argument("--results", required=True, type=Path)
    render.add_argument("--commit", required=True)
    render.add_argument("--run-url")
    render.add_argument("--lang", choices=LANGS, default="en")
    mode = render.add_mutually_exclusive_group()
    mode.add_argument("--require-complete", action="store_true",
                      help="refuse to replace the full results until each suite group has artifacts")
    mode.add_argument("--comparison-only", action="store_true",
                      help="refresh comparison summaries and versions while keeping the full record and charts")
    render.add_argument("--require-all-competitors", action="store_true",
                        help="refuse to update when any of the 7 expected competitors is missing")
    last = sub.add_parser("last-commit", help="print the commit recorded in the block")
    last.add_argument("--doc", required=True, type=Path)
    args = parser.parse_args(argv)

    try:
        text = args.doc.read_text(encoding="utf-8")
        if args.command == "last-commit":
            print(last_commit(text) or "")
            return 0
        if not args.results.is_dir():
            raise DocError(f"results directory not found: {args.results}")
        dirs = find_platform_dirs(args.results)
        if args.require_all_competitors:
            missing_os = missing_comparison_platforms(dirs)
            if missing_os:
                raise DocError(
                    "the comparison is missing platform result(s): "
                    + ", ".join(missing_os)
                    + "; refusing to overwrite published comparison tables"
                )
            lacking = missing_competitors(dirs)
            if lacking:
                raise DocError(
                    "the comparison is missing expected competitor(s): "
                    + ", ".join(lacking)
                    + "; refusing to overwrite published comparison tables"
                )
        wrote = False
        has_auto = START_RE.search(text) is not None or END in text
        if has_auto and not args.comparison_only:
            missing = missing_suite_groups(dirs) if args.require_complete else []
            if missing:
                raise DocError("the results do not contain " + ", ".join(missing) +
                               "; refusing to replace the recorded block with a partial run")
            text = replace_block(text, args.commit, render_body(args.results, args.commit, args.run_url,
                                                                lang=args.lang))
            wrote = True
        for start, end, build in ((SUMMARY_START, SUMMARY_END, summary_body),
                                  (VERSIONS_START, VERSIONS_END, versions_body)):
            if optional_span(text, start, end, start) is None:
                continue
            body = build(dirs, args.commit, args.lang)
            if args.run_url:
                body = f"{body.rstrip()}\n\n[{TEXT[args.lang]['run']}]({args.run_url})"
            text, _ = replace_optional(text, start, end, body)
            wrote = True
        if not wrote:
            raise DocError("no generated block found (bench-auto, bench-summary or bench-versions)")
        if not args.comparison_only:
            text = write_charts(text, args.doc, args.results, args.commit, args.run_url, args.lang)
        args.doc.write_text(text, encoding="utf-8")
    except DocError as exc:
        print(f"bench_docs: {args.doc}: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
