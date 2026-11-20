#!/usr/bin/env python3
"""Draw the hand-written measurement records of RESULTS.md as SVG charts.

`bench_charts.py` draws the automatic measurement from the run's `compare.json`
files. The records above it in `docs/performance/RESULTS.md` are hand-written:
each plan left a few tables of before/after numbers, and those tables stay the
record. This script turns the numbers those tables already state into pictures,
one file per figure, so the page leads with what the numbers mean and the exact
tables carry the precision.

Every number here is copied from the record tables in `RESULTS.md` and
`RESULTS-APPENDIX.md`; nothing is derived except the ratio labels, and each
figure lists the section it draws. `test_record_charts.py` fails when a number
in `TOKENS` is no longer in those documents, and `--check` fails when a
committed SVG differs from what this script produces, so the pictures cannot
drift away from the record.

    python3 scripts/perf/record_charts.py           # rewrite docs/performance/charts/record-*.svg
    python3 scripts/perf/record_charts.py --check   # exit 1 when a committed chart is stale

The output is a pure function of the tables below: no clock, no network, no
package beyond the standard library. English charts are `record-NN-name.svg`
and Korean ones `record-NN-name.ko.svg`. `bench_docs.py` never writes a file
whose name starts with `record-`, so the automatic render leaves these alone.
"""

from __future__ import annotations

import argparse
import math
import sys
from dataclasses import dataclass, field
from pathlib import Path

import bench_charts as base

ROOT = Path(__file__).resolve().parents[2]
CHARTS = ROOT / "docs" / "performance" / "charts"

WIDTH, PAD = base.WIDTH, base.PAD
INK, MUTED = base.INK, base.MUTED
BLUE, GREY, GRID, BORDER = base.BLUE, base.GREY, base.GRID, base.BORDER
FONT = base.FONT
ROW = 30
BAR = 16

LABELS: dict[str, dict[str, str]] = {
    "en": {
        "before": "before",
        "after": "after",
        "source": "Source: docs/performance/RESULTS.md · {section}",
        "lower": "lower is better",
        "scaled": "Each row is scaled to its own before value; read the numbers, not the bar lengths.",
        "na": "not measured",
        "axis": "{value}",
        "legend": "{name}",
    },
    "ko": {
        "before": "이전",
        "after": "이후",
        "source": "출처: docs/performance/RESULTS.md · {section}",
        "lower": "낮을수록 좋음",
        "scaled": "행마다 이전 값을 기준으로 크기를 맞췄습니다. 막대 길이가 아니라 수치를 읽습니다.",
        "na": "측정 안 됨",
        "axis": "{value}",
        "legend": "{name}",
    },
}


@dataclass(frozen=True)
class Pair:
    """One before/after row; `text` is the display string quoted from the record."""

    label: dict[str, str]
    before: float
    after: float
    text: dict[str, str]


@dataclass(frozen=True)
class Series:
    name: dict[str, str]
    values: tuple[float | None, ...]
    emphasis: bool = False


@dataclass(frozen=True)
class Chart:
    stem: str
    kind: str  # "pairs" or "series"
    title: dict[str, str]
    note: dict[str, str]
    section: dict[str, str]
    rows: tuple[Pair, ...] = ()
    x: tuple[dict[str, str], ...] = ()
    series: tuple[Series, ...] = ()
    log: bool = False
    tokens: tuple[str, ...] = field(default_factory=tuple)


def _p(label_en: str, label_ko: str, before: float, after: float, text: str) -> Pair:
    return Pair({"en": label_en, "ko": label_ko}, before, after, {"en": text, "ko": text})


def _n(en: str, ko: str) -> dict[str, str]:
    return {"en": en, "ko": ko}


