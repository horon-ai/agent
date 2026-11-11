#!/usr/bin/env bash
# Build the npm distribution of the Horon CLI: one package per platform plus
# the thin `@horon-ai/horon` wrapper that picks one of them at run time.
#
# usage: prepare_cli_packages.sh <out-dir> <assets-dir> <version>
#
# <assets-dir> holds the public release archives (`horon-agent-<suffix>.tar.gz`,
# `horon-tui-<suffix>.tar.gz` for the six suffixes) and their `SHA256SUMS`, the
# same names a `package_only` release run attaches. <version> is the release tag
# without its leading `v`; nothing here reads a version from a manifest, so the
# packages can never describe a different release than the archives they carry.
#
# Writes <out-dir>/<platform>/ for every platform and <out-dir>/horon/ for the
# wrapper. Every archive is checked against SHA256SUMS and unpacked into a
# staging directory first, so a bad or missing archive never leaves a partial
# set of packages behind.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <out-dir> <assets-dir> <version>" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
wrapper_src="$repo_root/npm/horon"

fail() {
  echo "prepare_cli_packages: $*" >&2
  exit 1
}

version="$3"
case "$version" in
  v*) fail "version must not start with v: $version" ;;
esac
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
  fail "version is not semver: $version"
fi

mkdir -p "$1"
out="$(cd "$1" && pwd)"
[ -d "$2" ] || fail "assets directory does not exist: $2"
assets="$(cd "$2" && pwd)"

[ -f "$wrapper_src/package.json" ] || fail "missing $wrapper_src/package.json"
[ -f "$wrapper_src/bin/horon.js" ] || fail "missing $wrapper_src/bin/horon.js"
[ -f "$wrapper_src/README.md" ] || fail "missing $wrapper_src/README.md"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    fail "need sha256sum or shasum to verify release archives"
  fi
}

sums="$assets/SHA256SUMS"
[ -f "$sums" ] || fail "missing $sums"

# Platform -> (npm platform, os, cpu, release suffix, archive shape).
# The shape decides which members an archive must hold and what the installed
# entry file is called. `wrapper` covers the portable linux-x86_64 build, whose
# shell wrapper execs `<artifact>.bin` by name; only the wrapper is renamed, so
# the payload keeps the name the wrapper looks for.
platforms="linux-x64 linux-arm64 darwin-x64 darwin-arm64 win32-x64 win32-arm64"

suffix_of() {
  case "$1" in
    linux-x64) echo "linux-x86_64" ;;
    linux-arm64) echo "linux-aarch64" ;;
    darwin-x64) echo "macos-x86_64" ;;
    darwin-arm64) echo "macos-aarch64" ;;
    win32-x64) echo "windows-x86_64" ;;
    win32-arm64) echo "windows-aarch64" ;;
  esac
}

os_of() {
  case "$1" in
    linux-*) echo "linux" ;;
    darwin-*) echo "darwin" ;;
    win32-*) echo "win32" ;;
  esac
}

cpu_of() {
  case "$1" in
    *-x64) echo "x64" ;;
    *-arm64) echo "arm64" ;;
  esac
}

shape_of() {
  case "$1" in
    linux-x64) echo "wrapper" ;;
    win32-*) echo "exe" ;;
    *) echo "file" ;;
  esac
}

# Resolve every required archive and its recorded digest before writing
# anything, so a bad checksum or a missing platform fails on its own name.
for platform in $platforms; do
  suffix="$(suffix_of "$platform")"
  for stem in horon-agent horon-tui; do
    archive="$stem-$suffix.tar.gz"
    [ -f "$assets/$archive" ] || fail "missing release archive: $archive"
    recorded="$(awk -v name="$archive" '$2 == name { print $1; exit }' "$sums")"
    [ -n "$recorded" ] || fail "missing SHA256SUMS entry: $archive"
    actual="$(sha256_of "$assets/$archive")"
    if [ "$recorded" != "$actual" ]; then
      fail "checksum mismatch: $archive (SHA256SUMS says $recorded, file is $actual)"
    fi
  done
done

staging="$(mktemp -d "$out/.staging.XXXXXX")"
cleanup() { rm -rf "$staging"; }
trap cleanup EXIT

# Unpack one archive into <bin-dir>, renaming only the entry file. `$1` is the
# archive stem, `$2` the release suffix, `$3` the install entry name, `$4` the
# target bin directory, `$5` the archive shape.
unpack() {
  local stem="$1" suffix="$2" entry="$3" bin_dir="$4" shape="$5"
  local archive="$stem-$suffix.tar.gz"
  local artifact="$stem-$suffix"
  local members expected

  members="$(tar -tzf "$assets/$archive" | sed 's#^\./##' | awk 'NF' | LC_ALL=C sort | tr '\n' ' ')"
  case "$shape" in
    wrapper) expected="$artifact $artifact.bin " ;;
    exe) expected="$artifact.exe " ;;
    *) expected="$artifact " ;;
  esac
  if [ "$members" != "$expected" ]; then
    fail "unexpected members in $archive: got [$members], expected [$expected]"
  fi

  tar -xzf "$assets/$archive" -C "$bin_dir"
  case "$shape" in
    wrapper)
      mv "$bin_dir/$artifact" "$bin_dir/$entry"
      chmod +x "$bin_dir/$entry" "$bin_dir/$artifact.bin"
      ;;
    exe)
      mv "$bin_dir/$artifact.exe" "$bin_dir/$entry"
      chmod +x "$bin_dir/$entry"
      ;;
    *)
      mv "$bin_dir/$artifact" "$bin_dir/$entry"
      chmod +x "$bin_dir/$entry"
      ;;
  esac
}

