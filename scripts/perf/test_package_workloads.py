#!/usr/bin/env python3
"""Tests for scripts/perf/package_workloads.py, limited to what can be wrong.

No binary, no mock server and no network: every case below runs on tmp files or
on plain values, and pins the three things a reader of the package cells relies
on:

* the installed fixture has no observation subscription and names exactly the
  binary this run was given, so `installed` measures installation alone;
* a scenario that must not call a tool reports `failed` when one ran, and a
  scenario that must call one reports `failed` when the round trip never
  happened — neither may read as a clean row;
* the baseline comparison exists only where both sides measured the cell, and
  the size facts are the bytes actually on disk.

Run with `python3 -m unittest scripts/perf/test_package_workloads.py`.
"""

from __future__ import annotations

import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))


def load(name: str, file: Path):
    spec = importlib.util.spec_from_file_location(name, file)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


pw = load("package_workloads", SCRIPTS / "package_workloads.py")

SHIPPED = """schema_version = 1
id = "hello"
version = "0.1.0"
name = "Hello package example"
protocol = "1.0"
events = ["turn.start", "turn.end"]

[[tools]]
name = "greet"
description = "Greet one name."

[targets]
x86_64-unknown-linux-gnu = "bin/x86_64-unknown-linux-gnu/horon-package-example"
"""