CHARTS_DATA: tuple[Chart, ...] = (
    Chart(
        stem="01-session-store",
        kind="pairs",
        title=_n("Session store: one request, before and after the plan",
                 "세션 저장소: 요청 하나의 이전과 이후"),
        note=_n("Measured on Apple M1 Max in the debug test profile, before and after alternating on one machine.",
                "Apple M1 Max, 디버그 테스트 프로필에서 같은 기계로 이전과 이후를 번갈아 쟀습니다."),
        section=_n("Measured results", "측정 결과"),
        rows=(
            _p("Source hash peak allocation (200 MiB log)", "소스 해시 최고 할당 (200 MiB 로그)",
               268435520, 65600, "268,435,520 B → 65,600 B (4,092×)"),
            _p("History page bytes read (100k events)", "이력 페이지 읽은 바이트 (10만 이벤트)",
               62169922, 18554, "62,169,922 B → 18,554 B"),
            _p("History page wall clock (100k events)", "이력 페이지 벽시계 (10만 이벤트)",
               26200, 5.9, "26.2 s → 5.9 ms"),
            _p("History page messages flattened", "이력 페이지 평탄화한 메시지",
               99999, 20, "99,999 → 20"),
            _p("Cache facade over-limit allocation", "캐시 facade 상한 초과 할당",
               3146158, 0, "3,146,158 B → 0 B"),
            _p("SSE parses of a 12-payload stream", "12개 페이로드 SSE 파싱 횟수",
               24, 12, "24 → 12"),
        ),
        tokens=("268,435,520", "65,600", "4,092", "62,169,922", "18,554",
                "26.2", "5.9", "99,999", "3,146,158"),
    ),
    Chart(
        stem="02-page-cost",
        kind="rows",
        title=_n("One history page costs the same as the log grows 100×",
                 "로그가 100배 커져도 이력 페이지 한 장 값은 그대로"),
        note=_n("20-item page from an index; the log grows 514,853 B → 51,888,853 B.",
                "인덱스에서 꺼내는 20개 페이지. 로그는 514,853 B → 51,888,853 B 로 큽니다."),
        section=_n("Is one page fetched at the same cost as the log grows?",
                   "로그가 커져도 페이지 한 장 값이 같은가?"),
        rows=(
            _p("1,000 events", "이벤트 1,000", 15375, 15375, "15,375 B · 4.28 ms"),
            _p("10,000 events", "이벤트 10,000", 15415, 15415, "15,415 B · 4.68 ms"),
            _p("100,000 events", "이벤트 100,000", 15455, 15455, "15,455 B · 4.80 ms"),
        ),
        tokens=("514,853", "5,168,853", "51,888,853", "15,375", "15,415", "15,455",
                "4.28", "4.68", "4.80"),
    ),
    Chart(
        stem="03-irc-throughput",
        kind="groups",
        title=_n("IRC bus throughput inside one process", "한 프로세스 안 IRC 버스 처리량"),
        note=_n("Median of three runs; messages per second on Apple M1 Max.",
                "세 번 실행의 중앙값. Apple M1 Max 에서 초당 메시지 수입니다."),
        section=_n("IRC throughput", "IRC 처리량"),
        x=(_n("direct_waiter", "direct_waiter"), _n("direct_deliver", "direct_deliver"),
           _n("broadcast64", "broadcast64")),
        series=(
            Series(_n("median msg/s", "중앙값 msg/s"), (107434, 2392230, 3141375), True),
            Series(_n("slowest run", "가장 느린 실행"), (105291, 2229008, 3141178)),
        ),
        log=True,
        tokens=("107,434", "105,291", "111,523", "2,392,230", "2,229,008", "2,410,858",
                "3,141,375", "3,141,178", "3,237,934"),
    ),
    Chart(
        stem="04-platform-time",
        kind="pairs",
        title=_n("Start path on macOS and Linux: before and after",
                 "macOS·Linux 시작 경로: 이전과 이후"),
        note=_n("Same-machine alternation, release profile. jcode's one-shot is shown for scale only.",
                "같은 기계에서 번갈아 잰 release 프로필. jcode 원샷은 규모 비교용입니다."),
        section=_n("Before and after across three environments", "세 환경의 이전과 이후"),
        rows=(
            _p("one-shot median, macOS", "원샷 중앙값, macOS", 872.4, 290.4,
               "872.4 → 290.4 ms (jcode 294.8 ms)"),
            _p("one-shot median, Linux (Docker)", "원샷 중앙값, Linux (Docker)", 207.9, 168.0,
               "207.9 → 168.0 ms (jcode 146.3 ms)"),
            _p("start→first request, macOS", "시작→첫 요청, macOS", 236.4, 78.7,
               "236.4 → 78.7 ms"),
            _p("start→first request, Linux (Docker)", "시작→첫 요청, Linux (Docker)", 57.0, 57.0,
               "57.0 → 57.0 ms"),
        ),
        tokens=("872.4", "290.4", "294.8", "207.9", "168.0", "146.3", "236.4", "78.7", "57.0"),
    ),
    Chart(
        stem="05-fsync-trend",
        kind="groups",
        title=_n("fsync calls per one-shot prompt on the GitHub Linux runner",
                 "GitHub Linux 러너의 원샷 프롬프트당 fsync 호출 수"),
        note=_n("Each point is a separate benchmark run; read the direction, not a same-job A/B.",
                "점마다 별도 벤치마크 실행입니다. 같은 잡의 A/B 가 아니라 방향만 읽습니다."),
        section=_n("Fewer fsyncs and the structural difference from jcode (#47)",
                   "fsync 줄이기와 jcode 와의 구조 차이 (#47)"),
        x=(_n("platform-adapter-perf", "platform-adapter-perf"),
           _n("cache-context-session-opt", "cache-context-session-opt"),
           _n("jcode-parity-optimization", "jcode-parity-optimization"),
           _n("fewer fsyncs (#47)", "fsync 줄이기 (#47)"),
           _n("close-save (#44)", "close-save (#44)")),
        series=(
            Series(_n("horon, cold", "horon, 콜드"), (40, 32, 27, 16, 15), True),
            Series(_n("horon, warm", "horon, 웜"), (35, 27, 22, 15, 14), True),
            Series(_n("jcode, warm", "jcode, 웜"), (5, 5, 5, 5, 5)),
        ),
        tokens=("40", "35", "32", "27", "22", "16", "15", "14", "5"),
    ),
    Chart(
        stem="06-per-session-memory",
        kind="groups",
        title=_n("Per-session cost after the jcode-parity plan",
                 "jcode-parity 계획 뒤 세션 하나당 비용"),
        note=_n("One job, same runner, baseline `b62a79c9` alternating with the candidate; jcode beside them.",
                "한 잡·같은 러너에서 기준선 `b62a79c9` 와 후보를 번갈아 쟀고, jcode 를 함께 뒀습니다."),
        section=_n("Measurements from the `jcode-parity-optimization` plan",
                   "`jcode-parity-optimization` 계획 측정"),
        x=(_n("memory slope, Linux", "메모리 기울기, Linux"),
           _n("memory slope, macOS", "메모리 기울기, macOS"),
           _n("threads / session, Linux", "세션당 스레드, Linux"),
           _n("threads / session, macOS", "세션당 스레드, macOS")),
        series=(
            Series(_n("before", "이전"), (254.7, 249.6, 1.14, 1.34), True),
            Series(_n("after", "이후"), (149.6, 185.1, 0.16, 0.26)),
            Series(_n("jcode", "jcode"), (57.4, 153.4, 0.07, None)),
        ),
        log=True,
        tokens=("254.7", "149.6", "57.4", "249.6", "185.1", "153.4", "1.14", "0.16", "0.07",
                "1.34", "0.26"),
    ),
    Chart(
        stem="07-skill-mcp",
        kind="pairs",
        title=_n("Skill and MCP loading: baseline and candidate in one job",
                 "스킬·MCP 로딩: 같은 잡에서 기준선과 후보"),
        note=_n("run 42461323656, `ubuntu-24.04`, 3 warmups and 30 measurements per side.",
                "실행 42461323656, `ubuntu-24.04`, 양쪽 3회 워밍업 뒤 30회 측정."),
        section=_n("Measurements from the `skill-mcp-loading-omp` plan",
                   "`skill-mcp-loading-omp` 계획 측정"),
        rows=(
            _p("skill_request_overlay, 200 skills", "skill_request_overlay, 스킬 200",
               28.30, 4.79, "28.30 → 4.79 ms"),
            _p("skill_request_overlay, 1000 skills", "skill_request_overlay, 스킬 1000",
               149.03, 26.46, "149.03 → 26.46 ms"),
            _p("skill_catalog, 200 skills", "skill_catalog, 스킬 200",
               4.41, 3.00, "4.41 → 3.00 ms"),
            _p("skill_catalog, 1000 skills", "skill_catalog, 스킬 1000",
               21.88, 15.00, "21.88 → 15.00 ms"),
            _p("mcp_config_load, 300 KB file", "mcp_config_load, 300 KB 파일",
               3.46, 0.22, "3.46 → 0.22 ms"),
        ),
        tokens=("28.30", "4.79", "149.03", "26.46", "4.41", "3.00", "21.88", "15.00",
                "3.46", "0.22"),
    ),
    Chart(
        stem="08-prompt-surface",
        kind="pairs",
        title=_n("What one turn sends before the model is called",
                 "모델을 부르기 전 한 턴이 보내는 것"),
        note=_n("Request bytes from the mock server, one measurement per surface.",
                "모의 서버가 받은 요청 바이트, 표면마다 한 번씩 쟀습니다."),
        section=_n("Prompt surface and session context", "프롬프트 표면과 세션 문맥"),
        rows=(
            _p("Eager core tool catalog (8 schemas)", "코어 도구 카탈로그 (스키마 8개)",
               9714, 9151, "9,714 → 9,151 B"),
            _p("Core one-shot first request `tools`", "코어 원샷 첫 요청 `tools`",
               9818, 9255, "9,818 → 9,255 B"),
            _p("Static prompt (core, no prompt files)", "고정 프롬프트 (코어, 프롬프트 파일 없음)",
               1367, 1322, "1,367 → 1,322 B"),
            _p("`# Additional tools` index", "`# Additional tools` 색인",
               629, 585, "629 → 585 B"),
        ),
        tokens=("9,714", "9,151", "9,818", "9,255", "1,367", "1,322", "629", "585"),
    ),
    Chart(
        stem="09-prompt-time",
        kind="pairs",
        title=_n("One-shot, core, skill and MCP time by OS, before and after",
                 "OS별 원샷·코어·스킬·MCP 시간, 이전과 이후"),
        note=_n("Medians in ms from run 43667470921, before and after alternating in one job.",
                "실행 43667470921 의 중앙값(ms). 한 잡에서 이전과 이후를 번갈아 쟀습니다."),
        section=_n("Time and memory (median ms, min–max in parentheses)",
                   "시간과 메모리 (중앙값 ms, 괄호는 최소–최대)"),
        rows=(
            _p("Linux · one-shot", "Linux · 원샷", 40.24, 29.35, "40.24 → 29.35 ms"),
            _p("Linux · core one-shot", "Linux · 코어 원샷", 42.92, 31.31, "42.92 → 31.31 ms"),
            _p("Linux · skill call", "Linux · 스킬 호출", 37.43, 26.31, "37.43 → 26.31 ms"),
            _p("Linux · MCP tool call", "Linux · MCP 도구 호출", 55.62, 40.16, "55.62 → 40.16 ms"),
            _p("macOS · one-shot", "macOS · 원샷", 54.93, 47.07, "54.93 → 47.07 ms"),
            _p("macOS · core one-shot", "macOS · 코어 원샷", 60.63, 49.28, "60.63 → 49.28 ms"),
            _p("macOS · skill call", "macOS · 스킬 호출", 108.27, 62.82, "108.27 → 62.82 ms"),
            _p("macOS · MCP tool call", "macOS · MCP 도구 호출", 82.51, 76.41, "82.51 → 76.41 ms"),
            _p("Windows · one-shot", "Windows · 원샷", 170.15, 188.26, "170.15 → 188.26 ms (tie)"),
            _p("Windows · core one-shot", "Windows · 코어 원샷", 200.21, 179.55, "200.21 → 179.55 ms"),
            _p("Windows · skill call", "Windows · 스킬 호출", 176.06, 145.75, "176.06 → 145.75 ms"),
            _p("Windows · MCP tool call", "Windows · MCP 도구 호출", 302.51, 268.93, "302.51 → 268.93 ms"),
        ),
        tokens=("40.24", "29.35", "42.92", "31.31", "37.43", "26.31", "55.62", "40.16",
                "54.93", "47.07", "60.63", "49.28", "108.27", "62.82", "82.51", "76.41",
                "170.15", "188.26", "200.21", "179.55", "176.06", "145.75", "302.51", "268.93"),
    ),
)


