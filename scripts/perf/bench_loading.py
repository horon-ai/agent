#!/usr/bin/env python3
"""Loading baseline: fixed-seed fixtures, real loading paths, COLD and WARM modes.

The Rust probe (`crates/base/examples/loading_probe.rs`) does the loading: skill
catalog discovery, a full session load, a startup-stub load, the owned request
snapshot, and a real save. This script only generates fixtures, runs the probe,
and aggregates - it never measures anything itself.

Two measurement modes, never mixed:

* ``cold`` - every measured run gets its own freshly copied HOME, so no derived
  cache from an earlier run is present. It answers "what does the first load of
  this fixture cost". ``--warmup`` is meaningless here and is refused.
* ``warm`` - one HOME per side, three discarded warmups per side, then the
  measured runs alternate between the two sides on those same HOMEs. It answers
  "what does a steady-state load cost". This is the mode A/B comparison uses.

The loading path is not read-only. Loading a session publishes derived caches
into the measured HOME: ``checkpoint.json``, ``sparse-index.jsonl``,
``summary.json`` and ``facade-cache.json`` plus ``.writer.lock`` in the session
directory, ``.catalog.index.jsonl`` and friends in ``<home>/sessions/``, and the
probe's own scratch session ``loading_probe_scratch``. ``events.jsonl`` itself is
not rewritten. That is why a second run on the same HOME reads less than the
first (a 1,000-event fixture drops ``attach_history`` from about 4.19 MB to
about 1.72 MB and ``full_source_hashes`` from 1 to 0) and why a sample only
means something with its mode attached.

Absolute byte counts are comparable only inside one report - never across machines, home layouts or
rounds - because the HOME path itself appears inside the cache payloads: the same binary measured an
empty fixture at 41,358 B under a short path and 42,059 B under one 80 characters longer.

What the report refuses to do:

* p99 is never reported. Sample counts here are set by ``--iterations``, and the
  baseline records p50 and p95 only.
* A counter the probe did not report stays a measurement failure for that
  fixture. A missing counter is never folded into a zero, because a zeroed
  counter would certify a win that was never measured.
* Each mode states the stability it requires and is judged only by that. WARM
  demands identical counters across the measured samples; COLD allows 1% of the
  median, because a fresh home writes a fresh catalog index whose timestamp
  digits shift a load's reads by a handful of bytes. The observed spread is
  recorded either way, so a jitter that is within tolerance is still visible.
* Both sides of every block are the same binary. That is the A/A control: the
  report can say ``control-no-change`` or ``control-drift``, never an
  improvement. Timings are not a verdict on their own; the counters are.
* Fixtures are regenerated once and compared by SHA-256. A fixture that does not
  reproduce from the same seed is a failure, not a measurement.
* A cold sample is never compared with a warm one. ``--compare`` refuses when
  the two reports were measured in different modes, and refuses anything that is
  not warm, because an A/B verdict taken across cold and warm is a verdict on
  the cache state, not on the change.

Usage:

    # steady-state baseline (the A/B comparison mode)
    python3 scripts/perf/bench_loading.py \
        --binary .target-task1/debug/examples/loading_probe \
        --mode warm --events 0,1000 --skills 0 \
        --warmup 3 --iterations 30 \
        --emit .omo/evidence/agent-rust-loading-durable-runtime/task-1-fix1/loading-warm-empty-1k.json

    # first-load baseline, one fresh HOME per sample
    python3 scripts/perf/bench_loading.py --mode cold --events 0,1000 --skills 0 ...

    # compare two warm reports
    python3 scripts/perf/bench_loading.py --compare baseline.json candidate.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# 3 added the skill/MCP read counters, the two loading stages and the fixture
# size fields; a report from before that cannot be compared field for field.
REPORT_SCHEMA_VERSION = 3
DEFAULT_SEED = 20261021
MODES = ("cold", "warm")
DEFAULT_MODE = "warm"
COMPARISON_MODE = "warm"
STAGES = (
    "skill_catalog",
    "skill_request_overlay",
    "mcp_config_load",
    "attach_history",
    "resume_ready",
    "provider_dispatch",
    "session_save",
)
# Stages whose counters describe the measured fixture. `session_save` is
# excluded from the stability claim because it recreates the scratch session,
# whose log carries timestamps and whose catalog rows accumulate. The two
# loading stages are listed by name so adding a stage never silently changes
# which stages make that claim.
STABILITY_STAGES = (
    "skill_catalog",
    "skill_request_overlay",
    "mcp_config_load",
    "attach_history",
    "resume_ready",
    "provider_dispatch",
)
REQUIRED_COUNTERS = (
    "store_read_bytes",
    "full_source_hashes",
    "request_deep_copies",
    "request_deep_copy_bytes",
    "skill_dir_reads",
    "skill_file_reads",
    "skill_bytes_read",
    "mcp_config_reads",
    "mcp_config_bytes",
)
STABILITY_RELATIVE_TOLERANCE = 0.01
STABILITY_CRITERION = {
    "warm": "identical",
    "cold": "within 1% of the median",
}
TOTAL_COUNTERS = tuple(
    dict.fromkeys(
        REQUIRED_COUNTERS
        + (
            "store_read_bytes",
            "io_read_bytes",
            "io_write_bytes",
            "io_published_bytes",
            "full_source_hash_bytes",
        )
    )
)
# Files the loading path publishes into the measured HOME. A fixture template
# that already carries one of them would silently turn a cold sample warm.
DERIVED_SESSION_FILES = (
    ".writer.lock",
    "checkpoint.json",
    "sparse-index.jsonl",
    "summary.json",
    "facade-cache.json",
)
DERIVED_HOME_FILES = (".catalog.dirty", ".catalog.index.jsonl", ".catalog.lock", ".origin-id")
MEASUREMENT_PROTOCOL = {
    "cold": (
        "COLD: every measured run gets its own freshly copied HOME; no derived cache from "
        "an earlier run exists, so each sample is a first load of the fixture."
    ),
    "warm": (
        "WARM: one HOME per side, --warmup discarded runs per side, then measured runs "
        "alternate between the two sides on those same HOMEs."
    ),
}


class MeasurementError(Exception):
    pass


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def skill_markdown(name: str, index: int, body_bytes: int) -> str:
    header = (
        "---\n"
        "name: %s\n"
        "description: fixture skill %d for the loading baseline\n"
        "---\n\n" % (name, index)
    )
    if body_bytes <= 0:
        return header + "Body of %s.\n" % name
    filler = "body of %s " % name
    body = (filler * (body_bytes // len(filler) + 1))[:body_bytes]
    return header + body


def write_skills(root: Path, count: int, body_bytes: int = 0) -> None:
    root.mkdir(parents=True, exist_ok=True)
    for index in range(count):
        name = "fixture-skill-%04d" % index
        directory = root / name
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "SKILL.md").write_text(skill_markdown(name, index, body_bytes), encoding="utf-8")


def claude_json(size: int) -> str:
    """A valid ~/.claude.json of exactly `size` bytes, padded with a JSON string."""
    prefix = '{"mcpServers":{"fixture-claude":{"command":"true"}},"padding":"'
    suffix = '"}'
    padding = size - len(prefix) - len(suffix)
    if padding < 0:
        raise MeasurementError(
            "--mcp-config-bytes %d is too small for the fixture shape (minimum %d)"
            % (size, len(prefix) + len(suffix))
        )
    return prefix + ("x" * padding) + suffix


def write_claude_json(home: Path, size: int) -> None:
    if size <= 0:
        return
    # `user_home_path` sandboxes user-level files under `<horon home>/external`.
    external = home / "external"
    external.mkdir(parents=True, exist_ok=True)
    (external / ".claude.json").write_text(claude_json(size), encoding="utf-8")


def private_directories(*paths: Path) -> None:
    """The session store refuses any directory that is not exactly owner-private."""
    for path in paths:
        path.mkdir(parents=True, exist_ok=True)
        os.chmod(path, 0o700)


def private_tree(root: Path) -> None:
    os.chmod(root, 0o700)
    for path in sorted(root.rglob("*")):
        os.chmod(path, 0o700 if path.is_dir() else 0o600)


def fixture_key(events: int, skills: int, skill_body_bytes: int, mcp_config_bytes: int) -> str:
    return "events-%d-skills-%d-body-%d-mcp-%d" % (events, skills, skill_body_bytes, mcp_config_bytes)


def derived_files(home: Path) -> list[str]:
    """Every file in `home` that the loading path, not the fixture, created."""
    found = []
    for path in sorted(home.rglob("*")):
        if not path.is_file():
            continue
        name = path.name
        if name in DERIVED_SESSION_FILES or name in DERIVED_HOME_FILES or name.startswith("local-"):
            found.append(str(path.relative_to(home)))
    return found


def generate_events(script: Path, events: int, seed: int, out_dir: Path) -> dict:
    completed = subprocess.run(
        [sys.executable, str(script), "--events", str(events), "--seed", str(seed), "--out", str(out_dir)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=600,
    )
    if completed.returncode != 0:
        raise MeasurementError(
            "fixture generation failed for %d events: %s"
            % (events, completed.stderr.decode("utf-8", "replace").strip())
        )
    return json.loads(completed.stdout.decode("utf-8", "replace").strip().splitlines()[-1])


def build_template(
    root: Path,
    events: int,
    skills: int,
    skill_body_bytes: int,
    mcp_config_bytes: int,
    seed: int,
    script: Path,
    verify: bool,
) -> dict:
    """Build the pristine fixture once. Every measured HOME is a copy of it."""
    home = root / "templates" / fixture_key(events, skills, skill_body_bytes, mcp_config_bytes)
    if home.exists():
        shutil.rmtree(home)
    write_skills(home / "skills", skills, skill_body_bytes)
    write_claude_json(home, mcp_config_bytes)
    # The MCP config stage needs a user-level source and the switch that turns
    # the Claude source on; with the default off, the fixture would never be read.
    (home / "config.toml").write_text("[mcp]\nclaude_user = true\n", encoding="utf-8")
    private_directories(home, home / "sessions")

    facts = {
        "events": events,
        "skills": skills,
        "skill_body_bytes": skill_body_bytes,
        "mcp_config_bytes": mcp_config_bytes,
        "template": str(home),
        "home": None,
        "session": "perf-events-%d" % events if events else "perf-empty",
        "log_path": None,
        "log_bytes": None,
        "sha256": None,
        "records": 0,
        "messages": 0,
        "reproduced": None,
        "derived_files_in_template": derived_files(home),
    }
    if facts["derived_files_in_template"]:
        raise MeasurementError(
            "fixture template %s already carries derived files %s; every cold sample would be warm"
            % (home, facts["derived_files_in_template"])
        )
    if events == 0:
        return facts

    session_dir = home / "sessions" / ("perf-events-%d" % events)
    summary = generate_events(script, events, seed, session_dir)
    private_directories(session_dir)
    replay_dir = root / "templates" / ("replay-%d" % events)
    private_directories(replay_dir)
    log = session_dir / "events.jsonl"
    os.chmod(log, 0o600)
    facts.update(
        {
            "log_path": str(log),
            "log_bytes": log.stat().st_size,
            "sha256": sha256_file(log),
            "records": summary["records"],
            "messages": summary["records"] - 1,
        }
    )

    if verify:
        replay = generate_events(script, events, seed, replay_dir)
        replay_log = replay_dir / "events.jsonl"
        facts["reproduced"] = sha256_file(replay_log) == facts["sha256"] and replay["records"] == facts["records"]
        shutil.rmtree(replay_dir, ignore_errors=True)
        if not facts["reproduced"]:
            raise MeasurementError(
                "seed %d did not reproduce the %d-event fixture; the baseline would not be comparable"
                % (seed, events)
            )
    return facts


def create_home(template: Path, facts: dict, destination: Path) -> Path:
    """Copy the pristine fixture to a fresh HOME and prove the copy is cold."""
    if destination.exists():
        shutil.rmtree(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(destination.parent, 0o700)
    shutil.copytree(template, destination)
    private_tree(destination)
    leftovers = derived_files(destination)
    if leftovers:
        raise MeasurementError(
            "fresh HOME %s already carries derived files %s; cold mode would measure a warm load"
            % (destination, leftovers)
        )
    if facts["sha256"] is not None:
        log = destination / "sessions" / facts["session"] / "events.jsonl"
        if not log.is_file() or sha256_file(log) != facts["sha256"]:
            raise MeasurementError("fresh HOME %s does not carry the fixture log byte for byte" % destination)
    return destination


def probe_environment(home: str) -> dict:
    env = dict(os.environ)
    env["HORON_AGENT_HOME"] = home
    env["HOME"] = home
    env["HORON_AGENT_PERF_COUNTERS"] = "1"
    # The measured fixture decides which MCP sources are in play. An ambient
    # switch would silently turn the Claude source off and report a real zero,
    # which looks like a measurement instead of a configuration change.
    env.pop("HORON_AGENT_DISABLE_CLAUDE_MCP", None)
    return env


def parse_report(stdout: str) -> dict:
    for line in reversed(stdout.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            return json.loads(line)
        except ValueError:
            continue
    raise MeasurementError("probe printed no JSON report")


def exit_code_label(returncode: int) -> str:
    """The probe's documented exit contract (see loading_probe.rs --help)."""
    if returncode == 1:
        return "1 (counter/fixture mismatch)"
    if returncode == 2:
        return "2 (preflight/unsafe-path/measurement-disabled)"
    return "%d (undocumented)" % returncode


