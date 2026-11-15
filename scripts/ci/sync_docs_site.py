#!/usr/bin/env python3
"""Mirror `docs/` into the Mintlify project `packages/docs-site` as MDX.

`docs/` is the source of truth: it is compiled into the binary and every page
has a Korean pair. The Mintlify site cannot read those files as they are, so
this script writes an MDX copy of each page under `reference/` and the SVG files
they show under `images/reference/`, and checks that `docs.json` lists exactly
the pages it wrote. Nothing generated is edited by hand.

    python3 scripts/ci/sync_docs_site.py           # rewrite the generated files
    python3 scripts/ci/sync_docs_site.py --check   # exit 1 when they are stale

What changes between a source page and its copy:

* the `# Title` line and the `Language:` switch line move into the front matter
  and the site's language switcher;
* relative links to other docs become site routes, links to other repository
  files become GitHub links, and image paths point at `images/reference/`;
* MDX reads `{`, `}` and `<name` outside code as JSX, so those are escaped there
  (code spans and fences are left alone);
* HTML comments, which MDX rejects, are dropped, and `<https://...>` autolinks
  become ordinary links.

Standard library only.
"""

from __future__ import annotations

import argparse
import functools
import json
import re
import shutil
import sys
from pathlib import Path, PurePosixPath

REPO = Path(__file__).resolve().parents[2]
DOCS = REPO / "docs"
SITE = REPO / "packages" / "docs-site"
REFERENCE = "reference"
IMAGES = "images/reference"
GITHUB_BLOB = "https://github.com/horon-ai/agent/blob/dev/"

# Language code -> (path prefix inside the site, source suffix, language line).
LANGUAGES = {
    "en": ("", ".md", re.compile(r"^Language:\s")),
}

# Sidebar labels for pages whose title is too long for a sidebar. Keyed by the
# source path under `docs/`; a page not listed uses its title.
SIDEBAR_TITLES = {
    "README.md": "Overview",
    "diagram/README.md": "Diagrams",
    "architecture/CRATES.md": "Crates",
    "performance/RESULTS.md": "Measured results",
    "performance/BENCHMARK.md": "Benchmarks",
    "performance/MEMORY.md": "Memory budgets",
    "runtime/SWARM_TASK_GRAPH.md": "Swarm task graph",
    "runtime/TOOL_SCHEDULING.md": "Tool scheduling",
}

# Descriptions for pages whose first paragraph does not read as one. A page not
# listed uses its first prose paragraph, cut at a sentence end.
DESCRIPTIONS = {
    "README.md": "How the reference documentation is organized, the fixed requirements, and the rules for keeping it current.",
    "architecture/CRATES.md": "Crate ownership boundaries, platform adapters, storage durability and the TLS trust set.",
    "architecture/SERVER.md": "The daemon, its sessions and sockets, and how clients attach to it.",
    "diagram/README.md": "Eight diagrams of the code: system overview, run modes, turn lifecycle, provider wires, ACP negotiation, session storage, tool loading and the release flow.",
    "performance/BENCHMARK.md": "The measurement standard, per-OS benchmarks, the structural gate, A/B measurement, and the skill and MCP config loading comparison.",
    "performance/MEMORY.md": "Memory regression budgets, idle session derived-cache release, and the incident response procedure.",
    "performance/RESULTS.md": "Measured results, how they were taken, and the limits of what they show.",
    "protocol/HARNESS_API.md": "Harness API credential setup and the request and event surface that SDK and desktop clients use.",
    "protocol/SESSION_STORE.md": "The session event log format, its delivery contract, the writer pool and the durable runtime journal.",
    "release/INSTALL.md": "Release asset verification and install-release, installing from npm, running on a Linux server, and the TLS trust set.",
    "release/NPM.md": "The npm packages of the CLI: installing and running them, versioning, the publish workflow and provenance.",
    "release/WINDOWS.md": "Windows build, native named pipes, process management, PowerShell installation and signing.",
    "runtime/IRC.md": "The inter-agent IRC mailbox bus: the irc tool, configuration and its performance gate.",
    "runtime/PROMPT.md": "How the system prompt is assembled, where skills come from and how they are invoked, and how tokens are estimated.",
    "runtime/SWARM_TASK_GRAPH.md": "The swarm task DAG design, a feature that is off by default, and the rationale of the horon-agent-plan crate.",
    "runtime/TOOLS.md": "Tool loading tiers, effect and approval tiers, edit safety, skill and memory reads, MCP config sources and large-output handling.",
    "runtime/TOOL_SCHEDULING.md": "How a message's tool calls are grouped and run, and how repeated calls are stopped.",
    "runtime/TURNS.md": "Turn recovery, mid-run message insertion, the prewalk handoff, goal continuation, runtime commands and --resume.",
}

