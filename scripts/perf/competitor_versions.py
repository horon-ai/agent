#!/usr/bin/env python3
"""Resolve live latest stable versions for horon and all npm competitors.

Why this exists:
- Hardcoded npm package pins rot within days of a benchmark PR landing.
- Shallow CI checkouts lack release tags, causing `build-meta` to stamp `v0.1.0-dev`
  unless the real release-line semver and commit SHA are resolved once up front.

Design:
- Query `https://registry.npmjs.org/<pkg>/latest` via `curl` (never sending
  `GH_TOKEN`/`GITHUB_TOKEN` to npm). When `dist-tags.latest` is a prerelease or
  deprecated, inspect the abbreviated packument and choose the highest stable
  `X.Y.Z` release instead.
- Resolve `horon` against `origin/main` by default so benchmark runs always target
  `main` unless an explicit `--ref` override is passed, and derive semver using
  `scripts/release/next_version.py` rules (exact tag => release semver; untagged
  commit => `<major>.<minor>.<latest_patch + 1>-dev`).
- Inspect installed `node_modules/<pkg>/package.json` after installation so
  `competitor-provenance.json` records verified package versions, npm `gitHead`,
  and tarball integrity/shasum digests.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from urllib.parse import quote

NPM_REGISTRY = "https://registry.npmjs.org"
GITHUB_API = "https://api.github.com/repos/horon-ai/agent"

STABLE_SEMVER_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
TAG_SEMVER_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
SAFE_VERSION_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z.+_-]{0,127}$")
HEX_SHA_RE = re.compile(r"^[0-9a-f]{40,64}$")

NPM_COMPETITORS: dict[str, dict[str, str]] = {
    "claude_code": {
        "package": "@anthropic-ai/claude-code",
        "env": "CLAUDE_VERSION",
        "bin": "claude",
        "label": "Claude Code",
    },
    "codex": {
        "package": "@openai/codex",
        "env": "CODEX_VERSION",
        "bin": "codex",
        "label": "Codex CLI",
    },
    "opencode": {
        "package": "opencode-ai",
        "env": "OPENCODE_VERSION",
        "bin": "opencode",
        "label": "OpenCode",
    },
    "pi": {
        "package": "@earendil-works/pi-coding-agent",
        "env": "PI_VERSION",
        "bin": "pi",
        "label": "pi",
    },
    "omp": {
        "package": "@oh-my-pi/pi-coding-agent",
        "env": "OMP_VERSION",
        "bin": "omp",
        "label": "oh-my-pi (omp)",
    },
    "omo": {
        "package": "omo-ai",
        "env": "OMO_VERSION",
        "bin": "omo",
        "label": "oh-my-openagent (omo)",
    },
    "bun": {
        "package": "bun",
        "env": "BUN_VERSION",
        "bin": "bun",
        "label": "Bun",
    },
}

DEFAULT_NPM_ORDER = ("claude_code", "codex", "opencode", "pi", "omp", "omo")


class ResolveError(Exception):
    """Raised when version or release resolution fails."""


def log(message: str) -> None:
    print(f"competitor_versions: {message}", file=sys.stderr, flush=True)


def normalize_tool_key(raw: str) -> str:
    key = raw.strip()
    return "claude_code" if key == "claude" else key


def requested_npm_tools(tools_raw: str | None) -> list[str]:
    """Return ordered npm tool keys from comma/space input, adding bun when needed."""
    if not tools_raw or not tools_raw.strip():
        chosen = list(DEFAULT_NPM_ORDER)
    else:
        chosen = []
        for token in tools_raw.replace(" ", ",").split(","):
            key = normalize_tool_key(token)
            if key in NPM_COMPETITORS and key not in chosen:
                chosen.append(key)
    if ("omp" in chosen or "omo" in chosen) and "bun" not in chosen:
        chosen.append("bun")
    return chosen


def _curl(url: str, *, accept: str | None = None, include_github_auth: bool = False, timeout: int = 120) -> bytes:
    if shutil.which("curl") is None:
        raise ResolveError("curl is required to query package metadata")
    cmd = [
        "curl",
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--retry",
        "5",
        "--retry-delay",
        "2",
    ]
    if accept:
        cmd += ["--header", f"Accept: {accept}"]
    if include_github_auth:
        token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        if token:
            cmd += ["--header", f"Authorization: Bearer {token}"]
    cmd.append(url)
    # Strip GitHub tokens when talking to npm so credentials never leave GitHub endpoints.
    env = dict(os.environ)
    if not include_github_auth:
        env.pop("GH_TOKEN", None)
        env.pop("GITHUB_TOKEN", None)
    try:
        done = subprocess.run(
            cmd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            env=env,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise ResolveError(f"curl could not query {url}: {exc}") from exc
    if done.returncode:
        detail = done.stderr.decode(errors="replace").strip()
        raise ResolveError(detail.splitlines()[-1] if detail else f"curl exited {done.returncode} for {url}")
    return done.stdout


def _fetch_json(url: str, *, accept: str | None = None, include_github_auth: bool = False) -> dict:
    body = _curl(url, accept=accept, include_github_auth=include_github_auth)
    try:
        data = json.loads(body.decode("utf-8"))
    except ValueError as exc:
        raise ResolveError(f"invalid JSON from {url}: {exc}") from exc
    if not isinstance(data, dict):
        raise ResolveError(f"unexpected JSON payload from {url}")
    return data


def _semver_tuple(version: str) -> tuple[int, int, int] | None:
    match = STABLE_SEMVER_RE.fullmatch((version or "").strip())
    if not match:
        return None
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def _npm_record(tool: str, spec: dict[str, str], doc: dict, selection: str) -> dict:
    dist = doc.get("dist") if isinstance(doc.get("dist"), dict) else {}
    return {
        "tool": tool,
        "label": spec["label"],
        "package": spec["package"],
        "version": str(doc.get("version") or ""),
        "selection": selection,
        "git_head": doc.get("gitHead") or None,
        "tarball": dist.get("tarball") or None,
        "shasum": dist.get("shasum") or None,
        "integrity": dist.get("integrity") or None,
    }


def _highest_stable_version(packument: dict) -> tuple[str, dict]:
    versions = packument.get("versions")
    if not isinstance(versions, dict):
        raise ResolveError("npm packument missing versions map")
    non_deprecated: list[tuple[tuple[int, int, int], str, dict]] = []
    deprecated_only: list[tuple[tuple[int, int, int], str, dict]] = []
    for raw_ver, entry in versions.items():
        if not isinstance(entry, dict):
            continue
        parsed = _semver_tuple(raw_ver)
        if parsed is None:
            continue
        bucket = deprecated_only if entry.get("deprecated") else non_deprecated
        bucket.append((parsed, raw_ver, entry))
    chosen = non_deprecated or deprecated_only
    if not chosen:
        raise ResolveError("no stable X.Y.Z release found in npm packument")
    _, best_ver, best_entry = max(chosen, key=lambda item: item[0])
    return best_ver, best_entry


def resolve_npm_tool(tool: str, *, env: dict[str, str] | None = None) -> dict:
    """Resolve one npm competitor (pinned by env if set, else latest stable non-prerelease)."""
    if tool not in NPM_COMPETITORS:
        raise ResolveError(f"unknown npm competitor tool: {tool!r}")
    spec = NPM_COMPETITORS[tool]
    package = spec["package"]
    pkg_path = quote(package, safe="@/")
    env_map = env if env is not None else os.environ
    pin = (env_map.get(spec["env"]) or "").strip()

    if pin:
        if not SAFE_VERSION_RE.fullmatch(pin):
            raise ResolveError(f"{spec['env']} contains unsafe characters: {pin!r}")
        try:
            doc = _fetch_json(f"{NPM_REGISTRY}/{pkg_path}/{quote(pin, safe='')}")
            if not doc.get("version"):
                doc["version"] = pin
            return _npm_record(tool, spec, doc, "pin")
        except ResolveError as exc:
            # Explicit env pins may run offline against a stub npm binary.
            return {
                "tool": tool,
                "label": spec["label"],
                "package": package,
                "version": pin,
                "selection": "pin",
                "git_head": None,
                "tarball": None,
                "shasum": None,
                "integrity": None,
                "metadata_error": str(exc),
            }

    latest_doc = _fetch_json(f"{NPM_REGISTRY}/{pkg_path}/latest")
    latest_ver = str(latest_doc.get("version") or "")
    if _semver_tuple(latest_ver) is not None and not latest_doc.get("deprecated"):
        return _npm_record(tool, spec, latest_doc, "npm dist-tag latest")

    # `dist-tags.latest` was a prerelease or deprecated; select the newest stable version.
    packument = _fetch_json(
        f"{NPM_REGISTRY}/{pkg_path}",
        accept="application/vnd.npm.install-v1+json",
    )
    picked_ver, picked_entry = _highest_stable_version(packument)
    try:
        full_doc = _fetch_json(f"{NPM_REGISTRY}/{pkg_path}/{quote(picked_ver, safe='')}")
    except ResolveError:
        full_doc = dict(picked_entry)
    if not full_doc.get("version"):
        full_doc["version"] = picked_ver
    return _npm_record(tool, spec, full_doc, "npm highest-stable")


def _load_resolved_map(raw: str | None) -> dict[str, dict]:
    if not raw or not raw.strip():
        return {}
    stripped = raw.strip()
    if stripped.startswith("{"):
        payload = json.loads(stripped)
    else:
        path = Path(stripped)
        if not path.is_file():
            return {}
        payload = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(payload, dict) and isinstance(payload.get("tools"), dict):
        return payload["tools"]
    if isinstance(payload, dict):
        return {k: v for k, v in payload.items() if isinstance(v, dict)}
    return {}


def resolve_npm_many(tools: list[str], *, env: dict[str, str] | None = None, strict: bool = True,
                     pre_resolved: str | None = None) -> dict:
    """Resolve all requested npm competitors plus bun runtime when needed."""
    env_map = env if env is not None else os.environ
    pre = _load_resolved_map(pre_resolved)
    resolved: dict[str, dict] = {}
    for tool in tools:
        spec = NPM_COMPETITORS[tool]
        pin = (env_map.get(spec["env"]) or "").strip()
        if pin:
            resolved[tool] = resolve_npm_tool(tool, env=env_map)
            continue
        if tool in pre and pre[tool].get("version"):
            entry = dict(pre[tool])
            entry.setdefault("tool", tool)
            entry.setdefault("label", spec["label"])
            entry.setdefault("package", spec["package"])
            resolved[tool] = entry
            continue
        try:
            resolved[tool] = resolve_npm_tool(tool, env=env_map)
        except ResolveError as exc:
            if strict:
                raise
            resolved[tool] = {
                "tool": tool,
                "label": spec["label"],
                "package": spec["package"],
                "version": "latest",
                "selection": "npm latest tag (unpinned fallback)",
                "git_head": None,
                "tarball": None,
                "shasum": None,
                "integrity": None,
                "resolve_error": str(exc),
            }
    return {
        "schema": 1,
        "resolved_at": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "tools": resolved,
    }


def _git(repo_root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=str(repo_root),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=check,
    )


def _cargo_base_version(manifest_text: str) -> tuple[int, int, int]:
    in_package = False
    for line in manifest_text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        match = re.match(r'version\s*=\s*"([^"]+)"', stripped)
        if in_package and match:
            parsed = _semver_tuple(match.group(1))
            if parsed is not None:
                return parsed
    raise ResolveError("Cargo.toml has no [package].version")


def _tag_commits(repo_root: Path) -> dict[tuple[int, int, int], tuple[str, str]]:
    """Map (major, minor, patch) -> (tag_name, peeled_commit_sha) for vX.Y.Z tags."""
    out = _git(
        repo_root,
        "for-each-ref",
        "--format=%(refname:short) %(*objectname) %(objectname)",
        "refs/tags",
    ).stdout
    releases: dict[tuple[int, int, int], tuple[str, str]] = {}
    for line in out.splitlines():
        parts = line.split()
        if not parts:
            continue
        name = parts[0]
        match = TAG_SEMVER_RE.fullmatch(name)
        if match is None or len(parts) < 2:
            continue
        sha = parts[1] if len(parts) >= 3 else parts[-1]
        version = tuple(int(p) for p in match.groups())  # type: ignore[assignment]
        releases[version] = (name, sha)
    return releases


def _resolve_git_commit(repo_root: Path, ref: str) -> str:
    requested = (ref or "").strip() or "main"
    candidates = [requested]
    if requested == "main":
        candidates = ["origin/main", "main"]
    elif not HEX_SHA_RE.fullmatch(requested) and "/" not in requested:
        candidates = [requested, f"origin/{requested}"]
    for cand in candidates:
        res = _git(repo_root, "rev-parse", "--verify", f"{cand}^{{commit}}", check=False)
        if res.returncode == 0:
            sha = res.stdout.strip()
            if HEX_SHA_RE.fullmatch(sha):
                return sha
    raise ResolveError(f"could not resolve horon ref {requested!r} to a git commit")


def _resolve_latest_horon_release(tags: dict[tuple[int, int, int], tuple[str, str]]) -> str:
    """Resolve horon's newest published stable release tag, falling back to git tags."""
    try:
        data = _fetch_json(f"{GITHUB_API}/releases/latest", include_github_auth=True)
        tag = str(data.get("tag_name") or "")
        if not data.get("draft") and not data.get("prerelease") and TAG_SEMVER_RE.fullmatch(tag):
            return tag
    except ResolveError:
        pass
    if tags:
        return tags[max(tags)][0]
    return ""