# --- drawing -----------------------------------------------------------------


def _t(x: float, y: float, text: str, size: float = 13, fill: str = INK, anchor: str = "start",
       weight: str = "normal") -> str:
    return base._t(x, y, text, size, fill, anchor, weight)


def _wrap(text: str, size: float, width: float) -> list[str]:
    return base._wrap(text, size, width)


def _width(text: str, size: float) -> float:
    """Wider than `base._text_width` on purpose.

    `base._text_width` under-estimates the fallback font a viewer picks for the
    SVG, and an over-long value string used to run off the right edge.
    """
    return base._text_width(text, size) * 1.2


def _num(value: float) -> str:
    if value >= 1000:
        return f"{value:,.0f}"
    if value >= 100:
        return f"{value:.0f}"
    if value >= 10:
        return f"{value:.0f}" if value == int(value) else f"{value:.1f}"
    if value == int(value):
        return f"{value:.0f}"
    return f"{value:.2f}".rstrip("0").rstrip(".")


def _nice(value: float) -> str:
    if value >= 1_000_000:
        return f"{value / 1_000_000:g}M"
    if value >= 1000:
        return f"{value / 1000:g}k"
    return f"{value:g}"


def _text_column(lines: list[str], x: float, cy: float, text: str, width: float) -> None:
    parts = _wrap(text, 11.5, width)
    for index, part in enumerate(parts[:2]):
        lines.append(_t(x, cy + 4 + 15 * index - (15 if len(parts) > 1 else 0), part, 11.5, MUTED))