class StageTest(unittest.TestCase):
    """The staged layout is what a run installs, so it is asserted literally.

    The installer refuses a root that holds both declarations, so the two kinds
    are staged apart: an extension root carries only `extension.toml` and its
    executable, and a plugin root carries `plugin.toml`, its resources and the
    bundled extension under `extension/`.
    """

    def source(self, root: Path) -> Path:
        source = root / "layouts"
        (source / "resources" / "rules").mkdir(parents=True)
        (source / "resources" / "rules" / "greeting.md").write_text("A rule\n", encoding="utf-8")
        (source / "resources" / "mcp").mkdir(parents=True)
        (source / "resources" / "mcp" / "servers.json").write_text("{}", encoding="utf-8")
        (source / "extension.toml").write_text(SHIPPED, encoding="utf-8")
        (source / "plugin.toml").write_text(
            'schema_version = 1\nid = "hello"\nextension = "extension/extension.toml"\n'
            '\n[resources]\nrules = ["resources/rules/greeting.md"]\n',
            encoding="utf-8",
        )
        return source

    def binary(self, root: Path) -> Path:
        binary = root / "built-example"
        binary.write_bytes(b"\x7fELF-not-really")
        return binary

    def stage(self, layout: str = "extension", keep_events: bool = True) -> tuple[Path, Path]:
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        source = self.source(root)
        staged = pw.stage_package(source, self.binary(root), root / "staged", layout, keep_events=keep_events)
        return staged, root

    def test_the_installed_scenario_may_drop_the_subscription(self) -> None:
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        staged = pw.stage_package(self.source(root), self.binary(root), root / "staged", "extension",
                                  keep_events=False)
        self.assertNotIn("events", (staged / "extension.toml").read_text(encoding="utf-8"))

    def test_the_called_scenario_keeps_the_shipped_declaration_events(self) -> None:
        # The author SDK refuses a declaration whose events disagree with the
        # code, so the copy a child is started from keeps them.
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        staged = pw.stage_package(self.source(root), self.binary(root), root / "staged", "extension",
                                  keep_events=True)
        text = (staged / "extension.toml").read_text(encoding="utf-8")
        self.assertIn('events = ["turn.start", "turn.end"]', text)
        self.assertIn('id = "hello"', text)

    def test_the_extension_root_carries_only_its_own_declaration(self) -> None:
        staged, _ = self.stage("extension", keep_events=False)
        text = (staged / "extension.toml").read_text(encoding="utf-8")
        self.assertNotIn("events", text, "an observing package is started by a turn, which this cell isolates")
        self.assertIn('id = "hello"', text)
        self.assertFalse((staged / "plugin.toml").exists(), "two declarations in one root are refused")
        self.assertFalse((staged / "resources").exists(), "an extension layout bundles no resources")
        self.assertTrue((staged / "bin" / pw.binary_name()).is_file())
        if not pw.bc.IS_WINDOWS:
            self.assertTrue(os.access(staged / "bin" / pw.binary_name(), os.X_OK), "the copy is executable")

    def test_the_target_table_names_exactly_the_binary_this_run_was_given(self) -> None:
        staged, _ = self.stage("extension")
        text = (staged / "extension.toml").read_text(encoding="utf-8")
        target = pw.host_target()
        self.assertIn(f'{target} = "bin/{pw.binary_name()}"', text)
        self.assertNotIn("x86_64-unknown-linux-gnu\"", text.replace(target, ""), "no other target key survives")

    def test_a_plugin_root_keeps_its_resources_and_bundles_the_extension(self) -> None:
        staged, _ = self.stage("plugin")
        self.assertFalse((staged / "extension.toml").exists(), "the plugin's own root holds plugin.toml only")
        self.assertTrue((staged / "plugin.toml").is_file())
        self.assertTrue((staged / "resources" / "rules" / "greeting.md").is_file())
        self.assertTrue((staged / "resources" / "mcp" / "servers.json").is_file())
        nested = staged / "extension" / "extension.toml"
        self.assertTrue(nested.is_file(), "plugin.toml names extension/extension.toml")
        self.assertIn('events = ["turn.start", "turn.end"]', nested.read_text(encoding="utf-8"))
        self.assertTrue((staged / "extension" / "bin" / pw.binary_name()).is_file())

    def test_a_scenario_that_installs_nothing_reports_no_layout_bytes(self) -> None:
        facts = pw.size_facts("/bin/agent", "/bin/example", "/tmp/home", None, Path("/tmp/plugin"))
        self.assertIsNone(facts["package_extension_bytes"], "`empty` installs no copy to measure")

    def test_the_plugin_payload_does_not_double_the_extension_payload(self) -> None:
        extension_root, _ = self.stage("extension")
        plugin_root, _ = self.stage("plugin")
        extension_bytes = pw.directory_facts(extension_root)["bytes"]
        plugin_bytes = pw.directory_facts(plugin_root)["bytes"]
        resources_bytes = pw.directory_facts(plugin_root / "resources")["bytes"]
        self.assertGreater(plugin_bytes, extension_bytes, "the plugin adds its resources and declaration")
        # The bundled extension adds exactly its declaration to the extension
        # payload; anything more means the executable was copied twice.
        bundled_bytes = pw.directory_facts(plugin_root / "extension")["bytes"]
        declaration_bytes = (plugin_root / "extension" / "extension.toml").stat().st_size
        self.assertEqual(bundled_bytes, extension_bytes - (extension_root / "extension.toml").stat().st_size
                         + declaration_bytes)
        self.assertEqual(
            plugin_bytes,
            bundled_bytes + resources_bytes + (plugin_root / "plugin.toml").stat().st_size,
            "the plugin root holds the bundled extension, its resources and its own declaration, once each",
        )

    def test_a_layout_without_a_declaration_is_refused(self) -> None:
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        source = root / "empty"
        source.mkdir()
        binary = root / "built-example"
        binary.write_bytes(b"x")
        with self.assertRaises(SystemExit):
            pw.stage_package(source, binary, root / "staged", "extension")


class HostTargetTest(unittest.TestCase):
    def test_machine_names_map_to_the_declared_target_keys(self) -> None:
        # Both inputs the function reads are patched, so each case is a literal
        # expectation rather than a re-computation of the same expression.
        for machine, expected in (
            ("arm64", "aarch64-unknown-linux-gnu"),
            ("aarch64", "aarch64-unknown-linux-gnu"),
            ("x86_64", "x86_64-unknown-linux-gnu"),
            ("AMD64", "x86_64-unknown-linux-gnu"),
        ):
            with mock.patch.object(pw.platform, "machine", return_value=machine), \
                    mock.patch.object(pw.sys, "platform", "linux"):
                self.assertEqual(pw.host_target(), expected, machine)
        with mock.patch.object(pw.platform, "machine", return_value="arm64"), \
                mock.patch.object(pw.sys, "platform", "darwin"):
            self.assertEqual(pw.host_target(), "aarch64-apple-darwin")
        with mock.patch.object(pw.platform, "machine", return_value="x86_64"), \
                mock.patch.object(pw.sys, "platform", "win32"):
            self.assertEqual(pw.host_target(), "x86_64-pc-windows-msvc")

    def test_an_unknown_platform_is_refused_rather_than_guessed(self) -> None:
        with mock.patch.object(pw.sys, "platform", "aix"):
            with self.assertRaises(SystemExit):
                pw.host_target()


