#!/usr/bin/env python3
"""Generate the documentation-site pages whose source lives outside the site.

The developer reference under `packages/docs-site/reference/` (and its Korean
pair under `ko/reference/`) is written in place as MDX; nothing generates it.
Four pages are the exception, because their text is maintained beside the code
they describe: `PHILOSOPHY.md` (with its Korean pair) and the READMEs of the
TypeScript SDK, the Python SDK and the npm launcher. This script writes their
MDX copies and checks that `docs.json` lists each of them.

    python3 scripts/ci/sync_docs_site.py           # rewrite the generated pages
    python3 scripts/ci/sync_docs_site.py --check   # exit 1 when they are stale

What changes between a source file and its page:

* the `# Title` line and the `Language:` switch line move into the front matter
  and the site's language switcher;
* links to site pages (`packages/docs-site/**/*.mdx`) and to the other generated
  pages become site routes, links to other repository files become GitHub links;
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
import sys
from pathlib import Path, PurePosixPath

REPO = Path(__file__).resolve().parents[2]
SITE = REPO / "packages" / "docs-site"
REFERENCE = "reference"
GITHUB_BLOB = "https://github.com/horon-ai/agent/blob/dev/"

# Language code -> (path prefix inside the site, source suffix, language line).
LANGUAGES = {
    "en": ("", ".md", re.compile(r"^Language:\s")),
    "ko": ("ko/", ".ko.md", re.compile(r"^언어:\s")),
}

# Generated pages, keyed by their path under `reference/` (which fixes the route)
# and valued by the English source in the repository. A Korean pair beside the
# source (`PHILOSOPHY.ko.md`) is used when it exists; a page with none appears in
# the English tab only.
EXTRA_SOURCES = {
    "philosophy.md": "PHILOSOPHY.md",
    "sdk/typescript.md": "packages/sdk/typescript/README.md",
    "sdk/python.md": "packages/sdk/python/README.md",
    "sdk/npm.md": "packages/npm/horon/README.md",
}

# Sidebar labels for pages whose title is too long for a sidebar. Keyed by the
# source path under `docs/`; a page not listed uses its title.
SIDEBAR_TITLES = {
    "philosophy.md": "Philosophy",
    "sdk/typescript.md": "TypeScript SDK",
    "sdk/python.md": "Python SDK",
    "sdk/npm.md": "npm launcher",
}

# Descriptions for pages whose first paragraph does not read as one. A page not
# listed uses its first prose paragraph, cut at a sentence end.
DESCRIPTIONS = {
}

SIDEBAR_TITLES_KO = {
    "philosophy.md": "철학",
}

DESCRIPTIONS_KO = {
}

SIDEBAR_BY_LANG = {"en": SIDEBAR_TITLES, "ko": SIDEBAR_TITLES_KO}
DESCRIPTIONS_BY_LANG = {"en": DESCRIPTIONS, "ko": DESCRIPTIONS_KO}

FENCE = re.compile(r"^\s*(`{3,}|~{3,})")
INLINE_CODE = re.compile(r"(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)")
LINK = re.compile(r'(!?\[(?:[^\]\\]|\\.)*\]\()\s*<?([^)\s>]+)>?((?:\s+"[^"]*")?\))')
AUTOLINK = re.compile(r"<(https?://[^>\s]+)>")
HTML_COMMENT_LINE = re.compile(r"^\s*<!--.*-->\s*$")
EXTERNAL = re.compile(r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|//)")
MASK = "\x00{}\x00"
MASKED = re.compile(r"\x00(\d+)\x00")
# A line that is only a block HTML tag is MDX-rendered as HTML, so it is the one
# form of `<` that must survive: `<details>`/`<summary>` fold a long table on
# GitHub and on the site. Anything else that looks like a tag is escaped.
BLOCK_TAG = re.compile(r"^(?:</?(?:details|summary)(?:\s[^<>]*)?>|<summary>[^<>]*</summary>)$")


def source_pages() -> list[PurePosixPath]:
    """Generated pages as paths under `reference/`, in a stable order."""
    return sorted(PurePosixPath(key) for key in EXTRA_SOURCES)


def repo_source(page: PurePosixPath, lang: str) -> PurePosixPath | None:
    """Repository path of the page's source in `lang`, or None when it has none."""
    path = PurePosixPath(EXTRA_SOURCES[page.as_posix()])
    if lang != "en":
        path = path.with_name(path.stem + LANGUAGES[lang][1])
    return path if (REPO / path).is_file() else None


def extra_page_for(repo_path: PurePosixPath) -> PurePosixPath | None:
    """The page an extra source (in any language) produces, if `repo_path` is one."""
    for key, english in EXTRA_SOURCES.items():
        base = PurePosixPath(english)
        if repo_path in {base.with_name(base.stem + suffix) for _, suffix, _ in LANGUAGES.values()}:
            return PurePosixPath(key)
    return None


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


