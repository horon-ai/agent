#!/usr/bin/env python3
"""Tests for the cross-platform daemon-socket helpers in bench_common.py.

On Windows the daemon listens on a named pipe whose name it derives from the
socket path (crates/platform-windows/src/ipc.rs, `path_to_pipe_name`). If the
benchmark derives a different name it can never connect, and every Windows
measurement turns into "daemon socket not ready". The cases below pin the
derivation to the Rust rules, including the ones its own tests pin.
"""

from __future__ import annotations

import os
import socket
import sys
import tempfile
import threading
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import bench_common as bc  # noqa: E402


class PipeName(unittest.TestCase):
    def test_case_and_separators_do_not_change_the_pipe(self) -> None:
        # Same pair as pipe_name_is_stable_and_normalizes_case_and_separators.
        a = bc.pipe_name(r"C:\Temp\horon-agent\server.sock")
        b = bc.pipe_name("c:/temp/HORON-AGENT/server.sock")
        self.assertEqual(a, b)
        self.assertTrue(a.startswith("\\\\.\\pipe\\server-"), a)
        self.assertEqual(len(a.rsplit("-", 1)[1]), 16)

    def test_empty_stem_falls_back_to_horon_agent(self) -> None:
        # Same input as pipe_name_falls_back_when_stem_is_empty.
        self.assertTrue(bc.pipe_name("...").startswith("\\\\.\\pipe\\horon-agent-"))

    def test_only_ascii_is_lowercased(self) -> None:
        # Rust uses to_ascii_lowercase, so a non-ASCII capital keeps its case.
        self.assertNotEqual(bc.pipe_name("C:/\u00c4/x.sock"), bc.pipe_name("c:/\u00e4/x.sock"))

    def test_stem_keeps_only_safe_characters_and_32_of_them(self) -> None:
        name = bc.pipe_name("C:/run/a.b c" + "x" * 40 + ".sock")
        stem = name[len("\\\\.\\pipe\\"):].rsplit("-", 1)[0]
        self.assertEqual(stem, ("abc" + "x" * 40)[:32])

    def test_main_and_debug_sockets_get_different_pipes(self) -> None:
        main = r"D:\a\_temp\hb-x\run\bench.sock"
        self.assertNotEqual(bc.pipe_name(main), bc.pipe_name(main.replace(".sock", "-debug.sock")))

class PipeBusy(unittest.TestCase):
    """A pipe between instances must be retried; a real failure must not be."""

    def test_busy_reported_by_open_is_retried(self) -> None:
        # open() on a busy pipe raises EINVAL without winerror (CI run 42352215328).
        self.assertTrue(bc._pipe_unavailable(OSError(22, "Invalid argument")))

    def test_busy_winerror_is_retried(self) -> None:
        exc = OSError(22, "busy")
        exc.winerror = bc.ERROR_PIPE_BUSY
        self.assertTrue(bc._pipe_unavailable(exc))

    def test_access_denied_is_not_retried(self) -> None:
        self.assertFalse(bc._pipe_unavailable(PermissionError(13, "Access is denied")))



@unittest.skipIf(bc.IS_WINDOWS, "Unix-socket round trip")
class UnixSocket(unittest.TestCase):
    def test_missing_socket_is_not_ready(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp") as tmp:
            self.assertFalse(bc.wait_for_socket(os.path.join(tmp, "absent.sock"), 0.2))

    def test_connect_round_trips_and_reports_eof(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp") as tmp:
            path = os.path.join(tmp, "s.sock")
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            server.bind(path)
            server.listen(1)

            def echo_once() -> None:
                conn, _ = server.accept()
                with conn:
                    conn.sendall(conn.recv(64).upper())

            thread = threading.Thread(target=echo_once, daemon=True)
            thread.start()
            try:
                stream = bc.ipc_connect(path, timeout=5)
                stream.sendall(b"ping\n")
                self.assertEqual(stream.recv(64), b"PING\n")
                self.assertEqual(stream.recv(64), b"")
                stream.close()
            finally:
                thread.join(5)
                server.close()


if __name__ == "__main__":
    unittest.main()
