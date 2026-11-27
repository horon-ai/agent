#!/usr/bin/env python3
"""Determine impacted workspace test packages for scoped pull-request runs.

Builds the reverse-dependency graph from `cargo metadata --no-deps --offline
--format-version 1` and augments it with explicit non-Cargo couplings
(`build.rs` embedded docs, `Cargo.toml` build metadata, `crates/platform-macos`
`#[path]` includes from `platform-linux`, SDK parity tests, and `include_str!`
contracts in `tests/support/acp_credentials.rs`).

Whenever uncertainty arises (push events, `ci:all-os`/`ci:full` labels, empty
or missing diffs, `Cargo.toml`/`Cargo.lock`/toolchain/CI-script edits, or
`cargo metadata` errors), the analyzer safely falls back to the full 26-package
(Linux) or 25-package (macOS/Windows) matrix.
"""

from __future__ import annotations

import argparse
from collections import defaultdict, deque
import fnmatch
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
FORCE_ALL_LABELS = frozenset({"ci:all-os", "ci:full"})

FULL_COMMON_PACKAGES: tuple[str, ...] = (
    "horon-agent",
    "horon-agent-app-core",
    "horon-agent-base",
    "horon-agent-protocol",
    "horon-agent-harness-api",
    "horon-agent-harness-api-server",
    "horon-agent-irc-core",
    "horon-agent-todo-policy",
    "horon-agent-tool-types",
    "horon-agent-memory-types",
    "horon-agent-message-types",
    "horon-agent-productivity-core",
    "horon-agent-provider-openai",
    "horon-agent-session-store",
    "horon-agent-sdk",
    "horon-tui",
    "horon-agent-time",
    "horon-agent-token-estimate",
    "horon-agent-platform",
    "horon-agent-import-core",
    "horon-agent-schema-dialect",
    "horon-agent-session-types",
    "horon-agent-logging",
    "horon-agent-config-types",
    "horon-agent-extension-sdk",
)
LINUX_ONLY_PACKAGES: tuple[str, ...] = ("horon-agent-embedding",)
PACKAGE_EXAMPLE_NAME = "horon-package-example"

