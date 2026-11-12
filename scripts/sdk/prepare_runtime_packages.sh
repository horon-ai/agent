#!/usr/bin/env bash
# Generate the six platform npm packages that carry the horon-agent runtime for
# `@horon-ai/agent-sdk`. They are not tracked in git: every manifest is derived
# from packages/sdk/typescript/package.json, so the SDK and its runtimes can never be
# released at different versions.
#
# usage: prepare_runtime_packages.sh <out-dir> [<release-assets-dir>]
#
# Writes <out-dir>/<platform>/{package.json,README.md,bin/}. With a release
# assets directory, each bin/ is filled from the matching release archive;
# without one, bin/ is left empty for the caller to fill (scripts/sdk/test_package.sh).
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 <out-dir> [<release-assets-dir>]" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
mkdir -p "$1"
out="$(cd "$1" && pwd)"
assets=""
if [ "$#" -eq 2 ]; then
  assets="$(cd "$2" && pwd)"
fi

prepare() {
  local platform="$1" os="$2" cpu="$3" archive="$4" archived_binary="$5" installed_binary="$6"
  local package_dir="$out/$platform"
  rm -rf "$package_dir"
  mkdir -p "$package_dir/bin"

  SDK_MANIFEST="$repo_root/packages/sdk/typescript/package.json" PACKAGE_DIR="$package_dir" \
    PLATFORM="$platform" OS="$os" CPU="$cpu" \
    node -e '
      const fs = require("node:fs");
      const sdk = JSON.parse(fs.readFileSync(process.env.SDK_MANIFEST, "utf8"));
      const scope = sdk.name.slice(0, sdk.name.indexOf("/"));
      const name = `${scope}/agent-${process.env.PLATFORM}`;
      if (sdk.optionalDependencies?.[name] !== sdk.version) {
        throw new Error(`${sdk.name} must depend on ${name}@${sdk.version} exactly`);
      }
      const manifest = {
        name,
        version: sdk.version,
        description: `HoronAgent runtime binary for ${process.env.OS} ${process.env.CPU}`,
        license: sdk.license,
        os: [process.env.OS],
        cpu: [process.env.CPU],
        files: ["bin", "README.md"],
        exports: { "./package.json": "./package.json" },
        repository: sdk.repository,
        publishConfig: { access: "public" },
      };
      const dir = process.env.PACKAGE_DIR;
      fs.writeFileSync(`${dir}/package.json`, JSON.stringify(manifest, null, 2) + "\n");
      fs.writeFileSync(
        `${dir}/README.md`,
        `# ${name}\n\nPlatform runtime used by \`${sdk.name}\`. Install the SDK rather than this package directly.\n`,
      );
    '

  if [ -n "$assets" ]; then
    tar -xzf "$assets/$archive" -C "$package_dir/bin"
    mv "$package_dir/bin/$archived_binary" "$package_dir/bin/$installed_binary"
    chmod +x "$package_dir/bin/$installed_binary"
  fi
}

prepare linux-x64 linux x64 horon-agent-linux-x86_64.tar.gz horon-agent-linux-x86_64 horon-agent
prepare linux-arm64 linux arm64 horon-agent-linux-aarch64.tar.gz horon-agent-linux-aarch64 horon-agent
prepare darwin-x64 darwin x64 horon-agent-macos-x86_64.tar.gz horon-agent-macos-x86_64 horon-agent
prepare darwin-arm64 darwin arm64 horon-agent-macos-aarch64.tar.gz horon-agent-macos-aarch64 horon-agent
prepare win32-x64 win32 x64 horon-agent-windows-x86_64.tar.gz horon-agent-windows-x86_64.exe horon-agent.exe
prepare win32-arm64 win32 arm64 horon-agent-windows-aarch64.tar.gz horon-agent-windows-aarch64.exe horon-agent.exe
