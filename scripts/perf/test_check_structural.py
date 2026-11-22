#!/usr/bin/env python3
"""Contract tests for scripts/perf/check_structural.py.

Run with `python3 -m unittest scripts/perf/test_check_structural.py`.

The failure cases carry the weight here. A gate that reports "OK" when it
measured nothing, or that passes a queue which accepted a flood without limit,
is worse than no gate: it certifies the shape it exists to catch. So this suite
pins the violation path, the missing-measurement path, the completeness of the
bounds file and of the report template, and the workflow trigger that decides
whether any of it runs on a `dev` pull request at all.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent.parent
GATE_FILE = SCRIPTS / "check_structural.py"
BUDGET_FILE = SCRIPTS / "structural_budget.json"
TEMPLATE_FILE = SCRIPTS / "report_template.json"
WORKFLOW_FILE = REPO_ROOT / ".github" / "workflows" / "structural-perf.yml"


def load_gate():
    spec = importlib.util.spec_from_file_location("check_structural", GATE_FILE)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


gate = load_gate()

HEALTHY_LINES = [
    "STRUCTURAL_PERF section=queue source=agent-runtime burst=64 queue_depth_high_water=64 "
    "queue_bytes_high_water=768 queue_bytes_buffered=0",
    "STRUCTURAL_PERF section=queue source=app-core burst=16 queue_depth_high_water=16 queue_bytes_high_water=192",
    "STRUCTURAL_PERF section=pending pending_requests=1 pending_requests_high_water=1",
    "STRUCTURAL_PERF section=pending pending_after_reply=0 pending_requests_high_water=1",
    "STRUCTURAL_PERF section=page events=1000 page_limit=20 warm_replayed=0 warm_flattened=20 "
    "warm_index_builds=0 warm_read_bytes=8192 log_bytes=262144",
    "STRUCTURAL_PERF section=page events=100000 page_limit=20 warm_replayed=0 warm_flattened=20 "
    "warm_index_builds=0 warm_read_bytes=9000 log_bytes=26214400",
    "STRUCTURAL_PERF section=hash buffer_bytes=65536 small_request=65536 large_request=65536 "
    "large_source_bytes=209715200",
    "STRUCTURAL_PERF section=frame source=bridge max_frame_bytes=67108864 accepted_frame_bytes=67108864 "
    "rejected_frame_bytes=67108865",
    "STRUCTURAL_PERF section=frame source=sdk max_frame_bytes=67108864 accepted_frame_bytes=67108864 "
    "rejected_frame_bytes=67108865",
    "STRUCTURAL_PERF section=soak cycles=20 fd_retention=0 thread_retention=-1 non_returning=0",
    "STRUCTURAL_PERF section=context source=app-core spill_kept_bytes=39349 read_stub_bytes=158 aging_batches=1",
    "STRUCTURAL_PERF section=irc source=irc-core mailbox_high_water=100 mailbox_dropped=900 "
    "broadcast_body_allocations=1 waiters_live_after_churn=0 broadcast_inflight_high_water=16 flood=1000 "
    "broadcast_targets=64",
    "STRUCTURAL_PERF section=skill source=skill-catalog skills=200 skill_catalog_file_reads=200 "
    "skill_catalog_bytes=40000",
    "STRUCTURAL_PERF section=skill source=skill-overlay skill_warmup_file_reads=50 "
    "skill_unchanged_request_file_reads=0 skill_unchanged_request_dir_reads=0",
    "STRUCTURAL_PERF section=mcp_config source=mcp-config mcp_config_warmup_reads=3 mcp_config_user_rereads=0",
    "STRUCTURAL_PERF section=hooks source=request-messages request_messages_in_probe=8 hook_process_spawns=0 "
    "request_message_copies=0",
    "STRUCTURAL_PERF section=prefix source=tool-catalog tools_bytes=809 tool_count=8 eager_count=8 "
    "per_tool=read:100,write:100,edit:100,apply_patch:100,bash:100,ls:100,agentgrep:100,toolbox:100",
    "STRUCTURAL_PERF section=prompt static_bytes=1322 "
    "static_sha256=380df8ba02b5de4cbdcc3ee203743df5887c004ca9591d3620a53a9593372936 static_tokens=305 "
    "index_bytes=585 index_clipped_rows=1 warm_source_reads=8 unchanged_request_source_reads=0",
    "STRUCTURAL_PERF section=session_context git_spawns_outside_repo=0 git_spawns_in_repo=1 "
    "gpu_probe_spawns_without_display=0 gpu_reported=0",
    "STRUCTURAL_PERF section=tool_dispatch source=app-core dispatches=200 small_allocs=2000 "
    "small_alloc_bytes=100000 large_allocs=2000 large_alloc_bytes=200000",
    "STRUCTURAL_PERF section=packages source=empty turns=2 packages=0 area_reads=0 dir_scans=0 "
    "area_writes=0 snapshot_builds=0 child_starts=0",
    "STRUCTURAL_PERF section=packages source=turn-warm turns=1 packages=1 area_reads=2 dir_scans=0 "
    "area_writes=3 snapshot_builds=1 child_starts=0",
    "STRUCTURAL_PERF section=packages source=turn-unchanged turns=1 packages=1 area_reads=0 dir_scans=0 "
    "area_writes=0 snapshot_builds=0 child_starts=0",
    "STRUCTURAL_PERF section=packages source=call calls=1 area_reads=3 child_starts=1",
    "STRUCTURAL_PERF section=packages source=call-repeat calls=1 child_starts=0",
]


def replace_line(prefix, replacement, marker=None):
    """The healthy log with one line swapped, so exactly one bound is violated."""
    return [
        replacement if line.startswith(prefix) and (marker is None or marker in line) else line
        for line in HEALTHY_LINES
    ]


class GateRun:
    def __init__(self, code, stdout, stderr):
        self.code = code
        self.stdout = stdout
        self.stderr = stderr

    @property
    def result(self):
        for line in self.stdout.splitlines():
            if line.startswith(gate.RESULT_PREFIX):
                return json.loads(line[len(gate.RESULT_PREFIX) :])
        raise AssertionError(f"no result line in output:\n{self.stdout}")


def run_gate(lines, extra_args=()):
    with tempfile.NamedTemporaryFile("w", suffix=".log", delete=False, encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
        path = handle.name
    stdout, stderr = io.StringIO(), io.StringIO()
    try:
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = gate.main(["--read", path, *extra_args])
    finally:
        os.unlink(path)
    return GateRun(code, stdout.getvalue(), stderr.getvalue())


class HealthyRunTest(unittest.TestCase):
    def test_a_bounded_run_passes_every_section(self):
        run = run_gate(HEALTHY_LINES)
        self.assertEqual(run.code, 0, run.stderr)
        result = run.result
        self.assertEqual(result["verdict"], "pass")
        self.assertEqual(result["failed"], 0)
        self.assertEqual(result["missing"], [])
        self.assertGreater(result["passed"], 0, "a pass must have checked something")
        self.assertEqual({check["section"].split("/")[0] for check in result["checks"]}, set(gate.REQUIRED_SECTIONS))


class ViolationTest(unittest.TestCase):
    """Each case is one deliberate defect, and the failure must name its counter."""

    def assert_fails_naming(self, lines, counter, extra_args=()):
        run = run_gate(lines, extra_args)
        self.assertEqual(run.code, 1, f"a violated bound must fail the gate:\n{run.stdout}\n{run.stderr}")
        result = run.result
        self.assertEqual(result["verdict"], "fail")
        named = {check["counter"] for check in result["checks"] if not check["ok"]}
        self.assertIn(counter, named, f"the failure must name {counter}, named {sorted(named)}")
        self.assertIn(counter, run.stderr, "the stderr report must name the counter a human has to fix")

    def test_an_unbounded_queue_fails_the_gate_naming_the_depth_counter(self):
        # The test double accepts a flood instead of a burst: 1,000 entries where
        # the queue is allowed 64. This is the plan's stated failure scenario.
        lines = replace_line(
            "STRUCTURAL_PERF section=queue source=agent-runtime",
            "STRUCTURAL_PERF section=queue source=agent-runtime burst=16 queue_depth_high_water=1000 "
            "queue_bytes_high_water=768 queue_bytes_buffered=0",
        )
        self.assert_fails_naming(lines, "queue_depth_high_water")

    def test_a_queue_deeper_than_its_own_burst_fails_even_under_the_ceiling(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=queue source=app-core",
            "STRUCTURAL_PERF section=queue source=app-core burst=16 queue_depth_high_water=48 "
            "queue_bytes_high_water=192",
        )
        self.assert_fails_naming(lines, "queue_depth_high_water")

    def test_a_queue_that_never_records_fails_rather_than_passing_on_a_zero(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=queue source=app-core",
            "STRUCTURAL_PERF section=queue source=app-core burst=16 queue_depth_high_water=0 "
            "queue_bytes_high_water=0",
        )
        self.assert_fails_naming(lines, "queue_depth_high_water")

    def test_buffered_bytes_after_commit_fail_the_gate(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=queue source=agent-runtime",
            "STRUCTURAL_PERF section=queue source=agent-runtime burst=64 queue_depth_high_water=64 "
            "queue_bytes_high_water=768 queue_bytes_buffered=512",
        )
        self.assert_fails_naming(lines, "queue_bytes_buffered")

    def test_an_outstanding_request_that_never_drains_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=pending",
            "STRUCTURAL_PERF section=pending pending_after_reply=3 pending_requests_high_water=3",
            marker="pending_after_reply",
        )
        self.assert_fails_naming(lines, "pending_after_reply")

    def test_a_page_that_replays_the_session_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=page events=100000",
            "STRUCTURAL_PERF section=page events=100000 page_limit=20 warm_replayed=99980 warm_flattened=20 "
            "warm_index_builds=0 warm_read_bytes=9000 log_bytes=26214400",
        )
        self.assert_fails_naming(lines, "warm_replayed")

    def test_a_page_that_rebuilds_its_index_when_warm_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=page events=1000",
            "STRUCTURAL_PERF section=page events=1000 page_limit=20 warm_replayed=0 warm_flattened=20 "
            "warm_index_builds=1 warm_read_bytes=8192 log_bytes=262144",
        )
        self.assert_fails_naming(lines, "warm_index_builds")

    def test_a_page_that_reads_most_of_its_log_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=page events=1000",
            "STRUCTURAL_PERF section=page events=1000 page_limit=20 warm_replayed=0 warm_flattened=20 "
            "warm_index_builds=0 warm_read_bytes=262144 log_bytes=262144",
        )
        self.assert_fails_naming(lines, "warm_read_bytes")

    def test_page_cost_that_follows_the_session_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=page events=100000",
            "STRUCTURAL_PERF section=page events=100000 page_limit=20 warm_replayed=0 warm_flattened=20 "
            "warm_index_builds=0 warm_read_bytes=81920 log_bytes=26214400",
        )
        self.assert_fails_naming(lines, "warm_read_bytes_growth")

    def test_a_hash_buffer_that_grows_with_the_source_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=hash",
            "STRUCTURAL_PERF section=hash buffer_bytes=65536 small_request=65536 large_request=209715200 "
            "large_source_bytes=209715200",
        )
        self.assert_fails_naming(lines, "large_request")

    def test_a_fixed_buffer_proven_only_on_a_tiny_source_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=hash",
            "STRUCTURAL_PERF section=hash buffer_bytes=65536 small_request=65536 large_request=65536 "
            "large_source_bytes=1024",
        )
        self.assert_fails_naming(lines, "large_source_bytes")

    def test_a_soak_that_leaks_descriptors_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=soak",
            "STRUCTURAL_PERF section=soak cycles=20 fd_retention=12 thread_retention=-1 non_returning=0",
        )
        self.assert_fails_naming(lines, "fd_retention")

    def test_a_soak_that_leaks_threads_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=soak",
            "STRUCTURAL_PERF section=soak cycles=20 fd_retention=0 thread_retention=9 non_returning=0",
        )
        self.assert_fails_naming(lines, "thread_retention")

    def test_a_soak_that_reports_a_non_returning_counter_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=soak",
            "STRUCTURAL_PERF section=soak cycles=20 fd_retention=0 thread_retention=-1 non_returning=1",
        )
        self.assert_fails_naming(lines, "non_returning")

    def test_a_soak_that_ran_the_wrong_length_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=soak",
            "STRUCTURAL_PERF section=soak cycles=200 fd_retention=0 thread_retention=-1 non_returning=0",
        )
        self.assert_fails_naming(lines, "cycles")

    def test_a_large_output_kept_past_the_condense_budget_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=context",
            "STRUCTURAL_PERF section=context source=app-core spill_kept_bytes=1048576 read_stub_bytes=158 "
            "aging_batches=1",
        )
        self.assert_fails_naming(lines, "spill_kept_bytes")

    def test_a_repeated_read_that_resends_the_file_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=context",
            "STRUCTURAL_PERF section=context source=app-core spill_kept_bytes=39349 read_stub_bytes=26000 "
            "aging_batches=1",
        )
        self.assert_fails_naming(lines, "read_stub_bytes")

    def test_aging_that_rewrites_the_prefix_every_request_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=context",
            "STRUCTURAL_PERF section=context source=app-core spill_kept_bytes=39349 read_stub_bytes=158 "
            "aging_batches=2",
        )
        self.assert_fails_naming(lines, "aging_batches")

    def test_aging_that_never_runs_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=context",
            "STRUCTURAL_PERF section=context source=app-core spill_kept_bytes=39349 read_stub_bytes=158 "
            "aging_batches=0",
        )
        self.assert_fails_naming(lines, "aging_batches")

    def test_a_mailbox_that_keeps_the_flood_fails(self):
        # The cap was reported at 101 after a 1,000-message flood: the bus
        # buffered past its own ceiling and kept the messages it should drop.
        lines = replace_line(
            "STRUCTURAL_PERF section=irc",
            "STRUCTURAL_PERF section=irc source=irc-core mailbox_high_water=101 mailbox_dropped=899 "
            "broadcast_body_allocations=1 waiters_live_after_churn=0 broadcast_inflight_high_water=16 flood=1000 "
            "broadcast_targets=64",
        )
        self.assert_fails_naming(lines, "mailbox_high_water")

    def test_an_unchanged_request_that_reads_a_skill_file_fails(self):
        # One SKILL.md read again although nothing changed: the overlay cache
        # fell back to rescanning the workspace on every request.
        lines = replace_line(
            "STRUCTURAL_PERF section=skill source=skill-overlay",
            "STRUCTURAL_PERF section=skill source=skill-overlay skill_warmup_file_reads=50 "
            "skill_unchanged_request_file_reads=1 skill_unchanged_request_dir_reads=0",
        )
        self.assert_fails_naming(lines, "skill_unchanged_request_file_reads")

    def test_a_catalog_that_reads_whole_bodies_fails(self):
        # 32 KiB per skill: the listing read every body instead of its
        # frontmatter prefix.
        lines = replace_line(
            "STRUCTURAL_PERF section=skill source=skill-catalog",
            "STRUCTURAL_PERF section=skill source=skill-catalog skills=200 skill_catalog_file_reads=200 "
            "skill_catalog_bytes=6553600",
        )
        self.assert_fails_naming(lines, "skill_catalog_bytes_per_skill")

    def test_skill_counters_that_read_zero_fail(self):
        # Every counter at 0 is what a run with the counters off reports; it
        # would pass every ceiling, so the floors must refuse it.
        lines = [
            line
            for line in HEALTHY_LINES
            if not line.startswith("STRUCTURAL_PERF section=skill")
        ] + [
            "STRUCTURAL_PERF section=skill source=skill-catalog skills=200 skill_catalog_file_reads=0 "
            "skill_catalog_bytes=0",
            "STRUCTURAL_PERF section=skill source=skill-overlay skill_warmup_file_reads=0 "
            "skill_unchanged_request_file_reads=0 skill_unchanged_request_dir_reads=0",
        ]
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        failed = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertTrue(
            {"skill_catalog_file_reads", "skill_catalog_bytes", "skill_warmup_file_reads"} <= failed, failed
        )

    def test_an_mcp_config_reread_on_every_session_fails(self):
        # The user config was parsed again for an unchanged file: the parse
        # cache is gone and every session start pays for ~/.claude.json again.
        lines = replace_line(
            "STRUCTURAL_PERF section=mcp_config",
            "STRUCTURAL_PERF section=mcp_config source=mcp-config mcp_config_warmup_reads=3 mcp_config_user_rereads=27",
        )
        self.assert_fails_naming(lines, "mcp_config_user_rereads")

    def test_mcp_config_counters_that_read_zero_fail(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=mcp_config",
            "STRUCTURAL_PERF section=mcp_config source=mcp-config mcp_config_warmup_reads=0 mcp_config_user_rereads=0",
        )
        self.assert_fails_naming(lines, "mcp_config_warmup_reads")

    def test_a_reader_that_accepts_a_frame_above_the_cap_fails(self):
        # The reader buffered past its own cap instead of refusing: one byte
        # over, and the next frame after it would be read as the tail of this
        # one.
        lines = replace_line(
            "STRUCTURAL_PERF section=frame source=bridge",
            "STRUCTURAL_PERF section=frame source=bridge max_frame_bytes=67108864 "
            "accepted_frame_bytes=67108865 rejected_frame_bytes=67108866",
        )
        self.assert_fails_naming(lines, "accepted_frame_bytes")

    def test_a_reader_that_never_refuses_an_oversized_frame_fails(self):
        # Both sizes at the cap: the boundary was reported but the reader has no
        # refusal path, which is the unbounded `read_line` this cap replaced.
        lines = replace_line(
            "STRUCTURAL_PERF section=frame source=sdk",
            "STRUCTURAL_PERF section=frame source=sdk max_frame_bytes=67108864 "
            "accepted_frame_bytes=67108864 rejected_frame_bytes=67108864",
        )
        self.assert_fails_naming(lines, "rejected_frame_bytes")

    def test_a_frame_cap_raised_without_a_budget_edit_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=frame source=bridge",
            "STRUCTURAL_PERF section=frame source=bridge max_frame_bytes=134217728 "
            "accepted_frame_bytes=134217728 rejected_frame_bytes=134217729",
        )
        self.assert_fails_naming(lines, "max_frame_bytes")

    def test_a_frame_measurement_that_never_reached_the_boundary_fails(self):
        # A 1 KiB frame accepted against a 64 MiB cap says nothing about the
        # cap: the refused size is not above it, so the reader was never tested
        # where it matters.
        lines = replace_line(
            "STRUCTURAL_PERF section=frame source=bridge",
            "STRUCTURAL_PERF section=frame source=bridge max_frame_bytes=67108864 "
            "accepted_frame_bytes=1024 rejected_frame_bytes=1025",
        )
        self.assert_fails_naming(lines, "rejected_frame_bytes")


class HooksSectionTest(unittest.TestCase):
    """An unset `pre_request` hook must stay free, and the probe must have run."""

    def test_a_spawned_process_with_no_hook_fails_the_gate(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=hooks",
            "STRUCTURAL_PERF section=hooks source=request-messages request_messages_in_probe=8 "
            "hook_process_spawns=1 request_message_copies=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertIn("hook_process_spawns", named)

    def test_a_copied_message_with_no_hook_fails_the_gate(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=hooks",
            "STRUCTURAL_PERF section=hooks source=request-messages request_messages_in_probe=8 "
            "hook_process_spawns=0 request_message_copies=1",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertIn("request_message_copies", named)

    def test_a_probe_that_built_no_request_fails_rather_than_passing_on_zeros(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=hooks",
            "STRUCTURAL_PERF section=hooks source=request-messages request_messages_in_probe=0 "
            "hook_process_spawns=0 request_message_copies=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertIn("request_messages_in_probe", named)

    def test_a_missing_hooks_section_fails_the_same_way(self):
        without_hooks = [
            line for line in HEALTHY_LINES if not line.startswith("STRUCTURAL_PERF section=hooks")
        ]
        run = run_gate(without_hooks)
        self.assertEqual(run.code, 2, "a hooks section that did not run is not a pass")
        self.assertEqual(run.result["missing"], ["hooks"])
        self.assertIn("hooks", run.stderr)


class PrefixSectionTest(unittest.TestCase):
    """The request prefix is bounded, and the bound is not met by losing tools."""

    def test_a_catalog_above_its_ceiling_fails_the_gate(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prefix",
            "STRUCTURAL_PERF section=prefix source=tool-catalog tools_bytes=70000 tool_count=8 eager_count=8 "
            "per_tool=read:900,write:700,edit:1100,apply_patch:800,bash:63000,ls:500,agentgrep:1900,toolbox:1091",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertIn("tools_bytes", named)

    def test_a_catalog_that_serialized_nothing_fails_rather_than_passing_on_a_zero(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prefix",
            "STRUCTURAL_PERF section=prefix source=tool-catalog tools_bytes=0 tool_count=0 eager_count=0 per_tool=",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertIn("tools_bytes", named)

    def test_a_catalog_that_lost_a_tool_fails_even_under_the_ceiling(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prefix",
            "STRUCTURAL_PERF section=prefix source=tool-catalog tools_bytes=708 tool_count=7 eager_count=8 "
            "per_tool=read:100,write:100,edit:100,apply_patch:100,bash:100,ls:100,agentgrep:100",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertEqual(named, {"tool_count"})

    def test_prefix_per_tool_sum_mismatch_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prefix",
            "STRUCTURAL_PERF section=prefix source=tool-catalog tools_bytes=809 tool_count=8 eager_count=8 "
            "per_tool=read:100,write:100,edit:100,apply_patch:100,bash:101,ls:100,agentgrep:100,toolbox:100",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertEqual(named, {"per_tool_bytes"})

    def test_a_prefix_line_without_a_breakdown_is_not_a_measurement(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prefix",
            "STRUCTURAL_PERF section=prefix source=tool-catalog tools_bytes=809 tool_count=8 eager_count=8",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 2, "a prefix line with no per-tool breakdown is not a pass")
        self.assertIn("per_tool", run.stderr)

    def test_prefix_over_ceiling_fails(self):
        # One byte over the committed ceiling, with a breakdown that adds up,
        # so the ceiling is the only check that can fail.
        ceiling = json.loads(BUDGET_FILE.read_text(encoding="utf-8"))["bounds"]["prefix_tools_bytes_max"]
        over = ceiling + 1
        sizes = [900, 700, 1100, 800, 0, 500, 1900, 1091]
        sizes[4] = over - 2 - 7 - sum(sizes)
        names = ["read", "write", "edit", "apply_patch", "bash", "ls", "agentgrep", "toolbox"]
        per_tool = ",".join(f"{name}:{size}" for name, size in zip(names, sizes))
        lines = replace_line(
            "STRUCTURAL_PERF section=prefix",
            f"STRUCTURAL_PERF section=prefix source=tool-catalog tools_bytes={over} tool_count=8 eager_count=8 "
            f"per_tool={per_tool}",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertEqual(named, {"tools_bytes"})

    def test_a_missing_prefix_section_fails_the_same_way(self):
        without_prefix = [line for line in HEALTHY_LINES if not line.startswith("STRUCTURAL_PERF section=prefix")]
        run = run_gate(without_prefix)
        self.assertEqual(run.code, 2, "a prefix section that did not run is not a pass")
        self.assertEqual(run.result["missing"], ["prefix"])


class PromptSectionTest(unittest.TestCase):
    """The static prompt line is a measurement, and a line of zeros is not one."""

    PROMPT_LINE = (
        "STRUCTURAL_PERF section=prompt static_bytes=1322 "
        "static_sha256=380df8ba02b5de4cbdcc3ee203743df5887c004ca9591d3620a53a9593372936 static_tokens=305 "
        "index_bytes=585 "
    )

    def test_prompt_section_missing_fails(self):
        without_prompt = [line for line in HEALTHY_LINES if not line.startswith("STRUCTURAL_PERF section=prompt")]
        run = run_gate(without_prompt)
        self.assertEqual(run.code, 2, "a prompt section that did not run is not a pass")
        self.assertEqual(run.result["missing"], ["prompt"])

    def test_prompt_counters_off_is_not_measured(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prompt",
            self.PROMPT_LINE + "index_clipped_rows=1 warm_source_reads=0 unchanged_request_source_reads=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertIn("warm_source_reads", named)

    def test_a_request_that_reads_a_prompt_source_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prompt",
            self.PROMPT_LINE + "index_clipped_rows=1 warm_source_reads=8 unchanged_request_source_reads=1",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertEqual(named, {"unchanged_request_source_reads"})

    def test_a_second_clipped_index_row_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prompt",
            self.PROMPT_LINE + "index_clipped_rows=2 warm_source_reads=8 unchanged_request_source_reads=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertEqual(named, {"index_clipped_rows"})

    def test_a_static_prompt_over_its_ceiling_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prompt",
            self.PROMPT_LINE.replace("static_bytes=1322", "static_bytes=1323")
            + "index_clipped_rows=1 warm_source_reads=8 unchanged_request_source_reads=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertEqual(named, {"static_bytes"})

    def test_a_digest_that_is_not_a_sha256_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=prompt",
            "STRUCTURAL_PERF section=prompt static_bytes=1322 static_sha256=unknown static_tokens=305 "
            "index_bytes=585 index_clipped_rows=1 warm_source_reads=8 unchanged_request_source_reads=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertIn("static_sha256", named)


class SessionContextSectionTest(unittest.TestCase):
    """The session-context line counts processes that were started, and a line of zeros does not."""

    def test_session_context_section_missing_fails(self):
        without = [line for line in HEALTHY_LINES if not line.startswith("STRUCTURAL_PERF section=session_context")]
        run = run_gate(without)
        self.assertEqual(run.code, 2, "a session-context section that did not run is not a pass")
        self.assertEqual(run.result["missing"], ["session_context"])

    def test_a_repository_that_started_no_git_was_not_measured(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=session_context",
            "STRUCTURAL_PERF section=session_context git_spawns_outside_repo=0 git_spawns_in_repo=0 "
            "gpu_probe_spawns_without_display=0 gpu_reported=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 1, run.stderr)
        named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
        self.assertEqual(named, {"git_spawns_in_repo"})

    def test_each_session_context_spawn_over_its_ceiling_fails_alone(self):
        for counter, line in (
            ("git_spawns_outside_repo", "git_spawns_outside_repo=1 git_spawns_in_repo=1 gpu_probe_spawns_without_display=0"),
            ("git_spawns_in_repo", "git_spawns_outside_repo=0 git_spawns_in_repo=2 gpu_probe_spawns_without_display=0"),
            (
                "gpu_probe_spawns_without_display",
                "git_spawns_outside_repo=0 git_spawns_in_repo=1 gpu_probe_spawns_without_display=1",
            ),
        ):
            lines = replace_line(
                "STRUCTURAL_PERF section=session_context",
                f"STRUCTURAL_PERF section=session_context {line} gpu_reported=0",
            )
            run = run_gate(lines)
            self.assertEqual(run.code, 1, run.stderr)
            named = {check["counter"] for check in run.result["checks"] if not check["ok"]}
            self.assertEqual(named, {counter})


class ToolDispatchSectionTest(unittest.TestCase):
    """The tool-dispatch line counts allocator requests per call, and a line of zeros does not."""

    PREFIX = "STRUCTURAL_PERF section=tool_dispatch"

    def line(self, **counters):
        values = {
            "dispatches": 200,
            "small_allocs": 2000,
            "small_alloc_bytes": 100000,
            "large_allocs": 2000,
            "large_alloc_bytes": 200000,
        }
        values.update(counters)
        fields = " ".join(f"{key}={value}" for key, value in values.items())
        return f"{self.PREFIX} source=app-core {fields}"

    def failed(self, **counters):
        run = run_gate(replace_line(self.PREFIX, self.line(**counters)))
        self.assertEqual(run.code, 1, run.stderr)
        return {check["counter"] for check in run.result["checks"] if not check["ok"]}

    def test_tool_dispatch_section_missing_fails(self):
        without = [line for line in HEALTHY_LINES if not line.startswith(self.PREFIX)]
        run = run_gate(without)
        self.assertEqual(run.code, 2, "a tool-dispatch section that did not run is not a pass")
        self.assertEqual(run.result["missing"], ["tool_dispatch"])

    def test_each_ceiling_fails_alone_one_request_over_it(self):
        bounds = gate.load_budget(str(gate.BUDGET_FILE))
        for size in ("small", "large"):
            allocations = bounds[f"tool_dispatch_{size}_allocs_per_dispatch_max"] * 200 + 1
            self.assertEqual(
                self.failed(**{f"{size}_allocs": allocations, f"{size}_alloc_bytes": allocations}),
                {f"{size}_allocs_per_dispatch"},
                "one allocation over the ceiling across the whole run must fail",
            )
            requested = bounds[f"tool_dispatch_{size}_alloc_bytes_per_dispatch_max"] * 200 + 1
            self.assertEqual(self.failed(**{f"{size}_alloc_bytes": requested}), {f"{size}_alloc_bytes_per_dispatch"})

    def test_a_counter_that_recorded_nothing_fails_rather_than_passing_on_zeros(self):
        self.assertEqual(self.failed(small_allocs=0, small_alloc_bytes=0), {"small_allocs"})
        self.assertEqual(self.failed(large_allocs=0, large_alloc_bytes=0), {"large_allocs"})

    def test_fewer_allocations_than_dispatches_is_not_a_measurement(self):
        self.assertEqual(self.failed(small_allocs=199), {"small_allocs"})

    def test_allocations_that_asked_for_no_bytes_fail(self):
        self.assertEqual(self.failed(large_alloc_bytes=1999), {"large_alloc_bytes"})

    def test_a_run_with_no_dispatch_fails(self):
        self.assertEqual(
            self.failed(dispatches=0, small_allocs=0, small_alloc_bytes=0, large_allocs=0, large_alloc_bytes=0),
            {"dispatches"},
        )

    def test_note_lines_are_repeated_and_never_judged(self):
        note = "STRUCTURAL_PERF_NOTE section=tool_dispatch stage=log_line calls=200 allocs=999999999 alloc_bytes=0"
        run = run_gate([*HEALTHY_LINES, note])
        self.assertEqual(run.code, 0, run.stderr)
        self.assertIn(note, run.stdout.splitlines())
        self.assertEqual(run.result, run_gate(HEALTHY_LINES).result, "a note changes no check")


class MissingMeasurementTest(unittest.TestCase):
    def test_a_missing_section_fails_instead_of_passing_on_nothing(self):
        without_soak = [line for line in HEALTHY_LINES if not line.startswith("STRUCTURAL_PERF section=soak")]
        run = run_gate(without_soak)
        self.assertEqual(run.code, 2, "a section that did not run is not a pass")
        result = run.result
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["missing"], ["soak"])
        self.assertIn("soak", run.stderr)

    def test_a_missing_frame_section_fails_the_same_way(self):
        without_frame = [line for line in HEALTHY_LINES if not line.startswith("STRUCTURAL_PERF section=frame")]
        run = run_gate(without_frame)
        self.assertEqual(run.code, 2, "a frame section that did not run is not a pass")
        result = run.result
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["missing"], ["frame"])
        self.assertIn("frame", run.stderr)

    def test_one_frame_reader_missing_fails_the_gate_by_name(self):
        # Two implementations of one contract: the bridge and the SDK. Gating
        # only the bridge would leave the SDK cap free to drift, which is the
        # hole this section exists to close.
        only_bridge = [
            line
            for line in HEALTHY_LINES
            if not line.startswith("STRUCTURAL_PERF section=frame source=sdk")
        ]
        run = run_gate(only_bridge)
        self.assertEqual(run.code, 2, "a reader that did not report is not a pass")
        self.assertEqual(run.result["missing"], ["frame/sdk"])
        self.assertIn("frame/sdk", run.stderr)

    def test_a_missing_irc_section_fails_the_same_way(self):
        without_irc = [line for line in HEALTHY_LINES if not line.startswith("STRUCTURAL_PERF section=irc")]
        run = run_gate(without_irc)
        self.assertEqual(run.code, 2, "an irc section that did not run is not a pass")
        result = run.result
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["missing"], ["irc"])
        self.assertIn("irc", run.stderr)

    def test_a_line_without_a_section_is_refused(self):
        with self.assertRaises(gate.GateError):
            gate.parse_lines("STRUCTURAL_PERF queue_depth_high_water=1")

    def test_a_missing_field_is_refused_rather_than_defaulted(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=soak",
            "STRUCTURAL_PERF section=soak cycles=20 fd_retention=0 non_returning=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 2)
        self.assertIn("thread_retention", run.stderr)

    def test_a_non_integer_value_is_refused(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=soak",
            "STRUCTURAL_PERF section=soak cycles=20 fd_retention=two thread_retention=0 non_returning=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 2)

    def test_one_section_can_be_checked_alone(self):
        run = run_gate(HEALTHY_LINES, extra_args=("--only", "page"))
        self.assertEqual(run.code, 0, run.stderr)
        covered = {check["section"].split("/")[0] for check in run.result["checks"]}
        self.assertEqual(covered, {"page"})


class PackageCostTest(unittest.TestCase):
    """The package section: nothing installed, installed, and actually called."""

    def assert_fails_naming(self, lines, counter, extra_args=()):
        run = run_gate(lines, extra_args)
        self.assertEqual(run.code, 1, f"a violated bound must fail the gate:\n{run.stdout}\n{run.stderr}")
        result = run.result
        named = {check["counter"] for check in result["checks"] if not check["ok"]}
        self.assertIn(counter, named, f"the failure must name {counter}, named {sorted(named)}")
        self.assertIn(counter, run.stderr, "the stderr report must name the counter a human has to fix")

    def test_a_package_free_home_that_reads_a_package_file_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=empty",
            "STRUCTURAL_PERF section=packages source=empty turns=2 packages=0 area_reads=1 dir_scans=0 "
            "area_writes=0 snapshot_builds=0 child_starts=0",
        )
        self.assert_fails_naming(lines, "area_reads")

    def test_a_package_free_home_that_grows_a_reference_directory_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=empty",
            "STRUCTURAL_PERF section=packages source=empty turns=2 packages=0 area_reads=0 dir_scans=0 "
            "area_writes=2 snapshot_builds=0 child_starts=0",
        )
        self.assert_fails_naming(lines, "area_writes")

    def test_a_package_free_home_that_installed_a_child_free_turn_fails_on_a_zero_turn_count(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=empty",
            "STRUCTURAL_PERF section=packages source=empty turns=0 packages=0 area_reads=0 dir_scans=0 "
            "area_writes=0 snapshot_builds=0 child_starts=0",
        )
        self.assert_fails_naming(lines, "turns")

    def test_an_unchanged_turn_that_rebuilt_its_snapshot_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=turn-unchanged",
            "STRUCTURAL_PERF section=packages source=turn-unchanged turns=1 packages=1 area_reads=0 dir_scans=0 "
            "area_writes=0 snapshot_builds=1 child_starts=0",
        )
        self.assert_fails_naming(lines, "snapshot_builds")

    def test_an_unchanged_turn_that_read_the_catalog_again_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=turn-unchanged",
            "STRUCTURAL_PERF section=packages source=turn-unchanged turns=1 packages=1 area_reads=1 dir_scans=0 "
            "area_writes=0 snapshot_builds=0 child_starts=0",
        )
        self.assert_fails_naming(lines, "area_reads")

    def test_a_warm_turn_that_measured_nothing_fails_rather_than_passing_on_zeros(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=turn-warm",
            "STRUCTURAL_PERF section=packages source=turn-warm turns=1 packages=1 area_reads=0 dir_scans=0 "
            "area_writes=0 snapshot_builds=0 child_starts=0",
        )
        self.assert_fails_naming(lines, "area_reads")

    def test_a_turn_that_started_a_package_child_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=turn-unchanged",
            "STRUCTURAL_PERF section=packages source=turn-unchanged turns=1 packages=1 area_reads=0 dir_scans=0 "
            "area_writes=0 snapshot_builds=0 child_starts=1",
        )
        self.assert_fails_naming(lines, "child_starts")

    def test_one_call_that_started_two_children_fails(self):
        # The marker keeps `source=call` from also matching `source=call-repeat`:
        # replacing both would drop a situation instead of violating a bound.
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=call",
            "STRUCTURAL_PERF section=packages source=call calls=1 area_reads=3 child_starts=2",
            marker="area_reads=3",
        )
        self.assert_fails_naming(lines, "child_starts")

    def test_a_second_call_that_started_another_child_fails(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=call-repeat",
            "STRUCTURAL_PERF section=packages source=call-repeat calls=1 child_starts=1",
        )
        self.assert_fails_naming(lines, "child_starts")

    def test_the_packages_section_missing_fails_instead_of_passing_on_nothing(self):
        lines = [
            line
            for line in HEALTHY_LINES
            if not line.startswith("STRUCTURAL_PERF section=packages")
        ]
        run = run_gate(lines)
        self.assertEqual(run.code, 2, run.stderr)
        self.assertIn("packages", run.stderr, "the report must name the section that did not run")

    def test_a_log_without_the_warm_turn_fails_instead_of_passing_on_the_empty_ones(self):
        # `empty` reports zeros for everything; on its own it certifies nothing
        # about a turn over an installed package, so the four remaining lines must
        # not add up to a pass.
        lines = [
            line
            for line in HEALTHY_LINES
            if not line.startswith("STRUCTURAL_PERF section=packages source=turn-warm")
        ]
        run = run_gate(lines)
        self.assertEqual(run.code, 2, f"a partial package log must not pass:\n{run.stdout}\n{run.stderr}")
        self.assertIn("turn-warm", run.stderr, "the report must name the situation that did not run")

    def test_an_unknown_package_source_is_refused(self):
        lines = replace_line(
            "STRUCTURAL_PERF section=packages source=call-repeat",
            "STRUCTURAL_PERF section=packages source=unknown calls=1 child_starts=0",
        )
        run = run_gate(lines)
        self.assertEqual(run.code, 2, run.stderr)
        self.assertIn("unknown source", run.stderr)


class BudgetTest(unittest.TestCase):
    def test_the_bounds_file_carries_every_key_the_checks_read(self):
        bounds = gate.load_budget(str(BUDGET_FILE))
        expected = {
            "queue_depth_high_water",
            "queue_bytes_high_water",
            "queue_bytes_buffered",
            "pending_requests",
            "pending_requests_high_water",
            "page_warm_replayed",
            "page_warm_index_builds",
            "page_read_ratio",
            "page_read_growth_factor",
            "hash_buffer_bytes",
            "hash_min_source_bytes",
            "frame_max_bytes",
            "soak_min_cycles",
            "soak_max_cycles",
            "soak_fd_retention",
            "soak_thread_retention",
            "soak_non_returning",
            "context_spill_kept_bytes",
            "context_read_stub_bytes",
            "context_aging_batches",
            "irc_mailbox_cap",
            "irc_broadcast_body_allocations",
            "irc_waiters_live_after_churn",
            "irc_broadcast_concurrency",
            "skill_catalog_bytes_per_skill_max",
            "skill_catalog_file_reads_per_skill_max",
            "skill_unchanged_request_file_reads_max",
            "skill_unchanged_request_dir_reads_max",
            "mcp_config_user_rereads_max",
            "hooks_unset_process_spawns_max",
            "hooks_unset_message_copies_max",
            "prefix_tools_bytes_max",
            "prompt_static_bytes_max",
            "prompt_unchanged_request_source_reads_max",
            "prompt_index_clipped_rows_max",
            "session_context_git_spawns_outside_repo_max",
            "session_context_git_spawns_in_repo_max",
            "session_context_gpu_probe_spawns_without_display_max",
            "tool_dispatch_small_allocs_per_dispatch_max",
            "tool_dispatch_small_alloc_bytes_per_dispatch_max",
            "tool_dispatch_large_allocs_per_dispatch_max",
            "tool_dispatch_large_alloc_bytes_per_dispatch_max",
            "packages_empty_area_reads_max",
            "packages_empty_area_dir_scans_max",
            "packages_empty_area_writes_max",
            "packages_empty_area_snapshot_builds_max",
            "packages_empty_area_child_starts_max",
            "packages_warm_turn_reads_min",
            "packages_warm_turn_snapshot_builds_min",
            "packages_warm_turn_writes_min",
            "packages_unchanged_turn_reads_max",
            "packages_unchanged_turn_dir_scans_max",
            "packages_unchanged_turn_writes_max",
            "packages_unchanged_turn_snapshot_builds_max",
            "packages_turn_child_starts_max",
            "packages_call_child_starts",
            "packages_call_repeat_child_starts",
        }
        self.assertEqual(set(bounds), expected, "the bounds file and the checks must not drift apart")

    def test_the_bounds_file_is_committed_not_generated(self):
        self.assertTrue(BUDGET_FILE.exists(), "the ceiling must live in the tree so a change to it is reviewable")
        data = json.loads(BUDGET_FILE.read_text(encoding="utf-8"))
        self.assertIn("notes", data, "a raised ceiling must carry its reason")

    def test_an_unusable_bounds_file_is_a_failure_not_a_pass(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as handle:
            handle.write("{}")
            path = handle.name
        stdout, stderr = io.StringIO(), io.StringIO()
        try:
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = gate.main(["--read", str(BUDGET_FILE), "--budget", path])
        finally:
            os.unlink(path)
        self.assertEqual(code, 2)
        self.assertIn("bounds", stderr.getvalue())

    def test_a_negative_bound_is_refused(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as handle:
            json.dump({"bounds": {"queue_depth_high_water": -1}}, handle)
            path = handle.name
        try:
            with self.assertRaises(gate.GateError):
                gate.load_budget(path)
        finally:
            os.unlink(path)


class ReportTemplateTest(unittest.TestCase):
    """The plan's required keys, asserted rather than eyeballed.

    Each entry is the plan's wording beside the path that carries it, so a
    template edit that drops a key fails here instead of at review time.
    """

    REQUIRED = (
        ("work id", "work_id"),
        ("B0 sha", "baseline.git_hash"),
        ("candidate sha", "candidate.git_hash"),
        ("machine", "machine.machine"),
        ("OS", "machine.system"),
        ("toolchain", "toolchain.rustc"),
        ("features", "baseline.features"),
        ("profile", "baseline.profile"),
        ("fixture hashes", "fixtures.0.sha256"),
        ("iteration count", "iterations.measured_per_side"),
        ("warmup", "iterations.warmup"),
        ("failure count", "counts.failure"),
        ("timeout count", "counts.timeout"),
        ("missing count", "counts.missing"),
        ("median", "timing.baseline.median_ms"),
        ("p95", "timing.baseline.p95_ms"),
        ("p99 key (null without enough samples, never absent)", "timing.baseline.p99_ms"),
        ("CPU", "resources.baseline.cpu_user_ms"),
        ("read bytes", "resources.baseline.read_bytes"),
        ("write bytes", "resources.baseline.write_bytes"),
        ("allocated peak", "resources.baseline.allocated_peak_bytes"),
        ("retained", "resources.baseline.retained_bytes"),
        ("RSS", "resources.baseline.rss_bytes"),
        ("PSS", "resources.baseline.pss_bytes"),
        ("queue high-water", "resources.baseline.queue_depth_high_water"),
        ("FD high-water", "resources.baseline.fd_count"),
        ("thread high-water", "resources.baseline.thread_count"),
        ("task high-water", "resources.baseline.task_high_water"),
        ("correctness suite result", "correctness.result"),
        ("interpretation", "interpretation"),
        ("adoption decision", "adoption_decision"),
        ("rollback commit", "rollback_commit"),
    )

    def template(self):
        return json.loads(TEMPLATE_FILE.read_text(encoding="utf-8"))

    def resolve(self, document, path):
        node = document
        for part in path.split("."):
            if isinstance(node, list):
                node = node[int(part)]
            elif isinstance(node, dict) and part in node:
                node = node[part]
            else:
                raise AssertionError(f"template has no key at {path}")
        return node

    def test_the_template_is_tracked(self):
        self.assertTrue(TEMPLATE_FILE.exists())
        relative = TEMPLATE_FILE.relative_to(REPO_ROOT).as_posix()
        self.assertFalse(relative.startswith(".omo/"), "the template must be committed, not an ignored local artifact")

    def test_the_template_carries_every_required_key(self):
        document = self.template()
        missing = []
        for label, path in self.REQUIRED:
            try:
                self.resolve(document, path)
            except AssertionError:
                missing.append(f"{label} ({path})")
        self.assertEqual(missing, [], "the report template is missing required keys: " + ", ".join(missing))

    def test_the_template_carries_both_sides_for_every_resource(self):
        document = self.template()
        baseline = set(document["resources"]["baseline"])
        candidate = set(document["resources"]["candidate"])
        self.assertEqual(baseline, candidate, "a one-sided report cannot be compared")

    def test_the_template_names_the_decision_vocabulary_the_runner_emits(self):
        document = self.template()
        allowed = {
            "adopt-pending-correctness",
            "not-adopted-no-measurable-change",
            "reject-regression",
            "control-no-change",
            "control-drift",
            "blocked",
        }
        placeholder = document["adoption_decision"]
        for option in placeholder.strip("<>").split("|"):
            self.assertIn(option, allowed, f"template offers a decision the runner never emits: {option}")


class WorkflowTest(unittest.TestCase):
    """The job is the deliverable that makes the rest run at all."""

    def workflow(self):
        return WORKFLOW_FILE.read_text(encoding="utf-8")

    def test_the_workflow_exists(self):
        self.assertTrue(WORKFLOW_FILE.exists(), "without a workflow file the gate never runs")

    def test_it_triggers_on_pull_requests_to_dev(self):
        text = self.workflow()
        self.assertIn("pull_request:", text)
        self.assertIn("branches: [dev, main]", text, "every PR in this repo targets dev")
        self.assertNotIn("master", text, "this repository has no master branch")

    def test_it_needs_no_secrets_and_no_release_binary(self):
        text = self.workflow()
        self.assertNotIn("secrets.", text)
        self.assertNotIn("--release", text, "the gate must not need a release binary")

    def test_it_runs_the_gate_and_its_contract_test(self):
        text = self.workflow()
        self.assertIn("scripts/perf/check_structural.py", text)
        self.assertIn("scripts/perf/test_check_structural.py", text)

    def test_it_declares_read_only_permissions(self):
        self.assertIn("permissions:\n  contents: read", self.workflow())


class MeasurementPlanTest(unittest.TestCase):
    def test_no_measurement_builds_a_release_binary(self):
        for measurement in gate.MEASUREMENTS:
            command = gate.measurement_command(measurement)
            self.assertNotIn("--release", command, f"{measurement['section']} must run in debug")

    def test_every_measurement_runs_one_test_against_one_target_dir(self):
        for measurement in gate.MEASUREMENTS:
            command = gate.measurement_command(measurement)
            self.assertIn("--test-threads=1", command, "process-wide counters need one test at a time")
            self.assertIn("--exact", command)

    def test_the_soak_runs_short_and_the_page_check_covers_three_sizes(self):
        soak = [m for m in gate.MEASUREMENTS if m["section"] == "soak"]
        self.assertEqual(len(soak), 1)
        self.assertEqual(soak[0]["env"]["HORON_AGENT_SOAK_CYCLES"], "20", "a short soak, not the 200-cycle gate")
        page = [m for m in gate.MEASUREMENTS if m["section"] == "page"]
        self.assertEqual(len(page), 1, "one test covers all three session sizes and prints one line each")

    def test_the_frame_section_covers_both_readers(self):
        frame = [m for m in gate.MEASUREMENTS if m["section"] == "frame"]
        self.assertEqual(
            sorted(m["source"] for m in frame),
            ["bridge", "sdk"],
            "the bridge and the SDK cap the same contract in two implementations",
        )
        packages = {m["args"][1] for m in frame}
        self.assertEqual(
            packages,
            {"horon-agent-harness-api-server", "horon-agent-harness-api"},
            "one measurement per reader, or a drift on one side goes ungated",
        )

    def test_every_required_section_has_a_measurement(self):
        planned = {measurement["section"] for measurement in gate.MEASUREMENTS}
        self.assertEqual(planned, set(gate.REQUIRED_SECTIONS))


if __name__ == "__main__":
    unittest.main()