def run_probe(binary: Path, fixture: dict, home: Path, timeout: float, mode: str) -> dict:
    command = [
        str(binary),
        "--session",
        fixture["session"],
        "--expect-events",
        str(fixture["events"]),
        "--expect-messages",
        str(fixture["messages"]),
        "--skill-count",
        str(fixture["skills"]),
    ]
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=probe_environment(str(home)),
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return {
            "status": "timeout",
            "reasons": ["probe exceeded %.1fs" % timeout],
            "command": command,
            "mode": mode,
            "home": str(home),
        }
    wall_ms = (time.perf_counter() - started) * 1000.0
    stdout = completed.stdout.decode("utf-8", "replace")
    stderr = completed.stderr.decode("utf-8", "replace")

    run = {
        "status": "ok",
        "mode": mode,
        "home": str(home),
        "wall_ms": wall_ms,
        "returncode": completed.returncode,
        "command": command,
        "reasons": [],
        "stage_ms": {},
        "stage_delta": {},
        "total_delta": {},
        "build": None,
        "counters_enabled": None,
    }
    if completed.returncode != 0:
        run["status"] = "failed"
        run["reasons"].append(
            "probe exited %s: %s"
            % (exit_code_label(completed.returncode), (stderr or stdout).strip()[-800:])
        )
        return run
    try:
        report = parse_report(stdout)
    except MeasurementError as error:
        run["status"] = "failed"
        run["reasons"].append(str(error))
        return run

    run["build"] = report.get("build")
    run["counters_enabled"] = (report.get("counters") or {}).get("enabled")
    if run["counters_enabled"] is not True:
        run["status"] = "failed"
        run["reasons"].append("stage counters were disabled; zeros are not measurements")
    if report.get("outcome") != "ok":
        run["status"] = "failed"
        run["reasons"].append("probe outcome %r: %s" % (report.get("outcome"), report.get("failures")))

    recorded_order = report.get("order")
    if recorded_order != list(STAGES):
        run["status"] = "failed"
        run["reasons"].append("probe stage order %r, expected %r" % (recorded_order, list(STAGES)))

    stages = report.get("stages") or []
    seen = [stage.get("name") for stage in stages]
    if seen != list(STAGES):
        run["status"] = "failed"
        run["reasons"].append("probe reported stages %r, expected %r" % (seen, list(STAGES)))

    for stage in stages:
        name = stage.get("name")
        if name not in STAGES:
            continue
        if not stage.get("ok"):
            run["status"] = "failed"
            run["reasons"].append("%s failed: %s" % (name, stage.get("failure")))
            continue
        run["stage_ms"][name] = float(stage.get("ms") or 0.0)
        moved = stage.get("delta") or {}
        run["stage_delta"][name] = moved
        missing = [counter for counter in REQUIRED_COUNTERS if counter not in moved]
        if missing:
            run["status"] = "failed"
            run["reasons"].append("%s is missing counters %s" % (name, ", ".join(missing)))

    if run["status"] == "ok" and int(fixture.get("mcp_config_bytes") or 0) > 0:
        reads = (run["stage_delta"].get("mcp_config_load") or {}).get("mcp_config_reads")
        if not reads:
            run["status"] = "failed"
            run["reasons"].append(
                "mcp_config_load read no config (mcp_config_reads=%r) while the fixture carries a %d-byte ~/.claude.json"
                % (reads, int(fixture["mcp_config_bytes"]))
            )

    if run["status"] == "ok":
        for name in STAGES:
            if name not in run["stage_ms"]:
                run["status"] = "failed"
                run["reasons"].append("stage %s produced no timing" % name)
        run["total_delta"] = {
            counter: sum(run["stage_delta"][name].get(counter, 0) for name in STAGES)
            for counter in TOTAL_COUNTERS
        }
    return run


