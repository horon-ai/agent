#!/usr/bin/env python3
"""Contract tests for `scripts/test/tui_pty_smoke.py`'s virtual terminal screen."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

SMOKE_PATH = Path(__file__).resolve().with_name("tui_pty_smoke.py")
SPEC = importlib.util.spec_from_file_location("tui_pty_smoke", SMOKE_PATH)
assert SPEC is not None and SPEC.loader is not None
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class TerminalScreenTests(unittest.TestCase):
    def test_differential_redraw_preserves_skipped_cells_from_previous_frame(self) -> None:
        """Ratatui skips cells whose char+style already match row 19 (#776).

        Captured verbatim from the 6 failing ubuntu-latest CI jobs:
        - /tmp/htui-t9qm44rl (col 34 't' skipped) -> 'nothing ma' + CUP(19,35) + 'ches that filter'
        - /tmp/htui-dcygvgkm (col 35 'c' skipped) -> 'nothing mat' + CUP(19,36) + 'hes that filter'
        - /tmp/htui-yjh1uu0b (col 36 'h' skipped) -> 'nothing matc' + CUP(19,37) + 'es that filter'
        """
        cases = (
            (
                "/tmp/htui-t9qm44rl/installed · 11-23 06:26",
                "\x1b[19;23H\x1b[;m nothing ma\x1b[19;35Hches that filter\x1b[;m               ",
            ),
            (
                "/tmp/htui-dcygvgkm/installed · 11-22 21:15",
                "\x1b[19;23H\x1b[;m nothing mat\x1b[19;36Hhes that filter\x1b[;m               ",
            ),
            (
                "/tmp/htui-yjh1uu0b/happy · 11-23 00:11",
                "\x1b[19;23H\x1b[;m nothing matc\x1b[19;37Hes that filter\x1b[;m           ",
            ),
        )
        for preview_header, diff_frame in cases:
            with self.subTest(preview_header=preview_header):
                # Prove the raw byte stream genuinely lacked the contiguous substring.
                self.assertFalse(SMOKE.contains(diff_frame, "nothing match"))

                screen = SMOKE.TerminalScreen(rows=40, cols=120)
                initial = f"\x1b[?1049h\x1b[19;23H\x1b[;m {preview_header}"
                screen.feed(initial)
                since = len(initial)
                self.assertFalse(screen.contains("nothing match", since=since))

                screen.feed(diff_frame)
                self.assertTrue(screen.contains("nothing match", since=since))
                self.assertIn("nothing matches that filter", screen.render())

    def test_since_ignores_untouched_rows_and_retains_transient_rows(self) -> None:
        screen = SMOKE.TerminalScreen(rows=10, cols=80)
        first = "\x1b[?1049h\x1b[2;1Hidle · mock/mock-model\x1b[4;1Harchive-pty-fixture"
        screen.feed(first)
        after_first = len(first)

        # Untouched rows from before `after_first` must not satisfy a fresh wait.
        self.assertFalse(screen.contains("archive-pty-fixture", since=after_first))
        self.assertFalse(screen.contains("idle", since=after_first))

        # A row modified after `after_first` matches even if a later frame overwrites it.
        second = "\x1b[2;1Hidle · mock/mock-model"
        screen.feed(second)
        third = "\x1b[2;1Hattached · mock/mock-model"
        screen.feed(third)
        self.assertTrue(screen.contains("idle", since=after_first))
        self.assertTrue(screen.contains("attached", since=after_first))
        self.assertFalse(screen.contains("archive-pty-fixture", since=after_first))

    def test_escape_sequences_split_across_reads_compose_identically(self) -> None:
        stream = (
            "\x1b[?1049h"
            "\x1b[19;23H\x1b[;m /tmp/htui-dcygvgkm/installed"
            "\x1b[19;23H\x1b[;m nothing mat\x1b[19;36Hhes that filter"
        )
        whole = SMOKE.TerminalScreen(rows=40, cols=120)
        whole.feed(stream)

        byte_by_byte = SMOKE.TerminalScreen(rows=40, cols=120)
        for ch in stream:
            byte_by_byte.feed(ch)

        self.assertEqual(byte_by_byte.render(), whole.render())
        self.assertTrue(byte_by_byte.contains("nothing match"))

    def test_alternate_screen_preserves_pre_handoff_stdout_lines(self) -> None:
        screen = SMOKE.TerminalScreen(rows=40, cols=120)
        chunk = (
            "Continuing session session_t-rex_1795382085910_abc\r\n"
            "\x1b[?1049h\x1b[1;1Hmock/mock-model"
        )
        screen.feed(chunk)
        self.assertTrue(screen.contains("Continuing session session_"))
        self.assertTrue(screen.contains("mock/mock-model"))
        # Alt screen itself does not carry main-screen text into its cells.
        self.assertNotIn("Continuing session", screen.render())


if __name__ == "__main__":
    unittest.main()
