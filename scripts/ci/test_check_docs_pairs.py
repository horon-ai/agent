"""Tests for the English/Korean documentation pair gate.

`check_docs_pairs.py` reads its tree from the module-level `REPO`, so every case
below patches `REPO` and `pair_files()` at a temporary directory. The fake
documents carry the language switch line and real (resolvable, same-language)
links, because the gate reports link and language problems beside the structure
comparison: a fixture that fails those checks would pass this test for the wrong
reason.
"""

from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_docs_pairs as gate

SWITCH_EN = "Language: **English** | [한국어](TUI.ko.md)"
SWITCH_KO = "언어: [English](TUI.md) | **한국어**"


class DocsPairGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        (self.root / "docs" / "runtime").mkdir(parents=True)

    def write_pair(self, english: str, korean: str, stem: str = "TUI") -> Path:
        """Write `docs/runtime/<stem>.md` and its Korean sibling."""
        english_path = self.root / "docs" / "runtime" / f"{stem}.md"
        english_path.write_text(english, encoding="utf-8")
        (self.root / "docs" / "runtime" / f"{stem}.ko.md").write_text(korean, encoding="utf-8")
        return english_path

    def run_gate(self) -> tuple[int, str, str]:
        files = sorted((self.root / "docs").rglob("*.md"))
        out, err = io.StringIO(), io.StringIO()
        with (
            mock.patch.object(gate, "REPO", self.root),
            mock.patch.object(gate, "pair_files", return_value=files),
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
        ):
            code = gate.main()
        return code, out.getvalue(), err.getvalue()

    def assert_only_structure_error(self, err: str, expected: str) -> None:
        """The fixture is otherwise clean, so `expected` is the whole report."""
        problems = [line for line in err.splitlines() if line and not line.startswith("check_docs_pairs:")]
        self.assertEqual(problems, [expected], err)

    def test_matching_pair_passes(self):
        self.write_pair(
            f"""# The terminal client

{SWITCH_EN}

Runs a session.

## Running it

Bare `horon` opens the client.

| Key | Action |
| --- | --- |
| `enter` | send |
""",
            f"""# 터미널 클라이언트

{SWITCH_KO}

세션을 실행한다.

## 실행 방법

인자 없는 `horon`이 클라이언트를 연다.

| 키 | 동작 |
| --- | --- |
| `enter` | 보내기 |
""",
        )
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertEqual(err, "")
        self.assertIn("check_docs_pairs: 1 English/Korean pairs ok", out)

    def test_heading_count_difference_names_the_level(self):
        self.write_pair(
            f"""# The terminal client

{SWITCH_EN}

## Running it

Text.

### Settings

More text.
""",
            f"""# 터미널 클라이언트

{SWITCH_KO}

## 실행 방법

본문.
""",
        )
        code, _, err = self.run_gate()
        self.assertEqual(code, 1, err)
        self.assert_only_structure_error(err, "docs/runtime/TUI.md: h3 differs: English 1, Korean 0")
        self.assertIn("check_docs_pairs: 1 problem(s)", err)

    def test_table_row_difference_names_the_key(self):
        self.write_pair(
            f"""# The terminal client

{SWITCH_EN}

| Key | Action |
| --- | --- |
| `enter` | send |
| `tab` | complete |
""",
            f"""# 터미널 클라이언트

{SWITCH_KO}

| 키 | 동작 |
| --- | --- |
| `enter` | 보내기 |
""",
        )
        code, _, err = self.run_gate()
        self.assertEqual(code, 1, err)
        # The header and separator rows count too: 4 English rows, 3 Korean.
        self.assert_only_structure_error(err, "docs/runtime/TUI.md: table_rows differs: English 4, Korean 3")

    def test_english_only_heading_addition_fails(self):
        """An English-only edit is a failure; this is the shape todo 16 reuses."""
        english_path = self.write_pair(
            f"""# The terminal client

{SWITCH_EN}

## Running it

Text.
""",
            f"""# 터미널 클라이언트

{SWITCH_KO}

## 실행 방법

본문.
""",
        )
        code, _, err = self.run_gate()
        self.assertEqual(code, 0, err)

        english = english_path.read_text(encoding="utf-8")
        english_path.write_text(f"{english}\n## Installing\n\nUntranslated text.\n", encoding="utf-8")
        code, _, err = self.run_gate()
        self.assertEqual(code, 1, err)
        self.assert_only_structure_error(err, "docs/runtime/TUI.md: h2 differs: English 2, Korean 1")

    def test_language_line_is_required_before_structure(self):
        """A missing switch line is reported as itself, not as a heading gap."""
        self.write_pair("# The terminal client\n\n## Running it\n", "# 터미널 클라이언트\n\n## 실행 방법\n")
        code, _, err = self.run_gate()
        self.assertEqual(code, 1, err)
        self.assertIn("docs/runtime/TUI.md: missing language line", err)
        self.assertIn("docs/runtime/TUI.ko.md: missing language line", err)


if __name__ == "__main__":
    unittest.main()
