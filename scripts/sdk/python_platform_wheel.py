#!/usr/bin/env python3
"""Retag the pure horon-agent-sdk wheel as a platform wheel carrying the runtime.

The Python SDK is pure Python, so ``uv build`` produces one
``py3-none-any`` wheel. Users still need a runtime, and the npm SDK expresses
that with optional per-platform packages. Python has no equivalent mechanism, so
each release asset is baked into a platform-tagged wheel instead: pip and uv
then pick the wheel that matches the consumer's machine, and
``horon_agent_sdk/_bin/horon-agent`` resolves without a download at import time.

This script is deliberately stdlib-only: it runs in the release workflow before
the SDK's own environment exists, and a retagging bug should be diagnosable with
``unzip -l`` rather than a dependency trace.

usage:
    python_platform_wheel.py --pure-wheel dist/x-1.2.0-py3-none-any.whl \
        --asset horon-agent-linux-x86_64.tar.gz --primary horon-agent-linux-x86_64 \
        --tag manylinux2014_x86_64 --out-dir dist
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import os
import pathlib
import re
import sys
import tarfile
import tempfile
import zipfile

WHEEL_NAME = re.compile(r"^(?P<name>[^-]+)-(?P<version>[^-]+)-(?P<py>[^-]+)-(?P<abi>[^-]+)-(?P<plat>[^-]+)\.whl$")
BIN_DIR = "horon_agent_sdk/_bin"
SKIPPED_NAMES = {".DS_Store"}
SKIPPED_PREFIXES = ("__MACOSX/",)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pure-wheel", required=True, type=pathlib.Path)
    parser.add_argument("--asset", required=True, type=pathlib.Path)
    parser.add_argument("--primary", required=True, help="name of the archive's launcher or binary")
    parser.add_argument("--tag", required=True, help="platform tag, e.g. manylinux2014_x86_64")
    parser.add_argument("--out-dir", required=True, type=pathlib.Path)
    return parser.parse_args(argv)


def extract_asset(asset: pathlib.Path, destination: pathlib.Path) -> None:
    """Extract every regular file in ``asset`` into ``destination``, flattened."""
    if tarfile.is_tarfile(asset):
        with tarfile.open(asset) as archive:
            members = archive.getmembers()
            for member in members:
                if not member.isfile():
                    continue
                name = _safe_name(member.name)
                if name is None:
                    continue
                source = archive.extractfile(member)
                if source is None:
                    raise SystemExit(f"could not read {member.name} from {asset}")
                target = destination / name
                target.write_bytes(source.read())
                target.chmod(member.mode & 0o777 or 0o644)
        return

    with zipfile.ZipFile(asset) as archive:
        for entry in archive.infolist():
            if entry.is_dir():
                continue
            name = _safe_name(entry.filename)
            if name is None:
                continue
            target = destination / name
            target.write_bytes(archive.read(entry))
            mode = (entry.external_attr >> 16) & 0o777
            target.chmod(mode or 0o644)


def _safe_name(member_name: str) -> str | None:
    if any(member_name.startswith(prefix) for prefix in SKIPPED_PREFIXES):
        return None
    base = os.path.basename(member_name)
    if not base or base in SKIPPED_NAMES:
        return None
    return base


def record_hash(data: bytes) -> str:
    digest = hashlib.sha256(data).hexdigest()
    encoded = base64.urlsafe_b64encode(bytes.fromhex(digest)).rstrip(b"=").decode("ascii")
    return f"sha256={encoded}"


def read_wheel(wheel: pathlib.Path) -> dict[str, tuple[bytes, int]]:
    with zipfile.ZipFile(wheel) as archive:
        return {
            entry.filename: (archive.read(entry), (entry.external_attr >> 16) & 0o777 or 0o644)
            for entry in archive.infolist()
            if not entry.is_dir()
        }


def retag_wheel(wheel_contents: dict[str, tuple[bytes, int]], tag: str) -> dict[str, tuple[bytes, int]]:
    """Point WHEEL at the platform tag and stop claiming a pure install root."""
    updated: dict[str, tuple[bytes, int]] = {}
    for name, (data, mode) in wheel_contents.items():
        if name.endswith(".dist-info/WHEEL"):
            text = data.decode("utf-8")
            text = re.sub(r"(?m)^Tag: .*$", f"Tag: py3-none-{tag}", text)
            if "Root-Is-Purelib:" in text:
                text = re.sub(r"(?m)^Root-Is-Purelib: .*$", "Root-Is-Purelib: false", text)
            else:
                text = text.rstrip("\n") + "\nRoot-Is-Purelib: false\n"
            data = text.encode("utf-8")
        updated[name] = (data, mode)
    return updated


def add_binaries(wheel_contents: dict[str, tuple[bytes, int]], binary_dir: pathlib.Path, primary: str) -> None:
    files = sorted(path for path in binary_dir.iterdir() if path.is_file())
    if not files:
        raise SystemExit(f"no files extracted from the runtime asset for {primary}")
    # MIT requires the notice to travel with every copy it covers, and this wheel
    # ships a copy of the binary outside the pure wheel's dist-info. A release
    # archive always carries LICENSE, so its absence here means the asset is
    # wrong, not that the terms are optional.
    if not any(path.name == "LICENSE" for path in files):
        raise SystemExit(f"the runtime asset for {primary} carries no LICENSE")

    primary_name = "horon-agent.exe" if primary.endswith(".exe") else "horon-agent"
    launcher_written = False
    for path in files:
        data = path.read_bytes()
        mode = 0o755 if os.access(path, os.X_OK) or path.name == primary else 0o644
        wheel_contents[f"{BIN_DIR}/{path.name}"] = (data, mode)
        if path.name == primary:
            # The entry point callers and ``bundled_horon_agent_binary`` expect.
            wheel_contents[f"{BIN_DIR}/{primary_name}"] = (data, 0o755)
            launcher_written = True
    if not launcher_written:
        raise SystemExit(f"the runtime asset does not contain the expected primary file {primary!r}")


def write_wheel(
    destination: pathlib.Path,
    wheel_contents: dict[str, tuple[bytes, int]],
) -> None:
    record_name = next(
        (name for name in wheel_contents if name.endswith(".dist-info/RECORD")),
        None,
    )
    if record_name is None:
        raise SystemExit("the pure wheel has no .dist-info/RECORD")

    lines: list[str] = []
    for name in sorted(wheel_contents):
        if name == record_name:
            continue
        data, _mode = wheel_contents[name]
        lines.append(f"{name},{record_hash(data)},{len(data)}")
    lines.append(f"{record_name},,")
    wheel_contents[record_name] = ("\n".join(lines) + "\n").encode("utf-8"), 0o644

    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED, allowZip64=True) as archive:
        for name in sorted(wheel_contents):
            data, mode = wheel_contents[name]
            info = zipfile.ZipInfo(name)
            info.date_time = (1980, 1, 1, 0, 0, 0)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (0o100000 | mode) << 16
            archive.writestr(info, data)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    match = WHEEL_NAME.match(args.pure_wheel.name)
    if match is None:
        raise SystemExit(f"not a wheel filename: {args.pure_wheel.name}")
    if match.group("plat") != "any":
        raise SystemExit(f"expected a pure wheel, got platform tag {match.group('plat')!r}")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    platform_wheel = args.out_dir / (
        f"{match.group('name')}-{match.group('version')}-py3-none-{args.tag}.whl"
    )

    with tempfile.TemporaryDirectory(prefix="horon-agent-python-wheel-") as temp:
        binary_dir = pathlib.Path(temp)
        extract_asset(args.asset, binary_dir)
        contents = retag_wheel(read_wheel(args.pure_wheel), args.tag)
        add_binaries(contents, binary_dir, args.primary)
        write_wheel(platform_wheel, contents)

    print(f"built {platform_wheel}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
