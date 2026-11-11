#!/usr/bin/env bash
# Install the npm distribution the way a user does, on this runner's platform.
#
# usage: smoke_npm_install.sh <horon-agent-binary> <horon-tui-binary>
#
# The two binaries are the ones this CI job just built. They are packed into the
# release archive shapes `scripts/npm/prepare_cli_packages.sh` expects (the host
# platform carries the real binaries, the other five carry stubs, because only
# the host package is ever installed and run), the prepare script generates the
# seven packages, and the host package plus the wrapper are installed together
# into a throwaway prefix with `npm install -g`.
#
# The four claims this proves, each of which has broken silently before:
#   * `npm install -g` of the two tarballs puts a working `horon` and `hrn` on
#     the prefix, and both launch the agent this job built;
#   * the platform package ships `horon-tui` beside `horon-agent`, so the agent
#     finds its terminal client;
#   * `npx --package <platform> --package <wrapper> horon` works without a
#     global install;
#   * installing the wrapper alone fails loudly and names the platform package,
#     which is the message a user with optional dependencies disabled sees.
#
# The install runs `--offline` against a private cache, so the other five
# optional dependencies cannot be fetched: npm skips an unresolvable optional
# dependency without a warning, and the run proves the two tarballs are
# self-contained. Any `npm warn` or `npm error` line fails the script; output is
# never filtered or quieted.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <horon-agent-binary> <horon-tui-binary>" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
prepare="$repo_root/scripts/npm/prepare_cli_packages.sh"
agent_binary="$1"
tui_binary="$2"

fail() {
  echo "smoke_npm_install: $*" >&2
  exit 1
}

note() { echo "smoke_npm_install: $*"; }

command -v node >/dev/null 2>&1 || fail "need node"
command -v npm >/dev/null 2>&1 || fail "need npm"
command -v tar >/dev/null 2>&1 || fail "need tar"
[ -f "$agent_binary" ] || fail "no agent binary at $agent_binary"
[ -f "$tui_binary" ] || fail "no terminal client binary at $tui_binary"
agent_binary="$(cd "$(dirname "$agent_binary")" && pwd)/$(basename "$agent_binary")"
tui_binary="$(cd "$(dirname "$tui_binary")" && pwd)/$(basename "$tui_binary")"

# Git Bash hands Windows programs POSIX paths it would read relative to a drive
# root, so every path that crosses the npm boundary is converted first.
native() {
  if command -v cygpath >/dev/null 2>&1 && [ "${RUNNER_OS:-}" = "Windows" ]; then
    cygpath -w "$1"
  else
    printf '%s' "$1"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    fail "need sha256sum or shasum"
  fi
}

platform_key() {
  local os arch
  case "$(uname -s)" in
    Darwin) os="darwin" ;;
    Linux) os="linux" ;;
    MINGW*|MSYS*|CYGWIN*) os="win32" ;;
    *) fail "unsupported host: $(uname -s)" ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch="x64" ;;
    arm64|aarch64) arch="arm64" ;;
    *) fail "unsupported host architecture: $(uname -m)" ;;
  esac
  printf '%s-%s' "$os" "$arch"
}

suffix_of() {
  case "$1" in
    linux-x64) printf 'linux-x86_64' ;;
    linux-arm64) printf 'linux-aarch64' ;;
    darwin-x64) printf 'macos-x86_64' ;;
    darwin-arm64) printf 'macos-aarch64' ;;
    win32-x64) printf 'windows-x86_64' ;;
    win32-arm64) printf 'windows-aarch64' ;;
  esac
}

host="$(platform_key)"
host_suffix="$(suffix_of "$host")"
note "host platform: $host ($host_suffix)"
node --version
npm --version

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

# --- version reported by the binary this job built -------------------------
# `horon-agent --version` prints `horon-agent vX.Y.Z[-dev] (hash)`; the npm
# version is that X.Y.Z[-dev] without the leading `v`, the same value the
# release tag names.
agent_version_line() { "$1" --version 2>&1 | tr -d '\r' | head -n 1; }

first_run="$(agent_version_line "$agent_binary")"
version="$(printf '%s' "$first_run" | grep -oE 'v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?' | head -n 1 | sed 's/^v//')"
[ -n "$version" ] || fail "cannot read a version from: $first_run"
note "binary version line: $first_run"
note "npm version: $version"

