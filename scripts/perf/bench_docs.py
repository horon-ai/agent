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
"""

from __future__ import annotations

import argparse
import datetime as _dt
import re
import sys
from pathlib import Path

import bench_charts

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
    ("startup.txt", "startup"),
    ("loading-cold.txt", "loading_cold"),
    ("loading-warm.txt", "loading_warm"),
    ("tools.md", "tools"),
    ("reload.md", "reload"),
    ("compile.md", "compile"),
)

LANGS = ("en", "ko")

TEXT = {
    "en": {
        "compare": "Competitor comparison",
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
    },
    "ko": {
        "compare": "경쟁 도구 비교",
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
    files = bench_charts.render_charts(find_platform_dirs(results), commit, lang, _run_id(run_url))
    folder = doc.parent / CHARTS_DIR
    if files:
        folder.mkdir(parents=True, exist_ok=True)
        for name, svg in files.items():
            (folder / name).write_text(svg, encoding="utf-8", newline="\n")
    body = bench_charts.markdown(lang, bool(files), CHARTS_DIR)
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


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    render = sub.add_parser("render", help="rewrite the generated block in place")
    render.add_argument("--doc", required=True, type=Path)
    render.add_argument("--results", required=True, type=Path)
    render.add_argument("--commit", required=True)
    render.add_argument("--run-url")
    render.add_argument("--lang", choices=LANGS, default="en")
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
        body = render_body(args.results, args.commit, args.run_url, lang=args.lang)
        text = replace_block(text, args.commit, body)
        text = write_charts(text, args.doc, args.results, args.commit, args.run_url, args.lang)
        args.doc.write_text(text, encoding="utf-8")
    except DocError as exc:
        print(f"bench_docs: {args.doc}: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
