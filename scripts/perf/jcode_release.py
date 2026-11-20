#!/usr/bin/env python3
"""Resolve, download and verify one public jcode release binary.

`install_competitors.sh` and the benchmark workflows use this instead of a
version constant, so a default run measures the executable GitHub currently
publishes to jcode's users, and every run records exactly what it installed.

Resolution:

    GET /repos/1jehuang/jcode/releases/latest       (or /releases/tags/<tag>)

The tag must be a stable `vX.Y.Z`; a draft, a prerelease or a tag without
release assets is refused rather than replaced by an older release or a source
build. The tag is peeled to its commit through the git refs API (an annotated
tag is followed to the commit it points at), so the record names a commit even
when the tag is moved later.

Verification, in this order, each a hard failure:

1. the release lists an asset with this platform's exact name;
2. the downloaded bytes match the digest for that exact name in the release's
   `SHA256SUMS` asset (both downloads fall back to the release asset API by id
   when the release download URL fails, and curl retries transient errors);
3. the extracted binary's own `--version` names the resolved tag.

`resolve` stops after the tag and its commit; `materialize` downloads, verifies
and prints the binary path. Metadata is written as JSON so the run artifact
keeps the tag, commit, asset name, download URL, SHA-256 and version output.

The release binary is executed with an environment that deliberately leaves out
`GH_TOKEN` and `GITHUB_TOKEN`: the token is only used for the GitHub API calls
that need it.

Subcommands:

    resolve     [--tag TAG] [--metadata PATH] [--github-output PATH]
    materialize [--tag TAG] [--expected-commit SHA] --dest DIR --metadata PATH
                [--skip-version-exec]

`--tag` falls back to `JCODE_VERSION`; without either the newest stable release
is used.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

REPO = "1jehuang/jcode"
API = f"https://api.github.com/repos/{REPO}"
WEB = f"https://github.com/{REPO}"
SUMS_ASSET = "SHA256SUMS"

TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
SHA_RE = re.compile(r"^[0-9a-f]{40,64}$")
SEMVER_RE = re.compile(r"(\d+)\.(\d+)\.(\d+)")

# What running a downloaded binary needs. Credentials are absent on purpose: a
# release binary must never see the token the API calls use.
VERSION_ENV = ("PATH", "HOME", "SystemRoot", "SystemDrive", "windir", "COMSPEC", "PATHEXT",
               "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "NO_COLOR")


class ReleaseError(Exception):
    """A release could not be resolved or verified; the message is the reason."""


def log(message: str) -> None:
    print(f"jcode_release: {message}", file=sys.stderr, flush=True)


def asset_name(system: str | None = None, machine: str | None = None) -> str | None:
    """The release asset name for one platform, or None when jcode ships none."""
    system = system or platform.system()
    machine = (machine or platform.machine()).lower()
    if system == "Darwin":
        arch = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}.get(machine)
        return f"jcode-macos-{arch}.tar.gz" if arch else None
    if system == "Linux":
        arch = {"aarch64": "aarch64", "arm64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}.get(machine)
        return f"jcode-linux-{arch}.tar.gz" if arch else None
    if system == "FreeBSD":
        return "jcode-freebsd-x86_64.tar.gz" if machine in ("amd64", "x86_64") else None
    if system == "Windows":
        if machine.startswith("arm"):
            return "jcode-windows-aarch64.exe"
        if machine in ("amd64", "x86_64"):
            return "jcode-windows-x86_64.exe"
    return None


def validate_tag(tag: str) -> str:
    """Accept only a stable `vX.Y.Z`, so a tag can never carry shell syntax."""
    if not TAG_RE.fullmatch(tag or ""):
        raise ReleaseError(f"expected a stable release tag like v1.2.3, got {tag!r}")
    return tag


def version_matches(tag: str, text: str) -> bool:
    """True when a `--version` line names exactly the resolved tag."""
    found = SEMVER_RE.search(text or "")
    want = TAG_RE.fullmatch(tag or "")
    return bool(found and want and found.groups() == want.groups())


def manifest_digest(manifest: str, name: str) -> str | None:
    """The digest SHA256SUMS records for the exact asset name, or None."""
    for line in manifest.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].lstrip("*") == name:
            digest = parts[0].lower()
            return digest if re.fullmatch(r"[0-9a-f]{64}", digest) else None
    return None


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _curl(args: list[str], timeout: int = 900) -> bytes:
    if shutil.which("curl") is None:
        raise ReleaseError("curl is required to reach the GitHub release")
    cmd = ["curl", "--fail", "--location", "--silent", "--show-error",
           "--retry", "5", "--retry-delay", "2", *args]
    try:
        done = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, timeout=timeout)
    except (OSError, subprocess.SubprocessError) as exc:
        raise ReleaseError(f"curl could not run: {exc}") from exc
    if done.returncode:
        detail = done.stderr.decode(errors="replace").strip()
        raise ReleaseError(detail.splitlines()[-1] if detail else f"curl exited {done.returncode}")
    return done.stdout


def _api_headers() -> list[str]:
    headers = ["--header", "Accept: application/vnd.github+json"]
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers += ["--header", f"Authorization: Bearer {token}"]
    return headers


def fetch_json(url: str) -> dict:
    try:
        body = _curl([*_api_headers(), url], timeout=120)
    except ReleaseError as exc:
        raise ReleaseError(f"could not read GitHub release metadata at {url}: {exc}") from exc
    try:
        data = json.loads(body.decode("utf-8"))
    except ValueError as exc:
        raise ReleaseError(f"invalid JSON from {url}: {exc}") from exc
    if not isinstance(data, dict):
        raise ReleaseError(f"unexpected JSON from {url}")
    return data


def release_for(tag: str | None) -> tuple[dict, str, str]:
    """(release JSON, stable tag, API URL) for the pin or the newest release."""
    if tag:
        tag = validate_tag(tag)
        url = f"{API}/releases/tags/{tag}"
    else:
        url = f"{API}/releases/latest"
    release = fetch_json(url)
    resolved = str(release.get("tag_name") or "")
    if release.get("draft") or release.get("prerelease"):
        raise ReleaseError(f"{resolved or url} is not a stable published release")
    resolved = validate_tag(resolved)
    if tag and resolved != tag:
        raise ReleaseError(f"release API returned {resolved!r} for the pinned tag {tag!r}")
    return release, resolved, url


def peeled_commit(tag: str) -> str:
    """Follow the tag ref (and any annotated tag objects) to its commit."""
    node = fetch_json(f"{API}/git/ref/tags/{tag}").get("object") or {}
    for _ in range(5):
        sha = str(node.get("sha") or "")
        if not SHA_RE.fullmatch(sha):
            raise ReleaseError(f"tag {tag} points at an invalid object sha {sha!r}")
        kind = node.get("type")
        if kind == "commit":
            return sha
        if kind != "tag":
            raise ReleaseError(f"tag {tag} points at a {kind}, not a commit")
        node = fetch_json(f"{API}/git/tags/{sha}").get("object") or {}
    raise ReleaseError(f"tag {tag} is nested too deeply to peel to a commit")


def find_asset(release: dict, name: str, tag: str) -> dict:
    assets = [a for a in release.get("assets") or [] if isinstance(a, dict)]
    for asset in assets:
        if asset.get("name") == name:
            return asset
    available = ", ".join(sorted(str(a.get("name")) for a in assets)) or "none"
    raise ReleaseError(f"release {tag} has no asset named {name} (available: {available})")


def fetch_asset(tag: str, asset: dict, out: Path, name: str) -> tuple[str, str]:
    """Download one release asset; return (URL that served it, how it was served).

    The release download URL is tried first, then the same asset by its numeric
    id through the release API. curl already retried transient errors inside
    each call. The returned URL is the one the bytes came from, so a record
    never claims the release download served a fallback transfer.
    """
    url = f"{WEB}/releases/download/{tag}/{name}"
    try:
        _curl(["--output", str(out), url])
        return url, "release-download"
    except ReleaseError as first:
        log(f"{name}: release download failed ({first}); retrying by asset id")
    asset_id = asset.get("id")
    if not isinstance(asset_id, int) or asset_id <= 0:
        raise ReleaseError(f"download failed: {url}; the release lists no numeric asset id")
    api_url = f"{API}/releases/assets/{asset_id}"
    try:
        _curl([*_api_headers(), "--header", "Accept: application/octet-stream",
               "--output", str(out), api_url])
    except ReleaseError as second:
        raise ReleaseError(f"download failed: {url} and {api_url} ({second})")
    return api_url, "asset-api"


def unpack(archive: Path, dest: Path, name: str) -> Path:
    """Extract the release archive into a fresh directory and return the binary."""
    dest.mkdir(parents=True, exist_ok=True)
    if name.endswith(".exe"):
        binary = dest / name
        shutil.copyfile(archive, binary)
        return binary
    with tarfile.open(archive, "r:*") as tar:
        for member in tar.getmembers():
            if not member.isfile():
                continue
            base = Path(member.name)
            if len(base.parts) != 1:
                raise ReleaseError(f"release archive {name} contains a path: {member.name}")
            source = tar.extractfile(member)
            if source is None:
                raise ReleaseError(f"release archive {name} could not read {member.name}")
            with source, open(dest / base.name, "wb") as target:
                shutil.copyfileobj(source, target)
            (dest / base.name).chmod(member.mode | 0o600)
    for candidate in sorted(dest.iterdir()):
        if candidate.is_file() and candidate.name.startswith("jcode-") and not candidate.name.endswith(".bin"):
            return candidate
    raise ReleaseError(f"release archive {name} contained no jcode binary")


def verify_version(binary: Path, tag: str) -> str:
    """Run `--version` once with no credentials and require the resolved tag."""
    env = {key: os.environ[key] for key in VERSION_ENV if key in os.environ}
    try:
        done = subprocess.run([str(binary), "--version"], stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              timeout=120, env=env)
    except (OSError, subprocess.SubprocessError) as exc:
        raise ReleaseError(f"--version could not run: {exc}") from exc
    text = done.stdout.decode(errors="replace").strip()
    if done.returncode:
        raise ReleaseError(f"--version exited {done.returncode}: {text[-300:]}")
    if not version_matches(tag, text):
        raise ReleaseError(f"--version reports {text[:160]!r}, expected {tag}")
    return text


def selection_of(args: argparse.Namespace) -> str:
    """How the tag was chosen: `latest` for the newest stable release, `pin` for an explicit tag.

    A caller that hands this resolver only a tag (a rerun, or an OS job that
    received the resolved tag) has pinned it, unless it also says the tag came
    from a latest lookup (`--selection`/`JCODE_RELEASE_SOURCE`).
    """
    declared = (getattr(args, "selection", None) or os.environ.get("JCODE_RELEASE_SOURCE") or "").strip().lower()
    if declared:
        if declared not in ("latest", "pin"):
            raise ReleaseError(f"selection must be latest or pin, got {declared!r}")
        return declared
    return "pin" if (args.tag or os.environ.get("JCODE_VERSION")) else "latest"


def metadata_record(*, tag: str, commit: str, selection: str, api_url: str,
                    name: str | None = None, download_url: str | None = None,
                    served_url: str | None = None, served_by: str | None = None,
                    sha256: str | None = None, binary: Path | None = None,
                    version_output: str | None = None,
                    verified_version: bool = False) -> dict:
    return {
        "tool": "jcode",
        "repo": REPO,
        # The tag choice (newest stable release, or a pin) is separate from the
        # transfer: an OS job that receives a resolved tag records `selection`
        # as latest while its own `served_by` names this runner's download path.
        "selection": selection,
        "tag": tag,
        "version": tag[1:],
        "commit": commit,
        "asset": name,
        "download_url": download_url,
        "served_url": served_url,
        "served_by": served_by,
        "sha256": sha256,
        "verified_sha256": sha256 is not None,
        "verified_version": verified_version,
        "version_output": version_output,
        "binary": str(binary) if binary else None,
        "api_url": api_url,
        "resolved_at": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }


def write_metadata(path: Path | None, record: dict) -> None:
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def write_github_output(path: str | None, record: dict) -> None:
    if not path:
        return
    with open(path, "a", encoding="utf-8") as out:
        out.write(f"tag={record['tag']}\n")
        out.write(f"version={record['version']}\n")
        out.write(f"commit={record['commit']}\n")
        out.write(f"selection={record['selection']}\n")


def resolve(args: argparse.Namespace) -> int:
    tag = args.tag or os.environ.get("JCODE_VERSION") or None
    selection = selection_of(args)
    release, tag, api_url = release_for(tag)
    commit = peeled_commit(tag)
    record = metadata_record(tag=tag, commit=commit, selection=selection, api_url=api_url)
    write_metadata(args.metadata, record)
    write_github_output(args.github_output, record)
    log(f"resolved {tag} (commit {commit}) from {api_url} ({selection})")
    print(f"tag={tag}")
    print(f"commit={commit}")
    print(f"selection={selection}")
    return 0


def resolve_platform_release(tag: str | None, selection: str, expected_commit: str | None) -> dict:
    """Resolve the platform's download before an install benchmark starts timing."""
    release, tag, api_url = release_for(tag)
    commit = peeled_commit(tag)
    if expected_commit:
        if not SHA_RE.fullmatch(expected_commit):
            raise ReleaseError(f"expected commit must be a hex sha, got {expected_commit!r}")
        if expected_commit != commit:
            raise ReleaseError(f"release ref moved: expected {expected_commit}, {tag} now points at {commit}")
    name = asset_name()
    if name is None:
        raise ReleaseError(f"no jcode release asset for {platform.system()}/{platform.machine()}")
    return {"tag": tag, "commit": commit, "selection": selection, "api_url": api_url, "name": name,
            "asset_id": find_asset(release, name, tag).get("id"),
            "sums_id": find_asset(release, SUMS_ASSET, tag).get("id")}