# --- fake release assets ---------------------------------------------------
# Only the host platform needs the real binaries: it is the one package npm
# installs and the one binary the smoke runs. The other five are stubs with the
# archive shape prepare expects, which still exercises the digest and layout
# checks for every platform.
assets="$work/assets"
stage="$work/stage"
mkdir -p "$assets" "$stage"
for platform in linux-x64 linux-arm64 darwin-x64 darwin-arm64 win32-x64 win32-arm64; do
  suffix="$(suffix_of "$platform")"
  for stem in horon-agent horon-tui; do
    dir="$stage/$stem-$suffix"
    mkdir -p "$dir"
    artifact="$stem-$suffix"
    if [ "$platform" = "$host" ]; then
      source_binary="$agent_binary"
      [ "$stem" = "horon-tui" ] && source_binary="$tui_binary"
    fi
    case "$platform" in
      linux-x64)
        # The portable Linux build is a shell wrapper around a `.bin` payload;
        # the host binary becomes the payload and a wrapper execs it by name.
        if [ "$platform" = "$host" ]; then
          cp "$source_binary" "$dir/$artifact.bin"
        else
          printf '#!/bin/sh\necho "horon-agent %s (stub)"\n' "$version" >"$dir/$artifact.bin"
        fi
        chmod +x "$dir/$artifact.bin"
        cat >"$dir/$artifact" <<WRAPPER
#!/bin/sh
exec "\$(dirname "\$0")/$artifact.bin" "\$@"
WRAPPER
        chmod +x "$dir/$artifact"
        tar -czf "$assets/$artifact.tar.gz" -C "$dir" "$artifact" "$artifact.bin"
        ;;
      win32-*)
        if [ "$platform" = "$host" ]; then
          cp "$source_binary" "$dir/$artifact.exe"
        else
          printf 'stub %s %s\r\n' "$stem" "$version" >"$dir/$artifact.exe"
        fi
        tar -czf "$assets/$artifact.tar.gz" -C "$dir" "$artifact.exe"
        ;;
      *)
        if [ "$platform" = "$host" ]; then
          cp "$source_binary" "$dir/$artifact"
        else
          printf '#!/bin/sh\necho "horon-agent %s (stub)"\n' "$version" >"$dir/$artifact"
        fi
        chmod +x "$dir/$artifact"
        tar -czf "$assets/$artifact.tar.gz" -C "$dir" "$artifact"
        ;;
    esac
  done