def _legend(lines: list[str], x: float, y: float, chart: Chart, lang: str) -> list[str]:
    for series in chart.series:
        width = _width(series.name[lang], 12)
        lines.append(f'<rect x="{x:.1f}" y="{y - 9:.1f}" width="12" height="12" rx="2"'
                     f' fill="{BLUE if series.emphasis else GREY}"/>')
        lines.append(_t(x + 18, y + 1, series.name[lang], 12, INK))
        x += 18 + width + 22
    return lines


def _panel_pairs(lines: list[str], y: float, chart: Chart, lang: str) -> float:
    """One row per item: a full-width before bar with the after bar drawn over it."""
    label_w = min(max([_width(row.label[lang], 12.5) for row in chart.rows] + [140]), 320)
    value_w = min(max([_width(row.text[lang], 11.5) for row in chart.rows] + [120]), 340)
    x0 = PAD + label_w + 12
    plot_w = WIDTH - PAD - x0 - value_w - 16
    for row in chart.rows:
        cy = y + 15
        lines.append(_t(x0 - 12, cy + 4, row.label[lang], 12.5, INK, "end"))
        lines.append(f'<rect x="{x0:.1f}" y="{cy - 9:.1f}" width="{plot_w:.1f}" height="18"'
                     f' rx="3" fill="{GREY}"/>')
        share = row.after / row.before if row.before > 0 else 0.0
        lines.append(f'<rect x="{x0:.1f}" y="{cy - 9:.1f}" width="{max(plot_w * share, 6.0):.1f}"'
                     f' height="18" rx="3" fill="{BLUE}"/>')
        _text_column(lines, x0 + plot_w + 14, cy, row.text[lang], value_w)
        y += 30
    lines.append(_t(PAD, y + 16, LABELS[lang]["scaled"], 12, MUTED))
    return y + 34