def resolve_horon(
    repo_root: Path,
    ref: str | None = None,
    *,
    main_ref: str = "origin/main",
    release_tag_override: str | None = None,
) -> dict:
    """Resolve horon commit SHA, tag-derived semver, release flag, and main ancestry."""
    sha = _resolve_git_commit(repo_root, ref or "main")
    short_sha = _git(repo_root, "rev-parse", "--short", sha).stdout.strip()
    cargo_text = _git(repo_root, "show", f"{sha}:Cargo.toml").stdout
    base_version = _cargo_base_version(cargo_text)
    tags = _tag_commits(repo_root)

    main_sha = None
    for cand in (main_ref, "main") if main_ref == "origin/main" else (main_ref,):
        probe = _git(repo_root, "rev-parse", "--verify", f"{cand}^{{commit}}", check=False)
        if probe.returncode == 0:
            main_sha = probe.stdout.strip()
            break
    on_main = False
    if main_sha:
        anc = _git(repo_root, "merge-base", "--is-ancestor", sha, main_sha, check=False)
        on_main = anc.returncode == 0

    exact_tag = ""
    for version, (tag_name, tag_sha) in sorted(tags.items(), reverse=True):
        if tag_sha == sha:
            exact_tag = tag_name
            semver = f"{version[0]}.{version[1]}.{version[2]}"
            release_build = True
            break
    else:
        same_line = [ver for ver in tags if ver[:2] == base_version[:2]]
        patch = max(base_version[2], max(ver[2] for ver in same_line) + 1) if same_line else base_version[2]
        semver = f"{base_version[0]}.{base_version[1]}.{patch}"
        release_build = False

    if release_tag_override and release_tag_override.strip():
        override = release_tag_override.strip()
        if not TAG_SEMVER_RE.fullmatch(override):
            raise ResolveError(f"horon_release_tag must match vX.Y.Z, got {override!r}")
        release_tag = override
    else:
        release_tag = _resolve_latest_horon_release(tags)

    return {
        "sha": sha,
        "short_sha": short_sha,
        "semver": semver,
        "release_build": release_build,
        "tag": exact_tag,
        "on_main": on_main,
        "main_sha": main_sha,
        "latest_release_tag": release_tag,
    }


