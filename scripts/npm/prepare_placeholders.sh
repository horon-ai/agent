#!/usr/bin/env bash
# Write the `0.0.0` placeholder packages that reserve the seven npm names of the
# Horon CLI, so each name exists before a Trusted Publisher can be registered
# for it.
#
# usage: prepare_placeholders.sh <out-dir>
#
# npm has no way to claim a name without publishing it, and a package's settings
# page (where the Trusted Publisher is added) only exists after the first
# publish. This writes <out-dir>/<name>/{package.json,README.md} for the seven
# names and prints the `npm publish --access public` command for each. It never
# publishes, and it registers nothing: the maintainer runs those commands once,
# by hand, with two-factor authentication. See docs/release/NPM.md.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <out-dir>" >&2
  exit 2
fi

mkdir -p "$1"
out="$(cd "$1" && pwd)"

# directory|description, in publish order: the six platform packages first, the
# wrapper last, the order `scripts/npm/release_npm_version.py publish-order`
# reports and the publish workflow follows.
packages=(
  "horon-linux-x64|Horon CLI binaries for Linux x64 (placeholder reserving the name)"
  "horon-linux-arm64|Horon CLI binaries for Linux arm64 (placeholder reserving the name)"
  "horon-darwin-x64|Horon CLI binaries for macOS x64 (placeholder reserving the name)"
  "horon-darwin-arm64|Horon CLI binaries for macOS arm64 (placeholder reserving the name)"
  "horon-win32-x64|Horon CLI binaries for Windows x64 (placeholder reserving the name)"
  "horon-win32-arm64|Horon CLI binaries for Windows arm64 (placeholder reserving the name)"
  "horon|Horon CLI launcher and platform packages (placeholder reserving the name)"
)

for entry in "${packages[@]}"; do
  directory="${entry%%|*}"
  description="${entry#*|}"
  name="@horon-ai/$directory"
  package="$out/$directory"
  rm -rf "$package"
  mkdir -p "$package"
  # No scripts, no `bin`, no payload: the tarball a first publish uploads holds
  # the manifest and one line of README, so a placeholder can never shadow a
  # release binary of the same name.
  cat >"$package/package.json" <<JSON
{
  "name": "$name",
  "version": "0.0.0",
  "description": "$description",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/horon-ai/agent.git"
  },
  "files": [
    "README.md"
  ],
  "publishConfig": {
    "access": "public"
  }
}
JSON
  printf '# %s\n\nPlaceholder that reserves this name. Real versions are published from github.com/horon-ai/agent.\n' \
    "$name" >"$package/README.md"
done

echo "Placeholders written under $out"
echo
echo "Check which names already exist (a name that answers has a package page):"
for entry in "${packages[@]}"; do
  echo "  npm view \"@horon-ai/${entry%%|*}\" version"
done
echo
echo "Publish the ones that do not exist yet, once, by hand (npm asks for the 2FA code):"
for entry in "${packages[@]}"; do
  echo "  npm publish \"$out/${entry%%|*}\" --access public"
done
