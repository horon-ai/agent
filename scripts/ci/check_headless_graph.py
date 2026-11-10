#!/usr/bin/env python3
"""Fail when the `horon-agent` package can reach a terminal-UI dependency.

Requirement 1 of `AGENTS.md`: the headless package (`serve`, `run`,
`api-bridge`, `acp`) never opens a terminal UI, and its dependency graph stays
free of `ratatui`, `crossterm`, `horon-tui`, and any `horon-agent-tui*` crate.
An interactive terminal client may exist only as the separate `horon-tui`
package and binary, which talks to the daemon through `horon-agent-sdk`.

This walks the resolved graph from `cargo metadata` instead of running one
`cargo tree -i` per forbidden name, so a transitive crate is reported with the
path that reaches it (`horon-agent -> x -> crossterm`). A forbidden crate that
is a workspace member but unreachable from `horon-agent` passes; that is what
lets `horon-tui` live in this workspace without entering the headless graph.

Dev-dependency edges are followed for workspace members, because
`cargo test --workspace` compiles them alongside the normal and build edges.

Usage:
  python3 scripts/ci/check_headless_graph.py
  python3 scripts/ci/check_headless_graph.py --metadata metadata.json
"""

from __future__ import annotations

import json
import subprocess
import sys
from collections import deque
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# The package whose graph is the requirement. `serve`, `run`, `api-bridge`, and
# `acp` are its subcommands, so one package name covers the whole headless
# binary.
TARGET = "horon-agent"

# Exact package names and prefixes that must never be reachable from TARGET.
FORBIDDEN_NAMES = {"ratatui", "crossterm", "horon-tui"}
FORBIDDEN_PREFIXES = ("horon-agent-tui",)


class GateError(Exception):
    """A metadata document the gate cannot evaluate, reported as a failure."""


def is_forbidden(name: str) -> bool:
    return name in FORBIDDEN_NAMES or name.startswith(FORBIDDEN_PREFIXES)


def cargo_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise GateError(f"cargo metadata failed with status {result.returncode}:\n{result.stderr}")
    return json.loads(result.stdout)


def read_metadata(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def follows(dep: dict, source_is_workspace_member: bool) -> bool:
    """Whether an edge is compiled as part of the package's dependency graph.

    Normal and build edges always count, for every package. Dev edges count for
    workspace members only: those are the ones `cargo test --workspace` builds,
    and a dev-only leak is still a terminal stack in the tree's test builds.
    """
    kinds = dep.get("dep_kinds") or []
    if not kinds:
        # Older metadata without dep_kinds: treat the edge as a normal one.
        return True
    for kind in kinds:
        if kind.get("kind") != "dev":
            return True
    return source_is_workspace_member


def graph_paths(metadata: dict) -> tuple[dict[str, list[str]], int]:
    """Return reachable package id -> name path from TARGET, and the count."""
    packages = metadata.get("packages", [])
    name_by_id = {package["id"]: package["name"] for package in packages}
    workspace_members = set(metadata.get("workspace_members") or [])
    resolve = metadata.get("resolve")
    if not resolve or "nodes" not in resolve:
        raise GateError("cargo metadata has no `resolve` section; cannot walk the graph")
    nodes = {node["id"]: node for node in resolve["nodes"]}

    targets = [package["id"] for package in packages if package["name"] == TARGET]
    if len(targets) != 1:
        raise GateError(
            f"expected exactly one package named `{TARGET}`, found {len(targets)}; "
            "the gate cannot pick a graph start"
        )
    start = targets[0]
    if start not in nodes:
        raise GateError(f"`{TARGET}` is missing from the resolved graph; cannot walk it")

    path_by_id = {start: [TARGET]}
    queue = deque([start])
    while queue:
        current = queue.popleft()
        source_is_workspace_member = current in workspace_members
        deps = sorted(
            (dep for dep in nodes[current].get("deps", []) if follows(dep, source_is_workspace_member)),
            key=lambda dep: (dep.get("name", ""), dep.get("pkg", "")),
        )
        for dep in deps:
            dep_id = dep.get("pkg")
            if not dep_id or dep_id in path_by_id or dep_id not in nodes:
                continue
            path_by_id[dep_id] = path_by_id[current] + [name_by_id.get(dep_id, dep_id)]
            queue.append(dep_id)

    return path_by_id, len(path_by_id)


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    metadata_path: Path | None = None
    index = 0
    while index < len(args):
        arg = args[index]
        if arg == "--metadata" and index + 1 < len(args):
            metadata_path = Path(args[index + 1])
            index += 2
        elif arg in {"-h", "--help"}:
            print(__doc__.strip())
            return 0
        else:
            print(f"error: unknown argument {arg}", file=sys.stderr)
            return 2

    try:
        metadata = read_metadata(metadata_path) if metadata_path else cargo_metadata()
        path_by_id, count = graph_paths(metadata)
    except GateError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    name_by_id = {package["id"]: package["name"] for package in metadata.get("packages", [])}

    failures = sorted(
        " -> ".join(path_by_id[package_id])
        for package_id in path_by_id
        if is_forbidden(name_by_id.get(package_id, ""))
    )

    if failures:
        for path in failures:
            print(f"error: {path}", file=sys.stderr)
        print(
            f"{TARGET} must stay headless: no ratatui, crossterm, horon-tui, "
            f"or horon-agent-tui* crate may be reachable ({count} packages checked)",
            file=sys.stderr,
        )
        return 1

    print(
        f"{TARGET}: no ratatui, crossterm, horon-tui, horon-agent-tui* "
        f"({count} packages checked)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())