# Shared files with hidden non-Cargo couplings.
MACOS_SHARED_LINUX_PATHS = frozenset(
    {
        "crates/platform-linux/src/child_exit.rs",
        "crates/platform-linux/src/unix.rs",
        "crates/platform-linux/src/unix_fs.rs",
        "crates/platform-linux/src/ipc.rs",
        "crates/platform-linux/src/unix_term.rs",
        "crates/platform-linux/src/signal.rs",
    }
)
ACP_CREDENTIALS_INCLUDE_PATHS = frozenset(
    {
        "crates/agent-sdk/src/launch.rs",
        "crates/platform-macos/src/lib.rs",
        "crates/platform/src/paths.rs",
        "crates/storage/src/lib.rs",
        "crates/base/src/mcp/oauth.rs",
    }
)
SDK_PARITY_PATTERNS = (
    "packages/sdk/typescript/*",
    "packages/sdk/python/*",
)
NOOP_DOC_PATTERNS = (
    "README*.ko.md",
    "docs/*.ko.md",
    "docs/assets/*",
    "PHILOSOPHY*.md",
    "AGENTS*.md",
    "CHANGELOG*.md",
    "LICENSE*",
    ".github/ISSUE_TEMPLATE/*",
    ".github/pull_request_template.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
    "packages/docs-site/*",
)
FULL_FALLBACK_PATTERNS = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/*",
    "tests/fixtures/package-example/*",
)


def full_packages_for_os(runner_os: str) -> list[str]:
    if runner_os == "Linux":
        return [*FULL_COMMON_PACKAGES, *LINUX_ONLY_PACKAGES]
    return list(FULL_COMMON_PACKAGES)


def parse_labels(raw: str | None) -> set[str]:
    if not raw or not raw.strip():
        return set()
    stripped = raw.strip()
    if stripped.startswith("["):
        try:
            loaded = json.loads(stripped)
            if isinstance(loaded, list):
                return {str(item).strip() for item in loaded if str(item).strip()}
        except json.JSONDecodeError:
            return set()
    return {item.strip() for item in stripped.split(",") if item.strip()}


def matches(path: str, patterns: tuple[str, ...]) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def load_workspace_metadata(repo_root: Path = ROOT) -> dict:
    completed = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(completed.stdout)


def build_reverse_graph(metadata: dict) -> tuple[dict[str, set[str]], dict[str, str]]:
    """Return (reverse_adj, rel_dir_to_package) from workspace cargo metadata."""
    workspace_root = Path(metadata["workspace_root"]).resolve()
    known_names = {pkg["name"] for pkg in metadata["packages"]}
    reverse_adj: dict[str, set[str]] = defaultdict(set)
    dir_to_pkg: dict[str, str] = {}

    for pkg in metadata["packages"]:
        name = pkg["name"]
        reverse_adj.setdefault(name, set())
        manifest_dir = Path(pkg["manifest_path"]).resolve().parent
        rel_dir = "." if manifest_dir == workspace_root else manifest_dir.relative_to(workspace_root).as_posix()
        dir_to_pkg[rel_dir] = name
        for dep in pkg.get("dependencies", []):
            dep_name = dep["name"]
            if dep_name in known_names and dep_name != name:
                reverse_adj[dep_name].add(name)

    return reverse_adj, dir_to_pkg


def is_embedded_english_doc(path: str) -> bool:
    if path.endswith(".ko.md"):
        return False
    if fnmatch.fnmatchcase(path, "README*.md"):
        return True
    return path.startswith("docs/") and path.endswith(".md")


def map_file_to_seeds(
    path: str,
    dir_to_pkg: dict[str, str],
) -> tuple[set[str] | None, str]:
    """Map a repository-relative path to seed workspace packages, or None for full fallback."""
    norm = path.replace("\\", "/")
    while norm.startswith("./") or norm.startswith("/"):
        norm = norm[2:] if norm.startswith("./") else norm[1:]
    if matches(norm, FULL_FALLBACK_PATTERNS):
        return None, f"shared manifest/toolchain path {norm!r}"

    seeds: set[str] = set()

    if norm in MACOS_SHARED_LINUX_PATHS:
        seeds.add("horon-agent-platform-macos")
    if norm in ACP_CREDENTIALS_INCLUDE_PATHS:
        seeds.add("horon-agent")

    if is_embedded_english_doc(norm):
        seeds.add("horon-agent-app-core")
        return seeds, ""
    if matches(norm, NOOP_DOC_PATTERNS):
        return seeds, ""
    if matches(norm, SDK_PARITY_PATTERNS):
        seeds.add("horon-agent-sdk")
        return seeds, ""

    # Longest non-root workspace member directory match first.
    member_dirs = sorted(
        (d for d in dir_to_pkg if d != "."),
        key=lambda item: len(item),
        reverse=True,
    )
    for rel_dir in member_dirs:
        if norm == rel_dir or norm.startswith(rel_dir + "/"):
            seeds.add(dir_to_pkg[rel_dir])
            return seeds, ""

    # Root crate (`horon-agent`) owns `src/`, `tests/`, `build.rs`.
    root_pkg = dir_to_pkg.get(".")
    if root_pkg and (
        norm == "build.rs"
        or norm.startswith("src/")
        or norm.startswith("tests/")
    ):
        seeds.add(root_pkg)
        return seeds, ""

    # Anything unmapped (CI scripts, perf scripts, shared tooling, unknown root files)
    # falls back conservatively to the full package set.
    return None, f"unmapped shared path {norm!r}"


def compute_impact(
    *,
    event: str,
    files: list[str] | None,
    runner_os: str = "Linux",
    labels: set[str] | list[str] | None = None,
    metadata: dict | None = None,
) -> dict:
    full = full_packages_for_os(runner_os)
    label_set = set(labels or ())

    def full_result(reason: str) -> dict:
        return {
            "scoped": False,
            "packages": full,
            "impacted": [*full, PACKAGE_EXAMPLE_NAME],
            "run_package_example": True,
            "reason": reason,
        }

    if event != "pull_request":
        return full_result(f"event {event!r} requires full suite")
    if label_set & FORCE_ALL_LABELS:
        return full_result("forced all-OS/full label present")
    if not files:
        return full_result("empty or unavailable changed file list")
    if metadata is None:
        try:
            metadata = load_workspace_metadata()
        except Exception as exc:  # noqa: BLE001 - metadata failure must safely fall back
            return full_result(f"cargo metadata unavailable ({type(exc).__name__})")

    try:
        reverse_adj, dir_to_pkg = build_reverse_graph(metadata)
    except Exception as exc:  # noqa: BLE001
        return full_result(f"invalid workspace metadata ({type(exc).__name__})")

    seeds: set[str] = set()
    for path in files:
        file_seeds, fallback_reason = map_file_to_seeds(path, dir_to_pkg)
        if file_seeds is None:
            return full_result(fallback_reason)
        seeds.update(file_seeds)

    if not seeds:
        return {
            "scoped": True,
            "packages": [],
            "impacted": [],
            "run_package_example": False,
            "reason": "no Rust package impacted by changed files",
        }

    closure: set[str] = set(seeds)
    queue: deque[str] = deque(seeds)
    while queue:
        current = queue.popleft()
        for dependent in reverse_adj.get(current, ()):
            if dependent not in closure:
                closure.add(dependent)
                queue.append(dependent)

    selected = [pkg for pkg in full if pkg in closure]
    if not selected:
        return full_result("impacted closure produced no compiled test package")
    if set(selected) == set(full) and PACKAGE_EXAMPLE_NAME in closure:
        return full_result("impacted closure covers full package set")

    return {
        "scoped": True,
        "packages": selected,
        "impacted": sorted(closure),
        "run_package_example": PACKAGE_EXAMPLE_NAME in closure,
        "reason": f"scoped {len(selected)}/{len(full)} compiled test packages from {len(files)} changed file(s)",
    }


def changed_files(base: str, head: str, cwd: Path = ROOT) -> list[str] | None:
    if not base or not head:
        return None
    try:
        out = subprocess.run(
            ["git", "diff", "--name-only", "-z", "--no-renames", f"{base}...{head}"],
            cwd=cwd,
            capture_output=True,
            encoding="utf-8",
            errors="surrogateescape",
            timeout=30,
            check=True,
        ).stdout
    except (subprocess.CalledProcessError, OSError, subprocess.TimeoutExpired):
        return None
    return [path for path in out.split("\0") if path]


def main(argv: list[str] | None = None) -> int:
    env = os.environ.get
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--event", default=env("GITHUB_EVENT_NAME", ""))
    parser.add_argument("--base", default="")
    parser.add_argument("--head", default="")
    parser.add_argument("--files-from", default="")
    parser.add_argument("--labels", default=env("PR_LABELS", ""))
    parser.add_argument("--runner-os", default=env("RUNNER_OS", "Linux"))
    parser.add_argument("--metadata-json", type=Path, default=None)
    parser.add_argument("--format", choices=("env", "json"), default="env")
    args = parser.parse_args(argv)

    labels = parse_labels(args.labels)
    files: list[str] | None
    if args.files_from:
        files = [
            line.strip()
            for line in Path(args.files_from).read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
    elif args.event == "pull_request":
        files = changed_files(args.base, args.head)
    else:
        files = None

    metadata = None
    if args.metadata_json is not None:
        metadata = json.loads(args.metadata_json.read_text(encoding="utf-8"))

    result = compute_impact(
        event=args.event,
        files=files,
        runner_os=args.runner_os,
        labels=labels,
        metadata=metadata,
    )

    if args.format == "json":
        print(json.dumps(result, indent=2))
        return 0

    pkgs = result["packages"]
    print(f"SCOPED={'true' if result['scoped'] else 'false'}")
    print("PACKAGES_FLAGS=" + " ".join(f"-p {pkg}" for pkg in pkgs))
    print("SCOPED_PACKAGES_CSV=" + (",".join(pkgs) if result["scoped"] else ""))
    print(f"RUN_PACKAGE_EXAMPLE={'true' if result['run_package_example'] else 'false'}")
    print("IMPACTED_CSV=" + ",".join(result["impacted"]))
    print(f"REASON={result['reason']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
