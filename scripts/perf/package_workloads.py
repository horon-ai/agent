#!/usr/bin/env python3
"""What user-installed packages cost: nothing installed, installed, and called.

Three scenarios, each one turn of the real `horon-agent` binary against the
mock model on localhost, all on one runner and in one process:

- ``empty``     no package area at all; the turn that every user who never
                installed a package runs.
- ``installed`` the example package installed from the shipped layouts and never
                called: the turn loads the declaration and lists the tool, and
                the model answers without issuing a call.
- ``called``    the same install, and the model issues one real
                ``ext__hello__greet`` call, so the child starts and answers.

Every run records the turn's wall time, the time from process start to the first
model request, peak tree RSS and whether a tool round trip really happened. The
size facts (the agent binary, the installed package area, its file count and the
example's own binary) are recorded once per build because a size does not need a
median.

A second build (``--baseline-binary``, the `dev` merge base built beside the
candidate) is measured on the same machine, alternating rows, so the cells both
builds support are a before/after comparison rather than two unrelated numbers.
The new surfaces (an installed package's on-disk size) have no baseline when the
baseline predates packages, and are then recorded as absolute values with the
comparison left out.

The observation subscription is removed from the installed fixture on purpose:
the shipped example subscribes to `turn.start`, and a package that observes a
turn is *started* by that turn, so keeping it would make "installed but never
called" measure a child start instead. `empty` and `installed` use the same
prompt and no package marker, so the only difference between them is what is
installed; `called` adds the marker the mock answers with one package call.

`horon-tui` is a separate binary with its own suite and is never measured here.
"""

from __future__ import annotations

import argparse
import os
import platform
import re
import shutil
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import bench_common as bc
import bench_compare as bcmp
import mock_llm_server as mm

SCENARIO_EMPTY = "empty"
SCENARIO_INSTALLED = "installed"
SCENARIO_CALLED = "called"
SCENARIOS = (SCENARIO_EMPTY, SCENARIO_INSTALLED, SCENARIO_CALLED)

# The prompt the mock answers without a tool call: no marker, and no shell tool
# is offered, so the turn is one model request.
PLAIN_PROMPT = "Reply with mock-ok and nothing else."
# The prompt the mock answers with one real call to the installed package's tool.
CALL_PROMPT = f"{mm.PACKAGE_MARKER} Greet one name with the installed package tool, then reply with mock-ok."

DEFAULT_PACKAGE_SOURCE = "examples/packages/example/layouts"
PACKAGE_ID = "hello"
PACKAGE_TOOL = "greet"


def package_tool_name() -> str:
    """The registry name the host gives the example's greet tool."""
    return f"ext__{PACKAGE_ID}__{PACKAGE_TOOL}"


def host_target() -> str:
    """The target key the example layout declares for this machine."""
    suffix = {"darwin": "apple-darwin", "linux": "unknown-linux-gnu", "win32": "pc-windows-msvc"}.get(sys.platform)
    if suffix is None:
        raise SystemExit(f"package_workloads has no target key for {sys.platform}")
    machine = platform.machine().lower()
    prefix = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
    return f"{prefix}-{suffix}"


def binary_name() -> str:
    return "horon-package-example.exe" if bc.IS_WINDOWS else "horon-package-example"


def strip_events(text: str) -> str:
    """Drop the observation subscription from a shipped declaration."""
    return re.sub(r'^events\s*=.*\n', "", text, flags=re.MULTILINE)


# The resource directories a shipped plugin declares, in the layout the example's
# own integration test stages them.
RESOURCE_DIRS = ("skills", "commands", "mcp", "rules", "prompts")


def copy_tree(source: Path, destination: Path) -> None:
    if not source.is_dir():
        return
    destination.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        target = destination / entry.name
        if entry.is_dir():
            copy_tree(entry, target)
        else:
            shutil.copy2(entry, target)