def materialize(args: argparse.Namespace) -> int:
    tag = args.tag or os.environ.get("JCODE_VERSION") or None
    expected_commit = args.expected_commit or None
    selection = selection_of(args)
    if plan := os.environ.get("JCODE_INSTALL_PLAN"):
        record = json.loads(plan)
        if (record["name"] != asset_name() or not SHA_RE.fullmatch(record["commit"])
                or (tag and tag != record["tag"]) or (expected_commit and expected_commit != record["commit"])
                or record["selection"] not in ("latest", "pin")):
            raise ReleaseError("install plan does not match the platform or resolved release")
    else:
        record = resolve_platform_release(tag, selection, expected_commit)
    tag = validate_tag(record["tag"])
    commit, selection, api_url, name = (record[key] for key in ("commit", "selection", "api_url", "name"))
    asset = {"id": record["asset_id"]}
    sums_asset = {"id": record["sums_id"]}
    dest = Path(args.dest)
    with tempfile.TemporaryDirectory(prefix="jcode-release-") as tmp:
        archive = Path(tmp) / name
        served_url, served_by = fetch_asset(tag, asset, archive, name)
        sums = Path(tmp) / SUMS_ASSET
        fetch_asset(tag, sums_asset, sums, SUMS_ASSET)
        expected = manifest_digest(sums.read_text(encoding="utf-8", errors="replace"), name)
        if not expected:
            raise ReleaseError(f"SHA256SUMS has no digest for {name}")
        actual = sha256_of(archive)
        if actual != expected:
            raise ReleaseError(f"SHA-256 mismatch for {name}: got {actual}, SHA256SUMS lists {expected}")
        # Only verified bytes reach the destination directory.
        shutil.rmtree(dest, ignore_errors=True)
        binary = unpack(archive, dest, name)
    version_output, verified = None, False
    if not args.skip_version_exec:
        version_output = verify_version(binary, tag)
        verified = True
        log(f"{binary} --version -> {version_output}")
    else:
        log(f"{binary}: version execution skipped (the timed first --version checks it)")
    download_url = f"{WEB}/releases/download/{tag}/{name}"
    record = metadata_record(tag=tag, commit=commit, selection=selection, api_url=api_url,
                             name=name, download_url=download_url, served_url=served_url,
                             served_by=served_by, sha256=actual, binary=binary,
                             version_output=version_output, verified_version=verified)
    write_metadata(Path(args.metadata) if args.metadata else None, record)
    log(f"resolved {tag} (commit {commit}) by {selection}, asset {name} via {served_by}, sha256 {actual}")
    print(binary)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name, help_text in (("resolve", "resolve the release tag and its commit only"),
                            ("materialize", "download, verify and print the binary path")):
        command = sub.add_parser(name, help=help_text)
        command.add_argument("--tag", help="published vX.Y.Z tag (default: JCODE_VERSION or the newest stable release)")
        command.add_argument("--selection", choices=("latest", "pin"),
                             help="how the tag was chosen, when a caller passes a tag resolved from the newest release "
                                  "(default: JCODE_RELEASE_SOURCE, else pin when a tag is given and latest otherwise)")
        command.add_argument("--metadata", type=Path, help="write the release record as JSON here")
        command.add_argument("--github-output", help="append tag/version/commit/selection to this GITHUB_OUTPUT file")
        if name == "materialize":
            command.add_argument("--dest", required=True, help="directory that receives the verified binary")
            command.add_argument("--expected-commit", help="refuse a release whose tag no longer points at this commit")
            command.add_argument("--skip-version-exec", action="store_true",
                                 help="do not run --version; the caller's timed first --version checks it instead")
    args = parser.parse_args(argv)
    try:
        return resolve(args) if args.command == "resolve" else materialize(args)
    except (ReleaseError, ValueError, KeyError, TypeError) as exc:
        log(str(exc))
        return 1


if __name__ == "__main__":
    sys.exit(main())
