#!/usr/bin/env python3
"""Draw the competitor comparison of a benchmark run as SVG charts.

`bench_docs.py render` calls this module with the per-OS `compare.json` files
that `bench_compare.py` wrote, and commits the result to
`docs/performance/charts/` next to RESULTS.md. Every chart is one metric with
three stacked panels (Linux, macOS, Windows) of horizontal bars; horon's bar is
blue and the rest are grey. A summary chart puts horon's value and rank for
every metric and OS on one page.

The output is a pure function of the input files, the commit and the language:
no clock, no network, no fonts or packages beyond the standard library. The
same inputs always give the same bytes, so a rerun of the docs workflow leaves
the charts unchanged and the diff shows only real measurement changes.

A value that the comparison table shows as "—" is drawn as "not measured", never
as a zero-length bar: a failed run, a run whose tool round trip never came back,
a tool missing on that OS, or a counter the OS cannot record (fsync calls need
strace, which only the Linux runner has).
"""

from __future__ import annotations

import datetime as _dt
import json
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Callable
from xml.sax.saxutils import escape

PLATFORMS = (("linux", "Linux"), ("macos", "macOS"), ("windows", "Windows"))
HIGHLIGHT = "horon"
MIB = 1024 * 1024

# Colours follow the GitHub light palette; the chart paints its own white card
# so it stays readable on a dark page as well.
BLUE = "#0b84de"
GREY = "#d3d9e1"
GRID = "#eaeef2"
INK = "#1f2328"
MUTED = "#59636e"
BORDER = "#d0d7de"
GOOD = "#dafbe1"
WARN = "#fff1d6"
NONE = "#f2f4f6"
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'Noto Sans KR', 'Apple SD Gothic Neo', Helvetica, Arial, sans-serif"

WIDTH = 960
PAD = 32
ROW = 30
BAR = 18


@dataclass(frozen=True)
class Metric:
    stem: str
    unit: str  # "ms", "bytes" or "count"
    title: dict[str, str]
    note: dict[str, str]
    pick: Callable[[dict], float | None]


def _get(data: dict | None, *path: str):
    for key in path:
        if not isinstance(data, dict):
            return None
        data = data.get(key)
    return data


def _number(value) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if not math.isfinite(value) or value < 0:
        return None
    return float(value)


def _comparable(data: dict | None) -> dict | None:
    """The comparison table blanks a scenario whose round trip failed; so do we."""
    if not isinstance(data, dict):
        return None
    if data.get("status") == "failed":
        return None
    if data.get("tool_roundtrip") is False or data.get("tool_round_trip") is False:
        return None
    return data


def _scenario(key: str, field: str) -> Callable[[dict], float | None]:
    return lambda result: _number(_get(_comparable(result.get(key)), field))


def _version(result: dict) -> float | None:
    return _number(_get(result, "version_ms", "summary", "median"))


def _most_sessions(result: dict) -> float | None:
    points = _get(result, "sessions_memory", "points") or []
    usable = [p for p in points if isinstance(p, dict) and _number(p.get("post_turn_rss_bytes")) is not None]
    if not usable:
        return None
    return _number(max(usable, key=lambda p: p.get("sessions") or 0)["post_turn_rss_bytes"])


def _memory(field: str) -> Callable[[dict], float | None]:
    return lambda result: _number(_get(result, "sessions_memory", field))