def interleaved_blocks(runs_per_side: int, seed: int) -> list[list[str]]:
    generator = random.Random(seed)
    blocks = []
    for index in range(runs_per_side):
        block = ["a", "b"] if index % 2 == 0 else ["b", "a"]
        if generator.random() < 0.5:
            block.reverse()
        blocks.append(block)
    return blocks


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * quantile
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    weight = position - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def median_absolute_deviation(values: list[float]) -> float | None:
    if not values:
        return None
    center = statistics.median(values)
    return statistics.median([abs(value - center) for value in values])


def summarize(values: list[float]) -> dict:
    return {
        "samples": len(values),
        "p50_ms": percentile(values, 0.50),
        "p95_ms": percentile(values, 0.95),
        "min_ms": min(values) if values else None,
        "max_ms": max(values) if values else None,
        "mad_ms": median_absolute_deviation(values),
    }


def noise_band(samples: list[float], center: float | None) -> float | None:
    mad = median_absolute_deviation(samples)
    if mad is None or center is None:
        return None
    return max(1.4826 * mad * 3.0, 0.01 * abs(center))


def stability_values_stable(values: list[int], criterion: str) -> bool:
    """WARM demands identical counters. COLD cannot: a fresh home writes a fresh
    catalog index whose timestamp digits shift a load's reads by a few bytes, so
    the cold criterion allows 1% of the median and the spread is reported."""
    distinct = set(values)
    if len(distinct) <= 1:
        return True
    if criterion == "identical":
        return False
    center = statistics.median(values)
    if center == 0:
        return False
    return max(distinct) - min(distinct) <= STABILITY_RELATIVE_TOLERANCE * abs(center)


