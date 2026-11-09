#!/usr/bin/env python3
"""Documentation gate for pull requests that change only docs.

Checks, all reported at once (exit status 1 on any failure):

* `README.md` and `docs/README.md` exist: `crates/app-core/build.rs` embeds the
  first and the `horon_agent_docs` tests look up both;
* every Markdown file is valid UTF-8, because `include_str!` rejects anything
  else at compile time, which would turn a docs typo into a build failure;
* every relative link and image in a Markdown file points at a file or folder
  that exists. External links and pure `#anchor` links are not fetched.

Standard library only. The English/Korean pair gate (`check_docs_pairs.py`)
is a separate script and is run by the same CI job when it is present.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

REPO = Path(__file__).resolve().parents[2]
LINK = re.compile(r"!?\[(?:[^\]\\]|\\.)*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
FENCE = re.compile(r"^\s*(```|~~~)")
EXTERNAL = re.compile(r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|//)")


def markdown_files() -> list[Path]:
    files = sorted(REPO.glob("*.md")) + sorted((REPO / "docs").rglob("*.md"))
    return [path for path in files if path.is_file()]


def links(text: str):
    in_fence = False
    for number, line in enumerate(text.splitlines(), 1):
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for match in LINK.finditer(re.sub(r"`[^`\n]*`", "", line)):
            yield number, match.group(1)


def main() -> int:
    errors: list[str] = []
    for required in ("README.md", "docs/README.md"):
        if not (REPO / required).is_file():
            errors.append(f"{required}: missing, it is part of the embedded corpus")
    for path in markdown_files():
        rel = path.relative_to(REPO).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError as exc:
            errors.append(f"{rel}: not valid UTF-8 ({exc.reason})")
            continue
        for number, target in links(text):
            if target.startswith("#") or EXTERNAL.match(target):
                continue
            file_part = unquote(target.split("#", 1)[0].split("?", 1)[0])
            if not file_part:
                continue
            resolved = (REPO / file_part.lstrip("/")) if file_part.startswith("/") \
                else (path.parent / file_part)
            if not resolved.exists():
                errors.append(f"{rel}:{number}: broken relative link `{target}`")
    for error in errors:
        print(f"::error::{error}")
    print(f"checked {len(markdown_files())} Markdown file(s), {len(errors)} problem(s)")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
