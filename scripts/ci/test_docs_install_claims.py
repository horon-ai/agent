#!/usr/bin/env python3
"""No document may offer an install command for a package that is not published.

`@horon-ai/horon` and the six platform packages exist on the npm registry only as
the `0.0.0` placeholders that reserve the names, `@horon-ai/agent-sdk` answers
404, and PyPI has no `horon-agent-sdk`. A reader who follows such a command gets
a launcher with no runtime behind it, or a 404, after a successful-looking
install step.

The rule this gate holds: wherever the tree shows one of those install commands,
the section it sits in says the package is unpublished. Publishing is a separate,
owner-gated step; the day it happens the maintenance action is exactly the one
this gate asks for, removing the caveat, so the gate is a tripwire rather than a
permanent restriction.

Standard library only, no network: the gate reads the tree, which is the state
that changes.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# The commands that only work once the registries carry the packages.
UNPUBLISHED_COMMANDS = (
    "npm i -g @horon-ai/horon",
    "npm install -g @horon-ai/horon",
    "npx @horon-ai/horon",
    "npm install @horon-ai/agent-sdk",
    "pip install horon-agent-sdk",
    "uv add horon-agent-sdk",
)

# A caveat counts when it is in the same section as the command, so a reader who
# jumps straight to the code block still sees it.
CAVEATS = (
    "not published",
    "not on npm",
    "not on pypi",
    "게시되지 않",
    "게시되지 않았",
    "게시된 뒤",
    "올라 있지 않",
    "once published",
)

HEADING = re.compile(r"^#{1,6}\s")


def documents() -> list[Path]:
    """Markdown that a reader can reach from the repository root."""
    files = sorted(REPO.glob("*.md")) + sorted((REPO / "docs").rglob("*.md"))
    files += sorted((REPO / "packages").rglob("README.md"))
    return [path for path in files if path.is_file()]


def section_above(lines: list[str], index: int) -> list[str]:
    """The lines of the section that contains `index`, from its heading down."""
    start = index
    while start > 0 and not HEADING.match(lines[start]):
        start -= 1
    return lines[start : index + 1]


def offences(text: str) -> list[tuple[int, str]]:
    lines = text.splitlines()
    found: list[tuple[int, str]] = []
    for number, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith(">"):
            stripped = stripped.lstrip("> ").strip()
        command = next((c for c in UNPUBLISHED_COMMANDS if c in stripped), None)
        if command is None:
            continue
        section = "\n".join(section_above(lines, number)).lower()
        if not any(caveat in section for caveat in CAVEATS):
            found.append((number + 1, command))
    return found


class UnpublishedInstallClaimTests(unittest.TestCase):
    def test_the_tree_itself_carries_no_unmarked_claim(self) -> None:
        problems = []
        for path in documents():
            for line, command in offences(path.read_text(encoding="utf-8")):
                rel = path.relative_to(REPO).as_posix()
                problems.append(f"{rel}:{line}: `{command}` without an unpublished caveat")
        self.assertEqual(problems, [])

    def test_a_bare_command_is_reported(self) -> None:
        text = "## Install\n\n```bash\nnpm i -g @horon-ai/horon\n```\n"
        self.assertEqual(offences(text), [(4, "npm i -g @horon-ai/horon")])

    def test_a_command_under_a_caveat_heading_is_accepted(self) -> None:
        text = "## Installing from npm (not published yet)\n\n```bash\nnpm i -g @horon-ai/horon\n```\n"
        self.assertEqual(offences(text), [])

    def test_a_caveat_in_an_earlier_section_does_not_count(self) -> None:
        # A reader who lands on the code block must see the caveat, so proximity
        # is measured by section, not by file.
        text = (
            "## Status\n\nNothing is published yet.\n\n"
            "## Install\n\n```bash\npip install horon-agent-sdk\n```\n"
        )
        self.assertEqual(offences(text), [(8, "pip install horon-agent-sdk")])

    def test_the_korean_caveat_counts(self) -> None:
        text = "## npm에서 설치 (아직 게시되지 않음)\n\n```bash\nnpm i -g @horon-ai/horon\n```\n"
        self.assertEqual(offences(text), [])

    def test_a_blockquote_caveat_counts(self) -> None:
        text = "## Install\n\n> **Not published yet.** The names are placeholders.\n\n```sh\nnpx @horon-ai/horon --help\n```\n"
        self.assertEqual(offences(text), [])


if __name__ == "__main__":
    unittest.main()