def stability(
    per_sample: dict[str, dict[str, list]], sides: tuple[str, ...], vacuous: bool, mode: str
) -> dict:
    """Per-counter distinct values across the measured samples, fixture stages only."""
    criterion = STABILITY_CRITERION[mode]

    def unstable_for(stages: tuple[str, ...], side: str) -> dict:
        unstable = {}
        for name in stages:
            counters = {}
            for counter, values in per_sample[side][name].items():
                if not stability_values_stable(values, criterion):
                    counters[counter] = sorted(set(values))
            if counters:
                unstable[name] = counters
        return unstable

    def spread_for(stages: tuple[str, ...], side: str) -> dict:
        spread = {}
        for name in stages:
            counters = {}
            for counter, values in per_sample[side][name].items():
                distinct = sorted(set(values))
                if len(distinct) > 1:
                    counters[counter] = {
                        "min": distinct[0],
                        "max": distinct[-1],
                        "distinct": len(distinct),
                    }
            if counters:
                spread[name] = counters
        return spread

    report = {
        "mode": mode,
        "criterion": criterion,
        "stages": list(STABILITY_STAGES),
        "note": (
            "session_save is excluded: it recreates the scratch session, whose log carries "
            "timestamps and whose catalog rows accumulate"
        ),
        "vacuous": vacuous,
        "stable": {},
        "unstable_counters": {},
        "spread": {},
        "unstable_all_stages": {},
    }
    for side in sides:
        unstable = unstable_for(STABILITY_STAGES, side)
        report["unstable_counters"][side] = unstable
        report["stable"][side] = not unstable
        report["spread"][side] = spread_for(STAGES, side)
        report["unstable_all_stages"][side] = unstable_for(STAGES, side)
    report["stable"] = all(report["stable"][side] for side in sides)
    return report