def _read_installed_version(dest: Path, package: str) -> str | None:
    manifest = dest / "node_modules" / Path(*package.split("/")) / "package.json"
    if not manifest.is_file():
        return None
    try:
        data = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    ver = data.get("version")
    return str(ver) if isinstance(ver, str) and ver else None


def _read_failures(path: Path) -> dict[str, str]:
    if not path.is_file():
        return {}
    out: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        name, sep, reason = line.partition("\t")
        if sep and name.strip():
            out[name.strip()] = reason.strip()
    return out


def finalize_provenance(dest: Path) -> dict:
    """Verify installed npm package versions match resolved specs and write provenance JSON."""
    resolved_path = dest / "npm-resolved.json"
    resolved_data = {}
    if resolved_path.is_file():
        resolved_data = json.loads(resolved_path.read_text(encoding="utf-8"))
    tools_meta = resolved_data.get("tools") if isinstance(resolved_data, dict) else {}
    if not isinstance(tools_meta, dict):
        tools_meta = {}
    failures = _read_failures(dest / "failures.txt")
    final_tools: dict[str, dict] = {}
    for tool, entry in tools_meta.items():
        if not isinstance(entry, dict):
            continue
        record = dict(entry)
        pkg = str(record.get("package") or "")
        install_dir = dest / tool
        installed = _read_installed_version(install_dir, pkg) if pkg else None
        record["installed_version"] = installed
        if tool in failures:
            record["status"] = "failed"
            record["error"] = failures[tool]
        elif installed:
            if record.get("version") in (None, "", "latest"):
                record["version"] = installed
            elif installed != record["version"]:
                record["version_mismatch"] = f"expected {record['version']}, installed {installed}"
            record["status"] = "installed"
        else:
            record["status"] = "present" if install_dir.exists() else "missing"
        final_tools[tool] = record
    provenance = {
        "schema": 1,
        "recorded_at": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "tools": final_tools,
    }
    (dest / "competitor-provenance.json").write_text(
        json.dumps(provenance, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return provenance


def _append_github_output(path: str | None, pairs: dict[str, str]) -> None:
    if not path:
        return
    with open(path, "a", encoding="utf-8") as handle:
        for key, value in pairs.items():
            handle.write(f"{key}={value}\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    npm_cmd = sub.add_parser("resolve-npm", help="resolve latest stable versions for npm competitors")
    npm_cmd.add_argument("--tools", default=",".join(DEFAULT_NPM_ORDER))
    npm_cmd.add_argument("--pre-resolved", default="")
    npm_cmd.add_argument("--metadata", type=Path)
    npm_cmd.add_argument("--github-output")
    npm_cmd.add_argument("--allow-fallback", action="store_true")

    prep_cmd = sub.add_parser("prepare-install", help="prepare $DEST/npm-resolved.json and shell version assignments")
    prep_cmd.add_argument("--dest", required=True, type=Path)
    prep_cmd.add_argument("--tools", required=True)
    prep_cmd.add_argument("--resolved", default="")
    prep_cmd.add_argument("--strict", action="store_true")

    fin_cmd = sub.add_parser("finalize-provenance", help="verify installed npm versions and write competitor-provenance.json")
    fin_cmd.add_argument("--dest", required=True, type=Path)

    horon_cmd = sub.add_parser("resolve-horon", help="resolve horon target commit (default origin/main) and semver")
    horon_cmd.add_argument("--repo", type=Path, default=Path("."))
    horon_cmd.add_argument("--ref", default="main")
    horon_cmd.add_argument("--main-ref", default="origin/main")
    horon_cmd.add_argument("--release-tag", default="")
    horon_cmd.add_argument("--require-main", action="store_true")
    horon_cmd.add_argument("--metadata", type=Path)
    horon_cmd.add_argument("--github-output")

    args = parser.parse_args(argv)
    try:
        if args.command == "resolve-npm":
            tools = requested_npm_tools(args.tools)
            payload = resolve_npm_many(
                tools,
                strict=not args.allow_fallback,
                pre_resolved=args.pre_resolved or None,
            )
            if args.metadata:
                args.metadata.parent.mkdir(parents=True, exist_ok=True)
                args.metadata.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
            compact = json.dumps(payload, separators=(",", ":"), ensure_ascii=False)
            _append_github_output(args.github_output, {"npm_versions_json": compact})
            print(compact)
            return 0

        if args.command == "prepare-install":
            tools = requested_npm_tools(args.tools)
            args.dest.mkdir(parents=True, exist_ok=True)
            payload = resolve_npm_many(
                tools,
                strict=args.strict,
                pre_resolved=args.resolved or None,
            )
            (args.dest / "npm-resolved.json").write_text(
                json.dumps(payload, indent=2, ensure_ascii=False) + "\n",
                encoding="utf-8",
            )
            for key, entry in payload["tools"].items():
                env_name = NPM_COMPETITORS[key]["env"]
                ver = str(entry.get("version") or "latest")
                print(f"{env_name}={ver}")
            return 0

        if args.command == "finalize-provenance":
            finalize_provenance(args.dest)
            return 0

        if args.command == "resolve-horon":
            info = resolve_horon(
                args.repo,
                args.ref,
                main_ref=args.main_ref,
                release_tag_override=args.release_tag or None,
            )
            if args.require_main and not info["on_main"]:
                raise ResolveError(
                    f"horon commit {info['sha']} is not an ancestor of {args.main_ref}"
                )
            if args.metadata:
                args.metadata.parent.mkdir(parents=True, exist_ok=True)
                args.metadata.write_text(json.dumps(info, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
            _append_github_output(
                args.github_output,
                {
                    "horon_sha": info["sha"],
                    "horon_short_sha": info["short_sha"],
                    "horon_semver": info["semver"],
                    "horon_release_build": "true" if info["release_build"] else "false",
                    "horon_on_main": "true" if info["on_main"] else "false",
                    "horon_release_tag": info["latest_release_tag"],
                },
            )
            print(json.dumps(info, separators=(",", ":"), ensure_ascii=False))
            return 0
    except ResolveError as exc:
        log(str(exc))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
