#!/usr/bin/env python3
"""Same-machine A/B performance runner.

Compares two horon-agent binaries on this machine and emits one JSON report with
every number the measurement evidence needs. It refuses to produce a verdict it
cannot support:

* a candidate built with a different profile or feature set is rejected before
  anything is timed, because that pair measures the build, not the change;
* p99 is reported only at 20 samples or more, and refused with a stated reason
  below that;
* a metric the workload did not report stays null and is named in
  ``counts.missing`` instead of becoming 0, because a zeroed metric certifies a
  fake win;
* the binary digest is re-read after the measurement, so a binary swapped
  underneath the run aborts it rather than silently labelling the wrong binary;
* every child runs in its own process group with a timeout, and the whole group
  is killed and reaped when the deadline passes;
* OS page caches are never dropped, and a macOS RSS reading is never presented
  as a Linux PSS reading.

Usage:

    python3 scripts/perf/ab_perf.py \
        --baseline target/release/horon-agent \
        --candidate target/release/horon-agent-candidate \
        --workload '["{binary}", "version", "--json"]' \
        --warmup 3 --iterations 10 \
        --emit .omo/evidence/report.json

A B0-vs-B0 run passes the same binary twice. Its median delta must fall inside
the reported noise band; that is the control for "no false improvement".
"""

import argparse
import json
import os
import platform
import random
import resource
import signal
import statistics
import subprocess
import sys
import time

REPORT_SCHEMA_VERSION = 1
P99_MIN_SAMPLES = 20
DEFAULT_TIMEOUT_SECONDS = 60.0
# Metric names the report claims to carry. A side that does not report one gets
# null, and the name lands in counts.missing.
RESOURCE_METRICS = (
    "cpu_user_ms",
    "cpu_system_ms",
    "read_bytes",
    "write_bytes",
    "allocated_bytes",
    "retained_bytes",
    "rss_bytes",
    "peak_rss_bytes",
    "pss_bytes",
    "queue_depth_high_water",
    "queue_bytes_high_water",
    "pending_requests_high_water",
    "fd_count",
    "thread_count",
)


class RunnerError(Exception):
    pass


def sha256_file(path):
    import hashlib

    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def binary_facts(path):
    """Identity of one binary, including the build inputs a comparison needs."""
    if not os.path.isfile(path):
        raise RunnerError("binary not found: %s" % path)
    if not os.access(path, os.X_OK):
        raise RunnerError("binary is not executable: %s" % path)
    facts = {
        "path": os.path.abspath(path),
        "sha256": sha256_file(path),
        "bytes": os.path.getsize(path),
        "mtime": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(os.path.getmtime(path))),
    }
    version = read_version(path)
    facts["version"] = version.get("version")
    facts["git_hash"] = version.get("git_hash")
    facts["profile"] = version.get("build_profile")
    facts["features"] = version.get("build_features")
    facts["release_build"] = version.get("release_build")
    facts["semver"] = version.get("semver")
    return facts