def _panel_rows(lines: list[str], y: float, chart: Chart, lang: str) -> float:
    """One bar per item, scaled to the largest value in the panel."""
    label_w = min(max([_width(row.label[lang], 12.5) for row in chart.rows] + [140]), 320)
    value_w = min(max([_width(row.text[lang], 11.5) for row in chart.rows] + [120]), 340)
    x0 = PAD + label_w + 12
    plot_w = WIDTH - PAD - x0 - value_w - 16
    top = max(row.before for row in chart.rows) or 1.0
    for row in chart.rows:
        cy = y + 15
        lines.append(_t(x0 - 12, cy + 4, row.label[lang], 12.5, INK, "end"))
        lines.append(f'<rect x="{x0:.1f}" y="{cy - 9:.1f}" width="{max(plot_w * row.before / top, 2.0):.1f}"'
                     f' height="18" rx="3" fill="{BLUE}"/>')
        _text_column(lines, x0 + plot_w + 14, cy, row.text[lang], value_w)
        y += 30
    return y + 6


def _log_ticks(low: float, high: float) -> list[float]:
    first = math.floor(math.log10(max(low, 1e-9)))
    last = math.ceil(math.log10(max(high, 1e-9)))
    return [10.0 ** power for power in range(first, last + 1)]


def _panel_groups(lines: list[str], y: float, chart: Chart, lang: str) -> float:
    """Category groups of vertical bars; one axis, drawn for horon or for a series."""
    values = [v for series in chart.series for v in series.values if v is not None]
    low, high = min(values), max(values)
    if chart.log:
        ticks = _log_ticks(low, high)
        lo, hi = ticks[0], ticks[-1]
        position = lambda v: (math.log10(v) - math.log10(lo)) / (math.log10(hi) - math.log10(lo))
    else:
        step = base._nice_step(high)
        ticks = [step * index for index in range(0, math.ceil(high / step) + 1)]
        lo, hi = 0.0, ticks[-1]
        position = lambda v: (v / hi) if hi > 0 else 0.0
    lines = _legend(lines, PAD, y + 8, chart, lang)
    y += 26
    left, right = PAD + 54, WIDTH - PAD - 8
    plot_h = 190
    axis_y = y + plot_h
    for tick in ticks:
        gy = axis_y - plot_h * position(tick)
        lines.append(f'<line x1="{left:.1f}" y1="{gy:.1f}" x2="{right:.1f}" y2="{gy:.1f}" stroke="{GRID}"/>')
        lines.append(_t(left - 10, gy + 4, _nice(tick), 11, MUTED, "end"))
    slot = (right - left) / len(chart.x)
    bar_w = min(slot * 0.72 / len(chart.series), 52)
    for index, item in enumerate(chart.x):
        centre = left + slot * (index + 0.5)
        start = centre - (len(chart.series) - 1) * (bar_w + 6) / 2
        for order, series in enumerate(chart.series):
            value = series.values[index]
            if value is None:
                continue
            x = start + order * (bar_w + 6)
            height = max(plot_h * position(value), 2.0)
            lines.append(f'<rect x="{x - bar_w / 2:.1f}" y="{axis_y - height:.1f}" width="{bar_w:.1f}"'
                         f' height="{height:.1f}" rx="2" fill="{BLUE if series.emphasis else GREY}"/>')
            lines.append(_t(x, axis_y - height - 6, _num(value), 10.5, MUTED, "middle"))
        for row, part in enumerate(_wrap(item[lang], 11.5, slot - 6)[:2]):
            lines.append(_t(centre, axis_y + 18 + 14 * row, part, 11.5, INK, "middle"))
    return axis_y + 48