METRICS = (
    Metric("01-version", "ms",
           {"en": "`--version` time", "ko": "`--version` 시간"},
           {"en": "Median of the --version runs.", "ko": "--version 실행 중앙값."},
           _version),
    Metric("02-install-size", "bytes",
           {"en": "Install size", "ko": "설치 크기"},
           {"en": "Binary or npm package directory size.", "ko": "바이너리 또는 npm 패키지 폴더 크기."},
           lambda result: _number(result.get("install_size_bytes"))),
    Metric("03-oneshot-time", "ms",
           {"en": "One-shot prompt time", "ko": "원샷 프롬프트 시간"},
           {"en": "Median, including startup, one ls tool call and the final answer.",
            "ko": "중앙값. 기동, ls 도구 호출 한 번, 최종 답변까지."},
           _scenario("oneshot", "ms")),
    Metric("04-oneshot-memory", "bytes",
           {"en": "One-shot prompt memory", "ko": "원샷 프롬프트 메모리"},
           {"en": "Peak process-tree RSS.", "ko": "프로세스 트리 RSS 최댓값."},
           _scenario("oneshot", "peak_rss_bytes")),
    Metric("05-skill-time", "ms",
           {"en": "Skill invocation time", "ko": "스킬 호출 시간"},
           {"en": "Median, process start to final answer with one fixture skill load.",
            "ko": "중앙값. 프로세스 시작부터 스킬 한 번 로드 후 최종 답변까지."},
           _scenario("skill", "ms")),
    Metric("06-mcp-time", "ms",
           {"en": "MCP tool call time", "ko": "MCP 도구 호출 시간"},
           {"en": "Median, including starting the stdio MCP server and one call.",
            "ko": "중앙값. stdio MCP 서버 기동과 호출 한 번 포함."},
           _scenario("mcp", "ms")),
    Metric("07-sessions-memory", "bytes",
           {"en": "Memory with the most sessions", "ko": "최대 세션 수에서의 메모리"},
           {"en": "Post-turn process-tree RSS at the largest measured session count.",
            "ko": "가장 많은 세션 수에서 턴 완료 후 프로세스 트리 RSS."},
           _most_sessions),
    Metric("08-per-session-memory", "bytes",
           {"en": "Memory per additional session", "ko": "세션 하나당 늘어나는 메모리"},
           {"en": "Least-squares slope of post-turn RSS across session counts.",
            "ko": "세션 수에 따른 턴 완료 후 RSS의 최소제곱 기울기."},
           _memory("each_additional_post_turn_rss_bytes")),
    Metric("09-fsync-calls", "count",
           {"en": "fsync calls per one-shot prompt", "ko": "원샷 프롬프트당 fsync 호출 수"},
           {"en": "Counted with strace, so only Linux records it.",
            "ko": "strace 로 세므로 Linux 에서만 기록합니다."},
           _scenario("oneshot", "fsync_calls")),
    Metric("10-first-session-memory", "bytes",
           {"en": "First session memory", "ko": "첫 세션 메모리"},
           {"en": "Post-turn process-tree RSS with one session.",
            "ko": "세션 하나로 턴을 마친 뒤 프로세스 트리 RSS."},
           _memory("first_session_post_turn_rss_bytes")),
)

TEXT = {
    "en": {
        "lower": "lower is better",
        "legend": "Blue = horon.",
        "na": "not measured",
        "no_os": "This run has no comparison result for {os}.",
        "empty": "No tool has a value for this metric on {os}.",
        "sessions": "{n} sessions",
        "source": "Source: horon-ai/agent {commit} · measured {when} UTC{run}",
        "run": " · run {run}",
        "runners": "Each OS ran on a different GitHub-hosted runner with a mock model: compare tools within one OS, not across OSes.",
        "summary": "Benchmark summary: horon against {n} tools",
        "summary_note": "Lower is better for every metric. Green: horon is lowest. Amber: a tool is lower, shown as its multiple of horon.",
        "metric": "Metric",
        "rank": "#{rank} of {count}",
        "next": "next: {tool} {ratio}",
        "best": "best: {tool} {ratio}",
        "alone": "no other tool measured",
        "no_strace": "(strace is not available on this runner.)",
    },
    "ko": {
        "lower": "낮을수록 좋음",
        "legend": "파랑 = horon.",
        "na": "측정 불가",
        "no_os": "이 실행에는 {os} 비교 결과가 없습니다.",
        "empty": "{os} 에서는 이 항목의 값이 있는 도구가 없습니다.",
        "sessions": "세션 {n}개",
        "source": "출처: horon-ai/agent {commit} · 측정 {when} UTC{run}",
        "run": " · 실행 {run}",
        "runners": "OS마다 다른 GitHub 호스티드 러너와 모의 모델로 쟀습니다. 같은 OS 안에서만 비교하고 OS끼리는 비교하지 않습니다.",
        "summary": "벤치마크 요약: horon 대 도구 {n}개",
        "summary_note": "모든 항목이 낮을수록 좋습니다. 초록: horon이 가장 낮음. 주황: 더 낮은 도구가 있으며 horon 대비 배수로 표시.",
        "metric": "항목",
        "rank": "{count}개 중 {rank}위",
        "next": "다음: {tool} {ratio}",
        "best": "최저: {tool} {ratio}",
        "alone": "비교할 다른 도구 없음",
        "no_strace": "(이 러너에는 strace 가 없습니다.)",
    },
}