def stage_declaration(source: Path, destination: Path, package_binary: Path) -> Path:
    """Write one rewritten declaration and the executable it names beside it.

    The example ships one target key per platform and expects the binary below
    its own root; the rewrite keeps the declaration as shipped except for two
    things: the observation subscription is dropped (a package that observes a
    turn is started by that turn), and the target table names only the binary
    this run was given.
    """
    if not source.is_file():
        raise SystemExit(f"{source} is not a package declaration")
    text = strip_events(source.read_text(encoding="utf-8"))
    text = re.sub(r"^\[targets\]\n(?:.*\n)*?(?=\n|\Z)", "", text, flags=re.MULTILINE)
    relative = Path("bin") / binary_name()
    text = text.rstrip() + f"\n\n[targets]\n{host_target()} = \"{relative.as_posix()}\"\n"
    declaration = destination / "extension.toml"
    declaration.write_text(text, encoding="utf-8")
    binary = destination / relative
    binary.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(package_binary, binary)
    if not bc.IS_WINDOWS:
        binary.chmod(binary.stat().st_mode | 0o755)
    return declaration


def stage_package(source: Path, package_binary: Path, root: Path, layout: str) -> Path:
    """One shipped layout staged the way the installer accepts it.

    The installer refuses a root that holds both declarations, so the two kinds
    are staged as the example's own integration test stages them: an extension
    root holds `extension.toml` and the executable, and a plugin root holds
    `plugin.toml`, its resources, and the bundled extension under `extension/`.
    Neither copy carries the other kind's declaration, and the bundled extension
    keeps its own `bin/` so nothing is duplicated at the root.
    """
    destination = root / layout
    destination.mkdir(parents=True, exist_ok=True)
    declaration = source / "extension.toml"
    if layout == "extension":
        stage_declaration(declaration, destination, package_binary)
        return destination
    if layout != "plugin":
        raise SystemExit(f"unknown package layout {layout}")
    plugin = source / "plugin.toml"
    if not plugin.is_file():
        raise SystemExit(f"{source} is not a plugin layout: no plugin.toml")
    (destination / "plugin.toml").write_text(
        strip_events(plugin.read_text(encoding="utf-8")), encoding="utf-8"
    )
    for resource in RESOURCE_DIRS:
        copy_tree(source / "resources" / resource, destination / "resources" / resource)
    bundled = destination / "extension"
    bundled.mkdir(exist_ok=True)
    stage_declaration(declaration, bundled, package_binary)
    return destination


def directory_facts(path: Path) -> dict:
    """The bytes and file count one installed copy occupies."""
    total = 0
    files = 0
    if not path.exists():
        return {"bytes": 0, "files": 0}
    for entry in path.rglob("*"):
        if not entry.is_file():
            continue
        total += entry.stat().st_size
        files += 1
    return {"bytes": total, "files": files}


def install(binary: str, source: Path, env: dict, timeout: float) -> str:
    """Install one layout through the CLI, which is the same path a user takes."""
    completed = bc.run_capture(
        [binary, "--no-update", "package", "install", str(source)], env=env, timeout=timeout
    )
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip()[-400:]
        raise RuntimeError(f"`horon package install` exited {completed.returncode}: {detail}")
    return (completed.stdout or completed.stderr).strip()


def first_request_ms(requests: list[dict], started_wall: float) -> float | None:
    stamps = [entry["ts"] for entry in requests if "ts" in entry]
    if not stamps:
        return None
    return round((min(stamps) - started_wall) * 1000, 2)


def tool_names(requests: list[dict]) -> list[str]:
    names = [entry.get("tool_name") for entry in requests if entry.get("tool_name")]
    seen: list[str] = []
    for name in names:
        if name not in seen:
            seen.append(name)
    return seen