class SizeTest(unittest.TestCase):
    def test_a_missing_area_reads_as_zero(self) -> None:
        self.assertEqual(pw.directory_facts(Path(tempfile.mkdtemp()) / "absent"), {"bytes": 0, "files": 0})

    def test_the_area_is_summed_per_file(self) -> None:
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        (root / "store" / "digest").mkdir(parents=True)
        (root / "store" / "digest" / "a.bin").write_bytes(b"12345")
        (root / "installed.json").write_bytes(b"{}")
        self.assertEqual(pw.directory_facts(root), {"bytes": 7, "files": 2})

    def test_the_layout_payload_is_the_directory_it_lives_in(self) -> None:
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        (root / "extension.toml").write_bytes(b"abc")
        (root / "bin").mkdir()
        (root / "bin" / "probe").write_bytes(b"12345")
        self.assertEqual(pw.payload_unpacked_bytes(root / "extension.toml"), 8)
        self.assertIsNone(pw.payload_unpacked_bytes(root / "absent.toml"))


def run(**overrides) -> dict:
    base = {
        "ms": 100.0,
        "peak_rss_bytes": 4096,
        "first_request_ms": 40.0,
        "tool_round_trip": False,
        "sentinel_seen": False,
        "last_tool_result": None,
        "requests": 1,
        "tools": [],
        "error": None,
    }
    base.update(overrides)
    return base


class SummarizeTest(unittest.TestCase):
    def test_a_scenario_that_forbids_a_call_fails_when_one_ran(self) -> None:
        row = pw.summarize([run(tool_round_trip=True)], expect_call=False)
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["failure"], "a tool call ran in a scenario that forbids one")
        self.assertTrue(row["tool_round_trip"])

    def test_a_calling_scenario_fails_when_the_call_never_came_back(self) -> None:
        row = pw.summarize([run(tool_round_trip=None, error="exit 1: boom")], expect_call=True)
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["failure"], "exit 1: boom")
        self.assertIsNone(row["ms"], "a failed run reports no time")

    def test_a_calling_scenario_that_only_issued_the_call_is_not_a_round_trip(self) -> None:
        row = pw.summarize([run(tool_round_trip=False)], expect_call=True)
        self.assertEqual(row["failure"], "the package call never came back")

    def test_a_clean_pair_reports_the_median_and_the_earliest_request(self) -> None:
        row = pw.summarize([run(ms=100.0), run(ms=120.0), run(ms=110.0)], expect_call=False)
        self.assertEqual(row["status"], "ok")
        self.assertEqual(row["ms"], 110.0)
        self.assertEqual(row["summary"]["min"], 100.0)
        self.assertEqual(row["summary"]["max"], 120.0)
        self.assertEqual(row["first_request_ms"]["samples"], 3)
        self.assertEqual(row["peak_rss_bytes"], 4096)


class DiagnosticTest(unittest.TestCase):
    """A failed round trip names the answer the host actually gave."""

    def test_the_row_keeps_the_last_tool_result_of_a_failed_call(self) -> None:
        row = pw.summarize(
            [run(tool_round_trip=False, last_tool_result="package hello did not answer the call: boom")],
            expect_call=True,
        )
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["last_tool_result"], "package hello did not answer the call: boom")

    def test_the_markdown_failure_line_quotes_the_answer(self) -> None:
        report = {
            "results": [
                {
                    "tool": "candidate",
                    "scenario": "called",
                    "ms": None,
                    "status": "failed",
                    "failure": "the package call never came back",
                    "last_tool_result": "package hello did not answer the call: undeclared event",
                }
            ],
            "skipped": [],
        }
        text = pw.markdown(report)
        self.assertIn(
            "- `candidate` `called`: the package call never came back — "
            "last tool result: `package hello did not answer the call: undeclared event`",
            text,
        )

    def test_a_clean_report_carries_no_failure_section(self) -> None:
        report = {"results": [{"tool": "candidate", "scenario": "called", "ms": 10.0, "status": "ok"}],
                  "skipped": []}
        self.assertNotIn("**Failures**", pw.markdown(report))


