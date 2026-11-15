#!/usr/bin/env python3
"""Build a Korean comparison report (HTML, ready to print as PDF) from one benchmark.yml run.

Two steps, both stdlib-only apart from Node for the chart server:

    python3 scripts/perf/bench_report.py fetch --run <run-id> --out <dir>
    python3 scripts/perf/bench_report.py build --in <dir> --out <report-dir> \
        --commit <sha> --release v0.1.9 --run-url <url>

`fetch` downloads the `benchmark-<os>-<run-id>` artifacts with `gh`. `build`
reads each OS's `compare.json`, draws every chart through the ztd-charts MCP
server (`npx ztd-charts-mcp`, spoken to over stdio, no other chart tool is used)
and writes `report.html`, `charts/*.svg`, `specs/*.json` and `summary.json`.
Printing `report.html` to PDF is left to any Chromium (`--headless --print-to-pdf`).

Numbers come only from the run's own files. A value the run did not record is
shown as "미측정" with the recorded reason; nothing is filled in from elsewhere.
"""

from __future__ import annotations

import argparse
import html
import json
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from bench_charts import (  # noqa: E402
    HIGHLIGHT,
    METRICS,
    PLATFORMS,
    Metric,
    _get,
    _measured_at,
    _num,
    _number,
    _ratio,
    _scale,
    _tools,
    _values,
    load_compare,
)
from bench_docs import find_platform_dirs  # noqa: E402

ZTD_PACKAGE = "ztd-charts-mcp@0.11.3"
CHART_WIDTH = 560
OS_LABEL = dict(PLATFORMS)
# Metrics whose run records keep a min and max beside the median.
SPREAD = {"01-version": ("version_ms",), "03-oneshot-time": ("oneshot",), "05-skill-time": ("skill",), "06-mcp-time": ("mcp",)}
UNIT_KO = {"ms": "ms", "count": "회"}


class ReportError(Exception):
    pass


def _scope_ids(svg: str, prefix: str) -> str:
    """Inline SVGs share one document: give every id a per-chart prefix so clip paths and gradients never collide."""
    for old in set(re.findall(r'\bid="([^"]+)"', svg)):
        svg = (svg.replace(f'id="{old}"', f'id="{prefix}{old}"').replace(f"url(#{old})", f"url(#{prefix}{old})")
               .replace(f'href="#{old}"', f'href="#{prefix}{old}"'))
    return svg


# --- fetch -------------------------------------------------------------------