# Run the host platform's agent once and echo what it printed. Downloading a
# binary and running it is only acceptable for the platform we are on, so the
# version check is host-only; the other packages are covered by checksums and
# the install smoke test of the publish workflow.
version_output() {
  local binary="$1" log pid ticks=0
  log="$(mktemp)"
  "$binary" --version >"$log" 2>&1 &
  pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    ticks=$((ticks + 1))
    if [ "$ticks" -ge 300 ]; then
      kill -TERM "$pid" 2>/dev/null || true
      kill -KILL "$pid" 2>/dev/null || true
      break
    fi
    sleep 0.1
  done
  wait "$pid" 2>/dev/null || true
  cat "$log"
  rm -f "$log"
}

host_platform() {
  local os arch
  case "$(uname -s)" in
    Darwin) os="darwin" ;;
    Linux) os="linux" ;;
    MINGW*|MSYS*|CYGWIN*) os="win32" ;;
    *) return 1 ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch="x64" ;;
    arm64|aarch64) arch="arm64" ;;
    *) return 1 ;;
  esac
  echo "$os-$arch"
}

for platform in $platforms; do
  package_dir="$staging/$platform"
  bin_dir="$package_dir/bin"
  mkdir -p "$bin_dir"
  suffix="$(suffix_of "$platform")"
  shape="$(shape_of "$platform")"

  if [ "$shape" = "exe" ]; then
    unpack horon-agent "$suffix" "horon-agent.exe" "$bin_dir" "$shape"
    unpack horon-tui "$suffix" "horon-tui.exe" "$bin_dir" "$shape"
  else
    unpack horon-agent "$suffix" "horon-agent" "$bin_dir" "$shape"
    unpack horon-tui "$suffix" "horon-tui" "$bin_dir" "$shape"
  fi

  PACKAGE_DIR="$package_dir" WRAPPER_MANIFEST="$wrapper_src/package.json" \
    PLATFORM="$platform" OS="$(os_of "$platform")" CPU="$(cpu_of "$platform")" \
    VERSION="$version" node -e '
      const fs = require("node:fs");
      const wrapper = JSON.parse(fs.readFileSync(process.env.WRAPPER_MANIFEST, "utf8"));
      const scope = wrapper.name.slice(0, wrapper.name.indexOf("/"));
      const name = `${scope}/horon-${process.env.PLATFORM}`;
      if (!(name in wrapper.optionalDependencies)) {
        throw new Error(`${name} is not pinned in ${wrapper.name} optionalDependencies`);
      }
      const manifest = {
        name,
        version: process.env.VERSION,
        description: `Horon CLI binaries for ${process.env.OS} ${process.env.CPU}`,
        license: wrapper.license,
        os: [process.env.OS],
        cpu: [process.env.CPU],
        files: ["bin", "README.md"],
        repository: wrapper.repository,
        publishConfig: { access: "public" },
      };
      const dir = process.env.PACKAGE_DIR;
      fs.writeFileSync(`${dir}/package.json`, JSON.stringify(manifest, null, 2) + "\n");
      fs.writeFileSync(
        `${dir}/README.md`,
        `# ${name}\n\nHoron CLI binaries for \`${process.env.OS}\` \`${process.env.CPU}\`. Install \`${wrapper.name}\` rather than this package directly.\n`,
      );
    '
done

# The wrapper ships the launcher and its manifest, with every platform package
# pinned to exactly the version the archives carry.
wrapper_dir="$staging/horon"
mkdir -p "$wrapper_dir/bin"
cp "$wrapper_src/bin/horon.js" "$wrapper_dir/bin/horon.js"
cp "$wrapper_src/README.md" "$wrapper_dir/README.md"
chmod +x "$wrapper_dir/bin/horon.js"
WRAPPER_MANIFEST="$wrapper_src/package.json" WRAPPER_DIR="$wrapper_dir" VERSION="$version" \
  node -e '
    const fs = require("node:fs");
    const wrapper = JSON.parse(fs.readFileSync(process.env.WRAPPER_MANIFEST, "utf8"));
    const version = process.env.VERSION;
    for (const [name, pinned] of Object.entries(wrapper.optionalDependencies ?? {})) {
      if (pinned !== wrapper.version) {
        throw new Error(`${name} must be pinned to ${wrapper.version}, found ${pinned}`);
      }
    }
    const manifest = { ...wrapper, version };
    manifest.optionalDependencies = Object.fromEntries(
      Object.keys(wrapper.optionalDependencies).map((name) => [name, version]),
    );
    fs.writeFileSync(
      `${process.env.WRAPPER_DIR}/package.json`,
      JSON.stringify(manifest, null, 2) + "\n",
    );
  '

if host="$(host_platform)"; then
  if [ -d "$staging/$host" ]; then
    entry="$staging/$host/bin/horon-agent"
    [ -f "$entry" ] || fail "missing $entry for host platform $host"
    output="$(version_output "$entry")"
    case "$output" in
      *"$version"*) ;;
      *) fail "$host/bin/horon-agent --version does not report $version: $output" ;;
    esac
  fi
fi

for platform in $platforms; do
  rm -rf "${out:?}/$platform"
  mv "$staging/$platform" "$out/$platform"
done
rm -rf "${out:?}/horon"
mv "$staging/horon" "$out/horon"

echo "prepared $version packages in $out:"
for platform in $platforms; do
  echo "  @horon-ai/horon-$platform -> $out/$platform"
done
echo "  @horon-ai/horon -> $out/horon"