class ComparisonTest(unittest.TestCase):
    def test_a_ratio_needs_both_sides(self) -> None:
        self.assertIsNone(pw.comparison(None, {"ms": 100.0}))
        self.assertIsNone(pw.comparison({"ms": 100.0}, None))
        self.assertIsNone(pw.comparison({"ms": None}, {"ms": 100.0}))

    def test_the_ratio_is_candidate_over_baseline(self) -> None:
        self.assertEqual(
            pw.comparison({"ms": 100.0}, {"ms": 110.0}),
            {"baseline_ms": 100.0, "candidate_ms": 110.0, "ratio": 1.1},
        )


class ResultExcerptTest(unittest.TestCase):
    def test_the_kept_answer_is_the_last_one_and_is_bounded(self) -> None:
        long = "x" * (pw.RESULT_EXCERPT_CHARS + 50)
        requests = [
            {"result_excerpt": "first"},
            {"result_excerpt": long},
            {"plan": "package"},
        ]
        kept = pw.last_result(requests)
        self.assertEqual(len(kept), 200, "the row quotes at most 200 characters")
        self.assertTrue(kept.startswith("x" * 10))

    def test_no_tool_result_is_no_snippet(self) -> None:
        self.assertIsNone(pw.last_result([{"plan": "package"}]))


class FirstRequestTest(unittest.TestCase):
    def test_the_earliest_request_stamp_is_the_startup_number(self) -> None:
        self.assertEqual(pw.first_request_ms([{"ts": 100.5}, {"ts": 100.25}], 100.0), 250.0)

    def test_no_request_is_no_number(self) -> None:
        self.assertIsNone(pw.first_request_ms([{"endpoint": "/v1/chat"}], 100.0))


class MarkdownTest(unittest.TestCase):
    def report(self) -> dict:
        rows = [
            {
                "tool": "candidate",
                "scenario": "called",
                "ms": 250.0,
                "summary": {"median": 250.0, "min": 240.0, "max": 260.0, "samples": 2},
                "first_request_ms": {"median": 40.0},
                "peak_rss_bytes": 8192,
                "tool_round_trip": True,
                "tools": ["ext__hello__greet"],
                "status": "ok",
                "comparison": {"baseline_ms": 200.0, "candidate_ms": 250.0, "ratio": 1.25},
                "installed_area_bytes": 4096,
                "installed_area_files": 3,
                "package_binary_bytes": 2048,
                "agent_binary_bytes": 81920,
            }
        ]
        return {"results": rows, "skipped": [{"tool": "baseline", "scenario": "called", "reason": "no package support"}]}

    def test_the_table_carries_the_ratio_the_round_trip_and_the_sizes(self) -> None:
        text = pw.markdown(self.report())
        self.assertIn("1.250×", text)
        self.assertIn("ext__hello__greet", text)
        self.assertIn("4.0 KB", text)
        self.assertIn("installed size", text.lower())
        self.assertIn("skipped `baseline` `called`: no package support", text)

    def test_a_failed_row_is_named_as_failed(self) -> None:
        report = self.report()
        report["results"][0]["status"] = "failed"
        report["results"][0]["comparison"] = None
        text = pw.markdown(report)
        self.assertIn("failed", text)
        self.assertIn("| – |", text, "a cell with no baseline is an em dash, never a zero")


