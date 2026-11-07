"""Tests for the platform cfg allowlist ratchet."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_platform_cfg as gate


class PlatformCfgGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        self.allowlist = self.root / "scripts" / "ci" / "platform_cfg_allowlist.json"

    def write_allowlist(self, files: dict[str, dict[str, object]]) -> None:
        self.allowlist.parent.mkdir(parents=True, exist_ok=True)
        self.allowlist.write_text(
            json.dumps({"version": 1, "files": files}, indent=2) + "\n",
            encoding="utf-8",
        )

    def write_source(self, relative: str, text: str) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def run_gate(self, *extra: str) -> tuple[int, str, str]:
        argv = [
            "--root",
            str(self.root),
            "--allowlist",
            str(self.allowlist),
            *extra,
        ]
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gate.main(argv)
        return code, out.getvalue(), err.getvalue()

    def test_platform_crates_are_ignored(self):
        self.write_source(
            "crates/platform/src/lib.rs",
            '#[cfg(unix)]\nfn a() {}\n',
        )
        self.write_source(
            "crates/platform-macos/src/lib.rs",
            'fn b() { cfg!(target_os = "macos"); }\n',
        )
        self.write_allowlist({})
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("Platform cfg budget OK", out)

    def test_uncovered_line_fails_with_path_and_line(self):
        self.write_source(
            "crates/base/src/background.rs",
            "fn spawn() {\n    #[cfg(unix)]\n    let _ = std::os::unix::fs::symlink;\n}\n",
        )
        self.write_allowlist({})
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/base/src/background.rs:2", err)
        self.assertIn("#[cfg(unix)]", err)

    def test_grown_count_fails_and_names_every_line(self):
        self.write_source(
            "crates/base/src/a.rs",
            "#[cfg(unix)]\nfn a() {}\n#[cfg(windows)]\nfn b() {}\n",
        )
        self.write_allowlist(
            {"crates/base/src/a.rs": {"count": 1, "reason": "console only"}}
        )
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/base/src/a.rs:1", err)
        self.assertIn("crates/base/src/a.rs:3", err)

    def test_count_drop_fails_until_update(self):
        self.write_source("crates/base/src/a.rs", "#[cfg(unix)]\nfn a() {}\n")
        self.write_allowlist(
            {"crates/base/src/a.rs": {"count": 2, "reason": "console only"}}
        )
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("--update", err)

        code, _out, err = self.run_gate("--update")
        self.assertEqual(code, 0, err)
        saved = json.loads(self.allowlist.read_text(encoding="utf-8"))
        self.assertEqual(saved["files"]["crates/base/src/a.rs"]["count"], 1)
        self.assertEqual(
            saved["files"]["crates/base/src/a.rs"]["reason"], "console only"
        )
        code, _out, err = self.run_gate()
        self.assertEqual(code, 0, err)

    def test_missing_reason_fails(self):
        self.write_source("crates/base/src/a.rs", "#[cfg(unix)]\nfn a() {}\n")
        for reason in ("", "   ", "TODO: classify"):
            with self.subTest(reason=reason):
                self.write_allowlist(
                    {"crates/base/src/a.rs": {"count": 1, "reason": reason}}
                )
                code, _out, err = self.run_gate()
                self.assertEqual(code, 1)
                self.assertIn("missing reason for crates/base/src/a.rs", err)

    def test_cfg_attr_and_cfg_macro_are_detected(self):
        self.write_source(
            "crates/base/src/link.rs",
            '#[cfg_attr(windows, link(name = "user32"))]\nextern "C" {}\n'
            'fn t() -> bool { cfg!(target_os = "linux") }\n',
        )
        self.write_allowlist({})
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/base/src/link.rs:1", err)
        self.assertIn("cfg_attr", err)
        self.assertIn("crates/base/src/link.rs:3", err)
        self.assertIn("cfg!(", err)

        self.write_allowlist(
            {"crates/base/src/link.rs": {"count": 2, "reason": "linking is platform-specific"}}
        )
        code, _out, err = self.run_gate()
        self.assertEqual(code, 0, err)

    def test_test_modules_are_counted_the_same(self):
        self.write_source(
            "crates/base/src/a.rs",
            "#[cfg(test)]\nmod tests {\n    #[cfg(unix)]\n    fn only_unix() {}\n}\n",
        )
        self.write_allowlist(
            {"crates/base/src/a.rs": {"count": 1, "reason": "asserts unix-only behaviour"}}
        )
        code, _out, err = self.run_gate()
        self.assertEqual(code, 0, err)

        self.write_allowlist({})
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/base/src/a.rs:3", err)

    def test_update_never_invents_a_reason(self):
        self.write_source("crates/base/src/new.rs", "#[cfg(windows)]\nfn w() {}\n")
        self.write_allowlist({})
        code, _out, err = self.run_gate("--update")
        self.assertEqual(code, 1)
        self.assertIn("missing reason for crates/base/src/new.rs", err)
        saved = json.loads(self.allowlist.read_text(encoding="utf-8"))
        self.assertEqual(
            saved["files"]["crates/base/src/new.rs"],
            {"count": 1, "reason": "TODO: classify"},
        )

        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)

        self.write_allowlist(
            {"crates/base/src/new.rs": {"count": 1, "reason": "Win32 console API"}}
        )
        code, _out, err = self.run_gate()
        self.assertEqual(code, 0, err)

    def test_inner_file_attributes_are_detected(self):
        self.write_source(
            "crates/base/tests/integration.rs",
            '#![cfg(unix)]\n\nfn t() {}\n',
        )
        self.write_allowlist({})
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/base/tests/integration.rs:1", err)
        self.assertIn("#![cfg(unix)]", err)

        self.write_allowlist(
            {
                "crates/base/tests/integration.rs": {
                    "count": 1,
                    "reason": "POSIX-shell integration test",
                }
            }
        )
        code, _out, err = self.run_gate()
        self.assertEqual(code, 0, err)

    def test_report_lists_path_count_and_reason(self):
        self.write_source("crates/base/src/a.rs", "#[cfg(unix)]\nfn a() {}\n")
        self.write_allowlist(
            {"crates/base/src/a.rs": {"count": 1, "reason": "macOS notifications"}}
        )
        report = self.root / "out" / "product-cfg.txt"
        code, out, err = self.run_gate("--report", str(report))
        self.assertEqual(code, 0, err)
        self.assertIn(str(report), out)
        text = report.read_text(encoding="utf-8")
        self.assertIn("crates/base/src/a.rs\t1\tmacOS notifications", text)


if __name__ == "__main__":
    unittest.main()
