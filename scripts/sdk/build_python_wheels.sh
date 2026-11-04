#!/usr/bin/env bash
# Build the Python SDK release: one sdist, one pure wheel, and six platform
# wheels carrying the runtime under horon_agent_sdk/_bin/.
#
# usage: build_python_wheels.sh <release-assets-dir> <out-dir>
#
# <release-assets-dir> holds the six archives published by the horon-agent
# release (the same names scripts/sdk/prepare_runtime_packages.sh consumes).
# <out-dir> is created from scratch and, on success, contains exactly the sdist,
# the pure wheel, and the six platform wheels.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <release-assets-dir> <out-dir>" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
assets="$(cd "$1" && pwd)"
mkdir -p "$2"
out="$(cd "$2" && pwd)"
# Pick an interpreter that actually runs. A stale PYTHON environment variable
# (a version-pinned wrapper that no longer exists) must not break a release.
resolve_python() {
  for candidate in "${PYTHON:-}" python3 python; do
    [ -n "$candidate" ] || continue
    if command -v "$candidate" >/dev/null 2>&1 \
      && "$candidate" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}
python="$(resolve_python)" || {
  echo "error: no Python 3.9+ interpreter found (set PYTHON to one)" >&2
  exit 1
}

if ! command -v uv >/dev/null 2>&1; then
  echo "error: uv is required to build the Python SDK (https://docs.astral.sh/uv/)" >&2
  exit 1
fi

# Fail before building anything when a release asset is missing: a half-filled
# dist directory looks like a successful release to an upload step.
require_asset() {
  if [ ! -f "$assets/$1" ]; then
    echo "error: missing release asset $assets/$1" >&2
    exit 1
  fi
}
for archive in \
  horon-agent-linux-x86_64.tar.gz \
  horon-agent-linux-aarch64.tar.gz \
  horon-agent-macos-x86_64.tar.gz \
  horon-agent-macos-aarch64.tar.gz \
  horon-agent-windows-x86_64.tar.gz \
  horon-agent-windows-aarch64.tar.gz
do
  require_asset "$archive"
done

rm -rf "$out"
mkdir -p "$out"

# The pure wheel and sdist contain no runtime: install them anywhere and
# launch() falls back to `horon-agent` on PATH.
(cd "$repo_root/sdk/python" && uv build --out-dir "$out")

pure_wheel="$(find "$out" -maxdepth 1 -name 'horon_agent_sdk-*-py3-none-any.whl' -print -quit)"
if [ -z "$pure_wheel" ]; then
  echo "error: uv build did not produce a pure wheel in $out" >&2
  exit 1
fi

build_platform() {
  local tag="$1" archive="$2" primary="$3"
  "$python" "$repo_root/scripts/sdk/python_platform_wheel.py" \
    --pure-wheel "$pure_wheel" \
    --asset "$assets/$archive" \
    --primary "$primary" \
    --tag "$tag" \
    --out-dir "$out"
}

# Linux keeps the launcher wrapper plus its `.bin` payload: the wrapper resolves
# `horon-agent-linux-x86_64.bin` next to itself, so both files must ship.
build_platform manylinux2014_x86_64 horon-agent-linux-x86_64.tar.gz horon-agent-linux-x86_64
build_platform manylinux2014_aarch64 horon-agent-linux-aarch64.tar.gz horon-agent-linux-aarch64
build_platform macosx_10_12_x86_64 horon-agent-macos-x86_64.tar.gz horon-agent-macos-x86_64
build_platform macosx_11_0_arm64 horon-agent-macos-aarch64.tar.gz horon-agent-macos-aarch64
build_platform win_amd64 horon-agent-windows-x86_64.tar.gz horon-agent-windows-x86_64.exe
build_platform win_arm64 horon-agent-windows-aarch64.tar.gz horon-agent-windows-aarch64.exe

echo "Python SDK release artifacts in $out:"
ls -1 "$out"