UNIT_LABEL = {
    "en": {"ms": "ms", "count": "calls"},
    "ko": {"ms": "ms", "count": "회"},
}


# --- input -----------------------------------------------------------------


def load_compare(dirs: dict[str, Path]) -> dict[str, dict]:
    """Read compare.json from each OS job directory; unreadable files count as absent."""
    found: dict[str, dict] = {}
    for key, _ in PLATFORMS:
        directory = dirs.get(key)
        path = directory / "compare.json" if directory else None
        if path is None or not path.is_file():
            continue
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if isinstance(data, dict) and isinstance(data.get("results"), list):
            found[key] = data
    return found


def _tools(runs: dict[str, dict]) -> list[tuple[str, str]]:
    """(name, label) in first-seen order across the OSes, horon first."""
    seen: dict[str, str] = {}
    for key, _ in PLATFORMS:
        for result in (runs.get(key) or {}).get("results", []):
            if isinstance(result, dict) and isinstance(result.get("name"), str):
                seen.setdefault(result["name"], str(result.get("label") or result["name"]))
    names = sorted(seen, key=lambda name: name != HIGHLIGHT)
    return [(name, seen[name]) for name in names]


def _values(run: dict | None, metric: Metric, tools: list[tuple[str, str]]) -> list[float | None]:
    by_name = {r.get("name"): r for r in (run or {}).get("results", []) if isinstance(r, dict)}
    out = []
    for name, _ in tools:
        result = by_name.get(name)
        out.append(None if result is None or result.get("error") else metric.pick(result))
    return out


def _measured_at(runs: dict[str, dict]) -> str:
    stamps = sorted(str(run.get("started_at")) for run in runs.values() if run.get("started_at"))
    if not stamps:
        return "—"
    try:
        when = _dt.datetime.strptime(stamps[0], "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        return escape(stamps[0][:16])
    return when.strftime("%Y-%m-%d %H:%M")


# --- formatting --------------------------------------------------------------


def _scale(metric: Metric, values: list[float | None]) -> tuple[float, str]:
    """Divisor and unit label so one chart uses one unit across its panels."""
    if metric.unit != "bytes":
        return 1.0, metric.unit
    top = max((v for v in values if v is not None), default=0.0)
    if top >= 10 * 1024 * MIB:
        return 1024.0 * MIB, "GB"
    if top >= MIB:
        return float(MIB), "MB"
    return 1024.0, "KB"


def _num(value: float) -> str:
    if value >= 1000:
        return f"{value:,.0f}"
    if value >= 100:
        return f"{value:.0f}"
    if value >= 10:
        return f"{value:.1f}"
    return f"{value:.2f}"


def _ratio(value: float, base: float) -> str:
    if base <= 0:
        return ""
    ratio = value / base
    return f"{ratio:.1f}×" if ratio >= 10 else f"{ratio:.2f}×"


def _with_unit(metric: Metric, value: float, lang: str) -> str:
    if metric.unit == "bytes":
        divisor, unit = _scale(metric, [value])
        return f"{_num(value / divisor)} {unit}"
    if metric.unit == "count":
        return f"{value:,.0f} {UNIT_LABEL[lang]['count']}"
    return f"{_num(value)} {UNIT_LABEL[lang][metric.unit]}"


def _nice_step(top: float) -> float:
    if top <= 0:
        return 1.0
    raw = top / 5
    power = 10 ** math.floor(math.log10(raw))
    for factor in (1, 2, 2.5, 5, 10):
        if raw <= factor * power:
            return factor * power
    return 10 * power


def _text_width(text: str, size: float) -> float:
    """Rough advance width; wide (CJK) characters count double."""
    return sum(size * (1.0 if ord(ch) > 0x2E80 else 0.58) for ch in text)


def _t(x: float, y: float, text: str, size: float = 13, fill: str = INK, anchor: str = "start",
       weight: str = "normal") -> str:
    return (f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" fill="{fill}" text-anchor="{anchor}"'
            f' font-weight="{weight}">{escape(text)}</text>')


def _svg(height: float, body: list[str], title: str) -> str:
    return "\n".join([
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height:.0f}" '
        f'viewBox="0 0 {WIDTH} {height:.0f}" font-family="{escape(FONT, {chr(34): "&quot;"})}" role="img">',
        f"<title>{escape(title)}</title>",
        f'<rect x="0.5" y="0.5" width="{WIDTH - 1}" height="{height - 1:.0f}" rx="12" fill="#ffffff" stroke="{BORDER}"/>',
        *body,
        "</svg>",
        "",
    ])


