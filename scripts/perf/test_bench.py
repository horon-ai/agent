#!/usr/bin/env python3
"""Gate over the perf scripts themselves: they parse, and the memory gate holds.

A brand rename can turn a Python identifier into the hyphenated CLI name and
leave the benchmark files unparseable, so this suite pins three things: every
perf Python file parses and compiles with no hyphenated identifier, the shell
gate is syntax clean, and a required metric that is missing or unmeasurable
never reads as a passing zero.
"""

from __future__ import annotations

import ast
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import bench_startup  # noqa: E402

PYTHON_FILES = [
    SCRIPTS / "ab_perf.py",
    SCRIPTS / "analyze_memory_log.py",
    SCRIPTS / "bench_common.py",
    SCRIPTS / "bench_compare.py",
    SCRIPTS / "bench_compile.py",
    SCRIPTS / "bench_loading.py",
    SCRIPTS / "bench_reload_storm.py",
    SCRIPTS / "bench_startup.py",
    SCRIPTS / "bench_tools.py",
    SCRIPTS / "check_structural.py",
    SCRIPTS / "package_workloads.py",
]
GATE = SCRIPTS / "check_memory.sh"

COMPLETE_METRICS = {"proc": {"rss_anon_kb": 1024}, "client": {"allocated_bytes": 2048}}


def run_gate(*args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(GATE), *args],
        capture_output=True,
        text=True,
        check=False,
        env={**os.environ, **(env or {})},
    )


def gate_payload(stdout: str) -> dict:
    for line in stdout.splitlines():
        if line.startswith("HORON_AGENT_MEMGATE_RESULT "):
            return json.loads(line[len("HORON_AGENT_MEMGATE_RESULT ") :])
    raise AssertionError(f"no result line in gate output:\n{stdout}")


def identifier_names(tree: ast.AST) -> list[str]:
    names: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            names.append(node.name)
        elif isinstance(node, ast.Name):
            names.append(node.id)
        elif isinstance(node, ast.Attribute):
            names.append(node.attr)
        elif isinstance(node, ast.arg):
            names.append(node.arg)
        elif isinstance(node, ast.keyword) and node.arg:
            names.append(node.arg)
        elif isinstance(node, ast.Global):
            names.extend(node.names)
    return names


class PerfSourcesParse(unittest.TestCase):
    def test_no_identifier_contains_a_hyphen(self) -> None:
        for path in PYTHON_FILES:
            with self.subTest(path=path.name):
                tree = ast.parse(path.read_text(), filename=str(path))
                offenders = sorted({name for name in identifier_names(tree) if "-" in name})
                self.assertEqual(offenders, [], f"{path.name} still has hyphenated identifiers")

    def test_every_file_compiles(self) -> None:
        for path in PYTHON_FILES:
            with self.subTest(path=path.name):
                result = subprocess.run(
                    [sys.executable, "-m", "py_compile", str(path)],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)


class ShellScriptsParse(unittest.TestCase):
    def test_gate_is_bash_n_clean(self) -> None:
        result = subprocess.run(["bash", "-n", str(GATE)], capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)


