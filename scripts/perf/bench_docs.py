#!/usr/bin/env python3
"""Write a benchmark run into the generated block of docs/performance/RESULTS.md.

`.github/workflows/benchmark-docs.yml` runs every benchmark suite after a push to
`main` passes CI, then calls this script once per branch (`main`, `dev`) to
replace the block between the two markers below. Everything outside the block is
left byte-for-byte as it was: the rest of RESULTS.md is a hand-written record.

    <!-- bench-auto:start commit=<sha> -->
    ...generated...
    <!-- bench-auto:end -->

The start marker carries the measured commit, so the next run can ask "did the
agent code change since the last measurement?" (`last-commit`).

Subcommands:

    render      --doc PATH --results DIR --commit SHA [--run-url URL] [--runner TEXT]
    last-commit --doc PATH     prints the recorded commit, or nothing
"""

from __future__ import annotations

import argparse
import datetime as _dt
import re
import sys
from pathlib import Path

START_RE = re.compile(r"^<!-- bench-auto:start(?: commit=([0-9a-f]{7,40}))? -->$", re.MULTILINE)
END = "<!-- bench-auto:end -->"
FENCE_RE = re.compile(r"^\s*(```|~~~)")
HEADING_RE = re.compile(r"^(#{1,6}) ")

# Fragments written by benchmark.yml, in the order they appear in the block.
# (file name, title used when the fragment is plain text or missing)
FRAGMENTS = (
    ("compare.md", "경쟁 도구 비교"),
    ("startup.txt", "기동"),
    ("loading-cold.txt", "세션 로딩 (COLD)"),
    ("loading-warm.txt", "세션 로딩 (WARM)"),
    ("tools.md", "도구 지연"),
    ("reload.md", "리로드 폭주"),
    ("compile.md", "컴파일 루프"),
)

# Fragment headings are shifted so the shallowest one becomes this level; the
# block itself sits under a `##` section of RESULTS.md.
FRAGMENT_LEVEL = 3


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


def _fragment(results: Path, name: str, title: str) -> str:
    heading = "#" * FRAGMENT_LEVEL + " " + title
    path = results / name
    if not path.is_file() or not path.read_text(encoding="utf-8").strip():
        return f"{heading}\n\n측정 안 됨: 이 실행에서 `{name}` 파일이 만들어지지 않았습니다."
    content = path.read_text(encoding="utf-8").strip("\n")
    if name.endswith(".md"):
        return demote_headings(content)
    return f"{heading}\n\n```text\n{content}\n```"


def render_body(
    results: Path,
    commit: str,
    run_url: str | None = None,
    runner: str | None = None,
    now: _dt.datetime | None = None,
) -> str:
    now = now or _dt.datetime.now(_dt.timezone.utc)
    version_file = results / "horon-version.txt"
    version = version_file.read_text(encoding="utf-8").strip() if version_file.is_file() else None
    lines = [
        "이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.",
        "",
        f"- 커밋: `{commit}`",
        f"- 측정 시각: {now.strftime('%Y-%m-%d %H:%M')} UTC",
    ]
    if version:
        lines.append(f"- 바이너리: `{version}`")
    if runner:
        lines.append(f"- 러너: `{runner}`")
    if run_url:
        lines.append(f"- 실행: <{run_url}> (원본 JSON 은 `benchmark-<run_id>` 아티팩트)")
    lines += [
        "",
        "호스티드 러너의 절대값은 기계마다 달라 기준선이 아닙니다. 같은 실행 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.",
    ]
    parts = ["\n".join(lines)]
    parts += [_fragment(results, name, title) for name, title in FRAGMENTS]
    return "\n\n".join(parts)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    render = sub.add_parser("render", help="rewrite the generated block in place")
    render.add_argument("--doc", required=True, type=Path)
    render.add_argument("--results", required=True, type=Path)
    render.add_argument("--commit", required=True)
    render.add_argument("--run-url")
    render.add_argument("--runner")
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
        body = render_body(args.results, args.commit, args.run_url, args.runner)
        args.doc.write_text(replace_block(text, args.commit, body), encoding="utf-8")
    except DocError as exc:
        print(f"bench_docs: {args.doc}: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