def _wrap(text: str, size: float, width: float) -> list[str]:
    lines, line = [], ""
    for word in text.split(" "):
        candidate = f"{line} {word}".strip()
        if line and _text_width(candidate, size) > width:
            lines.append(line)
            line = word
        else:
            line = candidate
    return lines + ([line] if line else [])


def _footer(y: float, commit: str, when: str, run_id: str | None, lang: str) -> tuple[list[str], float]:
    text = TEXT[lang]
    run = text["run"].format(run=run_id) if run_id else ""
    lines = [text["source"].format(commit=commit[:7], when=when, run=run)]
    lines += _wrap(text["runners"], 12, WIDTH - 2 * PAD)
    body = [_t(PAD, y + 18 * i, line, 12, MUTED) for i, line in enumerate(lines)]
    return body, y + 18 * len(lines)


# --- one metric ----------------------------------------------------------------


def _panel(y: float, title: str, values: list[float | None] | None, tools: list[tuple[str, str]],
           metric: Metric, divisor: float, label_w: float, lang: str, empty_note: str) -> tuple[list[str], float]:
    text = TEXT[lang]
    body = [f'<rect x="{PAD}" y="{y:.1f}" width="5" height="22" rx="1" fill="{BLUE}"/>',
            _t(PAD + 14, y + 18, title, 18, INK, weight="bold")]
    y += 40
    if values is None or all(v is None for v in values):
        body.append(_t(PAD, y + 8, empty_note, 13, MUTED))
        return body, y + 28
    x0 = PAD + label_w
    plot_w = WIDTH - PAD - x0 - 130
    scaled = [None if v is None else v / divisor for v in values]
    top = max(v for v in scaled if v is not None)
    step = _nice_step(top)
    ticks = max(1, math.ceil(top / step)) if top > 0 else 1
    span = step * ticks
    rows_h = ROW * len(tools)
    for i in range(ticks + 1):
        x = x0 + plot_w * i / ticks
        body.append(f'<line x1="{x:.1f}" y1="{y:.1f}" x2="{x:.1f}" y2="{y + rows_h:.1f}" stroke="{GRID}"/>')
        tick = step * i
        tick_text = f"{tick:,.0f}" if float(tick).is_integer() else _num(tick)
        body.append(_t(x, y + rows_h + 16, tick_text, 11, MUTED, "middle"))
    base = scaled[0] if tools and tools[0][0] == HIGHLIGHT else None
    for (name, label), value in zip(tools, scaled):
        mine = name == HIGHLIGHT
        cy = y + ROW / 2
        body.append(_t(x0 - 10, cy + 4.5, label, 12.5, INK, "end", "bold" if mine else "normal"))
        if value is None:
            body.append(f'<rect x="{x0:.1f}" y="{cy - BAR / 2:.1f}" width="96" height="{BAR}" rx="3" '
                        f'fill="none" stroke="{GREY}" stroke-dasharray="4 3"/>')
            body.append(_t(x0 + 48, cy + 4, text["na"], 11, MUTED, "middle"))
        else:
            width = max(plot_w * value / span, 2.0) if span else 2.0
            body.append(f'<rect x="{x0:.1f}" y="{cy - BAR / 2:.1f}" width="{width:.1f}" height="{BAR}" rx="3" '
                        f'fill="{BLUE if mine else GREY}"/>')
            label_text = f"{value:,.0f}" if metric.unit == "count" else _num(value)
            if not mine and base:
                label_text += f" ({_ratio(value, base)})"
            body.append(_t(x0 + width + 8, cy + 4.5, label_text, 12, INK if mine else MUTED, weight="bold" if mine else "normal"))
        y += ROW
    return body, y + 30