def run_turn(binary: str, env: dict, cwd: str, prompt: str, tools: str, mock, timeout: float) -> dict:
    """One `horon run` turn with the mock model, timed and sampled."""
    # The mock keeps only this turn's requests, so the offsets below are the
    # run's own: one process per turn and one reset per process.
    mock.reset()
    argv = bcmp.horon_turn_argv(binary, prompt, tools)
    started = time.perf_counter()
    started_wall = time.time()
    proc = bcmp.subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdin=bcmp.subprocess.DEVNULL,
        stdout=bcmp.subprocess.PIPE,
        stderr=bcmp.subprocess.PIPE,
        text=True,
        **bc.new_group_kwargs(),
    )
    sampler = bc.MemorySampler(proc.pid, interval=0.03).start()
    timed_out = False
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except bcmp.subprocess.TimeoutExpired:
        timed_out = True
        bc.terminate_pgroup(bc.pgid_of(proc.pid))
        try:
            stdout, stderr = proc.communicate(timeout=10)
        except bcmp.subprocess.TimeoutExpired:
            stdout, stderr = "", ""
    elapsed_ms = (time.perf_counter() - started) * 1000
    peak = sampler.stop()
    requests = mock.stats()["requests"]
    # A round trip is the call plus the child's own answer reaching the model:
    # the mock stamps the nonce it sent into the result it sees come back, so a
    # refusal or an empty answer does not read as a successful call.
    sentinel = any(entry.get("sentinel_seen") for entry in requests)
    round_trip = (
        any(entry.get("issued_tool_call") for entry in requests)
        and any(entry.get("has_tool_result") for entry in requests)
        and sentinel
    )
    error = None
    if timed_out:
        error = f"timeout after {timeout:.0f}s"
    elif proc.returncode not in (0, None):
        detail = (stderr or "").strip().splitlines()
        error = f"exit {proc.returncode}: " + (detail[-1] if detail else "no stderr")
    return {
        "ms": None if timed_out else round(elapsed_ms, 2),
        "peak_rss_bytes": peak or None,
        "first_request_ms": first_request_ms(requests, started_wall),
        "tool_round_trip": round_trip,
        "sentinel_seen": sentinel,
        "requests": len(requests),
        "tools": tool_names(requests),
        "exit_code": proc.returncode,
        "error": error,
    }


def summarize(runs: list[dict], expect_call: bool) -> dict:
    """Collapse the repeats into one row, keeping the evidence of a real turn."""
    times = [run["ms"] for run in runs if run.get("ms") is not None]
    firsts = [run["first_request_ms"] for run in runs if run.get("first_request_ms") is not None]
    peaks = [run["peak_rss_bytes"] for run in runs if run.get("peak_rss_bytes")]
    errors = [str(run["error"]) for run in runs if run.get("error")]
    calls = [run["tool_round_trip"] for run in runs if run.get("tool_round_trip") is not None]
    if expect_call:
        round_trip = bool(calls) and all(calls)
        # A timeout or a non-zero exit is the reason, even when another sample's
        # call came back: the row is only clean when every sample is.
        failure = errors[0] if errors else (None if round_trip else "the package call never came back")
    else:
        # A scenario that must not call a tool fails if it did: the `installed`
        # cell exists to measure installation alone.
        round_trip = any(calls)
        failure = None if not round_trip else "a tool call ran in a scenario that forbids one"
        if errors and failure is None:
            failure = errors[0]
    times_summary = bc.summarize(times)
    return {
        # A row that did not do what its scenario says reports no latency: a
        # fast number beside a failed call would read as a win.
        "ms": times_summary["median"] if times_summary and failure is None else None,
        "summary": times_summary,
        "first_request_ms": bc.summarize(firsts),
        "peak_rss_bytes": max(peaks) if peaks else None,
        "tool_round_trip": round_trip,
        "tools": sorted({name for run in runs for name in run.get("tools") or []}),
        "status": "ok" if failure is None else "failed",
        "failure": failure,
        "errors": errors,
        "runs": runs,
    }


def size_facts(binary: str, package_binary: str, home: str, extension_root: Path, plugin_root: Path) -> dict:
    store = Path(home) / "packages"
    return {
        "agent_binary_bytes": Path(binary).stat().st_size if Path(binary).is_file() else None,
        "package_binary_bytes": Path(package_binary).stat().st_size if Path(package_binary).is_file() else None,
        "package_extension_bytes": directory_facts(extension_root)["bytes"],
        "package_plugin_bytes": directory_facts(plugin_root)["bytes"],
        "installed_area_bytes": directory_facts(store)["bytes"],
        "installed_area_files": directory_facts(store)["files"],
    }