@functools.lru_cache(maxsize=None)
def anchor_map(source: PurePosixPath) -> dict[str, str]:
    """GitHub heading slug -> the id Mintlify gives that heading, where they differ.

    `source` is a repository path. GitHub drops `/` from a heading id (`A/B` ->
    `ab`); Mintlify keeps it (`a/b`), so a link written against the source needs
    the second form.
    """
    mapping: dict[str, str] = {}
    in_fence = False
    for line in (REPO / source).read_text(encoding="utf-8").splitlines():
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
    """Map one relative link target found in the repository file `origin` to a site URL."""
    if target.startswith("#"):
        return mapped_anchor(target[1:], origin)
    if EXTERNAL.match(target):
        return target
    path_part, _, fragment = target.partition("#")
    resolved = origin.parent
    for part in PurePosixPath(path_part).parts:
        if part == "..":
            resolved = resolved.parent
        elif part != ".":
            resolved = resolved / part
    exists = (REPO / resolved).is_file()
    anchor = mapped_anchor(fragment, resolved) if exists else (f"#{fragment}" if fragment else "")
    site_parts = SITE.relative_to(REPO).parts
    if resolved.parts[: len(site_parts)] == site_parts and resolved.suffix == ".mdx":
        # A hand-written site page: its route is its path inside the site.
        return "/" + PurePosixPath(*resolved.parts[len(site_parts):]).with_suffix("").as_posix() + anchor
    page = extra_page_for(resolved)
    if page is not None and page in pages:
        shown = lang if repo_source(page, lang) is not None else "en"
        return site_url(route(page), shown) + anchor
    return GITHUB_BLOB + resolved.as_posix() + anchor


def escape_text(text: str) -> str:
    """Escape what MDX would read as JSX, an expression or a wiki link."""
    text = re.sub(r"(?<!\\)([{}])", r"\\\1", text)
    text = re.sub(r"(?<!\\)\[\[", r"\\[\\[", text)
    # A tag name may start with a letter of any script (`<디렉터리>`) or `/`, `!`, `?`.
    return re.sub(r"(?<!\\)<(?=[^\W\d_]|[/!?])", r"\\<", text)


def convert_line(line: str, origin: PurePosixPath, pages: set[PurePosixPath], lang: str) -> str:
    if BLOCK_TAG.match(line.strip()):
        return line
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
    repo = repo_source(page, lang)
    assert repo is not None, f"{page} has no {lang} source"
    source = REPO / repo
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
        out.append(convert_line(line, repo, pages, lang))
    while out and not out[0].strip():
        out.pop(0)
    description = DESCRIPTIONS_BY_LANG[lang].get(page.as_posix())
    description = description or describe(out)
    front = ["---", f"title: {json.dumps(title, ensure_ascii=False)}"]
    sidebar = SIDEBAR_BY_LANG[lang].get(page.as_posix())
    if sidebar:
        front.append(f"sidebarTitle: {json.dumps(sidebar, ensure_ascii=False)}")
    front.append(f"description: {json.dumps(description, ensure_ascii=False)}")
    front.append("---")
    note = f"{{/* Generated from {repo.as_posix()} by scripts/ci/sync_docs_site.py. Edit that file, not this one. */}}"
    return "\n".join([*front, "", note, "", *out]).rstrip() + "\n"


def expected_files() -> dict[Path, str]:
    """Every generated page and its MDX text."""
    pages = set(source_pages())
    files: dict[Path, str] = {}
    for lang in LANGUAGES:
        for page in sorted(pages):
            if repo_source(page, lang) is not None:
                files[mdx_path(page, lang)] = convert_page(page, lang, pages)
    return files


def navigation_pages(node) -> list[str]:
    """Every page path listed under a `pages` key of the navigation."""
    if isinstance(node, list):
        return [page for item in node for page in navigation_pages(item)]
    if isinstance(node, dict):
        found: list[str] = []
        for key, value in node.items():
            if key == "pages" and isinstance(value, list):
                found += [item for item in value if isinstance(item, str)]
                found += [page for item in value if not isinstance(item, str) for page in navigation_pages(item)]
            elif isinstance(value, (list, dict)):
                found += navigation_pages(value)
        return found
    return []


def navigation_problems(expected: dict[Path, str]) -> list[str]:
    """Generated pages `docs.json` does not list, and listed pages with no file."""
    listed = set(navigation_pages(json.loads((SITE / "docs.json").read_text(encoding="utf-8"))["navigation"]))
    wanted = {path.relative_to(SITE).with_suffix("").as_posix() for path in expected}
    problems = [f"docs.json does not list {page}" for page in sorted(wanted - listed)]
    problems += [
        f"docs.json lists {page}, which has no page file"
        for page in sorted(listed)
        if not (SITE / f"{page}.mdx").is_file() and not (SITE / f"{page}.md").is_file()
    ]
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--check", action="store_true", help="exit 1 instead of rewriting stale pages")
    args = parser.parse_args()

    expected = expected_files()
    stale = [
        path for path, text in expected.items()
        if not path.is_file() or path.read_text(encoding="utf-8") != text
    ]
    problems = navigation_problems(expected)

    if args.check:
        for path in stale:
            print(f"stale:  {path.relative_to(REPO).as_posix()}")
        for problem in problems:
            print(f"nav:    {problem}")
        if stale or problems:
            print("run `python3 scripts/ci/sync_docs_site.py` and commit the result", file=sys.stderr)
            return 1
        print(f"sync_docs_site: {len(expected)} generated pages are current")
        return 0

    for path in stale:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(expected[path], encoding="utf-8")
    for problem in problems:
        print(f"nav:    {problem}", file=sys.stderr)
    print(f"sync_docs_site: wrote {len(stale)}, {len(expected)} generated pages")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