def render_metric(metric: Metric, runs: dict[str, dict], commit: str, lang: str,
                  run_id: str | None = None) -> str:
    text = TEXT[lang]
    tools = _tools(runs)
    per_os = {key: (_values(runs[key], metric, tools) if key in runs else None) for key, _ in PLATFORMS}
    everything = [v for vals in per_os.values() if vals for v in vals]
    divisor, unit = _scale(metric, everything)
    unit_text = UNIT_LABEL[lang].get(unit, unit)
    title = f"{metric.title[lang].replace('`', '')} ({unit_text}, {text['lower']})"
    body = [_t(PAD, 44, title, 22, INK, weight="bold"),
            _t(PAD, 68, f"{metric.note[lang]} {text['legend']}", 13, MUTED)]
    label_w = max([_text_width(label, 12.5) for _, label in tools] + [80]) + 18
    y = 96.0
    for key, os_title in PLATFORMS:
        heading = os_title
        if metric.stem == "07-sessions-memory" and key in runs:
            counts = [p.get("sessions") for r in runs[key]["results"] if isinstance(r, dict)
                      for p in (_get(r, "sessions_memory", "points") or []) if isinstance(p, dict)]
            counts = [c for c in counts if isinstance(c, int)]
            if counts:
                heading += " · " + text["sessions"].format(n=max(counts))
        note = text["no_os" if key not in runs else "empty"].format(os=os_title)
        if key in runs and metric.stem == "09-fsync-calls":
            skipped = [s for s in runs[key].get("skipped", []) if isinstance(s, dict) and s.get("name") == "syscalls"]
            if skipped:
                note += " " + text["no_strace"]
        part, y = _panel(y, heading, per_os[key], tools, metric, divisor, label_w, lang, note)
        body += part
    footer, y = _footer(y + 6, commit, _measured_at(runs), run_id, lang)
    return _svg(y + 22, body + footer, title)


# --- summary -------------------------------------------------------------------


def _cell(metric: Metric, run: dict | None, tools: list[tuple[str, str]], lang: str) -> tuple[str, str, str]:
    """(fill, first line, second line) for horon's value of one metric on one OS."""
    text = TEXT[lang]
    if run is None:
        return NONE, text["na"], ""
    values = _values(run, metric, tools)
    if not tools or tools[0][0] != HIGHLIGHT or values[0] is None:
        return NONE, text["na"], ""
    mine = values[0]
    others = sorted(((v, label) for (_, label), v in zip(tools[1:], values[1:]) if v is not None),
                    key=lambda item: item[0])
    if not others:
        return NONE, _with_unit(metric, mine, lang), text["alone"]
    rank = 1 + sum(1 for v, _ in others if v < mine)
    first = f"{_with_unit(metric, mine, lang)} · {text['rank'].format(rank=rank, count=len(others) + 1)}"
    if rank == 1:
        value, tool = others[0]
        return GOOD, first, text["next"].format(tool=tool, ratio=_ratio(value, mine) if mine else "")
    value, tool = others[0]
    return WARN, first, text["best"].format(tool=tool, ratio=_ratio(value, mine) if mine else "")