def payload_unpacked_bytes(declaration: Path) -> int | None:
    """The bytes the layout occupies, so a copy that grew is visible.

    A package's own files are the binary plus the resources the declaration
    lists; the number is read from the staged layout, not from a build log.
    """
    if not declaration.is_file():
        return None
    return directory_facts(declaration.parent)["bytes"]


@dataclass
class Prepared:
    """One build, staged and installed, ready to be timed scenario by scenario."""

    label: str
    binary: str
    package_binary: str
    root: Path
    extension_root: Path
    plugin_root: Path
    homes: dict[str, str]
    envs: dict[str, dict]
    cwds: dict[str, str]
    skipped: dict[str, str]


def prepare_build(
    label: str, binary: str, package_binary: str, package_source: Path, timeout: float, base_url: str
) -> Prepared:
    """Stage both layouts and install one copy per scenario, before any timing.

    Every build is prepared first and only then measured, so the measurement loop
    can interleave builds scenario by scenario: build A does not run to
    completion before build B starts.
    """
    root = Path(bc.short_temp_dir("hpkg-"))
    staged = root / "staged"
    extension_root = stage_package(package_source, Path(package_binary), staged, "extension")
    plugin_root = stage_package(package_source, Path(package_binary), staged, "plugin")
    homes: dict[str, str] = {}
    envs: dict[str, dict] = {}
    cwds: dict[str, str] = {}
    skipped: dict[str, str] = {}
    for scenario in SCENARIOS:
        work = root / scenario
        work.mkdir(parents=True, exist_ok=True)
        cwd = work / "cwd"
        cwd.mkdir(exist_ok=True)
        # One config per home writes the mock provider; the home itself is what
        # `HORON_HOME` names, so the run and the install agree on the package
        # area without any environment left over from the other build.
        bcmp.write_horon_config(str(work), base_url)
        env = envs.setdefault(scenario, bcmp.horon_env(str(work), binary))
        env["HORON_AGENT_PERF_COUNTERS"] = "1"
        homes[scenario] = env["HORON_HOME"]
        cwds[scenario] = str(cwd)
        if scenario == SCENARIO_EMPTY:
            continue
        try:
            install(binary, extension_root, env, timeout)
        except RuntimeError as exc:
            skipped[scenario] = str(exc)
    return Prepared(
        label=label,
        binary=binary,
        package_binary=package_binary,
        root=root,
        extension_root=extension_root,
        plugin_root=plugin_root,
        homes=homes,
        envs=envs,
        cwds=cwds,
        skipped=skipped,
    )


def surface(scenario: str) -> tuple[str, str]:
    """The prompt and tool surface one scenario runs with."""
    expect_call = scenario == SCENARIO_CALLED
    prompt = CALL_PROMPT if expect_call else PLAIN_PROMPT
    # The surface is named explicitly, so the two non-calling scenarios offer the
    # package tool's definition (with nothing installed it is simply not
    # registered) but no shell tool: the mock's default plan has no shell to call,
    # so a turn that must not call cannot, and `installed` still pays for listing
    # a real package tool definition.
    tools = package_tool_name() if expect_call else f"read,{package_tool_name()}"
    return prompt, tools