def fetch(run: str, out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    cmd = ["gh", "run", "download", run, "-D", str(out), "-p", f"benchmark-*-{run}"]
    done = subprocess.run(cmd, capture_output=True, text=True)
    if done.returncode != 0:
        raise ReportError(f"gh run download failed: {done.stderr.strip()}")


# --- ztd-charts over stdio ---------------------------------------------------


class ZtdCharts:
    """Minimal MCP client for the ztd-charts server (newline-delimited JSON-RPC)."""

    def __init__(self) -> None:
        self.proc = subprocess.Popen(
            ["npx", "-y", ZTD_PACKAGE], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, bufsize=1,
        )
        self.next_id = 0
        self._rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "bench-report", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, message: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def _rpc(self, method: str, params: dict) -> dict:
        assert self.proc.stdout is not None
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise ReportError("ztd-charts server closed the pipe")
            message = json.loads(line)
            if message.get("id") == self.next_id:
                if "error" in message:
                    raise ReportError(f"{method}: {message['error']}")
                return message["result"]

    def call(self, name: str, arguments: dict) -> list[str]:
        result = self._rpc("tools/call", {"name": name, "arguments": arguments})
        texts = [item["text"] for item in result.get("content", []) if item.get("type") == "text"]
        if result.get("isError"):
            raise ReportError(f"{name}: {' '.join(texts)[:400]}")
        return texts

    def render_svg(self, spec: dict) -> str:
        checked = " ".join(self.call("validate_spec", {"spec": spec}))
        if '"valid": false' in checked or '"valid":false' in checked:
            raise ReportError(f"invalid spec {spec.get('card', {}).get('title')}: {checked[:400]}")
        for text in self.call("render_chart", {"spec": spec, "format": "svg", "theme": "light",
                                               "width": CHART_WIDTH, "locale": "ko"}):
            if text.lstrip().startswith("<svg"):
                return text
        raise ReportError("render_chart returned no SVG")

    def close(self) -> None:
        self.proc.terminate()
        self.proc.wait(timeout=10)


# --- data --------------------------------------------------------------------


def _by_name(run: dict) -> dict[str, dict]:
    return {r["name"]: r for r in run.get("results", []) if isinstance(r, dict) and isinstance(r.get("name"), str)}


def _scenario_key(metric: Metric) -> str | None:
    for key in ("oneshot", "skill", "mcp"):
        if f"-{key}" in metric.stem or metric.stem.endswith(key):
            return key
    return None


def _spread(metric: Metric, result: dict) -> tuple[float, float, int] | None:
    path = SPREAD.get(metric.stem)
    if path is None:
        return None
    node = result.get(path[0]) or {}
    summary = node.get("summary") if path[0] != "version_ms" else (result.get("version_ms") or {}).get("summary")
    low, high, samples = (_number(_get(summary, k)) for k in ("min", "max", "samples"))
    if low is None or high is None or samples is None:
        return None
    return low, high, int(samples)


def _why(metric: Metric, result: dict | None, tool_reason: str | None, host_os: str | None) -> str:
    """The recorded reason a tool has no value for this metric."""
    if result is None:
        return tool_reason or "이 OS 실행에 결과 없음"
    if result.get("error"):
        return f"실행 오류: {str(result['error'])[:80]}"
    key = _scenario_key(metric)
    if key:
        scenario = result.get(key) or {}
        for field in ("failure", "error"):
            if scenario.get(field):
                return str(scenario[field])[:80]
        if scenario.get("tool_roundtrip") is False or scenario.get("tool_round_trip") is False:
            return "도구 왕복 확인 실패"
        if scenario.get("status") == "skipped" or scenario.get("skipped"):
            return str(scenario.get("skipped") or scenario.get("status"))[:80]
    if metric.stem == "09-fsync-calls":
        return "fsync 호출 수가 기록되지 않음" if host_os == "linux" else "strace 가 없는 OS 라 기록 불가"
    return "기록된 값 없음"


def collect(runs: dict[str, dict]) -> dict:
    """Per OS and metric: ordered measured rows plus the unmeasured tools with reasons."""
    tools = _tools(runs)
    out: dict = {"tools": [name for name, _ in tools], "labels": dict(tools), "os": {}}
    for os_key, _ in PLATFORMS:
        run = runs.get(os_key)
        if run is None:
            continue
        results = _by_name(run)
        skipped = {s.get("name"): str(s.get("reason")) for s in run.get("skipped", []) if isinstance(s, dict)}
        metrics = {}
        for metric in METRICS:
            values = _values(run, metric, tools)
            rows, missing = [], []
            for (name, label), value in zip(tools, values):
                result = results.get(name)
                if value is None:
                    reason = _why(metric, result, skipped.get(name), (run.get("host") or {}).get("os"))
                    missing.append({"tool": label, "reason": reason})
                    continue
                spread = _spread(metric, result)
                rows.append({"name": name, "tool": label, "value": value,
                             "min": spread[0] if spread else None, "max": spread[1] if spread else None,
                             "samples": spread[2] if spread else None})
            rows.sort(key=lambda r: (r["value"], r["name"]))
            metrics[metric.stem] = {"rows": rows, "missing": missing}
        out["os"][os_key] = {
            "metrics": metrics,
            "host": run.get("host"),
            "parameters": run.get("parameters"),
            "started_at": run.get("started_at"),
            "versions": {r.get("label") or n: r.get("version") for n, r in results.items()},
        }
    return out


# --- charts ------------------------------------------------------------------


def chart_spec(metric: Metric, os_key: str, block: dict) -> tuple[dict, str, float]:
    rows = block["rows"]
    divisor, unit = _scale(metric, [r["value"] for r in rows])
    unit = UNIT_KO.get(unit, unit)
    best = rows[0]
    base = next((r for r in rows if r["name"] == HIGHLIGHT), None)
    if base is None:
        takeaway = f"{HIGHLIGHT} 값 없음"
    elif best["name"] == HIGHLIGHT and len(rows) > 1:
        takeaway = f"{HIGHLIGHT}: {len(rows)}개 중 최저 ({_num(base['value'] / divisor)} {unit})"
    else:
        rank = [r["name"] for r in rows].index(HIGHLIGHT) + 1
        takeaway = f"{HIGHLIGHT}: {len(rows)}개 중 {rank}위 · 최저 {best['tool']}은 horon의 {_ratio(best['value'], base['value'])}"
    spec = {
        "type": "bar",
        "data": [{"tool": r["tool"], "value": round(r["value"] / divisor, 3)} for r in rows],
        "encoding": {"x": "tool", "y": "value"},
        "options": {"orientation": "horizontal", "highlight": HIGHLIGHT, "unit": f" {unit}",
                    "format": "decimal", "height": 60 + 34 * len(rows), "locale": "ko"},
        "card": {"title": metric.title["ko"], "subtitle": f"{OS_LABEL[os_key]} · 단위 {unit} · 낮을수록 좋음", "takeaway": takeaway},
    }
    return spec, unit, divisor


# --- html --------------------------------------------------------------------

CSS = """
@page { size: A4; margin: 14mm 12mm; }
body { font-family: 'Pretendard', 'Apple SD Gothic Neo', 'Noto Sans KR', sans-serif; color: #1f2328; font-size: 10.5pt; line-height: 1.5; }
h1 { font-size: 26pt; margin: 0 0 6pt; } h2 { font-size: 15pt; margin: 0 0 8pt; border-bottom: 2px solid #0b84de; padding-bottom: 3pt; }
h3 { font-size: 11.5pt; margin: 12pt 0 4pt; } .page { page-break-after: always; } .page:last-child { page-break-after: auto; }
.cover { padding-top: 70mm; } .sub { color: #59636e; font-size: 12pt; } .meta td { padding: 2pt 10pt 2pt 0; }
table { border-collapse: collapse; width: 100%; font-size: 8.5pt; margin: 4pt 0 8pt; }
th, td { border: 1px solid #d0d7de; padding: 2.5pt 5pt; text-align: right; } th:first-child, td:first-child { text-align: left; }
th { background: #f2f4f6; } tr.h td { background: #e6f2fc; font-weight: 600; }
.grid > div { display: inline-block; width: 49%; vertical-align: top; } .grid svg { width: 100%; height: auto; }
.note { color: #59636e; font-size: 8.5pt; } .warn { background: #fff1d6; padding: 4pt 8pt; border-radius: 4pt; font-size: 9pt; }
ul { margin: 2pt 0 6pt 14pt; padding: 0; }
"""


def _fmt(metric: Metric, value: float) -> str:
    if metric.unit == "bytes":
        divisor, unit = _scale(metric, [value])
        return f"{_num(value / divisor)} {unit}"
    return f"{_num(value)} {UNIT_KO.get(metric.unit, metric.unit)}" if metric.unit != "count" else f"{value:,.0f} 회"


def _table(metric: Metric, block: dict) -> str:
    rows = block["rows"]
    if not rows:
        return ""
    base = next((r for r in rows if r["name"] == HIGHLIGHT), None)
    head = "<tr><th>도구</th><th>중앙값</th><th>최소–최대</th><th>표본 수</th><th>horon 대비</th></tr>"
    body = []
    for r in rows:
        spread = f"{_fmt(metric, r['min'])} – {_fmt(metric, r['max'])}" if r["min"] is not None else "—"
        samples = str(r["samples"]) if r["samples"] is not None else "—"
        ratio = "기준" if r["name"] == HIGHLIGHT else (_ratio(r["value"], base["value"]) if base else "—")
        cls = ' class="h"' if r["name"] == HIGHLIGHT else ""
        body.append(f"<tr{cls}><td>{html.escape(r['tool'])}</td><td>{_fmt(metric, r['value'])}</td>"
                    f"<td>{spread}</td><td>{samples}</td><td>{ratio}</td></tr>")
    return f"<table>{head}{''.join(body)}</table>"


def render_html(data: dict, svgs: dict[str, str], meta: dict) -> str:
    esc = html.escape
    pages = []
    parameters = next((o["parameters"] for o in data["os"].values() if o.get("parameters")), {}) or {}
    cover_rows = [("기준 릴리스", meta["release"]), ("측정한 커밋", meta["commit"][:8]), ("측정 실행", meta.get("run_url") or "—"),
                  ("측정 일시(UTC)", _measured_at({k: {"started_at": v["started_at"]} for k, v in data["os"].items()})),
                  ("비교 대상", ", ".join(data["labels"][t] for t in data["tools"])),
                  ("원샷·스킬·MCP 반복", f"{parameters.get('oneshot_runs', '—')}회"), ("세션 수 / 턴", f"{parameters.get('sessions', '—')} / {parameters.get('turns', '—')}")]
    pages.append(f"<div class='page cover'><h1>Horon agent 벤치마크 비교 리포트</h1>"
                 f"<p class='sub'>horon {esc(meta['release'])} 대 다른 코딩 에이전트 · 헤드리스 · 모의 모델</p>"
                 f"<table class='meta'>{''.join(f'<tr><td>{esc(k)}</td><td style=\"text-align:left\">{esc(str(v))}</td></tr>' for k, v in cover_rows)}</table></div>")

    versions = {}
    for os_key, block in data["os"].items():
        for label, version in block["versions"].items():
            versions.setdefault(label, version)
    ver_rows = "".join(f"<tr><td>{esc(str(k))}</td><td style='text-align:left'>{esc(str(v))}</td></tr>" for k, v in versions.items())
    pages.append("<div class='page'><h2>측정 조건</h2><ul>"
                 "<li>측정은 GitHub Actions 호스티드 러너에서 <code>benchmark.yml</code>로 했고, OS(Linux·macOS·Windows)마다 러너가 다릅니다. 같은 OS 안에서만 비교하고 OS끼리는 비교하지 않습니다.</li>"
                 "<li>모든 도구가 같은 모의 모델 서버(<code>scripts/perf/mock_llm_server.py</code>)에 붙고, 같은 작업(<code>ls</code> 도구 호출 한 번, 스킬 로드 한 번, MCP 도구 호출 한 번)을 합니다. 실제 모델 지연과 토큰 비용은 포함되지 않으며 API 호출은 0건입니다.</li>"
                 "<li>한 표의 행은 같은 잡·같은 머신에서 함께 쟀습니다. 값은 중앙값이고 최소–최대와 표본 수를 함께 적습니다. 최소–최대가 겹치면 같은 수준으로 봅니다.</li>"
                 "<li>도구별 연결 방식(설정 파일, 도구 이름, 스킬·MCP 경로)은 <code>docs/performance/BENCHMARK.md</code>에 있습니다. 비교 대상은 그 스크립트가 고정한 버전입니다.</li></ul>"
                 f"<h3>측정한 버전</h3><table><tr><th>도구</th><th style='text-align:left'>버전</th></tr>{ver_rows}</table></div>")

    oses = list(data["os"])
    head = "".join(f"<th>{OS_LABEL[o]}</th>" for o in oses)
    summary_rows = []
    for metric in METRICS:
        cells = []
        for o in oses:
            rows = data["os"][o]["metrics"][metric.stem]["rows"]
            names = [r["name"] for r in rows]
            if HIGHLIGHT not in names:
                cells.append("미측정")
                continue
            mine = rows[names.index(HIGHLIGHT)]
            cells.append(f"{len(rows)}개 중 {names.index(HIGHLIGHT) + 1}위 ({_fmt(metric, mine['value'])})")
        summary_rows.append(f"<tr><td>{esc(metric.title['ko'])}</td>{''.join(f'<td>{c}</td>' for c in cells)}</tr>")
    pages.append("<div class='page'><h2>요약: horon 순위</h2>"
                 "<p class='note'>낮을수록 좋은 값의 중앙값 순위입니다. 순위 차이가 최소–최대 범위 안에 들면 같은 수준으로 읽어야 하며, 자세한 값은 OS별 표에 있습니다.</p>"
                 f"<table><tr><th>항목</th>{head}</tr>{''.join(summary_rows)}</table></div>")

    for os_key, os_block in data["os"].items():
        charts = "".join(f"<div>{svgs[f'{os_key}/{m.stem}']}</div>" for m in METRICS if f"{os_key}/{m.stem}" in svgs)
        host = os_block.get("host") or {}
        pages.append(f"<div class='page'><h2>{OS_LABEL[os_key]} 결과</h2>"
                     f"<p class='note'>러너: {esc(str(host.get('os', '—')))}/{esc(str(host.get('arch', '—')))}, CPU {esc(str(host.get('cpu_count', '—')))}개</p>"
                     f"<div class='grid'>{charts}</div></div>")
        tables = []
        unmeasured = []
        for metric in METRICS:
            block = os_block["metrics"][metric.stem]
            if block["rows"]:
                tables.append(f"<h3>{esc(metric.title['ko'])}</h3>{_table(metric, block)}")
            for miss in block["missing"]:
                unmeasured.append(f"{esc(metric.title['ko'])} · {esc(miss['tool'])}: 미측정, {esc(miss['reason'])}")
        warn = f"<div class='warn'><b>미측정</b><ul>{''.join(f'<li>{u}</li>' for u in unmeasured)}</ul></div>" if unmeasured else ""
        extra = _horon_only_html(os_block.get("horon_only") or {})
        pages.append(f"<div class='page'><h2>{OS_LABEL[os_key]} 표</h2>{''.join(tables)}{warn}</div>")
        if extra:
            pages.append(f"<div class='page'><h2>{OS_LABEL[os_key]} horon 단독 측정</h2>{extra}</div>")

    pages.append("<div class='page'><h2>한계와 원자료</h2><ul>"
                 "<li>모의 모델을 쓰므로 에이전트 자체의 기동·메모리·도구 왕복 비용만 잽니다. 실제 모델 응답 시간, 토큰 사용량, 작업 품질은 측정하지 않았습니다.</li>"
                 "<li>호스티드 러너의 절대값은 실행마다 달라집니다. 같은 실행·같은 OS 안의 상대 비교로만 읽어야 합니다.</li>"
                 "<li>메모리 지표는 OS마다 정의가 다릅니다(Linux PSS·RSS, macOS RSS, Windows 작업 집합). OS끼리 같은 수로 비교하지 않습니다.</li>"
                 "<li>도구마다 세션 구현이 다릅니다(데몬 안의 세션 대 세션당 프로세스). 세션 메모리는 절대값보다 세션 하나당 증가량으로 읽어야 합니다.</li>"
                 "<li>각 도구가 이 하네스의 시나리오를 소화한 경로(Via)가 달라 같은 일을 한 것은 아니지만, 최종 응답이 도구 결과를 거쳐 돌아왔는지(<code>tool_round_trip</code>)를 확인한 값만 실었습니다.</li></ul>"
                 f"<h3>원자료</h3><ul><li>GitHub Actions 실행: {esc(meta.get('run_url') or '—')} (아티팩트 <code>benchmark-&lt;os&gt;-&lt;run-id&gt;</code>, 30일 보관)</li>"
                 "<li>저장소: <code>docs/performance/BENCHMARK.md</code>, <code>scripts/perf/bench_report.py</code> (이 리포트를 다시 만드는 스크립트)</li>"
                 "<li>이 리포트의 차트 스펙: <code>specs/*.json</code>, 요약 수치: <code>summary.json</code></li></ul></div>")
    return f"<!doctype html><html lang='ko'><head><meta charset='utf-8'><title>Horon 벤치마크 비교 리포트</title><style>{CSS}</style></head><body>{''.join(pages)}</body></html>"


def horon_only(directory: Path) -> dict:
    """Startup stages and core-tool latency: measured for horon alone, so never ranked against another tool."""
    out: dict = {"startup": {}, "tools": []}
    text = (directory / "startup.txt").read_text(encoding="utf-8") if (directory / "startup.txt").is_file() else ""
    section = None
    for line in text.splitlines():
        head = re.match(r"^(--help \(binary load\)|--version|Server ready[^:]*):", line)
        if head:
            section = head.group(1)
        median = re.match(r"^\s+Median:\s+([\d.]+) ms", line)
        if median and section:
            out["startup"][section] = float(median.group(1))
            section = None
    path = directory / "tools.json"
    if path.is_file():
        for row in json.loads(path.read_text(encoding="utf-8")).get("results", []):
            latency = row.get("latency_ms") or {}
            if not row.get("error") and latency.get("median") is not None:
                out["tools"].append({"case": row["name"], "median": latency["median"], "min": latency["min"],
                                     "max": latency["max"], "samples": latency["samples"]})
    return out


def _horon_only_html(block: dict) -> str:
    parts = []
    if block.get("startup"):
        rows = "".join(f"<tr><td>{html.escape(k)}</td><td>{v:.2f} ms</td></tr>" for k, v in block["startup"].items())
        parts.append(f"<h3>기동 시간(중앙값)</h3><table><tr><th>항목</th><th>시간</th></tr>{rows}</table>")
    if block.get("tools"):
        rows = "".join(f"<tr><td>{html.escape(t['case'])}</td><td>{t['median']:.2f} ms</td><td>{t['min']:.2f} – {t['max']:.2f} ms</td><td>{t['samples']}</td></tr>"
                       for t in block["tools"])
        parts.append("<h3>핵심 도구 지연(디버그 소켓 왕복)</h3><table><tr><th>케이스</th><th>중앙값</th><th>최소–최대</th><th>표본 수</th></tr>"
                     f"{rows}</table>")
    if parts:
        parts.append("<p class='note'>이 두 항목은 비교 대상 도구에 같은 측정이 없어 horon 값만 싣습니다. 순위를 매기지 않습니다.</p>")
    return "".join(parts)


# --- main --------------------------------------------------------------------


def build(source: Path, out: Path, meta: dict) -> None:
    dirs = find_platform_dirs(source)
    runs = load_compare(dirs)
    if not runs:
        raise ReportError(f"no compare.json under {source}")
    data = collect(runs)
    for os_key, os_block in data["os"].items():
        os_block["horon_only"] = horon_only(dirs[os_key])
    (out / "charts").mkdir(parents=True, exist_ok=True)
    (out / "specs").mkdir(parents=True, exist_ok=True)
    svgs: dict[str, str] = {}
    server = ZtdCharts()
    try:
        for os_key, os_block in data["os"].items():
            for metric in METRICS:
                block = os_block["metrics"][metric.stem]
                if not block["rows"]:
                    continue
                spec, _, _ = chart_spec(metric, os_key, block)
                name = f"{os_key}-{metric.stem}"
                (out / "specs" / f"{name}.json").write_text(json.dumps(spec, ensure_ascii=False, indent=1), encoding="utf-8")
                svg = server.render_svg(spec)
                (out / "charts" / f"{name}.svg").write_text(svg, encoding="utf-8")
                svgs[f"{os_key}/{metric.stem}"] = _scope_ids(svg, f"{name}-")
    finally:
        server.close()
    (out / "summary.json").write_text(json.dumps({"meta": meta, **data}, ensure_ascii=False, indent=1), encoding="utf-8")
    (out / "report.html").write_text(render_html(data, svgs, meta), encoding="utf-8")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch")
    f.add_argument("--run", required=True)
    f.add_argument("--out", type=Path, required=True)
    b = sub.add_parser("build")
    b.add_argument("--in", dest="source", type=Path, required=True)
    b.add_argument("--out", type=Path, required=True)
    b.add_argument("--commit", required=True)
    b.add_argument("--release", required=True)
    b.add_argument("--run-url", default="")
    args = parser.parse_args(argv)
    try:
        if args.cmd == "fetch":
            fetch(args.run, args.out)
        else:
            build(args.source, args.out, {"commit": args.commit, "release": args.release, "run_url": args.run_url})
    except ReportError as error:
        print(f"bench_report: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