def render_summary(runs: dict[str, dict], commit: str, lang: str, run_id: str | None = None) -> str:
    text = TEXT[lang]
    tools = _tools(runs)
    title = text["summary"].format(n=max(len(tools) - 1, 0))
    body = [_t(PAD, 44, title, 22, INK, weight="bold")]
    note_lines = _wrap(text["summary_note"], 13, WIDTH - 2 * PAD)
    body += [_t(PAD, 68 + 18 * i, line, 13, MUTED) for i, line in enumerate(note_lines)]
    y = 68 + 18 * len(note_lines) + 14
    first_w = 250
    col_w = (WIDTH - 2 * PAD - first_w) / len(PLATFORMS)
    body.append(_t(PAD, y + 16, text["metric"], 13, MUTED, weight="bold"))
    for i, (_, os_title) in enumerate(PLATFORMS):
        body.append(_t(PAD + first_w + col_w * i + 10, y + 16, os_title, 13, MUTED, weight="bold"))
    y += 28
    cell_h = 50
    for metric in METRICS:
        name = metric.title[lang].replace("`", "")
        body.append(_t(PAD, y + 30, name, 13.5, INK, weight="bold"))
        for i, (key, _) in enumerate(PLATFORMS):
            fill, line1, line2 = _cell(metric, runs.get(key), tools, lang)
            x = PAD + first_w + col_w * i
            body.append(f'<rect x="{x + 3:.1f}" y="{y + 4:.1f}" width="{col_w - 6:.1f}" height="{cell_h - 8}" rx="6" fill="{fill}"/>')
            body.append(_t(x + 12, y + 22, line1, 12.5, INK, weight="bold"))
            if line2:
                body.append(_t(x + 12, y + 39, line2, 11.5, MUTED))
        y += cell_h
    footer, y = _footer(y + 22, commit, _measured_at(runs), run_id, lang)
    return _svg(y + 22, body + footer, title)


# --- files and markdown ------------------------------------------------------------


SUMMARY_STEM = "00-summary"
MD_TEXT = {
    "en": {
        "intro": "`.github/workflows/benchmark-docs.yml` redraws these charts from the competitor comparison of the automatic measurement below. Do not edit this section by hand. Lower is better in every chart, and horon is the blue bar.",
        "summary": "Benchmark summary",
        "missing": "The latest automatic measurement has no competitor comparison, so there are no charts.",
        "header": "| Metric | Chart |",
    },
    "ko": {
        "intro": "이 구간은 `.github/workflows/benchmark-docs.yml` 이 아래 자동 측정의 경쟁 도구 비교로 다시 그립니다. 손으로 고치지 않습니다. 모든 차트는 낮을수록 좋고 파란 막대가 horon 입니다.",
        "summary": "벤치마크 요약",
        "missing": "최신 자동 측정에 경쟁 도구 비교가 없어서 차트가 없습니다.",
        "header": "| 항목 | 차트 |",
    },
}


MD_TUI = {
    "en": {
        "intro": "The interactive client is a second binary; these charts are drawn from the `tui` suite only and are never mixed with the headless charts above.",
        "header": "| TUI metric | Chart |",
    },
    "ko": {
        "intro": "대화형 클라이언트는 별도 바이너리입니다. 이 차트는 `tui` 묶음만으로 그리며 위의 헤드리스 차트와 섞지 않습니다.",
        "header": "| TUI 항목 | 차트 |",
    },
}


def chart_name(stem: str, lang: str) -> str:
    return f"{stem}.svg" if lang == "en" else f"{stem}.{lang}.svg"