class OrderTest(unittest.TestCase):
    """The two builds interleave inside every repetition, not one after the other."""

    def prepared(self, label: str) -> object:
        return pw.Prepared(
            label=label,
            binary=f"/bin/{label}",
            package_binary="/bin/example",
            root=Path("/tmp"),
            layouts={scenario: Path("/tmp/ext") for scenario in pw.SCENARIOS},
            plugin_root=Path("/tmp/plugin"),
            homes={scenario: "/tmp/home" for scenario in pw.SCENARIOS},
            envs={scenario: {} for scenario in pw.SCENARIOS},
            cwds={scenario: "/tmp" for scenario in pw.SCENARIOS},
            skipped={},
        )

    def test_a_scenario_spends_every_repetition_on_both_builds_before_the_next(self) -> None:
        order: list[tuple[str, str]] = []

        def fake_turn(binary, env, cwd, prompt, tools, mock, timeout):
            order.append((binary, prompt))
            return {
                "ms": 10.0,
                "peak_rss_bytes": 1,
                "first_request_ms": 1.0,
                "tool_round_trip": False,
                "sentinel_seen": False,
                "last_tool_result": None,
                "requests": 1,
                "tools": [],
                "error": None,
            }

        with mock.patch.object(pw, "run_turn", fake_turn):
            measured = pw.measure_scenarios([self.prepared("a"), self.prepared("b")], 2, 1.0, None)

        self.assertEqual(len(order), len(pw.SCENARIOS) * 2 * 2)
        empty_prompt = pw.PLAIN_PROMPT
        first_four = order[:4]
        self.assertEqual(
            first_four,
            [("/bin/a", empty_prompt), ("/bin/b", empty_prompt)] * 2,
            "both builds run the same scenario before the next scenario starts",
        )
        self.assertEqual(
            order[4:6],
            [("/bin/a", empty_prompt), ("/bin/b", empty_prompt)],
            "the second scenario starts only after the first one is finished",
        )
        self.assertEqual(len(measured["a"]), len(pw.SCENARIOS))
        self.assertEqual(len(measured["b"]), len(pw.SCENARIOS))


class MainTest(unittest.TestCase):
    def test_a_missing_layout_is_an_error_rather_than_an_empty_report(self) -> None:
        missing = Path(tempfile.mkdtemp(prefix="hpkg-test-")) / "absent"
        code = pw.main(
            [
                "--binary", sys.executable,
                "--package-binary", sys.executable,
                "--package-source", str(missing),
            ]
        )
        self.assertEqual(code, 2)

    def report(self, statuses: dict[str, str]) -> dict:
        rows = []
        for label in ("baseline", "candidate"):
            for scenario in pw.SCENARIOS:
                status = statuses.get(f"{label}/{scenario}", "ok")
                rows.append({"tool": label, "scenario": scenario, "status": status, "ms": 10.0})
        return {"results": rows, "skipped": [], "host": {}, "sizes": {}, "runs_per_scenario": 1}

    def run_main(self, statuses: dict[str, str]) -> int:
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        source = root / "layouts"
        source.mkdir()
        (source / "extension.toml").write_text(SHIPPED, encoding="utf-8")
        binary = root / "example"
        binary.write_bytes(b"x")
        with mock.patch.object(pw, "measure", return_value=self.report(statuses)):
            return pw.main(
                [
                    "--binary", sys.executable,
                    "--package-binary", str(binary),
                    "--package-source", str(source),
                ]
            )

    def test_a_clean_candidate_exits_zero(self) -> None:
        self.assertEqual(self.run_main({}), 0)

    def test_a_failed_candidate_cell_exits_one_even_after_the_report_is_written(self) -> None:
        self.assertEqual(self.run_main({"candidate/called": "failed"}), 1)

    def test_a_missing_candidate_cell_exits_one(self) -> None:
        self.assertEqual(self.run_main({"candidate/installed": "missing"}), 1)

    def test_a_skipped_baseline_does_not_fail_the_run(self) -> None:
        self.assertEqual(self.run_main({"baseline/called": "failed", "baseline/installed": "failed"}), 0)

    def test_a_missing_package_executable_is_an_error_rather_than_a_zero_sized_package(self) -> None:
        root = Path(tempfile.mkdtemp(prefix="hpkg-test-"))
        source = root / "layouts"
        source.mkdir()
        (source / "extension.toml").write_text(SHIPPED, encoding="utf-8")
        code = pw.main(
            [
                "--binary", sys.executable,
                "--package-binary", str(root / "absent-binary"),
                "--package-source", str(source),
            ]
        )
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