FENCE = re.compile(r"^\s*(`{3,}|~{3,})")
INLINE_CODE = re.compile(r"(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)")
LINK = re.compile(r'(!?\[(?:[^\]\\]|\\.)*\]\()\s*<?([^)\s>]+)>?((?:\s+"[^"]*")?\))')
AUTOLINK = re.compile(r"<(https?://[^>\s]+)>")
HTML_COMMENT_LINE = re.compile(r"^\s*<!--.*-->\s*$")
EXTERNAL = re.compile(r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|//)")
MASK = "\x00{}\x00"
MASKED = re.compile(r"\x00(\d+)\x00")


def source_pages() -> list[PurePosixPath]:
    """English source pages as paths relative to `docs/`, in a stable order."""
    return sorted(
        PurePosixPath(path.relative_to(DOCS).as_posix())
        for path in DOCS.rglob("*.md")
        if not path.name.endswith(".ko.md")
    )


def source_for(page: PurePosixPath, lang: str) -> PurePosixPath:
    suffix = LANGUAGES[lang][1]
    return page if suffix == ".md" else page.with_name(page.stem + suffix)


def route(page: PurePosixPath) -> str:
    """Site route of a page, without a language prefix: `reference/runtime/tools`."""
    parts = list(page.with_suffix("").parts)
    if parts == ["README"]:
        return f"{REFERENCE}/index"
    parts[-1] = "index" if parts[-1] == "README" else parts[-1].lower().replace("_", "-")
    return "/".join([REFERENCE, *parts])


def site_url(route_path: str, lang: str) -> str:
    return "/" + LANGUAGES[lang][0] + route_path


def mdx_path(page: PurePosixPath, lang: str) -> Path:
    return SITE / (LANGUAGES[lang][0] + route(page) + ".mdx")


def image_url(rel: PurePosixPath) -> str:
    return f"/{IMAGES}/{rel.as_posix()}"


@functools.lru_cache(maxsize=None)
def anchor_map(source: PurePosixPath) -> dict[str, str]:
    """GitHub heading slug -> the id Mintlify gives that heading, where they differ.

    GitHub drops `/` from a heading id (`A/B` -> `ab`); Mintlify keeps it
    (`a/b`), so a link written against the source needs the second form.
    """
    mapping: dict[str, str] = {}
    in_fence = False
    for line in (DOCS / source).read_text(encoding="utf-8").splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        heading = None if in_fence else re.match(r"#{1,6}\s+(.*?)\s*#*\s*$", line)
        if not heading:
            continue
        text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", heading.group(1)).lower()
        text = re.sub(r"[`*]", "", text)
        github = re.sub(r"[^\w\- ]", "", text).replace(" ", "-")
        mintlify = re.sub(r"[^\w\-/ ]", "", text).replace(" ", "-")
        if github != mintlify:
            mapping.setdefault(github, mintlify)
    return mapping


def mapped_anchor(fragment: str, source: PurePosixPath) -> str:
    return f"#{anchor_map(source).get(fragment, fragment)}" if fragment else ""


def rewrite_target(target: str, origin: PurePosixPath, pages: set[PurePosixPath], lang: str) -> str:
    """Map one relative link target found in `docs/<origin>` to a site URL."""
    if target.startswith("#"):
        return mapped_anchor(target[1:], origin)
    if EXTERNAL.match(target):
        return target
    path_part, _, fragment = target.partition("#")
    resolved = PurePosixPath("docs") / origin.parent
    for part in PurePosixPath(path_part).parts:
        if part == "..":
            resolved = resolved.parent
        elif part != ".":
            resolved = resolved / part
    try:
        inside = resolved.relative_to("docs")
    except ValueError:
        return GITHUB_BLOB + resolved.as_posix() + (f"#{fragment}" if fragment else "")
    anchor = mapped_anchor(fragment, inside) if (DOCS / inside).is_file() else (f"#{fragment}" if fragment else "")
    english = inside
    if inside.name.endswith(".ko.md"):
        english = inside.with_name(inside.name[: -len(".ko.md")] + ".md")
    if english in pages:
        return site_url(route(english), lang) + anchor
    if inside.suffix == ".svg" and (DOCS / inside).is_file():
        return image_url(inside) + anchor
    return GITHUB_BLOB + resolved.as_posix() + anchor


def escape_text(text: str) -> str:
    """Escape what MDX would read as JSX, an expression or a wiki link."""
    text = re.sub(r"(?<!\\)([{}])", r"\\\1", text)
    text = re.sub(r"(?<!\\)\[\[", r"\\[\\[", text)
    return re.sub(r"(?<!\\)<(?=[A-Za-z/!?])", r"\\<", text)


def convert_line(line: str, origin: PurePosixPath, pages: set[PurePosixPath], lang: str) -> str:
    masked: list[str] = []
    in_table = line.lstrip().startswith("|")

    def hide(match: re.Match[str]) -> str:
        span = match.group(0)
        if in_table:
            # A table row ends a cell at every `|`, code span or not, unless it
            # is escaped; without this a span such as `a|b` splits the cell.
            span = re.sub(r"(?<!\\)\|", r"\\|", span)
        masked.append(span)
        return MASK.format(len(masked) - 1)

    line = INLINE_CODE.sub(hide, line)
    line = AUTOLINK.sub(lambda m: f"[{m.group(1)}]({m.group(1)})", line)
    line = LINK.sub(
        lambda m: m.group(1) + rewrite_target(m.group(2), origin, pages, lang) + m.group(3), line
    )
    # Escape only the prose between link destinations, which hold no braces.
    pieces = re.split(r"(\]\([^)]*\))", line)
    line = "".join(piece if piece.startswith("](") else escape_text(piece) for piece in pieces)
    return MASKED.sub(lambda m: masked[int(m.group(1))], line)


def plain(text: str) -> str:
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    return re.sub(r"[`*_]", "", text).strip()


def describe(body: list[str]) -> str:
    """First prose paragraph of a page, cut at a sentence end near 180 characters."""
    paragraph: list[str] = []
    in_fence = False
    for line in body:
        stripped = line.strip()
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if not stripped:
            if paragraph:
                break
            continue
        if stripped.startswith(("#", ">", "|", "-", "*", "![", "<")) or re.match(r"\d+\.\s", stripped):
            if paragraph:
                break
            continue
        paragraph.append(stripped)
    text = re.sub(r"\\([{}<])", r"\1", plain(" ".join(paragraph)))
    if len(text) <= 180:
        return text
    cut = text[:180]
    sentence = max(cut.rfind(". "), cut.rfind("다. "))
    if sentence >= 60:
        return cut[: sentence + 1].strip()
    return cut[: cut.rfind(" ")].rstrip(",;:") + "..."


def convert_page(page: PurePosixPath, lang: str, pages: set[PurePosixPath]) -> str:
    source = DOCS / source_for(page, lang)
    lines = source.read_text(encoding="utf-8").splitlines()
    switch = LANGUAGES[lang][2]
    title = ""
    out: list[str] = []
    in_fence = False
    fence_mark = ""
    for line in lines:
        match = FENCE.match(line)
        if match:
            mark = match.group(1)
            if not in_fence:
                in_fence, fence_mark = True, mark[0]
            elif mark[0] == fence_mark:
                in_fence = False
            out.append(line)
            continue
        if in_fence:
            out.append(line)
            continue
        if not title and line.startswith("# "):
            title = plain(line[2:])
            continue
        if switch.match(line) or HTML_COMMENT_LINE.match(line):
            continue
        out.append(convert_line(line, source_for(page, lang), pages, lang))
    while out and not out[0].strip():
        out.pop(0)
    description = DESCRIPTIONS.get(page.as_posix()) if lang == "en" else None
    description = description or describe(out)
    front = ["---", f"title: {json.dumps(title, ensure_ascii=False)}"]
    sidebar = SIDEBAR_TITLES.get(page.as_posix()) if lang == "en" else None
    if sidebar:
        front.append(f"sidebarTitle: {json.dumps(sidebar, ensure_ascii=False)}")
    front.append(f"description: {json.dumps(description, ensure_ascii=False)}")
    front.append("---")
    note = f"{{/* Generated from docs/{source_for(page, lang).as_posix()} by scripts/ci/sync_docs_site.py. Edit that file, not this one. */}}"
    return "\n".join([*front, "", note, "", *out]).rstrip() + "\n"


def expected_files() -> dict[Path, str | Path]:
    """Every generated file: MDX pages as text, images as the source path to copy."""
    pages = set(source_pages())
    files: dict[Path, str | Path] = {}
    for lang in LANGUAGES:
        for page in sorted(pages):
            files[mdx_path(page, lang)] = convert_page(page, lang, pages)
    for svg in sorted(DOCS.rglob("*.svg")):
        files[SITE / IMAGES / svg.relative_to(DOCS).as_posix()] = svg
    return files


def generated_roots() -> list[Path]:
    roots = [SITE / REFERENCE, SITE / IMAGES]
    roots += [SITE / prefix.rstrip("/") / REFERENCE for prefix, _, _ in LANGUAGES.values() if prefix]
    return roots


def existing_files() -> set[Path]:
    return {path for root in generated_roots() if root.exists() for path in root.rglob("*") if path.is_file()}


def navigation_pages(node) -> list[str]:
    if isinstance(node, str):
        return [node]
    if isinstance(node, list):
        return [page for item in node for page in navigation_pages(item)]
    if isinstance(node, dict):
        return [page for value in node.values() for page in navigation_pages(value)]
    return []


def navigation_problems(expected: dict[Path, str | Path]) -> list[str]:
    listed = {
        page
        for page in navigation_pages(json.loads((SITE / "docs.json").read_text(encoding="utf-8"))["navigation"])
        if "reference/" in page or page.endswith("reference")
    }
    wanted = {path.relative_to(SITE).with_suffix("").as_posix() for path in expected if path.suffix == ".mdx"}
    problems = [f"docs.json does not list {page}" for page in sorted(wanted - listed)]
    problems += [f"docs.json lists {page}, which no source page produces" for page in sorted(listed - wanted)]
    return problems


def current(path: Path) -> bytes | None:
    return path.read_bytes() if path.is_file() else None


def wanted_bytes(value: str | Path) -> bytes:
    return value.encode("utf-8") if isinstance(value, str) else value.read_bytes()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--check", action="store_true", help="exit 1 instead of rewriting stale files")
    args = parser.parse_args()

    expected = expected_files()
    stale = [path for path, value in expected.items() if current(path) != wanted_bytes(value)]
    orphans = sorted(existing_files() - set(expected))
    problems = navigation_problems(expected)

    if args.check:
        for path in stale:
            print(f"stale:  {path.relative_to(REPO).as_posix()}")
        for path in orphans:
            print(f"orphan: {path.relative_to(REPO).as_posix()}")
        for problem in problems:
            print(f"nav:    {problem}")
        if stale or orphans or problems:
            print("run `python3 scripts/ci/sync_docs_site.py` and commit the result", file=sys.stderr)
            return 1
        print(f"sync_docs_site: {len(expected)} generated files are current")
        return 0

    for path in stale:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(wanted_bytes(expected[path]))
    for path in orphans:
        path.unlink()
    for root in generated_roots():
        if root.exists():
            for folder in sorted((p for p in root.rglob("*") if p.is_dir()), reverse=True):
                if not any(folder.iterdir()):
                    shutil.rmtree(folder)
    for problem in problems:
        print(f"nav:    {problem}", file=sys.stderr)
    print(f"sync_docs_site: wrote {len(stale)}, removed {len(orphans)}, {len(expected)} generated files")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