def render_charts(dirs: dict[str, Path], commit: str, lang: str, run_id: str | None = None) -> dict[str, str]:
    """Map file name to SVG text; empty when no OS produced a comparison."""
    runs = load_compare(dirs)
    if not runs:
        return {}
    files = {chart_name(SUMMARY_STEM, lang): render_summary(runs, commit, lang, run_id)}
    for metric in METRICS:
        files[chart_name(metric.stem, lang)] = render_metric(metric, runs, commit, lang, run_id)
    return files


# --- TUI charts ----------------------------------------------------------------
#
# The interactive client is measured by the `tui` suite, whose numbers never share
# a table or a chart with the headless comparison: its bars are timings of one
# program against itself, not of horon against other tools. Two charts keep the
# page short — latency in milliseconds, memory and size in bytes — and each OS
# gets its own panel, as in the comparison charts.


@dataclass(frozen=True)
class TuiMetric:
    stem: str
    unit: str
    title: dict[str, str]
    note: dict[str, str]
    rows: tuple[tuple[str, str], ...]


TUI_METRICS = (
    TuiMetric(
        "01-latency",
        "ms",
        {"en": "TUI latency", "ko": "TUI 지연"},
        {"en": "Median per run. `first_frame_ms` is the bare agent until the first screen, "
               "`direct_tui` is `horon-tui` started on its own, `input_echo_p95` is key press "
               "to echoed character, and `render` is one drawn frame with 1000 messages.",
         "ko": "실행별 중앙값. `first_frame_ms` 는 맨 에이전트가 첫 화면까지, "
               "`direct_tui` 는 `horon-tui` 를 단독 실행한 경우, `input_echo_p95` 는 키 입력에서 "
               "글자가 화면에 찍힐 때까지, `render` 는 메시지 1000개 상태의 프레임 한 장입니다."},
        (
            ("first_frame_ms", "agent first frame"),
            ("direct_tui_first_frame_ms", "tui first frame"),
            ("input_echo_p95_ms", "input echo p95"),
            ("render_p50_ms", "render p50"),
            ("render_p95_ms", "render p95"),
        ),
    ),
    TuiMetric(
        "02-memory",
        "bytes",
        {"en": "TUI memory and size", "ko": "TUI 메모리와 크기"},
        {"en": "Median per run. `idle` is the process tree 5 s after the first frame, `post-turn` "
               "is after one mock turn, and `install size` is the `horon-tui` binary.",
         "ko": "실행별 중앙값. `idle` 은 첫 화면 5초 뒤 프로세스 트리, `post-turn` 은 모의 턴 한 번 뒤, "
               "`install size` 는 `horon-tui` 바이너리 크기입니다."},
        (
            ("idle_rss_bytes", "idle RSS"),
            ("post_turn_rss_bytes", "post-turn RSS"),
            ("install_size_bytes", "install size"),
        ),
    ),
)

TUI_LABELS = {
    "en": {"agent first frame": "agent first frame", "tui first frame": "tui first frame",
           "input echo p95": "input echo p95", "render p50": "render p50", "render p95": "render p95",
           "idle RSS": "idle RSS", "post-turn RSS": "post-turn RSS", "install size": "install size"},
    "ko": {"agent first frame": "에이전트 첫 화면", "tui first frame": "tui 첫 화면",
           "input echo p95": "입력 반향 p95", "render p50": "렌더 p50", "render p95": "렌더 p95",
           "idle RSS": "유휴 RSS", "post-turn RSS": "턴 후 RSS", "install size": "설치 크기"},
}


def load_tui(dirs: dict[str, Path]) -> dict[str, dict]:
    """Read tui.json from each OS job directory; unreadable files count as absent."""
    found: dict[str, dict] = {}
    for key, _ in PLATFORMS:
        directory = dirs.get(key)
        path = directory / "tui.json" if directory else None
        if path is None or not path.is_file():
            continue
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if isinstance(data, dict) and data.get("suite") == "tui":
            found[key] = data
    return found