def read_version(path):
    try:
        completed = subprocess.run(
            [path, "version", "--json"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=120,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise RunnerError("cannot read version from %s: %s" % (path, error))
    if completed.returncode != 0:
        raise RunnerError(
            "`%s version --json` exited %d: %s"
            % (path, completed.returncode, completed.stderr.decode("utf-8", "replace").strip())
        )
    try:
        return json.loads(completed.stdout.decode("utf-8", "replace"))
    except ValueError as error:
        raise RunnerError("`%s version --json` did not print JSON: %s" % (path, error))


def require_comparable(baseline, candidate):
    """Refuse a pair whose build inputs differ, naming what differs."""
    problems = []
    for key in ("profile", "features"):
        left, right = baseline.get(key), candidate.get(key)
        if left is None or right is None:
            problems.append(
                "cannot read %s from both binaries (baseline=%r candidate=%r); "
                "rebuild both with a binary that reports it" % (key, left, right)
            )
        elif left != right:
            problems.append("%s differs: baseline=%r candidate=%r" % (key, left, right))
    if problems:
        raise RunnerError(
            "refusing to compare binaries with different build inputs:\n  "
            + "\n  ".join(problems)
        )


def machine_facts():
    facts = {
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "python": platform.python_version(),
    }
    facts["cpu"] = cpu_brand()
    facts["cpu_count"] = os.cpu_count()
    facts["memory_bytes"] = total_memory_bytes()
    return facts


def cpu_brand():
    if platform.system() == "Darwin":
        try:
            completed = subprocess.run(
                ["sysctl", "-n", "machdep.cpu.brand_string"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=10,
            )
            text = completed.stdout.decode("utf-8", "replace").strip()
            if text:
                return text
        except (OSError, subprocess.SubprocessError):
            pass
    try:
        with open("/proc/cpuinfo", "r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if line.lower().startswith("model name"):
                    return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or None


def total_memory_bytes():
    if platform.system() == "Darwin":
        try:
            completed = subprocess.run(
                ["sysctl", "-n", "hw.memsize"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=10,
            )
            return int(completed.stdout.decode().strip())
        except (OSError, ValueError, subprocess.SubprocessError):
            return None
    try:
        with open("/proc/meminfo", "r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if line.startswith("MemTotal:"):
                    return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        pass
    return None


def toolchain_facts():
    facts = {"python": platform.python_version()}
    for name, command in (("rustc", ["rustc", "--version"]), ("cargo", ["cargo", "--version"])):
        try:
            completed = subprocess.run(
                command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30
            )
            facts[name] = completed.stdout.decode("utf-8", "replace").strip() or None
        except (OSError, subprocess.SubprocessError):
            facts[name] = None
    return facts


def git_facts(repo):
    facts = {}
    for key, command in (
        ("head", ["git", "-C", repo, "rev-parse", "HEAD"]),
        ("describe", ["git", "-C", repo, "describe", "--tags", "--always"]),
        ("dirty", ["git", "-C", repo, "status", "--porcelain"]),
    ):
        try:
            completed = subprocess.run(
                command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30
            )
            text = completed.stdout.decode("utf-8", "replace").strip()
            facts[key] = (text != "") if key == "dirty" else (text or None)
        except (OSError, subprocess.SubprocessError):
            facts[key] = None
    return facts


def fixture_facts(paths):
    facts = []
    for path in paths:
        if not os.path.isfile(path):
            raise RunnerError("fixture not found: %s" % path)
        facts.append(
            {"path": os.path.abspath(path), "sha256": sha256_file(path), "bytes": os.path.getsize(path)}
        )
    return facts


def parse_counters(text):
    """Extract the stage-counter object from a workload's stdout.

    The workload may print the counters bare or nested under a `counters` key;
    either shape is accepted, and anything else on stdout is ignored. No counter
    found returns None, which the caller records as a missing metric rather than
    as zeros.
    """
    if not text:
        return None
    for value in reversed(json_objects(text)):
        if not isinstance(value, dict):
            continue
        nested = value.get("counters")
        if isinstance(nested, dict) and "enabled" in nested:
            return nested
        if "enabled" in value:
            return value
    return None


def parse_process_snapshot(text):
    if not text:
        return None
    for candidate in json_objects(text):
        if isinstance(candidate, dict) and isinstance(candidate.get("process"), dict):
            return candidate["process"]
    return None


def json_objects(text):
    """Every JSON object in `text`, outermost first, nested ones included."""
    found = []
    depth = 0
    start = None
    for index, character in enumerate(text):
        if character == "{":
            if depth == 0:
                start = index
            depth += 1
        elif character == "}" and depth > 0:
            depth -= 1
            if depth == 0 and start is not None:
                try:
                    found.append(json.loads(text[start : index + 1]))
                except ValueError:
                    pass
                start = None
    return found


def child_cpu_ms():
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime * 1000.0, usage.ru_stime * 1000.0


def run_once(command, timeout, env=None):
    """Run one child in its own process group, killing the group on timeout.

    CPU is the delta of reaped-children usage around this one child; children run
    serially, so no other reaped child moves the counters inside the window.
    """
    cpu_before = child_cpu_ms()
    started = time.perf_counter()
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        start_new_session=True,
    )
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        kill_process_group(process)
        try:
            stdout, stderr = process.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            stdout, stderr = b"", b""
    wall_ms = (time.perf_counter() - started) * 1000.0
    cpu_after = child_cpu_ms()
    return {
        "cpu_user_ms": max(cpu_after[0] - cpu_before[0], 0.0),
        "cpu_system_ms": max(cpu_after[1] - cpu_before[1], 0.0),
        "returncode": process.returncode,
        "timed_out": timed_out,
        "wall_ms": wall_ms,
        "stdout": stdout.decode("utf-8", "replace"),
        "stderr": stderr.decode("utf-8", "replace"),
    }


def kill_process_group(process):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(os.getpgid(process.pid), sig)
        except (ProcessLookupError, PermissionError, OSError):
            return
        try:
            process.wait(timeout=10)
            return
        except subprocess.TimeoutExpired:
            continue


def expand_workload(template, binary):
    return [part.replace("{binary}", binary) for part in template]


def percentile(values, quantile):
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


def median_absolute_deviation(values):
    if not values:
        return None
    center = statistics.median(values)
    return statistics.median([abs(value - center) for value in values])


def summarize(samples, extra_p99=False):
    """Timing summary. p99 is always stated: a number at 20+ samples, a refusal below."""
    summary = {
        "samples": len(samples),
        "median_ms": percentile(samples, 0.50),
        "p95_ms": percentile(samples, 0.95),
        "min_ms": min(samples) if samples else None,
        "max_ms": max(samples) if samples else None,
        "mad_ms": median_absolute_deviation(samples),
    }
    if len(samples) >= P99_MIN_SAMPLES:
        summary["p99_ms"] = percentile(samples, 0.99)
        summary["p99_refused"] = None
    else:
        summary["p99_ms"] = None
        summary["p99_refused"] = "requires at least %d samples, got %d%s" % (
            P99_MIN_SAMPLES,
            len(samples),
            "" if extra_p99 else " (and --p99 was not requested)",
        )
    return summary


def noise_band(baseline_samples, median_ms):
    """Half-width of the band a delta must exceed to count as a change.

    Three scaled MADs, which is the usual robust stand-in for three standard
    deviations, floored at 1% of the baseline median so an unrealistically tight
    spread cannot turn sub-millisecond jitter into a claimed improvement.
    """
    mad = median_absolute_deviation(baseline_samples)
    if mad is None or median_ms is None:
        return None, None
    scaled = 1.4826 * mad * 3.0
    floor = 0.01 * abs(median_ms)
    return max(scaled, floor), "max(3*1.4826*MAD, 1% of baseline median)"


def interleaved_blocks(runs_per_side, seed):
    """Blocks of two runs, alternating which side leads, to cancel drift."""
    generator = random.Random(seed)
    blocks = []
    for index in range(runs_per_side):
        block = ["baseline", "candidate"]
        if index % 2 == 1:
            block.reverse()
        if generator.random() < 0.5:
            block.reverse()
        blocks.append(block)
    return blocks


def measure_side(name, binary, template, timeout, env):
    command = expand_workload(template, binary)
    result = run_once(command, timeout, env)
    counters = parse_counters(result["stdout"])
    process = parse_process_snapshot(result["stdout"])
    metrics = {
        "cpu_user_ms": result["cpu_user_ms"],
        "cpu_system_ms": result["cpu_system_ms"],
    }
    if counters:
        metrics["read_bytes"] = counters.get("io_read_bytes")
        metrics["write_bytes"] = counters.get("io_write_bytes")
        metrics["queue_depth_high_water"] = counters.get("queue_depth_high_water")
        metrics["queue_bytes_high_water"] = counters.get("queue_bytes_high_water")
        metrics["pending_requests_high_water"] = counters.get("pending_requests_high_water")
        metrics["hash_bytes_read"] = counters.get("hash_bytes_read")
        metrics["blob_bytes_read"] = counters.get("blob_bytes_read")
        metrics["page_hydrations"] = counters.get("page_hydrations")
        metrics["interrupt_poll_wakeups"] = counters.get("interrupt_poll_wakeups")
        metrics["counters_enabled"] = counters.get("enabled")
    if process:
        metrics["rss_bytes"] = process.get("rss_bytes")
        metrics["pss_bytes"] = process.get("pss_bytes")
        metrics["fd_count"] = process.get("fd_count")
        metrics["thread_count"] = process.get("thread_count")
        metrics["peak_rss_bytes"] = process.get("peak_rss_bytes")
        metrics["allocated_bytes"] = process.get("allocator_live_bytes")
        metrics["retained_bytes"] = process.get("allocator_retained_bytes")
    return {
        "side": name,
        "wall_ms": result["wall_ms"],
        "returncode": result["returncode"],
        "timed_out": result["timed_out"],
        "stderr_tail": result["stderr"][-2000:],
        "metrics": metrics,
    }


def side_resource_summary(runs):
    summary = {}
    missing = []
    for metric in RESOURCE_METRICS:
        values = [
            run["metrics"][metric]
            for run in runs
            if run["metrics"].get(metric) is not None
        ]
        if not values:
            summary[metric] = None
            missing.append(metric)
            continue
        summary[metric] = {
            "samples": len(values),
            "median": statistics.median(values),
            "max": max(values),
        }
    return summary, missing


def run_correctness(command, timeout):
    if not command:
        return {"suite": None, "result": "not_run", "command": None, "detail": "no suite supplied"}
    result = run_once(["/bin/sh", "-c", command], timeout)
    if result["timed_out"]:
        return {
            "suite": command,
            "result": "timeout",
            "command": command,
            "detail": "exceeded %.1fs" % timeout,
        }
    return {
        "suite": command,
        "result": "pass" if result["returncode"] == 0 else "fail",
        "command": command,
        "detail": (result["stdout"] + result["stderr"])[-4000:],
    }


def build_report(args):
    baseline = binary_facts(args.baseline)
    candidate = binary_facts(args.candidate)
    if not args.skip_comparable_check:
        require_comparable(baseline, candidate)

    same_binary = baseline["sha256"] == candidate["sha256"]
    template = json.loads(args.workload)
    if not isinstance(template, list) or not template:
        raise RunnerError("--workload must be a JSON array of arguments")

    env = dict(os.environ)
    env.update({"HORON_AGENT_PERF_COUNTERS": env.get("HORON_AGENT_PERF_COUNTERS", "1")})

    counts = {"failure": 0, "timeout": 0, "missing": 0}
    warmup_runs = {"baseline": [], "candidate": []}
    for _ in range(args.warmup):
        for side, binary in (("baseline", baseline["path"]), ("candidate", candidate["path"])):
            run = measure_side(side, binary, template, args.timeout, env)
            warmup_runs[side].append(run)
            if run["timed_out"]:
                counts["timeout"] += 1

    measured = {"baseline": [], "candidate": []}
    for block in interleaved_blocks(args.iterations, args.seed):
        for side in block:
            binary = baseline["path"] if side == "baseline" else candidate["path"]
            run = measure_side(side, binary, template, args.timeout, env)
            if run["timed_out"]:
                counts["timeout"] += 1
            elif run["returncode"] != 0:
                counts["failure"] += 1
            else:
                measured[side].append(run)

    baseline_times = [run["wall_ms"] for run in measured["baseline"]]
    candidate_times = [run["wall_ms"] for run in measured["candidate"]]
    baseline_timing = summarize(baseline_times, args.p99)
    candidate_timing = summarize(candidate_times, args.p99)

    delta_ms = None
    delta_pct = None
    band_ms = None
    band_method = None
    within_band = None
    if baseline_timing["median_ms"] is not None and candidate_timing["median_ms"] is not None:
        delta_ms = candidate_timing["median_ms"] - baseline_timing["median_ms"]
        if baseline_timing["median_ms"]:
            delta_pct = 100.0 * delta_ms / baseline_timing["median_ms"]
        band_ms, band_method = noise_band(baseline_times, baseline_timing["median_ms"])
        if band_ms is not None:
            within_band = abs(delta_ms) <= band_ms

    baseline_resources, baseline_missing = side_resource_summary(measured["baseline"])
    candidate_resources, candidate_missing = side_resource_summary(measured["candidate"])
    missing_metrics = sorted(set(baseline_missing) | set(candidate_missing))
    counts["missing"] = len(missing_metrics)

    # The digest is re-read after the measurement so a binary replaced mid-run
    # cannot be reported as the one that was timed.
    baseline_after = sha256_file(baseline["path"])
    candidate_after = sha256_file(candidate["path"])
    if baseline_after != baseline["sha256"] or candidate_after != candidate["sha256"]:
        raise RunnerError(
            "a binary changed while it was being measured; the timings do not belong "
            "to the digests in this report (baseline %s -> %s, candidate %s -> %s)"
            % (baseline["sha256"], baseline_after, candidate["sha256"], candidate_after)
        )

    if args.require_counters and "read_bytes" in missing_metrics:
        raise RunnerError(
            "the workload reported no stage counters; %s is missing. Run a workload that "
            "prints them (for example the perf_counters_surface example)." % "read_bytes"
        )

    interpretation, adoption = interpret(
        delta_ms,
        delta_pct,
        band_ms,
        within_band,
        same_binary,
        counts,
        min(len(baseline_times), len(candidate_times)),
    )

    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "work_id": args.work_id,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "baseline": baseline,
        "candidate": candidate,
        "same_binary": same_binary,
        "machine": machine_facts(),
        "toolchain": toolchain_facts(),
        "source": git_facts(args.repo),
        "workload": {
            "argv_template": template,
            "timeout_seconds": args.timeout,
            "counters_env": "HORON_AGENT_PERF_COUNTERS=%s" % env["HORON_AGENT_PERF_COUNTERS"],
        },
        "fixtures": fixture_facts(args.fixture),
        "iterations": {
            "warmup": args.warmup,
            "measured_per_side": args.iterations,
            "seed": args.seed,
            "order": "interleaved blocks of two, alternating leader",
        },
        "counts": counts,
        "timing": {
            "baseline": baseline_timing,
            "candidate": candidate_timing,
            "delta_ms": delta_ms,
            "delta_pct": delta_pct,
            "noise_band_ms": band_ms,
            "noise_band_method": band_method,
            "within_noise_band": within_band,
        },
        "resources": {
            "baseline": baseline_resources,
            "candidate": candidate_resources,
            "missing_metrics": missing_metrics,
            "notes": [
                "read_bytes/write_bytes come from the workload's own stage counters.",
                "allocated_bytes is a point-in-time allocator reading, not a peak; peak_rss_bytes is the only peak in this report.",
                "rss_bytes is a macOS reading; it is never compared against a Linux PSS.",
                "each side's peak is that process's own; peaks from separate processes are never summed.",
                "cpu_user_ms/cpu_system_ms are reaped-child CPU deltas around each run, not whole-machine CPU.",
            ],
        },
        "correctness": run_correctness(args.correctness_cmd, args.timeout),
        "interpretation": interpretation,
        "adoption_decision": adoption,
        "rollback_commit": args.rollback_commit,
    }


def interpret(delta_ms, delta_pct, band_ms, within_band, same_binary, counts, samples_per_side):
    if delta_ms is None:
        return (
            "No timing comparison: one side produced no measured run "
            "(%d failures, %d timeouts)." % (counts["failure"], counts["timeout"]),
            "blocked",
        )
    if within_band:
        if same_binary:
            return (
                "Control run: the same binary on both sides. The median delta "
                "%.2f ms (%.2f%%) is inside the noise band of +/-%.2f ms, so the "
                "runner reports no improvement, which is the correct reading."
                % (delta_ms, delta_pct or 0.0, band_ms or 0.0),
                "control-no-change",
            )
        return (
            "The median delta %.2f ms (%.2f%%) is inside the noise band of +/-%.2f ms: "
            "no change is claimable at this sample count." % (delta_ms, delta_pct or 0.0, band_ms or 0.0),
            "not-adopted-no-measurable-change",
        )
    if delta_ms < 0:
        return (
            "The candidate median is %.2f ms (%.2f%%) lower, beyond the noise band of "
            "+/-%.2f ms, over %d measured runs per side."
            % (abs(delta_ms), abs(delta_pct or 0.0), band_ms or 0.0, samples_per_side),
            "adopt-pending-correctness",
        )
    return (
        "The candidate median is %.2f ms (%.2f%%) higher, beyond the noise band of "
        "+/-%.2f ms: a regression, not an improvement."
        % (abs(delta_ms), abs(delta_pct or 0.0), band_ms or 0.0),
        "reject-regression",
    )


def human_summary(report):
    timing = report["timing"]
    lines = [
        "work id      %s" % report["work_id"],
        "baseline     %s %s" % (report["baseline"]["sha256"][:12], report["baseline"]["path"]),
        "candidate    %s %s" % (report["candidate"]["sha256"][:12], report["candidate"]["path"]),
        "same binary  %s" % report["same_binary"],
        "profile      %s / features %s"
        % (report["baseline"]["profile"], ",".join(report["baseline"]["features"] or [])),
        "samples      baseline %d, candidate %d (warmup %d)"
        % (
            timing["baseline"]["samples"],
            timing["candidate"]["samples"],
            report["iterations"]["warmup"],
        ),
        "median       baseline %.2f ms, candidate %.2f ms"
        % (timing["baseline"]["median_ms"] or 0.0, timing["candidate"]["median_ms"] or 0.0),
        "p95          baseline %.2f ms, candidate %.2f ms"
        % (timing["baseline"]["p95_ms"] or 0.0, timing["candidate"]["p95_ms"] or 0.0),
        "delta        %s ms (%s%%)"
        % (
            "n/a" if timing["delta_ms"] is None else "%.2f" % timing["delta_ms"],
            "n/a" if timing["delta_pct"] is None else "%.2f" % timing["delta_pct"],
        ),
        "noise band   +/-%s ms, within=%s"
        % (
            "n/a" if timing["noise_band_ms"] is None else "%.2f" % timing["noise_band_ms"],
            timing["within_noise_band"],
        ),
        "counts       failure %d, timeout %d, missing %d"
        % (
            report["counts"]["failure"],
            report["counts"]["timeout"],
            report["counts"]["missing"],
        ),
        "correctness  %s" % report["correctness"]["result"],
        "decision     %s" % report["adoption_decision"],
    ]
    if timing["baseline"].get("p99_refused"):
        lines.append("p99          refused: %s" % timing["baseline"]["p99_refused"])
    return "\n".join(lines)


def parse_args(argv):
    parser = argparse.ArgumentParser(description="Same-machine A/B performance runner")
    parser.add_argument("--baseline", required=True, help="path to the B0 binary")
    parser.add_argument("--candidate", required=True, help="path to the candidate binary")
    parser.add_argument("--work-id", default="unlabelled")
    parser.add_argument(
        "--workload",
        default='["{binary}", "version", "--json"]',
        help="JSON array of arguments; {binary} is replaced by each side's path",
    )
    parser.add_argument("--iterations", type=int, default=10, help="measured runs per side")
    parser.add_argument("--warmup", type=int, default=2, help="warmup runs per side")
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--seed", type=int, default=20261021)
    parser.add_argument("--fixture", action="append", default=[], help="fixture path to hash")
    parser.add_argument("--correctness-cmd", default=None)
    parser.add_argument("--rollback-commit", default=None)
    parser.add_argument("--repo", default=".")
    parser.add_argument("--emit", default=None, help="write the JSON report here")
    parser.add_argument("--p99", action="store_true", help="ask for p99; refused below 20 samples")
    parser.add_argument("--require-counters", action="store_true")
    parser.add_argument(
        "--skip-comparable-check",
        action="store_true",
        help="measure anyway when profiles differ (the report then says so)",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv if argv is not None else sys.argv[1:])
    try:
        report = build_report(args)
    except RunnerError as error:
        sys.stderr.write("ab_perf: %s\n" % error)
        return 2
    text = json.dumps(report, indent=2, sort_keys=True)
    if args.emit:
        directory = os.path.dirname(os.path.abspath(args.emit))
        if directory:
            os.makedirs(directory, exist_ok=True)
        with open(args.emit, "w", encoding="utf-8") as handle:
            handle.write(text)
            handle.write("\n")
    print(human_summary(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
