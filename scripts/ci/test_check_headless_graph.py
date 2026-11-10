"""Tests for the `horon-agent` headless dependency graph gate.

Each case feeds a fake `cargo metadata` document to the gate, so the tests run
without a workspace and without cargo.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_headless_graph as gate


class HeadlessGraphGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)

    def run_gate(
        self,
        edges: dict[str, list[tuple[str, str]]],
        workspace: tuple[str, ...] = ("horon-agent",),
        extra_packages: tuple[str, ...] = (),
    ) -> tuple[int, str, str]:
        names = {gate.TARGET} | set(workspace) | set(extra_packages)
        for name, deps in edges.items():
            names.add(name)
            names.update(dep_name for dep_name, _kind in deps)
        ids = {name: f"path+file:///repo#{name}@0.1.0" for name in sorted(names)}

        nodes = []
        for name in sorted(names):
            deps = [
                {
                    "name": dep_name,
                    "pkg": ids[dep_name],
                    "dep_kinds": [{"kind": None if kind == "normal" else kind, "target": None}],
                }
                for dep_name, kind in edges.get(name, [])
            ]
            nodes.append(
                {
                    "id": ids[name],
                    "deps": deps,
                    "dependencies": [dep["pkg"] for dep in deps],
                }
            )

        metadata = {
            "packages": [{"id": ids[name], "name": name} for name in sorted(names)],
            "workspace_members": [ids[name] for name in workspace],
            "resolve": {"nodes": nodes, "root": None},
        }
        path = self.root / "metadata.json"
        path.write_text(json.dumps(metadata), encoding="utf-8")

        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gate.main(["--metadata", str(path)])
        return code, out.getvalue(), err.getvalue()

    def test_clean_graph_passes(self):
        code, out, err = self.run_gate({"horon-agent": [("x", "normal")]})
        self.assertEqual(code, 0, err)
        self.assertEqual(
            out.strip(),
            "horon-agent: no ratatui, crossterm, horon-tui, horon-agent-tui* (2 packages checked)",
        )

    def test_transitive_crossterm_fails_with_path(self):
        code, out, err = self.run_gate(
            {
                "horon-agent": [("x", "normal")],
                "x": [("crossterm", "normal")],
            }
        )
        self.assertEqual(code, 1)
        self.assertIn("horon-agent -> x -> crossterm", err)
        self.assertNotIn("no ratatui", out)

    def test_horon_agent_tui_dependency_fails_with_name(self):
        code, _out, err = self.run_gate(
            {"horon-agent": [("horon-agent-tui-foo", "normal")]}
        )
        self.assertEqual(code, 1)
        self.assertIn("horon-agent -> horon-agent-tui-foo", err)

    def test_workspace_member_unreachable_from_target_passes(self):
        # crates/tui will be a workspace member that depends on crossterm; only
        # reachability from horon-agent decides the gate.
        code, out, err = self.run_gate(
            {
                "horon-agent": [("x", "normal")],
                "horon-tui": [("crossterm", "normal"), ("ratatui", "normal")],
            },
            workspace=("horon-agent", "horon-tui"),
        )
        self.assertEqual(code, 0, err)
        self.assertIn("(2 packages checked)", out)

    def test_workspace_member_dev_dependency_fails(self):
        code, _out, err = self.run_gate({"horon-agent": [("ratatui", "dev")]})
        self.assertEqual(code, 1)
        self.assertIn("horon-agent -> ratatui", err)

    def test_build_dependency_fails(self):
        code, _out, err = self.run_gate(
            {
                "horon-agent": [("build-helper", "build")],
                "build-helper": [("horon-tui", "normal")],
            }
        )
        self.assertEqual(code, 1)
        self.assertIn("horon-agent -> build-helper -> horon-tui", err)

    def test_missing_target_package_fails(self):
        names = {"something-else": "path+file:///repo#something-else@0.1.0"}
        metadata = {
            "packages": [{"id": names["something-else"], "name": "something-else"}],
            "workspace_members": [names["something-else"]],
            "resolve": {"nodes": [{"id": names["something-else"], "deps": []}], "root": None},
        }
        path = self.root / "metadata.json"
        path.write_text(json.dumps(metadata), encoding="utf-8")

        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gate.main(["--metadata", str(path)])
        self.assertEqual(code, 1)
        self.assertIn("expected exactly one package named `horon-agent`", err.getvalue())


if __name__ == "__main__":
    unittest.main()