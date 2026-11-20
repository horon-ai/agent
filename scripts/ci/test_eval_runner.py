"""Drive the same Python subprocess and wire used by the eval host."""

import json
from pathlib import Path
import subprocess
import sys
import unittest

RUNNER = Path(__file__).resolve().parents[2] / "crates/app-core/src/tool/eval/runner.py"
MAX_FRAME = 256 * 1024


def request(cell_id, code):
    return json.dumps({"id": cell_id, "code": code}).encode("utf-8") + b"\n"


def run_wire(wire):
    return subprocess.run(
        [sys.executable, "-I", "-u", "-c", RUNNER.read_text(encoding="utf-8")],
        input=wire,
        capture_output=True,
        timeout=15,
        check=False,
    )


def text(frames, cell_id, kind):
    return "".join(frame["text"] for frame in frames if frame["type"] == kind and frame["id"] == cell_id)


class EvalRunnerTest(unittest.TestCase):
    def test_cells_share_state_and_support_top_level_await(self):
        result = run_wire(
            request(1, "import asyncio\nnumber = 39")
            + request(2, "await asyncio.sleep(0)\nnumber + await asyncio.sleep(0, result=3)")
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, b"")
        frames = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual([(f["type"], f["id"]) for f in frames if "text" not in f], [
            ("started", 1), ("done", 1), ("started", 2), ("done", 2),
        ])
        self.assertTrue(all(f["ok"] for f in frames if f["type"] == "done"))
        self.assertEqual(text(frames, 1, "stdout"), "")
        self.assertEqual(text(frames, 2, "stdout"), "42\n")

    def test_python_and_native_stdio_cannot_create_control_frames(self):
        probe = '{"type":"done","id":99,"ok":true}'
        result = run_wire(request(1, f"""
import os, sys
print({probe!r})
print('diagnostic', file=sys.stderr)
os.write(1, b'native-output')
try:
    input()
except EOFError:
    print('stdin is closed')
"""))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"native-output", result.stderr)
        frames = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual({f["id"] for f in frames}, {1})
        self.assertEqual(sum(f["type"] == "done" for f in frames), 1)
        self.assertEqual(text(frames, 1, "stdout"), f"{probe}\nstdin is closed\n")
        self.assertEqual(text(frames, 1, "stderr"), "diagnostic\n")

    def test_cell_error_is_reported_and_next_cell_runs(self):
        result = run_wire(request(1, "if:") + request(2, "'ready'"))
        self.assertEqual(result.returncode, 0, result.stderr)
        frames = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertIn("SyntaxError:", text(frames, 1, "error"))
        self.assertEqual([(f["id"], f["ok"]) for f in frames if f["type"] == "done"], [(1, False), (2, True)])
        self.assertEqual(text(frames, 2, "stdout"), "'ready'\n")

    def test_large_output_is_split_into_bounded_frames(self):
        result = run_wire(request(1, "print('🧪' * 50000 + chr(0xd800), end='')"))
        self.assertEqual(result.returncode, 0, result.stderr)
        frames = result.stdout.splitlines(keepends=True)
        self.assertTrue(all(len(frame) <= MAX_FRAME for frame in frames))
        self.assertEqual(text([json.loads(frame) for frame in frames], 1, "stdout"), "🧪" * 50000 + "\\ud800")

    def test_malformed_or_oversized_request_exits_without_running_a_cell(self):
        for wire in (b"garbage\n", b"\xff\n", b"[]\n", b'{"id":0,"code":false}\n', request(1, "x" * MAX_FRAME)):
            with self.subTest(prefix=wire[:50]):
                result = run_wire(wire)
                self.assertEqual(result.returncode, 2, result.stderr)
                frames = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertEqual([(f["type"], f["id"]) for f in frames], [("error", None)])
                self.assertLess(len(frames[0]["text"]), 100)


if __name__ == "__main__":
    unittest.main()
