#!/usr/bin/env bash
# Build the working tree and install it as a complete release through the
# executable's own `install-release` path.
#
# Usage: scripts/install/install_from_source.sh [--fast] [--assets <dir>]
#
# --assets defaults to $HORON_AGENT_RELEASE_ASSETS. The directory must hold the
# same release's SHA256SUMS; a local build without that asset is refused rather
# than installed without a verifiable release.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

profile="${HORON_AGENT_RELEASE_PROFILE:-release-lto}"
assets_dir="${HORON_AGENT_RELEASE_ASSETS:-}"

usage() {
  echo "Usage: $0 [--fast] [--assets <dir>]" >&2
  exit 1
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --fast)
      profile="release"
      shift
      ;;
    --assets)
      [[ "$#" -ge 2 ]] || usage
      assets_dir="$2"
      shift 2
      ;;
    *)
      usage
      ;;
  esac
done

case "$profile" in
  release-lto)
    echo "Building with LTO (this takes a few minutes)..."
    ;;
  release)
    echo "Building fast release profile (no LTO)..."
    ;;
  *)
    echo "Unsupported profile: $profile (expected: release or release-lto)" >&2
    exit 1
    ;;
esac

host_os="$(uname -s)"
host_arch="$(uname -m)"
case "$host_os/$host_arch" in
  Linux/x86_64|Linux/aarch64|Linux/arm64) ;;
  Darwin/arm64|Darwin/x86_64) ;;
  FreeBSD/x86_64|FreeBSD/amd64) ;;
  *)
    echo "Unsupported host for a release install: $host_os $host_arch" >&2
    exit 1
    ;;
esac

if [[ -z "$assets_dir" ]]; then
  echo "A local release install needs the release assets: pass --assets <dir> or set HORON_AGENT_RELEASE_ASSETS." >&2
  exit 1
fi
if [[ ! -d "$assets_dir" ]]; then
  echo "Release assets directory does not exist: $assets_dir" >&2
  exit 1
fi
assets_dir="$(cd "$assets_dir" && pwd)"
if [[ ! -f "$assets_dir/SHA256SUMS" ]]; then
  echo "Release assets are missing SHA256SUMS in $assets_dir" >&2
  exit 1
fi

agent_version="$(
  awk '/^\[package\]/{seen=1; next} /^\[/{seen=0} seen && /^version[[:space:]]*=/{gsub(/[",]/, "", $3); print $3; exit}' \
    "$repo_root/Cargo.toml"
)"
if [[ -z "$agent_version" ]]; then
  echo "Could not read the package version from $repo_root/Cargo.toml" >&2
  exit 1
fi

git_hash=""
git_date=""
git_dirty="0"
if command -v git >/dev/null 2>&1; then
  if git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1; then
    git_hash="$(git -C "$repo_root" rev-parse --short HEAD 2>/dev/null || true)"
    git_date="$(git -C "$repo_root" log -1 --format=%ci 2>/dev/null || true)"
    if [[ -n "${git_hash}" ]] && [[ -n "$(git -C "$repo_root" status --porcelain 2>/dev/null || true)" ]]; then
      git_dirty="1"
    fi
  fi
fi

if [[ -n "$git_hash" ]]; then
  HORON_AGENT_BUILD_GIT_HASH="$git_hash" \
    HORON_AGENT_BUILD_GIT_DATE="$git_date" \
    HORON_AGENT_BUILD_GIT_DIRTY="$git_dirty" \
    cargo build --profile "$profile" --manifest-path "$repo_root/Cargo.toml"
else
  cargo build --profile "$profile" --manifest-path "$repo_root/Cargo.toml"
fi
bin="$repo_root/target/$profile/horon-agent"

if [[ ! -x "$bin" ]]; then
  echo "Release binary not found: $bin" >&2
  exit 1
fi

if [[ -n "$git_hash" ]]; then
  expected_git_identity="($git_hash)"
  if [[ "$git_dirty" == "1" ]]; then
    expected_git_identity="($git_hash, dirty)"
  fi
  if [[ "$("$bin" --version)" != *"$expected_git_identity"* ]]; then
    echo "Release binary does not report expected git identity: $expected_git_identity" >&2
    exit 1
  fi
fi

# The executable verifies the release, and only then advances the immutable
# version, channels and launcher with rollback. A failure leaves the previous
# installation untouched.
HORON_AGENT_NO_AUTO_UPDATE=1 \
  "$bin" install-release \
    --assets "$assets_dir" \
    --agent-version "$agent_version" \
    --source-binary "$bin"

install_dir="${HORON_AGENT_INSTALL_DIR:-${HORON_AGENT_HOME:+$HORON_AGENT_HOME/bin}}"
install_dir="${install_dir:-$HOME/.local/bin}"

echo "Installed horon-agent $agent_version"

# Configure supported desktop launch hotkeys as part of installation. This is
# idempotent and best-effort because headless installs may not expose a desktop
# session; the first interactive launch retries automatically.
case "$host_os" in
  Darwin)
    if "$install_dir/horon-agent" setup-launcher </dev/null >/dev/null 2>&1; then
      echo "Installed macOS launcher and turn-notification broker."
    fi
    if "$install_dir/horon-agent" setup-hotkey </dev/null >/dev/null 2>&1; then
      echo "Configured system-wide horon-agent launch hotkeys (when supported)."
    fi
    ;;
  Linux)
    if "$install_dir/horon-agent" setup-hotkey </dev/null >/dev/null 2>&1; then
      echo "Configured system-wide horon-agent launch hotkeys (when supported)."
    fi
    ;;
esac

# Gracefully reload any running background server onto the binary we just
# installed (issue #291). `server reload` only reloads when the running daemon
# is genuinely older, hands live headless/swarm sessions to the new process, and
# is a no-op when no server is running, so it is safe to call unconditionally.
if [ "${HORON_AGENT_SKIP_SERVER_RELOAD:-}" != "1" ]; then
  if "$install_dir/horon-agent" server reload </dev/null >/dev/null 2>&1; then
    echo "Reloaded the running horon-agent server onto $agent_version (if one was active)."
  fi
fi

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$install_dir"; then
  echo ""
  echo "Tip: add $install_dir to PATH if needed."
fi

# Ensure the launcher dir is on PATH for bash, zsh and fish in future shells.
# shellcheck source=scripts/install/lib/configure_path.sh
. "$(dirname "$0")/lib/configure_path.sh"
horon_agent_configure_path "$install_dir"