def measure_fixture(
    binary: Path,
    facts: dict,
    iterations: int,
    warmup: int,
    seed: int,
    timeout: float,
    mode: str,
    work_root: Path,
    keep: bool,
) -> dict:
    template = Path(facts["template"])
    base = work_root / mode / fixture_key(
        facts["events"], facts["skills"], facts["skill_body_bytes"], facts["mcp_config_bytes"]
    )
    homes = {}
    discarded = 0
    if mode == "warm":
        for side in ("a", "b"):
            homes[side] = create_home(template, facts, base / ("side-%s" % side))
        for _ in range(warmup):
            for side in ("a", "b"):
                run_probe(binary, facts, homes[side], timeout, mode)
                discarded += 1

    sequence = {"n": 0}

    def sample(side: str) -> dict:
        sequence["n"] += 1
        if mode == "warm":
            return run_probe(binary, facts, homes[side], timeout, mode)
        home = base / ("run-%04d-%s" % (sequence["n"], side))
        create_home(template, facts, home)
        try:
            return run_probe(binary, facts, home, timeout, mode)
        finally:
            if not keep:
                shutil.rmtree(home, ignore_errors=True)

    samples = {"a": {name: [] for name in STAGES}, "b": {name: [] for name in STAGES}}
    counters = {"a": {name: [] for name in STAGES}, "b": {name: [] for name in STAGES}}
    per_sample = {"a": {name: {} for name in STAGES}, "b": {name: {} for name in STAGES}}
    totals = {"a": {counter: [] for counter in TOTAL_COUNTERS}, "b": {counter: [] for counter in TOTAL_COUNTERS}}
    runs = {"a": [], "b": []}
    for block in interleaved_blocks(iterations, seed):
        for side in block:
            run = sample(side)
            runs[side].append(run)
            if run["status"] != "ok":
                continue
            for name in STAGES:
                samples[side][name].append(run["stage_ms"][name])
                counters[side][name].append(run["stage_delta"][name])
                for counter in REQUIRED_COUNTERS:
                    per_sample[side][name].setdefault(counter, []).append(
                        run["stage_delta"][name][counter]
                    )
            for counter in TOTAL_COUNTERS:
                totals[side][counter].append(run["total_delta"][counter])

    if mode == "warm" and not keep:
        shutil.rmtree(base, ignore_errors=True)

    builds = {json.dumps(run["build"], sort_keys=True) for run in runs["a"] + runs["b"] if run["build"]}
    reasons = []
    for side in ("a", "b"):
        for run in runs[side]:
            if run["status"] != "ok":
                reasons.extend(run["reasons"])
    if len(builds) > 1:
        reasons.append("the probe reported more than one build identity: %s" % sorted(builds))

    stages = {}
    for name in STAGES:
        left = summarize(samples["a"][name])
        right = summarize(samples["b"][name])
        delta = None
        band = None
        within = None
        if left["p50_ms"] is not None and right["p50_ms"] is not None:
            delta = right["p50_ms"] - left["p50_ms"]
            band = noise_band(samples["a"][name], left["p50_ms"])
            if band is not None:
                within = abs(delta) <= band
        missing = [
            counter
            for counter in REQUIRED_COUNTERS
            if not counters["a"][name] or any(counter not in moved for moved in counters["a"][name] + counters["b"][name])
        ]
        if left["samples"] == 0 or right["samples"] == 0:
            verdict = "measurement-failure"
            reasons.append("%s produced no valid sample" % name)
        elif missing:
            verdict = "measurement-failure"
            reasons.append("%s is missing counters %s" % (name, ", ".join(missing)))
        elif within:
            verdict = "control-no-change"
        else:
            verdict = "control-drift"
        stages[name] = {
            "a": left,
            "b": right,
            "delta_p50_ms": delta,
            "noise_band_ms": band,
            "within_noise_band": within,
            "verdict": verdict,
            "improvement": False,
            "counters": {
                "a": {counter: [moved[counter] for moved in counters["a"][name]] for counter in REQUIRED_COUNTERS},
                "b": {counter: [moved[counter] for moved in counters["b"][name]] for counter in REQUIRED_COUNTERS},
            },
        }

    deduped = sorted(set(reasons))
    return {
        "fixture": facts,
        "mode": mode,
        "protocol": MEASUREMENT_PROTOCOL[mode],
        "warmup_discarded_per_side": discarded // 2 if discarded else 0,
        "status": "measurement_failure" if deduped else "measured",
        "reasons": deduped,
        "stages": stages,
        "totals": {
            side: {counter: summarize([float(value) for value in values]) for counter, values in totals[side].items()}
            for side in ("a", "b")
        },
        "stability": stability(per_sample, ("a", "b"), vacuous=facts["events"] == 0, mode=mode),
        "runs": {"a": len(runs["a"]), "b": len(runs["b"])},
        "build": json.loads(builds.pop()) if len(builds) == 1 else None,
    }