def tui_chart_names(lang: str) -> list[str]:
    return [chart_name(f"tui-{metric.stem}", lang) for metric in TUI_METRICS]


def tui_chart_name(lang: str) -> str:
    """The chart the TUI section links first; both charts sit in the same folder."""
    return tui_chart_names(lang)[0]


def _tui_rows(run: dict | None, metric: TuiMetric) -> list[float | None]:
    return [_number(_get(run, key, "median")) for key, _ in metric.rows]


def render_tui_chart(metric: TuiMetric, runs: dict[str, dict], commit: str, lang: str,
                     run_id: str | None = None) -> str:
    text = TEXT[lang]
    labels = TUI_LABELS[lang]
    tools = [(key, labels[label]) for key, label in metric.rows]
    per_os = {key: (_tui_rows(runs.get(key), metric) if key in runs else None) for key, _ in PLATFORMS}
    everything = [v for vals in per_os.values() if vals for v in vals]
    pseudo = Metric(metric.stem, metric.unit, metric.title, metric.note, lambda result: None)
    divisor, unit = _scale(pseudo, everything)
    unit_text = UNIT_LABEL[lang].get(unit, unit)
    title = f"{metric.title[lang].replace('`', '')} ({unit_text}, {text['lower']})"
    body = [_t(PAD, 44, title, 22, INK, weight="bold")]
    for i, line in enumerate(_wrap(f"{metric.note[lang]}", 13, WIDTH - 2 * PAD)):
        body.append(_t(PAD, 68 + 18 * i, line, 13, MUTED))
    label_w = max([_text_width(label, 12.5) for _, label in tools] + [80]) + 18
    y = 68 + 18 * max(len(_wrap(metric.note[lang], 13, WIDTH - 2 * PAD)), 1) + 14
    for key, os_title in PLATFORMS:
        skipped = _tui_skip_note(runs.get(key))
        note = text["no_os" if key not in runs else "empty"].format(os=os_title)
        if skipped:
            note += " " + skipped
        part, y = _panel(y, os_title, per_os[key], tools, pseudo, divisor, label_w, lang, note)
        body += part
    footer, y = _footer(y + 6, commit, _measured_at(runs), run_id, lang)
    return _svg(y + 22, body + footer, title)


def _tui_skip_note(run: dict | None) -> str:
    """Name the recorded skip reason once instead of leaving the bars silently absent."""
    for entry in (run or {}).get("skipped", []) or []:
        if isinstance(entry, dict) and isinstance(entry.get("reason"), str):
            return f"({entry['reason']})"
    return ""


def render_tui_charts(dirs: dict[str, Path], commit: str, lang: str,
                      run_id: str | None = None) -> dict[str, str]:
    """Map file name to SVG text; empty when no OS produced a TUI result."""
    runs = load_tui(dirs)
    if not runs:
        return {}
    return {
        chart_name(f"tui-{metric.stem}", lang): render_tui_chart(metric, runs, commit, lang, run_id)
        for metric in TUI_METRICS
    }


def markdown(lang: str, has_charts: bool, folder: str = "charts", has_tui: bool = False) -> str:
    text = MD_TEXT[lang]
    if not has_charts:
        return text["missing"]
    lines = [
        text["intro"],
        "",
        f"![{text['summary']}]({folder}/{chart_name(SUMMARY_STEM, lang)})",
        "",
        text["header"],
        "| --- | --- |",
    ]
    for metric in METRICS:
        name = chart_name(metric.stem, lang)
        lines.append(f"| {metric.title[lang]} | [{name}]({folder}/{name}) |")
    if has_tui:
        lines += [
            "",
            MD_TUI[lang]["intro"],
            "",
            MD_TUI[lang]["header"],
            "| --- | --- |",
        ]
        for metric in TUI_METRICS:
            name = chart_name(f"tui-{metric.stem}", lang)
            lines.append(f"| {metric.title[lang]} | [{name}]({folder}/{name}) |")
    return "\n".join(lines)
