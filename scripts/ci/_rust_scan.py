"""Shared Rust source scanning for the CI budget ratchets.

Four gates in this directory (`check_code_size`, `check_test_size`,
`check_panics`, `check_swallowed_errors`) all need the same two answers: which
files are production code rather than test code, and, for the pattern budgets,
which lines of a file sit outside `#[cfg(test)]` blocks. Keeping one
implementation here means a fix to either rule reaches every budget at once.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

CFG_TEST_RE = re.compile(r"^\s*#\s*\[\s*cfg\s*\(\s*(?:all\s*\(\s*)?test\s*[,)]")
ITEM_START_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:mod|fn)\b")


def is_test_rust_file(path: Path) -> bool:
    """Whether this file carries test code rather than production code."""
    if path.suffix != ".rs":
        return False
    parts = path.relative_to(REPO_ROOT).as_posix().split("/")
    if parts[0] == "tests" or any(
        part == "tests"
        or part.endswith("_tests")
        or part.endswith("_test")
        or part.startswith("tests_")
        for part in parts
    ):
        return True
    name = path.name
    return (
        name == "tests.rs"
        or name.endswith("_tests.rs")
        or name.endswith("_test.rs")
        or name.startswith("tests_")
    )


def rust_files(roots: tuple[Path, ...]) -> list[Path]:
    """Every `.rs` file below `roots`, in stable order."""
    files: list[Path] = []
    for root in roots:
        if not root.exists():
            continue
        files.extend(sorted(root.rglob("*.rs")))
    return files


def production_rust_files(roots: tuple[Path, ...]) -> list[Path]:
    return [path for path in rust_files(roots) if not is_test_rust_file(path)]


def test_rust_files(roots: tuple[Path, ...]) -> list[Path]:
    return [path for path in rust_files(roots) if is_test_rust_file(path)]


def line_count(path: Path) -> int:
    with path.open("r", encoding="utf-8") as handle:
        return sum(1 for _ in handle)


def brace_delta(line: str) -> int:
    """Approximate Rust block nesting for budget classification.

    The pattern budgets are ratchets, not parsers. This intentionally simple scan
    ignores comments and strings, which is acceptable for excluding normal
    `#[cfg(test)] mod tests { ... }` blocks from production counts.
    """
    return line.count("{") - line.count("}")


def production_lines(path: Path) -> list[str]:
    """Lines of `path` that sit outside `#[cfg(test)]` blocks."""
    lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    output: list[str] = []
    skip_stack: list[int] = []
    pending_cfg_test = False

    for line in lines:
        stripped = line.strip()
        current_depth = sum(skip_stack)
        if current_depth == 0:
            if pending_cfg_test and ITEM_START_RE.match(line):
                delta = brace_delta(line)
                if delta > 0:
                    skip_stack.append(delta)
                pending_cfg_test = False
                continue
            if pending_cfg_test and stripped and not stripped.startswith("#"):
                pending_cfg_test = False
            if CFG_TEST_RE.match(line):
                pending_cfg_test = True
                continue
            output.append(line)
        else:
            skip_stack[-1] += brace_delta(line)
            if skip_stack[-1] <= 0:
                skip_stack.pop()
    return output