def measure_scenarios(prepared: list[Prepared], runs: int, timeout: float, mock) -> dict[str, dict[str, dict]]:
    """Scenario first, then repetition, then build: the two builds interleave.

    Repeats are spent on one scenario of every build before the next scenario
    starts, so a machine that drifts mid-run does not favour whichever build ran
    first, and the reset/measure windows of the two builds sit next to each other.
    """
    by_label: dict[str, dict[str, dict]] = {build.label: {} for build in prepared}
    for scenario in SCENARIOS:
        prompt, tools = surface(scenario)
        repeats: dict[str, list[dict]] = {build.label: [] for build in prepared}
        for _ in range(max(1, runs)):
            for build in prepared:
                if scenario in build.skipped:
                    continue
                try:
                    repeats[build.label].append(
                        run_turn(
                            build.binary,
                            build.envs[scenario],
                            build.cwds[scenario],
                            prompt,
                            tools,
                            mock,
                            timeout,
                        )
                    )
                except Exception as exc:  # noqa: BLE001
                    repeats[build.label].append(
                        {
                            "ms": None,
                            "peak_rss_bytes": None,
                            "first_request_ms": None,
                            "tool_round_trip": None,
                            "sentinel_seen": False,
                            "requests": 0,
                            "tools": [],
                            "error": str(exc)[:300],
                        }
                    )
        for build in prepared:
            if scenario in build.skipped:
                continue
            row = {
                "tool": build.label,
                "scenario": scenario,
                **summarize(repeats[build.label], scenario == SCENARIO_CALLED),
            }
            row.update(
                size_facts(build.binary, build.package_binary, build.homes[scenario],
                           build.extension_root, build.plugin_root)
            )
            by_label[build.label][scenario] = row
    return by_label


def comparison(baseline: dict | None, candidate: dict | None) -> dict | None:
    """Candidate against baseline for one cell both builds measured."""
    if not baseline or not candidate:
        return None
    before = baseline.get("ms")
    after = candidate.get("ms")
    if not before or not after:
        return None
    return {"baseline_ms": before, "candidate_ms": after, "ratio": round(after / before, 3)}


def markdown(report: dict) -> str:
    rows = report["results"]
    host = report.get("host") or bc.host_info()
    lines: list[str] = [
        "# User-installed package cost (headless, mock model)",
        "",
        f"Host: {host['os']}/{host['arch']}, {host['cpu_count']} CPUs, Python {host['python']}. "
        f"One turn per sample; samples per scenario: {report.get('runs_per_scenario', 1)}. "
        "Cells are measured per OS; never compare absolute values across OSes.",
        "",
    ]
    headers = [
        "Build",
        "Scenario",
        "Turn (median)",
        "Min–max",
        "First request",
        "Peak tree RSS",
        "Tool round trip",
        "Tools called",
        "vs baseline",
        "Status",
    ]
    table: list[list[str]] = []
    for row in rows:
        summary = row.get("summary") or {}
        span = "–".join(bc.fmt_ms(value) for value in (summary.get("min"), summary.get("max"))) if summary else "–"
        first = row.get("first_request_ms") or {}
        delta = row.get("comparison") or {}
        table.append(
            [
                row["tool"],
                row["scenario"],
                bc.fmt_ms(row.get("ms")),
                span,
                bc.fmt_ms(first.get("median")),
                bc.fmt_bytes(row.get("peak_rss_bytes")),
                "yes" if row.get("tool_round_trip") else "no",
                ", ".join(row.get("tools") or []) or "–",
                f"{delta['ratio']:.3f}×" if delta.get("ratio") else "–",
                row.get("status", "–"),
            ]
        )
    lines.append(bc.markdown_table(headers, table))
    sizes = [row for row in rows if row.get("installed_area_bytes") is not None]
    if sizes:
        lines.append("")
        lines.append("**Installed size** (absolute: a size does not need a median)")
        lines.append("")
        size_table = [
            [
                row["tool"],
                row["scenario"],
                bc.fmt_bytes(row.get("installed_area_bytes")),
                str(row.get("installed_area_files") or 0),
                bc.fmt_bytes(row.get("package_binary_bytes")),
                bc.fmt_bytes(row.get("agent_binary_bytes")),
            ]
            for row in sizes
        ]
        lines.append(
            bc.markdown_table(
                ["Build", "Scenario", "Package area", "Files", "Package binary", "Agent binary"],
                size_table,
            )
        )
    for note in report.get("skipped") or []:
        lines.append("")
        lines.append(f"- skipped `{note.get('tool')}` `{note.get('scenario')}`: {note.get('reason')}")
    return "\n".join(lines) + "\n"


