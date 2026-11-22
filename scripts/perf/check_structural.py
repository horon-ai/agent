#!/usr/bin/env python3
"""Gate the structural performance bounds, without a release binary or a network.

The A/B runner (`scripts/perf/ab_perf.py`) answers "is the candidate faster". This
script answers a different and earlier question: "is the shape still bounded".
A queue that grows with the flood, a history page that replays the session, a
hash buffer that follows the log, a frame reader that accepts a frame past its
cap, or an attach/detach cycle that leaks a descriptor are all visible in
counters that already exist, and none of them needs a wall-clock number to
detect.

That matters because the plan forbids a numeric latency or RSS threshold until
the B0 distributions are pinned. Wall time moves with the machine; a counter
does not. So every check here reads a counter:

- `queue_depth_high_water`, `queue_bytes_high_water`, `queue_bytes_buffered`;
- `pending_requests`, `pending_requests_high_water`;
- `history_page_replayed`, `history_page_flattened`, `history_index_builds`
  and the bytes a warm page reads against the size of its log;
- the peak read request of the streaming hash against the source length;
- the largest frame each NDJSON reader accepts against its own cap, and the
  smallest frame it refuses;
- FD and thread retention across a short attach/detach soak;
- the IRC mailbox cap, the messages a flood drops, one body allocation per
  broadcast, waiters left parked after churn, and the broadcast concurrency
  ceiling;
- the request projection with no `pre_request` hook configured: no child
  process spawned and no message copied.
- the heap allocations and the bytes one tool dispatch asks for, with a short
  input and with a 10 KB one;
- the package-area reads, directory scans, writes, snapshot builds and child
  starts of a home with no packages, of an unchanged catalog generation, and of
  one real call.

The measurements are the repo's own tests. Each one prints one
`STRUCTURAL_PERF section=<name> k=v k=v` line, so this gate reads the numbers
the test already asserted rather than re-measuring them with a second
implementation that could drift. The tests run one at a time: the counters are
process-wide and `reset()` is only meaningful when nothing else in the binary
is recording.

Usage:

    python3 scripts/perf/check_structural.py              # run every measurement
    python3 scripts/perf/check_structural.py --print-plan # show the cargo commands
    python3 scripts/perf/check_structural.py --only page  # one section
    python3 scripts/perf/check_structural.py --read ci.log # re-check a saved log

Exit status: 0 every bound holds, 1 a bound was violated, 2 a measurement is
missing or the budget file is unusable. A missing measurement is never a pass:
a zero because nothing ran certifies nothing.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
BUDGET_FILE = REPO_ROOT / "scripts" / "perf" / "structural_budget.json"
RESULT_PREFIX = "STRUCTURAL_PERF_RESULT "
# A measurement may print lines with this prefix to say where its counts come
# from. The gate repeats them in its own output and never judges them.
NOTE_PREFIX = "STRUCTURAL_PERF_NOTE "
LINE_PATTERN = re.compile(r"^STRUCTURAL_PERF\s+(.*)$")

# One cargo invocation per measurement. `--test-threads=1` and `--exact` keep a
# process-wide counter from being raised by a sibling test, and the soak keeps
# its own cycle count small: 20 cycles resolves a per-cycle leak and costs about
# a minute, where the gate default of 200 costs ten.
MEASUREMENTS = (
    {
        "section": "queue",
        "source": "agent-runtime",
        "args": ["-p", "horon-agent-agent-runtime", "queue_tests::queue_high_water_increases_under_a_burst"],
    },
    {
        "section": "queue",
        "source": "app-core",
        "args": [
            "-p",
            "horon-agent-app-core",
            "server::queue_tests::a_soft_interrupt_burst_raises_the_queue_high_water_counter",
        ],
    },
    {
        "section": "pending",
        "source": "bridge",
        "args": [
            "-p",
            "horon-agent-harness-api-server",
            "translate::tests::pending_request_gauge_rises_on_a_request_and_falls_on_its_reply",
        ],
    },
    {
        "section": "page",
        "source": "harness-api-server",
        "args": [
            "-p",
            "horon-agent-harness-api-server",
            "translate::history_page_perf_tests::one_page_cost_does_not_grow_with_the_session",
        ],
    },
    {
        "section": "hash",
        "source": "session-store",
        "args": [
            "-p",
            "horon-agent-session-store",
            "--test",
            "hash_stream",
            "the_stream_buffer_is_fixed_and_the_source_length_only_adds_passes",
        ],
    },
    # Two readers, one bound: the bridge refuses an oversized inbound frame and
    # the SDK client refuses an oversized inbound frame, and they are separate
    # implementations of the same 64 MiB contract. Gating one would leave the
    # other free to drift, which is the whole reason the section has two
    # sources the way `queue` does.
    {
        "section": "frame",
        "source": "bridge",
        "args": [
            "-p",
            "horon-agent-harness-api-server",
            "framing_tests::the_frame_cap_is_exact_and_reported_to_the_structural_gate",
        ],
    },
    {
        "section": "frame",
        "source": "sdk",
        "args": [
            "-p",
            "horon-agent-harness-api",
            "schema_snapshot_tests::read_frame_cap_is_exact_and_reported_to_the_structural_gate",
        ],
    },
    {
        "section": "soak",
        "source": "e2e",
        "args": [
            "-p",
            "horon-agent",
            "--test",
            "e2e",
            "soak_attach_detach::soak_attach_detach_returns_every_counter_to_baseline",
        ],
        "env": {"HORON_AGENT_SOAK_CYCLES": "20"},
        # The soak drives a real daemon over a Unix socket.
        "unix_only": True,
    },
    {
        "section": "context",
        "source": "app-core",
        "args": [
            "-p",
            "horon-agent-app-core",
            "--lib",
            "tool::context_condense_perf_tests::context_condensation_stays_bounded",
        ],
    },
    {
        "section": "irc",
        "source": "irc-core",
        "args": [
            "-p",
            "horon-agent-irc-core",
            "--lib",
            "perf_tests::irc_bus_stays_bounded",
        ],
    },
    # Two shapes of one promise, so two sources: listing the catalog reads a
    # frontmatter prefix per skill, never a body, and a request against an
    # unchanged workspace reads no skill file or directory at all.
    {
        "section": "skill",
        "source": "skill-catalog",
        "args": [
            "-p",
            "horon-agent-base",
            "--lib",
            "skill::sources_tests::structural_catalog_frontmatter_only",
        ],
    },
    {
        "section": "skill",
        "source": "skill-overlay",
        "args": [
            "-p",
            "horon-agent-base",
            "--lib",
            "skill::overlay_cache_tests::structural_unchanged_request",
        ],
    },
    {
        "section": "mcp_config",
        "source": "mcp-config",
        "args": [
            "-p",
            "horon-agent-base",
            "--lib",
            "mcp::sources::tests::structural_user_config_rereads",
        ],
    },
    # The request projection is free when no `pre_request` hook is configured:
    # no child process, and no message copied. The probe floor is what keeps a
    # zero from certifying a measurement that never built a request.
    {
        "section": "hooks",
        "source": "request-messages",
        "args": [
            "-p",
            "horon-agent-app-core",
            "--lib",
            "agent::request_messages::tests::structural_unset_pre_request_is_free",
        ],
    },
    # The tool catalog is the request prefix every turn pays for: bounded in
    # bytes, and measured beside the number of tools so a catalog that shrank
    # its way under the ceiling is visible in the same line.
    {
        "section": "prefix",
        "source": "tool-catalog",
        "args": [
            "-p",
            "horon-agent-app-core",
            "--lib",
            "agent::tests::tool_request_bytes::core_profile_request_prefix_is_measured",
        ],
    },
    # The static system prompt is the other half of the cached request prefix.
    # Its size, its digest and the source files probed to assemble it are
    # measured from the agent's own builder, on a session that changed nothing
    # between requests.
    {
        "section": "prompt",
        "source": "static-prompt",
        "args": [
            "-p",
            "horon-agent-app-core",
            "--lib",
            "agent::tests::prompt_request_bytes::core_profile_static_prompt_is_measured",
        ],
    },
    # A session context is built when a session is created or restored, and
    # its git and GPU lines come from child processes. They are counted in
    # three fixed situations: a directory outside any repository, a fresh
    # repository, and the GPU probe on its own.
    {
        "section": "session_context",
        "source": "session-context",
        "args": [
            "-p",
            "horon-agent-base",
            "--lib",
            "prompt::session_context_tests::session_context_child_processes_are_measured",
        ],
    },
    # Every tool call goes through `Registry::execute`. Its cost is counted as
    # allocator requests, in an integration target of its own because the
    # counting allocator is per binary.
    {
        "section": "tool_dispatch",
        "source": "app-core",
        "args": [
            "-p",
            "horon-agent-app-core",
            "--test",
            "tool_dispatch_perf",
            "one_dispatch_allocates_a_bounded_amount",
        ],
    },
    # User-installed packages must cost nothing to the people who have none, and
    # a turn over an unchanged catalog must cost nothing beyond the check that it
    # is unchanged. One integration target drives a package-free session, a daemon
    # generation, two turns of one session and two real calls of the installed
    # `horon-package-probe` child; it prints one line per situation because each
    # is a different promise.
    {
        "section": "packages",
        "source": "app-core",
        "args": [
            "-p",
            "horon-agent-app-core",
            "--test",
            "package_cost",
            "structural_package_costs_are_measured",
        ],
    },
)

REQUIRED_SECTIONS = (
    "queue",
    "pending",
    "page",
    "hash",
    "frame",
    "soak",
    "context",
    "irc",
    "skill",
    "mcp_config",
    "hooks",
    "prefix",
    "prompt",
    "session_context",
    "tool_dispatch",
    "packages",
)


class GateError(Exception):
    pass


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--print-plan", action="store_true", help="print the cargo commands and exit")
    parser.add_argument("--only", action="append", default=None, help="run one section (repeatable)")
    parser.add_argument("--read", default=None, help="parse STRUCTURAL_PERF lines from a saved log instead of running")
    parser.add_argument("--budget", default=str(BUDGET_FILE), help="budget file to gate against")
    return parser.parse_args(argv)


def load_budget(path: str) -> dict[str, int]:
    """Bounds, from a file that only a deliberate edit may raise.

    This is the `scripts/ci/check_code_size.py` ratchet, in the same shape: a
    committed ceiling that a regression cannot raise by itself.
    """
    file = Path(path)
    if not file.exists():
        raise GateError(f"missing budget file: {file}")
    try:
        data = json.loads(file.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise GateError(f"budget file is not valid JSON: {file}: {error}") from error
    bounds = data.get("bounds")
    if not isinstance(bounds, dict) or not bounds:
        raise GateError(f"budget file has no bounds object: {file}")
    for name, value in bounds.items():
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise GateError(f"budget {name} must be a non-negative integer, got {value!r}")
    return bounds


def measurement_command(measurement: dict) -> list[str]:
    return ["cargo", "test", "-q", *measurement["args"], "--", "--exact", "--nocapture", "--test-threads=1"]


def print_plan() -> int:
    for measurement in MEASUREMENTS:
        env = measurement.get("env") or {}
        prefix = " ".join(f"{key}={value}" for key, value in env.items())
        command = " ".join(measurement_command(measurement))
        print(f"# section {measurement['section']} ({measurement['source']})")
        print(f"{prefix + ' ' if prefix else ''}{command}")
    return 0


def run_measurement(measurement: dict) -> str:
    env = dict(os.environ)
    env.update(measurement.get("env") or {})
    completed = subprocess.run(
        measurement_command(measurement),
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    # The tests print their line with `println!` on stdout and the soak prints
    # with `eprintln!`, so both streams are scanned.
    combined = completed.stdout + completed.stderr
    if completed.returncode != 0:
        tail = "\n".join(combined.strip().splitlines()[-25:])
        raise GateError(
            "measurement failed: cargo test %s exited %d\n%s"
            % (" ".join(measurement["args"]), completed.returncode, tail)
        )
    return combined


def parse_lines(text: str) -> list[dict[str, str]]:
    records: list[dict[str, str]] = []
    for raw in text.splitlines():
        match = LINE_PATTERN.match(raw.strip())
        if not match:
            continue
        record: dict[str, str] = {}
        for field in match.group(1).split():
            if "=" not in field:
                continue
            key, value = field.split("=", 1)
            record[key] = value
        if "section" not in record:
            raise GateError(f"STRUCTURAL_PERF line has no section: {raw.strip()}")
        records.append(record)
    return records


def note_lines(text: str) -> list[str]:
    return [raw.strip() for raw in text.splitlines() if raw.strip().startswith(NOTE_PREFIX)]


def as_int(record: dict[str, str], key: str, section: str) -> int:
    value = record.get(key)
    if value is None:
        raise GateError(f"section {section}: measurement is missing {key}")
    try:
        return int(value)
    except ValueError as error:
        raise GateError(f"section {section}: {key} is not an integer: {value!r}") from error


class Report:
    def __init__(self) -> None:
        self.checks: list[dict] = []

    def check(self, section: str, counter: str, observed: int, ok: bool, bound: str) -> None:
        self.checks.append(
            {
                "section": section,
                "counter": counter,
                "observed": observed,
                "bound": bound,
                "ok": ok,
            }
        )

    def at_most(self, section: str, counter: str, observed: int, ceiling: int) -> None:
        self.check(
            section,
            counter,
            observed,
            observed <= ceiling,
            f"<= {ceiling}",
        )

    def at_least(self, section: str, counter: str, observed: int, floor: int) -> None:
        self.check(section, counter, observed, observed >= floor, f">= {floor}")

    def equal(self, section: str, counter: str, observed: int, expected: int) -> None:
        self.check(section, counter, observed, observed == expected, f"== {expected}")

    def failures(self) -> list[dict]:
        return [check for check in self.checks if not check["ok"]]


def check_queue(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """The queue is bounded, and the burst that proves it is bounded too.

    Two bounds, not one: the depth may not exceed the burst the test pushed
    (nothing appears in the queue that was never put there), and it may not
    exceed the committed ceiling. A queue that accepts a flood without limit
    fails both, and the failure names `queue_depth_high_water`.
    """
    for record in records:
        source = record.get("source", "?")
        section = f"queue/{source}"
        burst = as_int(record, "burst", section)
        depth = as_int(record, "queue_depth_high_water", section)
        report.at_least(section, "queue_depth_high_water", depth, 1)
        report.at_most(section, "queue_depth_high_water", depth, burst)
        report.at_most(section, "queue_depth_high_water", depth, bounds["queue_depth_high_water"])
        report.at_most(
            section,
            "queue_bytes_high_water",
            as_int(record, "queue_bytes_high_water", section),
            bounds["queue_bytes_high_water"],
        )
        if "queue_bytes_buffered" in record:
            report.at_most(
                section,
                "queue_bytes_buffered",
                as_int(record, "queue_bytes_buffered", section),
                bounds["queue_bytes_buffered"],
            )


def check_pending(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """Outstanding requests rise while one is in flight and drain on its reply.

    The gauge is sampled twice in one test, before and after the reply, so the
    two lines are one measurement rather than two.
    """
    merged: dict[str, str] = {}
    for record in records:
        merged.update(record)
    records = [merged]
    for record in records:
        section = "pending"
        report.at_least(section, "pending_requests", as_int(record, "pending_requests", section), 1)
        report.at_most(
            section,
            "pending_requests",
            as_int(record, "pending_requests", section),
            bounds["pending_requests_high_water"],
        )
        report.at_most(
            section,
            "pending_after_reply",
            as_int(record, "pending_after_reply", section),
            bounds["pending_requests"],
        )
        report.at_most(
            section,
            "pending_requests_high_water",
            as_int(record, "pending_requests_high_water", section),
            bounds["pending_requests_high_water"],
        )


def check_page(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """A page costs the page, never the session.

    `warm_replayed` is the replay the index replaces and must be zero;
    `warm_flattened` is the page size, so it may not exceed the requested limit;
    `warm_index_builds` is zero once the page is warm; and the bytes a warm page
    reads must stay below a fraction of the log it is slicing, at every session
    size. The last one is what separates "bounded by the page" from "bounded by
    a smaller constant that still follows the session".
    """
    reads: list[int] = []
    for record in records:
        events = as_int(record, "events", "page")
        section = f"page/{events}"
        limit = as_int(record, "page_limit", section)
        report.equal(section, "warm_replayed", as_int(record, "warm_replayed", section), bounds["page_warm_replayed"])
        report.equal(
            section, "warm_index_builds", as_int(record, "warm_index_builds", section), bounds["page_warm_index_builds"]
        )
        report.at_most(section, "warm_flattened", as_int(record, "warm_flattened", section), limit)
        warm_read = as_int(record, "warm_read_bytes", section)
        log_bytes = as_int(record, "log_bytes", section)
        report.check(
            section,
            "warm_read_bytes",
            warm_read,
            warm_read * bounds["page_read_ratio"] <= log_bytes,
            f"* {bounds['page_read_ratio']} <= log_bytes ({log_bytes})",
        )
        reads.append(warm_read)

    if len(reads) >= 2:
        # Across session sizes, the cost of one page may not follow the session.
        report.check(
            "page/all",
            "warm_read_bytes_growth",
            max(reads),
            max(reads) <= min(reads) * bounds["page_read_growth_factor"],
            f"<= {min(reads)} * {bounds['page_read_growth_factor']}",
        )


def check_hash(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    for record in records:
        section = "hash"
        buffer_bytes = as_int(record, "buffer_bytes", section)
        small = as_int(record, "small_request", section)
        large = as_int(record, "large_request", section)
        source_bytes = as_int(record, "large_source_bytes", section)
        report.at_most(section, "buffer_bytes", buffer_bytes, bounds["hash_buffer_bytes"])
        report.equal(section, "large_request", large, small)
        report.equal(section, "small_request", small, buffer_bytes)
        # A "fixed buffer" proven against a tiny source proves nothing.
        report.at_least(section, "large_source_bytes", source_bytes, bounds["hash_min_source_bytes"])


def check_frame(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """A frame is bounded, and the bound is the one the budget committed.

    Four statements, because any one of them alone certifies nothing. The cap
    itself is compared, not bounded, so a silent raise is a budget edit and not
    a passing measurement. The largest accepted frame may not exceed the cap,
    and the smallest refused frame must sit above it: a reader that clips a
    legitimate frame fails the first, and one that accepts an oversized frame
    fails the second. The two sizes must also be neighbours, which is what
    separates "the boundary is where the cap says" from "the test never got
    near the boundary".
    """
    for record in records:
        source = record.get("source", "?")
        section = f"frame/{source}"
        cap = as_int(record, "max_frame_bytes", section)
        accepted = as_int(record, "accepted_frame_bytes", section)
        rejected = as_int(record, "rejected_frame_bytes", section)
        report.equal(section, "max_frame_bytes", cap, bounds["frame_max_bytes"])
        report.at_least(section, "accepted_frame_bytes", accepted, 1)
        report.at_most(section, "accepted_frame_bytes", accepted, cap)
        report.check(section, "rejected_frame_bytes", rejected, rejected > cap, f"> {cap}")
        report.check(
            section,
            "rejected_frame_bytes",
            rejected,
            rejected == accepted + 1,
            f"== accepted_frame_bytes + 1 ({accepted + 1})",
        )


def check_soak(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    for record in records:
        section = "soak"
        report.at_most(section, "cycles", as_int(record, "cycles", section), bounds["soak_max_cycles"])
        report.at_least(section, "cycles", as_int(record, "cycles", section), bounds["soak_min_cycles"])
        report.at_most(section, "fd_retention", as_int(record, "fd_retention", section), bounds["soak_fd_retention"])
        report.at_most(
            section, "thread_retention", as_int(record, "thread_retention", section), bounds["soak_thread_retention"]
        )
        report.at_most(section, "non_returning", as_int(record, "non_returning", section), bounds["soak_non_returning"])


def check_context(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """What a large output, a repeated read, and a pressure crossing cost.

    A 1 MiB output must reach the model within the generic condense budget; a
    second read of an unchanged file must be a one-line notice; and two
    requests past the aging pressure point must rewrite the prompt prefix
    exactly once. Zero batches fails too: aging that never runs is not bounded,
    it is absent.
    """
    for record in records:
        section = "context"
        report.at_least(section, "spill_kept_bytes", as_int(record, "spill_kept_bytes", section), 1)
        report.at_most(
            section,
            "spill_kept_bytes",
            as_int(record, "spill_kept_bytes", section),
            bounds["context_spill_kept_bytes"],
        )
        report.at_least(section, "read_stub_bytes", as_int(record, "read_stub_bytes", section), 1)
        report.at_most(
            section,
            "read_stub_bytes",
            as_int(record, "read_stub_bytes", section),
            bounds["context_read_stub_bytes"],
        )
        report.equal(
            section,
            "aging_batches",
            as_int(record, "aging_batches", section),
            bounds["context_aging_batches"],
        )


def check_irc(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """The mailbox stops at its cap, a broadcast shares one body, churn leaks no waiter.

    Five statements about the bus shape. The flood is measured, not the cap:
    `flood - cap` messages must have been dropped, so a mailbox that silently
    kept them all fails on `mailbox_dropped` even if `mailbox_high_water` was
    never sampled above the ceiling. A broadcast allocates its body once for
    every leg, and a timeout/cancel churn leaves nothing parked. The in-flight
    count is read where it peaked, so a concurrency limit raised without a
    budget edit shows up here.
    """
    for record in records:
        section = "irc"
        flood = as_int(record, "flood", section)
        cap = bounds["irc_mailbox_cap"]
        inflight = as_int(record, "broadcast_inflight_high_water", section)
        report.at_most(section, "mailbox_high_water", as_int(record, "mailbox_high_water", section), cap)
        report.equal(section, "mailbox_dropped", as_int(record, "mailbox_dropped", section), flood - cap)
        report.equal(
            section,
            "broadcast_body_allocations",
            as_int(record, "broadcast_body_allocations", section),
            bounds["irc_broadcast_body_allocations"],
        )
        report.equal(
            section,
            "waiters_live_after_churn",
            as_int(record, "waiters_live_after_churn", section),
            bounds["irc_waiters_live_after_churn"],
        )
        report.at_most(section, "broadcast_inflight_high_water", inflight, bounds["irc_broadcast_concurrency"])


def per_item(total: int, items: int) -> int:
    """`total / items` rounded up, so a fraction over the bound still fails."""
    return -(-total // items)


def check_skill(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """The skill catalog reads frontmatter only, and an unchanged workspace reads nothing.

    The floors carry as much weight as the ceilings: with the counters off
    every read reports 0, which would pass every ceiling. A catalog that read
    fewer files than it listed skills, or a cold request that read nothing,
    measured nothing.
    """
    for record in records:
        source = record.get("source", "?")
        section = f"skill/{source}"
        if source == "skill-catalog":
            skills = as_int(record, "skills", section)
            reads = as_int(record, "skill_catalog_file_reads", section)
            read_bytes = as_int(record, "skill_catalog_bytes", section)
            report.at_least(section, "skills", skills, 1)
            report.at_least(section, "skill_catalog_file_reads", reads, skills)
            report.at_least(section, "skill_catalog_bytes", read_bytes, 1)
            report.at_most(
                section,
                "skill_catalog_file_reads_per_skill",
                per_item(reads, max(skills, 1)),
                bounds["skill_catalog_file_reads_per_skill_max"],
            )
            report.at_most(
                section,
                "skill_catalog_bytes_per_skill",
                per_item(read_bytes, max(skills, 1)),
                bounds["skill_catalog_bytes_per_skill_max"],
            )
        elif source == "skill-overlay":
            report.at_least(
                section, "skill_warmup_file_reads", as_int(record, "skill_warmup_file_reads", section), 1
            )
            report.at_most(
                section,
                "skill_unchanged_request_file_reads",
                as_int(record, "skill_unchanged_request_file_reads", section),
                bounds["skill_unchanged_request_file_reads_max"],
            )
            report.at_most(
                section,
                "skill_unchanged_request_dir_reads",
                as_int(record, "skill_unchanged_request_dir_reads", section),
                bounds["skill_unchanged_request_dir_reads_max"],
            )
        else:
            raise GateError(f"section skill: unknown source {source}")


def check_mcp_config(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """A session start against unchanged user MCP configs reads no file.

    The warm-up floor keeps a run with the counters off from passing: it would
    report zero re-reads because it measured no read at all.
    """
    for record in records:
        section = "mcp_config"
        report.at_least(section, "mcp_config_warmup_reads", as_int(record, "mcp_config_warmup_reads", section), 1)
        report.at_most(
            section,
            "mcp_config_user_rereads",
            as_int(record, "mcp_config_user_rereads", section),
            bounds["mcp_config_user_rereads_max"],
        )


def check_hooks(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """An unset `pre_request` hook costs no process and no message copy.

    The floor on the probe itself carries the weight here: a request projection
    that never ran would report zero spawns and zero copies for the same reason
    a correct one does, and the gate must not read that as a pass.
    """
    for record in records:
        section = "hooks"
        report.at_least(
            section,
            "request_messages_in_probe",
            as_int(record, "request_messages_in_probe", section),
            1,
        )
        report.at_most(
            section,
            "hook_process_spawns",
            as_int(record, "hook_process_spawns", section),
            bounds["hooks_unset_process_spawns_max"],
        )
        report.at_most(
            section,
            "request_message_copies",
            as_int(record, "request_message_copies", section),
            bounds["hooks_unset_message_copies_max"],
        )


def per_tool_sizes(record: dict[str, str], section: str) -> list[int]:
    """The bytes of each catalog entry, from `per_tool=name:bytes,name:bytes`."""
    value = record.get("per_tool")
    if value is None:
        raise GateError(f"section {section}: measurement is missing per_tool")
    sizes: list[int] = []
    for entry in value.split(","):
        if not entry:
            continue
        name, separator, size = entry.rpartition(":")
        if not separator or not name or not size.isdigit():
            raise GateError(f"section {section}: per_tool entry is not name:bytes: {entry!r}")
        sizes.append(int(size))
    return sizes


def check_prefix(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """The request prefix stays inside its committed byte ceiling.

    Four statements, because a ceiling alone certifies nothing. The floor
    rejects a zero, which is what a catalog that never serialized reports; the
    ceiling is the bound; the tool count is compared against the eager
    definitions the same measurement saw, so a catalog that fell under the
    ceiling by losing a tool fails here instead of passing quietly; and the
    per-tool sizes must account for the total, so the breakdown a later change
    is judged against cannot drift from the number the ceiling bounds.
    """
    for record in records:
        section = "prefix"
        tools = as_int(record, "tools_bytes", section)
        count = as_int(record, "tool_count", section)
        report.at_least(section, "tools_bytes", tools, 1)
        report.at_most(section, "tools_bytes", tools, bounds["prefix_tools_bytes_max"])
        report.at_least(section, "tool_count", count, 1)
        report.equal(section, "tool_count", count, as_int(record, "eager_count", section))
        sizes = per_tool_sizes(record, section)
        report.equal(section, "per_tool_count", len(sizes), count)
        # A JSON array spends two brackets and one comma between entries.
        report.equal(section, "per_tool_bytes", sum(sizes) + 2 + max(len(sizes) - 1, 0), tools)


SHA256_HEX = re.compile(r"[0-9a-f]{64}")


def check_prompt(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """The static prompt stays inside its ceiling and costs no read per request.

    The floors refuse a line that measured nothing. A prompt of zero bytes was
    never built, and a first build that probed no source file means the
    counters were off, in which case the zero beside it for an unchanged
    request would read as a pass it did not earn. The digest is the identity
    of the prefix, so a value that is not a SHA-256 fails as well. The
    ceilings hold what the session snapshot bought: the static bytes, no
    source read once the session captured its sources, and at most one index
    row cut mid-sentence.
    """
    for record in records:
        section = "prompt"
        static_bytes = as_int(record, "static_bytes", section)
        report.at_least(section, "static_bytes", static_bytes, 1)
        report.at_most(section, "static_bytes", static_bytes, bounds["prompt_static_bytes_max"])
        report.at_least(section, "static_tokens", as_int(record, "static_tokens", section), 1)
        report.at_least(section, "index_bytes", as_int(record, "index_bytes", section), 1)
        digest = record.get("static_sha256")
        if digest is None:
            raise GateError(f"section {section}: measurement is missing static_sha256")
        report.check(
            section,
            "static_sha256",
            len(digest),
            SHA256_HEX.fullmatch(digest) is not None,
            "64 lowercase hex digits",
        )
        clipped = as_int(record, "index_clipped_rows", section)
        report.at_least(section, "index_clipped_rows", clipped, 0)
        report.at_most(section, "index_clipped_rows", clipped, bounds["prompt_index_clipped_rows_max"])
        report.at_least(section, "warm_source_reads", as_int(record, "warm_source_reads", section), 1)
        unchanged = as_int(record, "unchanged_request_source_reads", section)
        report.at_least(section, "unchanged_request_source_reads", unchanged, 0)
        report.at_most(
            section,
            "unchanged_request_source_reads",
            unchanged,
            bounds["prompt_unchanged_request_source_reads_max"],
        )


def check_session_context(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """The session context starts no `git` outside a repository, one inside, and no GPU probe without a display.

    The floor refuses a line that measured nothing: describing a repository
    takes at least one `git` process, so a zero there means the counters were
    off, and the zeros beside it would read as a pass they did not earn.
    """
    for record in records:
        section = "session_context"
        outside = as_int(record, "git_spawns_outside_repo", section)
        report.at_least(section, "git_spawns_outside_repo", outside, 0)
        report.at_most(
            section,
            "git_spawns_outside_repo",
            outside,
            bounds["session_context_git_spawns_outside_repo_max"],
        )
        in_repo = as_int(record, "git_spawns_in_repo", section)
        report.at_least(section, "git_spawns_in_repo", in_repo, 1)
        report.at_most(section, "git_spawns_in_repo", in_repo, bounds["session_context_git_spawns_in_repo_max"])
        gpu = as_int(record, "gpu_probe_spawns_without_display", section)
        report.at_least(section, "gpu_probe_spawns_without_display", gpu, 0)
        report.at_most(
            section,
            "gpu_probe_spawns_without_display",
            gpu,
            bounds["session_context_gpu_probe_spawns_without_display_max"],
        )


def check_tool_dispatch(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """What one tool dispatch asks the allocator for.

    Wall-clock CPU time moves with the runner; the number of allocations and
    the bytes requested do not. A second serialization of the input, a deep
    copy of it, or a log field built twice raises both, so the per-dispatch
    ceilings hold the dispatcher to the work it does today. The two input
    sizes separate the fixed cost of a call from the cost that grows with the
    arguments. The floors matter as much: a counter that recorded nothing
    would pass every ceiling.
    """
    for record in records:
        section = "tool_dispatch"
        dispatches = as_int(record, "dispatches", section)
        report.at_least(section, "dispatches", dispatches, 1)
        if dispatches < 1:
            continue
        for size in ("small", "large"):
            allocations = as_int(record, f"{size}_allocs", section)
            requested = as_int(record, f"{size}_alloc_bytes", section)
            report.at_least(section, f"{size}_allocs", allocations, dispatches)
            report.at_least(section, f"{size}_alloc_bytes", requested, allocations)
            report.at_most(
                section,
                f"{size}_allocs_per_dispatch",
                per_item(allocations, dispatches),
                bounds[f"tool_dispatch_{size}_allocs_per_dispatch_max"],
            )
            report.at_most(
                section,
                f"{size}_alloc_bytes_per_dispatch",
                per_item(requested, dispatches),
                bounds[f"tool_dispatch_{size}_alloc_bytes_per_dispatch_max"],
            )


# The five situations the packages section promises. A log that carries only
# some of them is incomplete, not green: `empty`'s zeros certify nothing about a
# turn over an installed package, and `turn-warm` is what proves the counters
# record at all.
PACKAGE_SOURCES = ("empty", "turn-warm", "turn-unchanged", "call", "call-repeat")


def check_packages(report: Report, records: list[dict[str, str]], bounds: dict[str, int]) -> None:
    """What packages cost when none are installed, installed, and called.

    Five situations, and every one of them needs its floor: with the counters
    off each reports zero, which would pass every ceiling. So a turn reports the
    number of turns it ran and how many packages the catalog listed, and a call
    reports the call it made. The `empty` line is read against `turn-warm` for
    exactly this reason: the warm turn proves the same counters do record reads,
    writes and one snapshot build when there is something to measure.

    `turn-unchanged` is the promise that a session which already captured this
    catalog generation neither reads a package file nor rebuilds its snapshot,
    and `call`/`call-repeat` split "a call starts one child" from "the next call
    starts none".
    """
    for record in records:
        source = record.get("source", "?")
        section = f"packages/{source}"
        if source == "empty":
            report.at_least(section, "turns", as_int(record, "turns", section), 1)
            report.equal(section, "packages", as_int(record, "packages", section), 0)
            report.at_most(
                section,
                "area_reads",
                as_int(record, "area_reads", section),
                bounds["packages_empty_area_reads_max"],
            )
            report.at_most(
                section,
                "dir_scans",
                as_int(record, "dir_scans", section),
                bounds["packages_empty_area_dir_scans_max"],
            )
            report.at_most(
                section,
                "area_writes",
                as_int(record, "area_writes", section),
                bounds["packages_empty_area_writes_max"],
            )
            report.at_most(
                section,
                "snapshot_builds",
                as_int(record, "snapshot_builds", section),
                bounds["packages_empty_area_snapshot_builds_max"],
            )
            report.at_most(
                section,
                "child_starts",
                as_int(record, "child_starts", section),
                bounds["packages_empty_area_child_starts_max"],
            )
        elif source == "turn-warm":
            report.at_least(section, "turns", as_int(record, "turns", section), 1)
            report.at_least(section, "packages", as_int(record, "packages", section), 1)
            report.at_least(
                section,
                "area_reads",
                as_int(record, "area_reads", section),
                bounds["packages_warm_turn_reads_min"],
            )
            report.at_least(
                section,
                "snapshot_builds",
                as_int(record, "snapshot_builds", section),
                bounds["packages_warm_turn_snapshot_builds_min"],
            )
            report.at_least(
                section,
                "area_writes",
                as_int(record, "area_writes", section),
                bounds["packages_warm_turn_writes_min"],
            )
            report.at_most(
                section,
                "child_starts",
                as_int(record, "child_starts", section),
                bounds["packages_turn_child_starts_max"],
            )
        elif source == "turn-unchanged":
            report.at_least(section, "turns", as_int(record, "turns", section), 1)
            report.at_least(section, "packages", as_int(record, "packages", section), 1)
            report.at_most(
                section,
                "area_reads",
                as_int(record, "area_reads", section),
                bounds["packages_unchanged_turn_reads_max"],
            )
            report.at_most(
                section,
                "dir_scans",
                as_int(record, "dir_scans", section),
                bounds["packages_unchanged_turn_dir_scans_max"],
            )
            report.at_most(
                section,
                "area_writes",
                as_int(record, "area_writes", section),
                bounds["packages_unchanged_turn_writes_max"],
            )
            report.at_most(
                section,
                "snapshot_builds",
                as_int(record, "snapshot_builds", section),
                bounds["packages_unchanged_turn_snapshot_builds_max"],
            )
            report.at_most(
                section,
                "child_starts",
                as_int(record, "child_starts", section),
                bounds["packages_turn_child_starts_max"],
            )
        elif source == "call":
            report.at_least(section, "calls", as_int(record, "calls", section), 1)
            report.equal(
                section,
                "child_starts",
                as_int(record, "child_starts", section),
                bounds["packages_call_child_starts"],
            )
        elif source == "call-repeat":
            report.at_least(section, "calls", as_int(record, "calls", section), 1)
            report.equal(
                section,
                "child_starts",
                as_int(record, "child_starts", section),
                bounds["packages_call_repeat_child_starts"],
            )
        else:
            raise GateError(f"section packages: unknown source {source}")
    seen = {record.get("source") for record in records}
    absent = [source for source in PACKAGE_SOURCES if source not in seen]
    if absent:
        raise GateError(
            "section packages: no measurement for source(s): "
            + ", ".join(absent)
            + " (a partial log does not certify the situations it omits)"
        )


CHECKS = {
    "queue": check_queue,
    "pending": check_pending,
    "page": check_page,
    "hash": check_hash,
    "frame": check_frame,
    "soak": check_soak,
    "context": check_context,
    "irc": check_irc,
    "skill": check_skill,
    "mcp_config": check_mcp_config,
    "hooks": check_hooks,
    "prefix": check_prefix,
    "prompt": check_prompt,
    "session_context": check_session_context,
    "tool_dispatch": check_tool_dispatch,
    "packages": check_packages,
}


def skipped_sections(sections: set[str]) -> list[str]:
    """Sections that cannot run on this platform, named rather than passed.

    A skipped section is reported as skipped and excluded from the required set,
    never counted as a measurement: on Windows the soak has no Unix socket to
    drive, and the CI job that owns this gate runs on Linux.
    """
    if sys.platform == "win32":
        return [measurement["section"] for measurement in MEASUREMENTS if measurement.get("unix_only") and measurement["section"] in sections]
    return []


def missing_measurements(records: list[dict[str, str]], sections: set[str], skipped: list[str]) -> list[str]:
    """Measurements that should have printed a line and did not.

    The unit is the measurement, not the section, wherever a section is built
    from more than one: `frame` has two because the bridge and the SDK cap the
    same contract in two independent implementations, and a section that reports
    only the bridge would leave the SDK's cap ungated. A wholly absent section is
    named once, by its section, so the report stays readable.
    """
    present = {(record["section"], record.get("source")) for record in records}
    running = {record["section"] for record in records}
    per_section: dict[str, int] = {}
    for measurement in MEASUREMENTS:
        per_section[measurement["section"]] = per_section.get(measurement["section"], 0) + 1
    missing: list[str] = []
    for measurement in MEASUREMENTS:
        section = measurement["section"]
        if section not in sections or section in skipped:
            continue
        # A section with one measurement is present or absent as a whole. A
        # section built from several independent ones (queue's two call sites,
        # frame's two readers) must account for each of them.
        if section not in running:
            label = section
        elif per_section[section] == 1:
            label = None
        else:
            label = None if (section, measurement["source"]) in present else f"{section}/{measurement['source']}"
        if label is not None and label not in missing:
            missing.append(label)
    return missing

def gate(records: list[dict[str, str]], bounds: dict[str, int], sections: set[str]) -> tuple[Report, list[str], list[str]]:
    skipped = skipped_sections(sections)
    missing = missing_measurements(records, sections, skipped)
    report = Report()
    for section in REQUIRED_SECTIONS:
        if section not in sections or section in skipped:
            continue
        selected = [record for record in records if record["section"] == section]
        if not selected:
            continue
        CHECKS[section](report, selected, bounds)
    return report, missing, skipped


def emit(report: Report, missing: list[str], skipped: list[str], checked: list[str]) -> int:
    failures = report.failures()
    payload = {
        "checks": report.checks,
        "passed": len(report.checks) - len(failures),
        "failed": len(failures),
        "missing": missing,
        "skipped": skipped,
        "verdict": "pass" if not failures and not missing else "fail",
    }
    print(RESULT_PREFIX + json.dumps(payload, sort_keys=True))

    if missing:
        print("Structural performance gate: no measurement for: " + ", ".join(missing), file=sys.stderr)
        print(
            "A section that did not run is not a pass. Run `python3 scripts/perf/check_structural.py --print-plan` "
            "to see the measurement commands.",
            file=sys.stderr,
        )
        return 2

    if failures:
        print(
            "Structural performance gate failed: %d bound(s) violated "
            "(bounds live in scripts/perf/structural_budget.json and may only be raised deliberately):" % len(failures),
            file=sys.stderr,
        )
        for check in failures:
            print(
                "  - {section}: {counter} = {observed}, bound {bound}".format(**check),
                file=sys.stderr,
            )
        return 1

    print("Structural performance gate OK: %d bound(s) hold across %s" % (len(report.checks), ", ".join(checked)))
    return 0


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.print_plan:
        return print_plan()

    try:
        bounds = load_budget(args.budget)
    except GateError as error:
        print(f"Structural performance gate: {error}", file=sys.stderr)
        return 2

    sections = set(args.only) if args.only else set(REQUIRED_SECTIONS)
    unknown = sections - set(REQUIRED_SECTIONS)
    if unknown:
        print(f"Structural performance gate: unknown section(s): {', '.join(sorted(unknown))}", file=sys.stderr)
        return 2

    if args.read:
        path = Path(args.read)
        if not path.exists():
            print(f"Structural performance gate: no such log: {path}", file=sys.stderr)
            return 2
        text = path.read_text(encoding="utf-8", errors="replace")
        records = parse_lines(text)
    else:
        text = ""
        for measurement in MEASUREMENTS:
            if measurement["section"] not in sections:
                continue
            if measurement.get("unix_only") and sys.platform == "win32":
                continue
            try:
                text += run_measurement(measurement) + "\n"
            except GateError as error:
                print(f"Structural performance gate: {error}", file=sys.stderr)
                return 2
        records = parse_lines(text)
    for note in note_lines(text):
        print(note)

    try:
        report, missing, skipped = gate(records, bounds, sections)
    except GateError as error:
        print(f"Structural performance gate: {error}", file=sys.stderr)
        return 2
    checked = [
        section
        for section in REQUIRED_SECTIONS
        if section in sections and section not in skipped and any(record["section"] == section for record in records)
    ]
    return emit(report, missing, skipped, checked)


if __name__ == "__main__":
    raise SystemExit(main())
