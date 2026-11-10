#!/usr/bin/env python3
"""Check that the documentation is a complete English/Korean pair set.

English is the default language: `X.md` is canonical and `X.ko.md` is its Korean
counterpart in the same folder. The checked set is `README.md`/`README.ko.md`,
`PHILOSOPHY.md`/`PHILOSOPHY.ko.md` and every Markdown file under `docs/`.

Failures (all reported, exit status 1):

* a document without its counterpart;
* a pair whose structure differs: heading count per level, table rows, code
  fences, or inline links;
* a missing language switch line (docs only; the READMEs carry theirs in HTML);
* a relative link whose file or `#anchor` does not exist, or a link that leaves
  the document's own language (an English document linking a `.ko.md` file, or
  the reverse) other than the language switch;
* Hangul in an English document outside code, or no Hangul at all in a Korean
  one. The generated benchmark block (`bench-auto`) is exempt because it is
  rewritten by CI.

Standard library only; runs in well under a second.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

REPO = Path(__file__).resolve().parents[2]

FENCE = re.compile(r"^\s*(```|~~~)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
TABLE_ROW = re.compile(r"^\s*\|")
LINK = re.compile(r"(?<!!)\[(?:[^\]\\]|\\.)*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HTML_REF = re.compile(r"""(?:href|src)="([^"]+)\"""")
HANGUL = re.compile(r"[\u3131-\u318e\uac00-\ud7a3]")
INLINE_CODE = re.compile(r"`[^`\n]*`")
BENCH_BLOCK = re.compile(r"<!-- bench-auto:start.*?<!-- bench-auto:end -->", re.DOTALL)


def pair_files() -> list[Path]:
    files = [REPO / "README.md", REPO / "README.ko.md"]
    files += [REPO / "PHILOSOPHY.md", REPO / "PHILOSOPHY.ko.md"]
    files += sorted((REPO / "docs").rglob("*.md"))
    return files


def is_ko(path: Path) -> bool:
    return path.name.endswith(".ko.md")


def counterpart(path: Path) -> Path:
    if is_ko(path):
        return path.with_name(path.name[: -len(".ko.md")] + ".md")
    return path.with_name(path.name[: -len(".md")] + ".ko.md")


def lines_outside_fences(text: str) -> list[str]:
    out, in_fence = [], False
    for line in text.splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if not in_fence:
            out.append(line)
    return out


def slug(title: str) -> str:
    title = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", title)
    title = title.replace("`", "").strip().lower()
    title = re.sub(r"[^\w\- ]", "", title, flags=re.UNICODE)
    return title.replace(" ", "-")


def anchors(text: str) -> set[str]:
    seen: dict[str, int] = {}
    found: set[str] = set()
    for line in lines_outside_fences(text):
        match = HEADING.match(line)
        if not match:
            continue
        base = slug(match.group(2))
        count = seen.get(base, 0)
        seen[base] = count + 1
        found.add(base if count == 0 else f"{base}-{count}")
    # Explicit HTML anchors.
    found.update(re.findall(r'<a\s+(?:name|id)="([^"]+)"', text))
    return found


def structure(text: str) -> dict[str, int]:
    shape: dict[str, int] = {"fences": 0, "table_rows": 0, "links": 0}
    in_fence = False
    for line in text.splitlines():
        if FENCE.match(line):
            if not in_fence:
                shape["fences"] += 1
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        match = HEADING.match(line)
        if match:
            key = f"h{len(match.group(1))}"
            shape[key] = shape.get(key, 0) + 1
        if TABLE_ROW.match(line):
            shape["table_rows"] += 1
        shape["links"] += len(LINK.findall(line))
    return shape


def links(text: str) -> list[str]:
    targets = []
    for line in lines_outside_fences(text):
        targets += LINK.findall(line)
        targets += HTML_REF.findall(line)
    return targets


def check_links(path: Path, text: str, errors: list[str]) -> None:
    rel = path.relative_to(REPO)
    ko = is_ko(path)
    own_anchors = anchors(text)
    for raw in links(text):
        if re.match(r"^[a-z][a-z0-9+.-]*:", raw, re.IGNORECASE) or raw.startswith("//"):
            continue
        target, _, fragment = raw.partition("#")
        fragment = unquote(fragment)
        if target:
            dest = (path.parent / unquote(target)).resolve()
            if not dest.exists():
                errors.append(f"{rel}: broken link {raw}")
                continue
            if dest.suffix == ".md" and REPO in dest.parents:
                is_switch = dest == counterpart(path).resolve()
                if not is_switch and is_ko(dest) != ko and dest.name not in ("AGENTS.md", "AGENTS.ko.md"):
                    side = "Korean" if ko else "English"
                    errors.append(f"{rel}: {side} document links the other language: {raw}")
            if fragment and dest.suffix == ".md" and fragment not in anchors(dest.read_text(encoding="utf-8")):
                errors.append(f"{rel}: missing anchor in {raw}")
        elif fragment and fragment not in own_anchors:
            errors.append(f"{rel}: missing anchor {raw}")


def check_language(path: Path, text: str, errors: list[str]) -> None:
    rel = path.relative_to(REPO)
    body = BENCH_BLOCK.sub("", text)
    # The language switch line names the other language in its own script.
    body = "\n".join(line for line in body.splitlines() if "Language:" not in line)
    prose = INLINE_CODE.sub("", "\n".join(lines_outside_fences(body)))
    if is_ko(path):
        if not HANGUL.search(prose):
            errors.append(f"{rel}: Korean document has no Hangul")
    elif HANGUL.search(prose):
        line = next(line for line in prose.splitlines() if HANGUL.search(line))
        errors.append(f"{rel}: Hangul in an English document: {line.strip()[:60]}")


def check_switch_line(path: Path, text: str, errors: list[str]) -> None:
    rel = path.relative_to(REPO)
    if path.parent == REPO:
        return
    other = counterpart(path).name
    if is_ko(path):
        expected = f"언어: [English]({other}) | **한국어**"
    else:
        expected = f"Language: **English** | [한국어]({other})"
    if expected not in text.splitlines()[:6]:
        errors.append(f"{rel}: missing language line `{expected}` in the first lines")


def main() -> int:
    errors: list[str] = []
    files = pair_files()
    docs = 0
    for path in files:
        if not path.exists():
            errors.append(f"{path.relative_to(REPO)}: file is missing")
            continue
        other = counterpart(path)
        if not other.exists():
            errors.append(f"{path.relative_to(REPO)}: counterpart {other.name} is missing")
            continue
        text = path.read_text(encoding="utf-8")
        check_links(path, text, errors)
        check_language(path, text, errors)
        check_switch_line(path, text, errors)
        docs += 1
        if not is_ko(path):
            shape_en = structure(text)
            shape_ko = structure(other.read_text(encoding="utf-8"))
            for key in sorted(set(shape_en) | set(shape_ko)):
                if shape_en.get(key, 0) != shape_ko.get(key, 0):
                    errors.append(
                        f"{path.relative_to(REPO)}: {key} differs: English {shape_en.get(key, 0)}, "
                        f"Korean {shape_ko.get(key, 0)}"
                    )
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        print(f"check_docs_pairs: {len(errors)} problem(s)", file=sys.stderr)
        return 1
    print(f"check_docs_pairs: {docs // 2} English/Korean pairs ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