def measure(binary: str, package_binary: str, baseline_binary: str | None,
            package_source: Path, runs: int, timeout: float) -> dict:
    started_at = bc.utc_now_iso()
    labels: list[tuple[str, str]] = [("candidate", binary)]
    if baseline_binary:
        labels.insert(0, ("baseline", baseline_binary))
    mock = bcmp.MockServer().start()
    try:
        # Both builds are staged and installed first; only then are the scenarios
        # timed, alternating build by build inside every repetition.
        prepared = [
            prepare_build(label, path, package_binary, package_source, timeout, mock.base_url)
            for label, path in labels
        ]
        measured = measure_scenarios(prepared, runs, timeout, mock)
    finally:
        mock.stop()

    skipped: list[dict] = []
    for build in prepared:
        skipped.extend(
            {"tool": build.label, "scenario": scenario, "reason": reason}
            for scenario, reason in build.skipped.items()
        )
    results: list[dict] = []
    baseline_rows = measured.get("baseline") or {}
    for label in ("baseline", "candidate"):
        for scenario in SCENARIOS:
            row = measured.get(label, {}).get(scenario)
            if row is None:
                continue
            if label == "candidate":
                row["comparison"] = comparison(baseline_rows.get(scenario), row)
            results.append(row)
    candidate = next(build for build in prepared if build.label == "candidate")
    sizes = {
        "payload_extension_bytes": payload_unpacked_bytes(candidate.extension_root / "extension.toml"),
        "payload_plugin_bytes": payload_unpacked_bytes(candidate.plugin_root / "plugin.toml"),
    }
    report = bc.build_report("packages", results, skipped, started_at=started_at)
    report["sizes"] = sizes
    report["runs_per_scenario"] = max(1, runs)
    report["notes"] = [
        f"One turn of `horon run` against the mock model per sample; the median of {max(1, runs)}.",
        "Both builds are staged and installed first, then the scenarios are timed with the builds "
        "alternating inside every repetition.",
        "`installed` and `called` install the shipped example layout with its observation "
        "subscription removed, so a turn that does not call it never starts the child.",
        "Size facts are absolute and are read from the installed area, not from a build log.",
        "`horon-tui` is measured by its own suite and never appears here.",
    ]
    return report


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True, help="the horon-agent binary under test")
    parser.add_argument("--package-binary", required=True, help="the built example package executable")
    parser.add_argument("--baseline-binary", default=None, help="a horon-agent build to compare against")
    parser.add_argument("--package-source", default=DEFAULT_PACKAGE_SOURCE, help="the shipped example layouts")
    parser.add_argument("--runs", type=int, default=5, help="samples per scenario (the median needs at least 5)")
    parser.add_argument("--timeout", type=float, default=180.0, help="seconds one turn may take")
    parser.add_argument("--json-out", default=None)
    parser.add_argument("--markdown-out", default=None)
    args = parser.parse_args(argv)

    source = Path(args.package_source)
    if not source.is_dir():
        print(f"package_workloads: no package layout at {source}", file=sys.stderr)
        return 2
    if not Path(args.package_binary).is_file():
        print(f"package_workloads: no package executable at {args.package_binary}", file=sys.stderr)
        return 2
    report = measure(
        args.binary,
        args.package_binary,
        args.baseline_binary,
        source,
        args.runs,
        args.timeout,
    )
    text = markdown(report)
    if args.json_out:
        bc.write_json(args.json_out, report)
    if args.markdown_out:
        bc.write_markdown(args.markdown_out, text)
    print(text)
    # A baseline that predates packages may legitimately skip; the candidate may
    # not. Every one of its three scenarios must exist and be clean, or the job
    # fails instead of publishing a row that silently measured nothing.
    candidate = {row["scenario"]: row for row in report["results"] if row["tool"] == "candidate"}
    broken = [
        scenario
        for scenario in SCENARIOS
        if scenario not in candidate or candidate[scenario].get("status") != "ok"
    ]
    if broken:
        print(
            "package_workloads: the candidate did not measure " + ", ".join(broken),
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