def parse_csv(value: str, name: str) -> list[int]:
    try:
        parsed = [int(part) for part in value.split(",") if part.strip() != ""]
    except ValueError:
        raise SystemExit("--%s must be a comma-separated list of whole numbers" % name)
    if not parsed:
        raise SystemExit("--%s must not be empty" % name)
    return parsed


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Loading baseline with fixed-seed fixtures, cold and warm modes")
    parser.add_argument(
        "--binary",
        default=str(repo_root() / ".target-task1" / "debug" / "examples" / "loading_probe"),
        help="path to the loading_probe example binary",
    )
    parser.add_argument(
        "--mode",
        choices=MODES,
        default=DEFAULT_MODE,
        help="cold = fresh HOME per sample; warm = warmup then steady state on one HOME (the A/B mode)",
    )
    parser.add_argument("--events", default="0,1000,10000,100000", help="event counts to fixture")
    parser.add_argument("--skills", default="0,200,1000", help="skill counts to fixture")
    parser.add_argument(
        "--skill-body-bytes",
        type=int,
        default=0,
        help="body bytes after each fixture skill's frontmatter; 0 keeps the historical short body",
    )
    parser.add_argument(
        "--mcp-config-bytes",
        type=int,
        default=0,
        help="size of the ~/.claude.json fixture copied into every HOME; 0 (default) writes none",
    )
    parser.add_argument("--iterations", type=int, default=30, help="measured runs per side")
    parser.add_argument("--warmup", type=int, default=3, help="warmup runs per side (warm mode only)")
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--timeout", type=float, default=300.0, help="per-probe deadline in seconds")
    parser.add_argument("--work-root", default=None, help="fixture root (default: a temp dir)")
    parser.add_argument("--keep-fixtures", action="store_true", help="do not delete the generated fixtures")
    parser.add_argument("--no-verify-regeneration", action="store_true")
    parser.add_argument("--fixture-script", default=str(repo_root() / "scripts" / "perf" / "fixtures" / "gen_event_log.py"))
    parser.add_argument("--work-id", default="loading-baseline")
    parser.add_argument("--emit", default=None, help="write the JSON report here")
    parser.add_argument(
        "--compare",
        nargs=2,
        metavar=("BASELINE", "CANDIDATE"),
        default=None,
        help="compare two warm reports instead of measuring; refuses cold or mixed-mode input",
    )
    return parser.parse_args(argv)