def render_chart(chart: Chart, lang: str) -> str:
    title = chart.title[lang]
    body = [f'<rect x="{PAD}" y="20" width="5" height="26" rx="1" fill="{BLUE}"/>',
            _t(PAD + 14, 42, title, 21, INK, weight="bold")]
    y = 62.0
    for line in _wrap(chart.note[lang], 13, WIDTH - 2 * PAD):
        body.append(_t(PAD, y + 10, line, 13, MUTED))
        y += 18
    y += 16
    if chart.kind == "pairs":
        y = _panel_pairs(body, y, chart, lang)
    elif chart.kind == "rows":
        y = _panel_rows(body, y, chart, lang)
    elif chart.kind == "groups":
        y = _panel_groups(body, y, chart, lang)
    else:
        raise SystemExit(f"{chart.stem}: unknown kind {chart.kind!r}")
    body.append(_t(PAD, y + 8, LABELS[lang]["source"].format(section=chart.section[lang]), 12, MUTED))
    return base._svg(y + 26, body, title)


def chart_name(stem: str, lang: str) -> str:
    return f"record-{stem}.svg" if lang == "en" else f"record-{stem}.{lang}.svg"


def render(lang: str) -> dict[str, str]:
    return {chart_name(chart.stem, lang): render_chart(chart, lang) for chart in CHARTS_DATA}


def expected(lang: str) -> dict[Path, str]:
    return {CHARTS / name: text for name, text in render(lang).items()}


def write() -> list[Path]:
    written = []
    for lang in ("en", "ko"):
        for path, text in expected(lang).items():
            path.write_text(text, encoding="utf-8", newline="\n")
            written.append(path)
    return written


def stale() -> list[Path]:
    out = []
    for lang in ("en", "ko"):
        for path, text in expected(lang).items():
            if not path.is_file() or path.read_text(encoding="utf-8") != text:
                out.append(path)
    return out


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="exit 1 when a committed chart is stale")
    args = parser.parse_args(argv)
    if args.check:
        out = stale()
        for path in out:
            print(f"stale: {path.relative_to(ROOT)}")
        print(f"record charts: {len(out)} stale of {len(CHARTS_DATA) * 2}")
        return 1 if out else 0
    for path in write():
        print(f"wrote {path.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