done
(cd "$assets" && for archive in ./*.tar.gz; do printf '%s  %s\n' "$(sha256_of "$archive")" "${archive#./}"; done >SHA256SUMS)
note "built $(find "$assets" -name '*.tar.gz' | wc -l | tr -d ' ') fake archives (host carries the real binaries)"

# --- generate the seven packages ------------------------------------------
out="$work/out"
bash "$prepare" "$out" "$assets" "$version" >"$work/prepare.log" 2>&1 ||
  fail "prepare_cli_packages.sh failed: $(cat "$work/prepare.log")"
tail -n 8 "$work/prepare.log"

pack="$work/pack"
mkdir -p "$pack/platform" "$pack/wrapper"
(cd "$out/$host" && npm pack --pack-destination "$pack/platform" >"$work/pack-platform.log" 2>&1) ||
  fail "npm pack failed for the $host package: $(cat "$work/pack-platform.log")"
(cd "$out/horon" && npm pack --pack-destination "$pack/wrapper" >"$work/pack-wrapper.log" 2>&1) ||
  fail "npm pack failed for the wrapper: $(cat "$work/pack-wrapper.log")"
platform_tarball="$(find "$pack/platform" -maxdepth 1 -name '*.tgz' | head -n 1)"
wrapper_tarball="$(find "$pack/wrapper" -maxdepth 1 -name '*.tgz' | head -n 1)"
[ -n "$platform_tarball" ] || fail "no platform tarball was produced"
[ -n "$wrapper_tarball" ] || fail "no wrapper tarball was produced"
note "packed $(basename "$platform_tarball") and $(basename "$wrapper_tarball")"

# --- refusal helper --------------------------------------------------------
assert_clean_log() {
  local log="$1" label="$2"
  if grep -E '^(npm warn|npm error)' "$log" >/dev/null; then
    fail "$label printed npm warnings or errors: $(grep -E '^(npm warn|npm error)' "$log")"
  fi
  if grep -E '^> @horon-ai/' "$log" >/dev/null; then
    fail "$label ran an install script: $(grep -E '^> @horon-ai/' "$log")"
  fi
}

install_pair() {
  local prefix="$1" log="$2" cache="$3"
  npm install -g --prefix "$(native "$prefix")" --no-audit --no-fund --offline \
    --foreground-scripts --cache "$(native "$cache")" \
    "$(native "$platform_tarball")" "$(native "$wrapper_tarball")" >"$log" 2>&1
}

# --- happy path: platform package + wrapper ------------------------------
prefix="$work/prefix"
cache="$work/cache"
install_pair "$prefix" "$work/install.log" "$cache" || fail "npm install failed: $(cat "$work/install.log")"
cat "$work/install.log"
assert_clean_log "$work/install.log" "npm install"

bin_dir="$prefix/bin"
[ -d "$bin_dir" ] || bin_dir="$prefix" # Windows puts the shims in the prefix itself
for shim in horon hrn; do
  [ -f "$bin_dir/$shim" ] || [ -f "$bin_dir/$shim.cmd" ] || fail "no $shim launcher in $bin_dir: $(ls -A "$bin_dir")"
done

root="$(npm root -g --prefix "$(native "$prefix")" | tr -d '\r')"
if command -v cygpath >/dev/null 2>&1 && [ "${RUNNER_OS:-}" = "Windows" ]; then
  root="$(cygpath -u "$root")"
fi
[ -d "$root/@horon-ai/horon" ] || fail "wrapper is not installed in $root (@horon-ai/horon)"
[ -d "$root/@horon-ai/horon-$host" ] || fail "platform package is not installed in $root (@horon-ai/horon-$host)"
case "$host" in
  win32-*) entry="horon-agent.exe"; companion="horon-tui.exe" ;;
  *) entry="horon-agent"; companion="horon-tui" ;;
esac
[ -f "$root/@horon-ai/horon-$host/bin/$entry" ] ||
  fail "platform package has no bin/$entry: $(ls -A "$root/@horon-ai/horon-$host/bin")"
[ -f "$root/@horon-ai/horon-$host/bin/$companion" ] ||
  fail "platform package has no bin/$companion beside bin/$entry: $(ls -A "$root/@horon-ai/horon-$host/bin")"
note "installed tree: $(cd "$root/@horon-ai" && find . -mindepth 1 -maxdepth 1 | sed 's#^\./##' | LC_ALL=C sort | tr '\n' ' ')"

launcher="$bin_dir/horon"
[ -f "$launcher" ] || launcher="$bin_dir/horon.cmd"
run_launcher() { "$launcher" --version 2>&1 | tr -d '\r'; }

installed_line="$(run_launcher)"
[ "$installed_line" = "$first_run" ] ||
  fail "installed horon printed [$installed_line], expected [$first_run]"
note "horon --version: $installed_line"

hrn_launcher="$bin_dir/hrn"
[ -f "$hrn_launcher" ] || hrn_launcher="$bin_dir/hrn.cmd"
hrn_line="$("$hrn_launcher" --version 2>&1 | tr -d '\r')"
[ "$hrn_line" = "$first_run" ] ||
  fail "installed hrn printed [$hrn_line], expected [$first_run]"
note "hrn --version: $hrn_line"

# --- npx path: no global install ----------------------------------------
npx_dir="$work/npx"
mkdir -p "$npx_dir"
npx_line="$(
  cd "$npx_dir" &&
    npm exec --yes --offline --cache "$(native "$work/cache-npx")" \
      --package "$(native "$platform_tarball")" --package "$(native "$wrapper_tarball")" \
      -- horon --version 2>&1 | tr -d '\r'
)"
[ "$npx_line" = "$first_run" ] ||
  fail "npx horon printed [$npx_line], expected [$first_run]"
note "npx horon --version: $npx_line"

# --- failure: the wrapper without its platform package ------------------
# A user who disables optional dependencies installs only the wrapper. That
# install succeeds, and the launch must fail with a message naming the package
# that is missing instead of a stack trace.
solo_prefix="$work/solo"
npm install -g --prefix "$(native "$solo_prefix")" --no-audit --no-fund --offline \
  --foreground-scripts --cache "$(native "$work/cache-solo")" \
  "$(native "$wrapper_tarball")" >"$work/solo-install.log" 2>&1 ||
  fail "wrapper-only install failed: $(cat "$work/solo-install.log")"
assert_clean_log "$work/solo-install.log" "wrapper-only install"

solo_bin="$solo_prefix/bin"
[ -d "$solo_bin" ] || solo_bin="$solo_prefix"
solo_launcher="$solo_bin/horon"
[ -f "$solo_launcher" ] || solo_launcher="$solo_bin/horon.cmd"
solo_status=0
solo_output="$("$solo_launcher" --version 2>&1)" || solo_status=$?
[ "$solo_status" -eq 1 ] || fail "wrapper-only horon exited $solo_status, expected 1: $solo_output"
printf '%s' "$solo_output" | grep -F "@horon-ai/horon-$host" >/dev/null ||
  fail "wrapper-only failure does not name @horon-ai/horon-$host: $solo_output"
note "wrapper-only failure: $solo_output"

echo "ok: npm install smoke passed for $host at version $version"
