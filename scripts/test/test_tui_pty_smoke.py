#!/usr/bin/env python3
"""Contract tests for `scripts/test/tui_pty_smoke.py`'s virtual terminal screen."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

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

    def test_live_contains_rejects_overwritten_pre_turn_idle_echo_until_post_turn_idle(self) -> None:
        screen = SMOKE.TerminalScreen(rows=40, cols=120)
        initial = "\x1b[?1049h\x1b[40;1Hidle · mock/mock-model · ~/work"
        screen.feed(initial)
        start = len(initial)

        # Untouched live status row before `start` does not satisfy a fresh turn wait.
        self.assertFalse(screen.live_contains("idle · mock/mock-model", since=start))

        # Pre-turn typing echoes redraw the old `idle` status row after `start`, then the
        # submitted turn switches row 40 to `running` before reply text arrives.
        screen.feed("\x1b[39;1H> [tui:context-proof] original\x1b[40;1Hidle · mock/mock-model · ~/work")
        screen.feed("\x1b[40;1Hrunning ⠋ · mock/mock-model · ~/work")
        screen.feed("\x1b[38;1Hcontext-verified-original (still not idle)")
        self.assertFalse(screen.live_contains("idle · mock/mock-model", since=start))

        screen.feed("\x1b[40;1Hidle · mock/mock-model · ~/work      ")
        self.assertTrue(screen.live_contains("idle · mock/mock-model", since=start))

    def test_live_contains_matches_coalesced_reply_and_idle_frame(self) -> None:
        screen = SMOKE.TerminalScreen(rows=40, cols=120)
        initial = "\x1b[?1049h\x1b[40;1Hidle · mock/mock-model · ~/work"
        screen.feed(initial)
        start = len(initial)

        screen.feed("\x1b[40;1Hrunning ⠋ · mock/mock-model · ~/work")
        screen.feed(
            "\x1b[38;1Hcontext-verified-reset"
            "\x1b[40;1Hidle · mock/mock-model · ~/work      "
        )
        self.assertTrue(screen.contains("context-verified-reset", since=start))
        self.assertTrue(screen.live_contains("idle · mock/mock-model", since=start))

    def test_session_id_resume_wait_ignores_redrawn_previous_session_until_target_arrives(self) -> None:
        screen = SMOKE.TerminalScreen(rows=40, cols=120)
        before_close = (
            "\x1b[?1049h"
            "\x1b[10;1H  Swarm children"
            "\x1b[12;1H  parent: session_parent_2"
        )
        screen.feed(before_close)
        attach_start = len(before_close)

        # Closing the picker redraws the old session's transcript before attach completes.
        screen.feed("\x1b[10;1H\x1b[K\x1b[12;1H\x1b[K\x1b[38;1H• resumed session_child_1")
        self.assertFalse(screen.contains("resumed session_parent_2", since=attach_start))

        # Differential redraw replaces only the trailing session suffix on row 38.
        screen.feed("\x1b[38;19Hparent_2")
        self.assertTrue(screen.contains("resumed session_parent_2", since=attach_start))


class ReadinessAndDiagnosticsTests(unittest.TestCase):
    def test_format_failure_diagnostics_includes_phase_state_and_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "prime-server-stderr.log").write_text("bind failed: address in use\n", encoding="utf-8")
            Path(tmp, "run").mkdir()
            Path(tmp, "run", "horon-agent.sock").write_text("", encoding="utf-8")
            home = Path(tmp, "home")
            home.mkdir()
            formatted = SMOKE.format_failure_diagnostics(
                phase="first screen",
                state="exited 1",
                cwd=tmp,
                home=str(home),
                rendered="screen contents",
                transcript="raw bytes",
            )
        self.assertIn("[phase=first screen] (exited 1)", formatted)
        self.assertIn("screen contents", formatted)
        self.assertIn("raw bytes", formatted)
        self.assertIn("bind failed: address in use", formatted)
        self.assertIn("horon-agent.sock", formatted)

    def test_wait_for_server_ready_polls_without_wasting_fixed_sleep(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stderr_path = str(Path(tmp, "prime-server-stderr.log"))
            Path(tmp, "run").mkdir()
            proc = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(5)"])
            try:
                calls = {"n": 0}

                def probe() -> bool:
                    calls["n"] += 1
                    return calls["n"] >= 2

                started = time.monotonic()
                SMOKE.wait_for_server_ready(proc, probe, stderr_path, tmp, tmp, str(Path(tmp, "run")), timeout=5.0)
                elapsed = time.monotonic() - started
                self.assertEqual(calls["n"], 2)
                self.assertLess(elapsed, 2.0)
            finally:
                proc.terminate()
                proc.wait(timeout=5)

    def test_wait_for_server_ready_reports_early_exit_with_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stderr_path = str(Path(tmp, "prime-server-stderr.log"))
            Path(tmp, "run").mkdir()
            with open(stderr_path, "w", encoding="utf-8") as handle:
                proc = subprocess.Popen(
                    [sys.executable, "-c", "import sys; sys.stderr.write('boom on bind\\n'); sys.exit(7)"],
                    stderr=handle,
                )
                proc.wait(timeout=5)
            with self.assertRaises(SMOKE.Failure) as ctx:
                SMOKE.wait_for_server_ready(proc, lambda: False, stderr_path, tmp, tmp, str(Path(tmp, "run")), timeout=5.0)
            msg = str(ctx.exception)
            self.assertIn("[phase=prime_server]", msg)
            self.assertIn("status 7", msg)
            self.assertIn("boom on bind", msg)

    def test_wait_for_server_ready_times_out_with_phase_and_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stderr_path = str(Path(tmp, "prime-server-stderr.log"))
            Path(tmp, "run").mkdir()
            Path(stderr_path).write_text("still initializing\n", encoding="utf-8")
            proc = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(10)"])
            try:
                with self.assertRaises(SMOKE.Failure) as ctx:
                    SMOKE.wait_for_server_ready(proc, lambda: False, stderr_path, tmp, tmp, str(Path(tmp, "run")), timeout=0.15)
                msg = str(ctx.exception)
                self.assertIn("[phase=prime_server]", msg)
                self.assertIn("did not become ready", msg)
                self.assertIn("still initializing", msg)
            finally:
                if proc.poll() is None:
                    proc.terminate()
                    proc.wait(timeout=5)

    def test_posix_terminal_read_until_fails_immediately_on_early_exit(self) -> None:
        if not SMOKE.POSIX:
            self.skipTest("POSIX PTY only")
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "run").mkdir()
            terminal = SMOKE.PosixTerminal(
                [sys.executable, "-c", "import sys; print('boot-error', flush=True); sys.exit(5)"],
                cwd=tmp,
                env={"HORON_HOME": tmp, "HORON_AGENT_RUNTIME_DIR": str(Path(tmp, "run"))},
            )
            try:
                started = time.monotonic()
                with self.assertRaises(SMOKE.Failure) as ctx:
                    terminal.read_until("never-arrives", timeout=30.0, what="first screen")
                elapsed = time.monotonic() - started
                self.assertLess(elapsed, 3.0)
                msg = str(ctx.exception)
                self.assertIn("[phase=first screen] (exited 5)", msg)
                self.assertIn("boot-error", msg)
            finally:
                terminal.close()

    def test_windows_terminal_read_until_fails_immediately_on_early_exit(self) -> None:
        class FakePtyProcess:
            def __init__(self) -> None:
                self._chunks = ["early-conpty-death\r\n"]
                self.exitstatus = 9
                self.alive = True

            def read(self, _size: int) -> str:
                if self._chunks:
                    return self._chunks.pop(0)
                self.alive = False
                return ""

            def isalive(self) -> bool:
                return self.alive

            def terminate(self, force: bool = False) -> None:
                self.alive = False

        fake_pty = type("FakePtyType", (), {"spawn": staticmethod(lambda *a, **k: FakePtyProcess())})
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "run").mkdir()
            with mock.patch.object(SMOKE, "PtyProcess", fake_pty):
                terminal = SMOKE.WindowsTerminal(
                    ["horon-tui.exe"],
                    cwd=tmp,
                    env={"HORON_HOME": tmp, "HORON_AGENT_RUNTIME_DIR": str(Path(tmp, "run"))},
                )
            try:
                started = time.monotonic()
                with self.assertRaises(SMOKE.Failure) as ctx:
                    terminal.read_until("never-arrives", timeout=30.0, what="first screen")
                elapsed = time.monotonic() - started
                self.assertLess(elapsed, 3.0)
                msg = str(ctx.exception)
                self.assertIn("[phase=first screen] (client exited 9)", msg)
                self.assertIn("early-conpty-death", msg)
            finally:
                terminal.close()


if __name__ == "__main__":
    unittest.main()