def compare_reports(paths: list[str]) -> int:
    reports = []
    for path in paths:
        try:
            reports.append(json.loads(Path(path).read_text(encoding="utf-8")))
        except (OSError, ValueError) as error:
            print("cannot read report %s: %s" % (path, error))
            return 2
    schemas = [report.get("schema_version") for report in reports]
    if schemas[0] != schemas[1] or schemas[0] != REPORT_SCHEMA_VERSION:
        print(
            "refusing to compare: reports must share schema_version %d (found %r); "
            "regenerate the older report with this script" % (REPORT_SCHEMA_VERSION, schemas)
        )
        return 2
    modes = [report.get("mode") for report in reports]
    if any(mode not in MODES for mode in modes):
        print("refusing to compare: a report carries no usable mode label (found %r)" % (modes,))
        return 2
    if modes[0] != modes[1]:
        print("refusing to compare: reports were measured in different modes %r" % (modes,))
        return 2
    if modes[0] != COMPARISON_MODE:
        print(
            "refusing to compare: A/B comparison requires %s mode, both reports are %r"
            % (COMPARISON_MODE, modes[0])
        )
        return 2

    def index(report: dict) -> dict:
        return {
            (
                fixture["fixture"]["events"],
                fixture["fixture"]["skills"],
                fixture["fixture"].get("skill_body_bytes", 0),
                fixture["fixture"].get("mcp_config_bytes", 0),
            ): fixture
            for fixture in report.get("fixtures") or []
        }

    baseline = index(reports[0])
    candidate = index(reports[1])
    common = sorted(set(baseline) & set(candidate))
    if not common:
        print("refusing to compare: the two reports share no fixture")
        return 2
    print("mode  %s (both sides)" % modes[0])
    for events, skills, skill_body_bytes, mcp_config_bytes in common:
        left = baseline[(events, skills, skill_body_bytes, mcp_config_bytes)]
        right = candidate[(events, skills, skill_body_bytes, mcp_config_bytes)]
        label = "events=%d skills=%d body=%d mcp=%d" % (
            events,
            skills,
            skill_body_bytes,
            mcp_config_bytes,
        )
        if left.get("status") != "measured" or right.get("status") != "measured":
            print("%s: measurement failure on one side, not compared" % label)
            continue
        print(label)
        for name in STAGES:
            left_stage = left["stages"][name]
            right_stage = right["stages"][name]
            left_p50 = left_stage["a"]["p50_ms"]
            right_p50 = right_stage["a"]["p50_ms"]
            delta = None if left_p50 is None or right_p50 is None else right_p50 - left_p50
            print(
                "  %-17s p50 %9.3f -> %9.3f ms (%+9.3f ms)  verdict %s -> %s"
                % (name, left_p50 or 0.0, right_p50 or 0.0, delta or 0.0, left_stage["verdict"], right_stage["verdict"])
            )
            for counter in REQUIRED_COUNTERS:
                before = sorted(set(left_stage.get("counters", {}).get("a", {}).get(counter) or []))
                after = sorted(set(right_stage.get("counters", {}).get("a", {}).get(counter) or []))
                if not before or not after:
                    print(
                        "      counter %s: not measured (baseline %r, candidate %r)" % (counter, before, after)
                    )
                elif before != after:
                    print("      counter %s: %s -> %s" % (counter, before, after))
    return 0


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.compare:
        return compare_reports(args.compare)
    if args.skill_body_bytes < 0:
        print("--skill-body-bytes must be zero or positive")
        return 2
    if args.mcp_config_bytes < 0:
        print("--mcp-config-bytes must be zero or positive")
        return 2
    if args.mcp_config_bytes > 0:
        try:
            claude_json(args.mcp_config_bytes)
        except MeasurementError as error:
            print(str(error))
            return 2

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print("probe binary not found: %s" % binary)
        print("Build it: cargo build --locked -p horon-agent-base --example loading_probe")
        return 2
    digest = sha256_file(binary)
    script = Path(args.fixture_script).resolve()

    root = Path(args.work_root) if args.work_root else Path(tempfile.mkdtemp(prefix="horon-loading-bench-"))
    root.mkdir(parents=True, exist_ok=True)
    os.chmod(root, 0o700)
    print("binary   %s (%s)" % (binary, digest[:12]))
    print("mode     %s" % args.mode)
    print("protocol %s" % MEASUREMENT_PROTOCOL[args.mode])
    print("fixtures %s" % root)
    if args.mode == "cold" and args.warmup > 0:
        print("note     --warmup %d is ignored in cold mode: every sample gets a fresh HOME" % args.warmup)

    event_counts = parse_csv(args.events, "events")
    skill_counts = parse_csv(args.skills, "skills")
    results = []
    failure = False
    try:
        for events in event_counts:
            for skills in skill_counts:
                facts = build_template(
                    root,
                    events,
                    skills,
                    args.skill_body_bytes,
                    args.mcp_config_bytes,
                    args.seed,
                    script,
                    verify=not args.no_verify_regeneration,
                )
                measured = measure_fixture(
                    binary,
                    facts,
                    args.iterations,
                    args.warmup if args.mode == "warm" else 0,
                    args.seed,
                    args.timeout,
                    args.mode,
                    root,
                    args.keep_fixtures,
                )
                results.append(measured)
                if measured["status"] != "measured" or not measured["stability"]["stable"]:
                    failure = True
                print(
                    "fixture events=%d skills=%d body=%d mcp=%d mode=%s -> %s (fixture-stage counters stable: %s, criterion %s%s)"
                    % (
                        events,
                        skills,
                        args.skill_body_bytes,
                        args.mcp_config_bytes,
                        args.mode,
                        measured["status"],
                        measured["stability"]["stable"],
                        measured["stability"]["criterion"],
                        " - vacuous, this fixture has no session" if measured["stability"]["vacuous"] else "",
                    )
                )
                for name in STAGES:
                    stage = measured["stages"][name]
                    left, right = stage["a"], stage["b"]
                    print(
                        "  %-17s p50 %8.3f/%-8.3f p95 %8.3f/%-8.3f delta %+7.3f ms  %s"
                        % (
                            name,
                            left["p50_ms"] or 0.0,
                            right["p50_ms"] or 0.0,
                            left["p95_ms"] or 0.0,
                            right["p95_ms"] or 0.0,
                            stage["delta_p50_ms"] or 0.0,
                            stage["verdict"],
                        )
                    )
                totals = measured["totals"]["a"]
                print(
                    "  total store_read_bytes p50 %.0f  full_source_hashes p50 %.0f  skill_dir_reads p50 %.0f"
                    % (
                        totals["store_read_bytes"]["p50_ms"] or 0.0,
                        totals["full_source_hashes"]["p50_ms"] or 0.0,
                        totals["skill_dir_reads"]["p50_ms"] or 0.0,
                    )
                )
                print(
                    "  total skill_file_reads p50 %.0f  skill_bytes_read p50 %.0f  mcp_config_reads p50 %.0f  mcp_config_bytes p50 %.0f"
                    % (
                        totals["skill_file_reads"]["p50_ms"] or 0.0,
                        totals["skill_bytes_read"]["p50_ms"] or 0.0,
                        totals["mcp_config_reads"]["p50_ms"] or 0.0,
                        totals["mcp_config_bytes"]["p50_ms"] or 0.0,
                    )
                )
                if not measured["stability"]["stable"]:
                    for side, unstable in measured["stability"]["unstable_counters"].items():
                        for name, counters in unstable.items():
                            print("  UNSTABLE %s %s %s" % (side, name, counters))
                for side, unstable in measured["stability"]["unstable_all_stages"].items():
                    for name in unstable:
                        if name not in measured["stability"]["unstable_counters"].get(side, {}):
                            print("  DRIFT    %s %s %s (outside the stability claim)" % (side, name, unstable[name]))
                for side, spread in measured["stability"]["spread"].items():
                    for name, counters in spread.items():
                        for counter, detail in counters.items():
                            print(
                                "  SPREAD   %s %s %s %s..%s (%d distinct values)"
                                % (side, name, counter, detail["min"], detail["max"], detail["distinct"])
                            )
                for reason in measured["reasons"]:
                    print("  FAIL %s" % reason)
    finally:
        if not args.keep_fixtures and args.work_root is None:
            shutil.rmtree(root, ignore_errors=True)

    after = sha256_file(binary)
    if after != digest:
        failure = True
        print("FAIL the probe binary changed while it was measured")

    report = {
        "schema_version": REPORT_SCHEMA_VERSION,
        "work_id": args.work_id,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "mode": args.mode,
        "protocol": {
            "mode": args.mode,
            "definition": MEASUREMENT_PROTOCOL[args.mode],
            "warmup_per_side": args.warmup if args.mode == "warm" else 0,
            "warmup_requested": args.warmup,
            "measured_per_side": args.iterations,
            "alternating": True,
            "seed": args.seed,
            "cold": MEASUREMENT_PROTOCOL["cold"],
            "warm": MEASUREMENT_PROTOCOL["warm"],
            "comparison_mode": COMPARISON_MODE,
        },
        "binary": {"path": str(binary), "sha256": digest, "sha256_after": after},
        "aa_control": True,
        "same_binary_both_sides": True,
        "improvement_claim": False,
        "p99": "not reported: the baseline records p50 and p95 only",
        "load_path_writes": {
            "session_directory": list(DERIVED_SESSION_FILES),
            "home_sessions_directory": list(DERIVED_HOME_FILES),
            "scratch_session": "loading_probe_scratch",
            "events_jsonl": "not rewritten; the probe records sha256 before and after",
        },
        "modes_compared": False,
        "stages": list(STAGES),
        "stability_stages": list(STABILITY_STAGES),
        "stability_criterion": STABILITY_CRITERION[args.mode],
        "required_counters": list(REQUIRED_COUNTERS),
        "counts": {
            "fixtures": len(results),
            "measurement_failures": sum(1 for result in results if result["status"] != "measured"),
            "unstable_fixtures": sum(1 for result in results if not result["stability"]["stable"]),
        },
        "fixtures": results,
        "work_root": str(root),
        "fixtures_kept": bool(args.keep_fixtures or args.work_root),
    }
    text = json.dumps(report, indent=2, sort_keys=True)
    if args.emit:
        target = Path(args.emit)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text + "\n", encoding="utf-8")
        print("report   %s" % target)
    print(
        "mode %s, fixtures %d, measurement failures %d, unstable fixtures %d, verdict %s"
        % (
            args.mode,
            report["counts"]["fixtures"],
            report["counts"]["measurement_failures"],
            report["counts"]["unstable_fixtures"],
            "measurement-failure" if failure else "control-no-change",
        )
    )
    return 2 if failure else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
