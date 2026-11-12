#!/usr/bin/env bash
# Install a release-shaped asset directory into a temp HORON_HOME and prove the
# installed layout, so CI exercises the same archive shapes a release publishes.
#
# Release packaging runs in the release workflow only, and it cannot run before
# the change is on `main`: nothing on a pull request ever called
# `install-release --assets`. This packages the binaries the Build & Test job
# already compiled under the names `update-core` looks for on this platform,
# installs them with the command the shell installer hands over, and leaves a
# layout the terminal smoke can drive through `bin/horon`.
#
# The agent archive holds the platform asset stem and the licence the release
# ships with it, because `release_asset::stage_package` requires exactly one
# top-level executable with that name and installs the LICENSE beside it. The
# optional client ships the same way under its own stem.
#
# Usage: install_release_layout_smoke.sh <horon-agent> <horon-tui> <work-dir>

set -euo pipefail

agent=${1:?usage: install_release_layout_smoke.sh <horon-agent> <horon-tui> <work-dir>}
tui=${2:?usage: install_release_layout_smoke.sh <horon-agent> <horon-tui> <work-dir>}
work=${3:?usage: install_release_layout_smoke.sh <horon-agent> <horon-tui> <work-dir>}

for binary in "$agent" "$tui"; do
  if [ ! -x "$binary" ]; then
    echo "not an executable: $binary" >&2
    exit 1
  fi
done

# The same asset names `update-core::get_asset_name` compiles in for a platform.
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) stem=horon-agent-linux-x86_64 ;;
  Linux:aarch64 | Linux:arm64) stem=horon-agent-linux-aarch64 ;;
  Darwin:x86_64) stem=horon-agent-macos-x86_64 ;;
  Darwin:arm64 | Darwin:aarch64) stem=horon-agent-macos-aarch64 ;;
  *)
    echo "no release asset name for $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac
tui_stem="horon-tui${stem#horon-agent}"

home="$work/home"
assets="$work/assets"
stage="$work/stage"
rm -rf "$home" "$assets" "$stage"
mkdir -p "$assets" "$stage/agent" "$stage/client"

cp "$agent" "$stage/agent/$stem"
cp "$tui" "$stage/client/$tui_stem"
chmod +x "$stage/agent/$stem" "$stage/client/$tui_stem"
# The repository licence travels in every archive, so the smoke packages it too.
cp "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/LICENSE" "$stage/agent/LICENSE"
cp "$stage/agent/LICENSE" "$stage/client/LICENSE"

# COPYFILE_DISABLE keeps macOS tar from adding `._` members, which the
# extractor would reject as an unexpected member.
COPYFILE_DISABLE=1 tar -czf "$assets/$stem.tar.gz" -C "$stage/agent" "$stem" LICENSE
COPYFILE_DISABLE=1 tar -czf "$assets/$tui_stem.tar.gz" -C "$stage/client" "$tui_stem" LICENSE

# GNU sha256sum is on the ubuntu runner, shasum on macOS.
if command -v sha256sum >/dev/null 2>&1; then
  digest() { sha256sum "$1"; }
else
  digest() { shasum -a 256 "$1"; }
fi
(
  cd "$assets"
  for archive in "$stem.tar.gz" "$tui_stem.tar.gz"; do
    digest "$archive"
  done >SHA256SUMS
)
echo "--- assets ---"
cat "$assets/SHA256SUMS"

# The version directory name comes from --agent-version, and 0.0.0 is never a
# published tag, so an accidental collision with a real install is impossible.
HORON_HOME="$home" "$agent" install-release --assets "$assets" --agent-version 0.0.0

version_dir="$home/builds/versions/0.0.0"
echo "--- installed version directory ---"
ls -l "$version_dir"

for name in horon-agent horon-tui LICENSE; do
  if [ ! -f "$version_dir/$name" ]; then
    echo "installed layout is missing $version_dir/$name" >&2
    exit 1
  fi
done
for name in horon-agent horon-tui; do
  if [ ! -x "$version_dir/$name" ]; then
    echo "installed layout has a non-executable $version_dir/$name" >&2
    exit 1
  fi
done
if [ ! -L "$home/bin/horon" ]; then
  echo "$home/bin/horon is not the launcher symlink the installer leaves" >&2
  exit 1
fi
# The extractor renames the platform executable to the name the launcher and the
# sibling lookup use, so the published asset name must not survive as a file.
if [ -e "$version_dir/$stem" ]; then
  echo "the published asset name leaked into $version_dir" >&2
  exit 1
fi

echo "installed $version_dir with the optional client, launcher at $home/bin/horon"