class GateMetricContract(unittest.TestCase):
    def test_complete_metrics_pass(self) -> None:
        result = run_gate("--metrics-json", json.dumps(COMPLETE_METRICS))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(gate_payload(result.stdout)["status"], "pass")

    def test_threshold_breach_fails(self) -> None:
        metrics = {"proc": {"rss_anon_kb": 10**9}, "client": {"allocated_bytes": 2048}}
        result = run_gate("--metrics-json", json.dumps(metrics))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(gate_payload(result.stdout)["status"], "fail")

    def test_missing_client_metric_is_measurement_error(self) -> None:
        metrics = {"proc": {"rss_anon_kb": 1024}, "client": {}}
        result = run_gate("--metrics-json", json.dumps(metrics))
        self.assertNotEqual(result.returncode, 0)
        payload = gate_payload(result.stdout)
        self.assertEqual(payload["status"], "measurement_error")
        self.assertIn("measurement_error", result.stdout)

    def test_missing_proc_metric_is_measurement_error(self) -> None:
        metrics = {"proc": {}, "client": {"allocated_bytes": 2048}}
        result = run_gate("--metrics-json", json.dumps(metrics))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(gate_payload(result.stdout)["status"], "measurement_error")

    def test_missing_metric_never_reads_as_zero(self) -> None:
        metrics = {"proc": {"rss_anon_kb": 1024}, "client": {}}
        result = run_gate("--metrics-json", json.dumps(metrics))
        payload = gate_payload(result.stdout)
        self.assertNotIn("idle_live_heap_bytes", payload)
        self.assertNotIn('"status":"pass"', result.stdout)

    def test_null_metric_is_unsupported_not_zero(self) -> None:
        metrics = {
            "proc": {"rss_anon_kb": None, "rss_anon_kb_reason": "no /proc on Darwin"},
            "client": {"allocated_bytes": 2048},
        }
        result = run_gate("--metrics-json", json.dumps(metrics))
        self.assertNotEqual(result.returncode, 0)
        payload = gate_payload(result.stdout)
        self.assertEqual(payload["status"], "unsupported")
        self.assertEqual(payload["metric"], "idle_rss_anon_kb")
        self.assertNotIn("idle_rss_anon_kb", payload)

    def test_metrics_file_uses_last_idle_phase(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "probe.jsonl"
            path.write_text(
                "\n".join(
                    [
                        json.dumps({"phase": "post_connect", "proc": {"rss_anon_kb": 1}, "client": {"allocated_bytes": 1}}),
                        json.dumps({"phase": "idle", "proc": {"rss_anon_kb": 2048}, "client": {"allocated_bytes": 4096}}),
                    ]
                )
                + "\n"
            )
            result = run_gate("--metrics-file", str(path))
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = gate_payload(result.stdout)
        self.assertEqual(payload["idle_rss_anon_kb"], 2048)
        self.assertEqual(payload["idle_live_heap_bytes"], 4096)


class StartupProfileParse(unittest.TestCase):
    LOG = (
        "[2026-11-14 20:00:00.001] [INFO] server starting\n"
        "[2026-11-14 20:00:00.020] [INFO] === Startup Profile (20.4ms total) ===\n"
        "[2026-11-14 20:00:00.020] [INFO]       1.2ms      1.2ms    5.9%  args_parsed                    ███\n"
        "[2026-11-14 20:00:00.020] [INFO]      12.0ms     10.8ms   52.9%  config_loaded                  ███████████████████████████\n"
        "[2026-11-14 20:00:00.020] [INFO]      17.3ms      5.3ms   26.0%  server_new                     █████████████\n"
        "[2026-11-14 20:00:00.020] [INFO]      20.4ms      3.1ms   15.2%  socket_bound                   ████████\n"
        "[2026-11-14 20:00:00.021] [INFO] [session:abc] accepted client\n"
    )

    def test_startup_profile_parsed(self) -> None:
        stages = bench_startup.parse_startup_profile(self.LOG)
        self.assertEqual(
            [stage["stage"] for stage in stages],
            ["args_parsed", "config_loaded", "server_new", "socket_bound"],
        )
        self.assertEqual(
            stages[-1],
            {"stage": "socket_bound", "at_ms": 20.4, "delta_ms": 3.1, "percent": 15.2},
        )

    def test_log_without_a_profile_has_no_stages(self) -> None:
        log = "[2026-11-14 20:00:00.001] [INFO] request took 12.0ms  3.0ms  1.0%  of budget\n"
        self.assertEqual(bench_startup.parse_startup_profile(log), [])

    def test_only_the_last_table_is_reported(self) -> None:
        earlier = self.LOG.replace("20.4ms      3.1ms", "99.0ms      3.1ms")
        stages = bench_startup.parse_startup_profile(earlier + self.LOG)
        self.assertEqual(len(stages), 4)
        self.assertEqual(stages[-1]["at_ms"], 20.4)


@unittest.skipIf(Path("/proc").is_dir(), "this assertion is about platforms without /proc")
class ProcMetricOnNonLinux(unittest.TestCase):
    def test_live_collection_reports_unsupported_for_proc_metric(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            fake_binary = Path(tmp) / "horon-agent"
            fake_binary.write_text(
                "#!/usr/bin/env bash\n"
                "cat <<'JSON'\n"
                '{"process": {"allocator": {"name": "system", "stats_available": false},'
                ' "rss_bytes": 4096}}\n'
                "JSON\n"
            )
            fake_binary.chmod(0o755)
            result = run_gate(env={"HORON_AGENT_MEMGATE_BINARY": str(fake_binary)})

        self.assertNotEqual(result.returncode, 0, "an unmeasurable metric must not pass")
        payload = gate_payload(result.stdout)
        self.assertEqual(payload["status"], "unsupported")
        self.assertEqual(payload["metric"], "idle_rss_anon_kb")
        self.assertIn("no /proc", payload["reason"])
        self.assertNotIn("idle_rss_anon_kb", payload)


if __name__ == "__main__":
    unittest.main()
