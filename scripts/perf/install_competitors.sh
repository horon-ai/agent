#!/usr/bin/env bash
# Install the comparison tools into one throwaway directory.
#
# Usage:
#   scripts/perf/install_competitors.sh <dest_dir> [comma-list]
#
# <comma-list> defaults to "jcode,claude_code,codex,opencode,pi"; "horon" is
# supplied by the caller as a locally built binary and is never installed here.
# "omp" (@oh-my-pi/pi-coding-agent) and "omo" (omo-ai) install the same way as
# the other npm tools but need Bun (omp) or Node 24+ / Bun (omo) on the runner.
# When `bun` is not on PATH they fall back to the npm package `bun`, which the
# workflow's setup-bun step cannot always deliver (a GitHub release download
# answered 504 in run 42469553718 and omp was skipped). A runtime that neither
# path can provide leaves the tool out of bins.args with a reason to stderr.
# Every tool lands in <dest_dir> and <dest_dir>/bins.args receives one line:
#
#   --bin jcode=/path --bin codex=/path ...
#
# Tools that fail to install are omitted from that line and the reason goes to
# stderr and to <dest_dir>/failures.txt (one `name<TAB>reason` line each), which
# bench_compare.py reads through --install-failures so the report names the real
# reason. A missing competitor degrades the report instead of failing CI.
#
# jcode comes from its GitHub release: the download URL is retried on transient
# errors, then the same asset is fetched by id through the release API, and the
# bytes are accepted only when they match the release SHA256SUMS.
#
# On Windows (Git Bash) jcode comes as a release `.exe`, npm tools are recorded
# by their `.cmd` shim, and every path is written in `C:/...` form so the Python
# benchmark can open it.
#
# Versions are pinned so a workflow rerun measures the same tools:
#   JCODE_VERSION, CLAUDE_VERSION, CODEX_VERSION, OPENCODE_VERSION, PI_VERSION,
#   OMP_VERSION, OMO_VERSION, BUN_VERSION
# Override any of them through the environment.
set -uo pipefail

DEST="${1:-}"
TOOLS="${2:-jcode,claude_code,codex,opencode,pi}"

if [ -z "$DEST" ]; then
  echo "usage: $0 <dest_dir> [comma-list]" >&2
  exit 2
fi

mkdir -p "$DEST"
BINS_ARGS="$DEST/bins.args"
FAILURES="$DEST/failures.txt"
: >"$BINS_ARGS"
: >"$FAILURES"

JCODE_VERSION="${JCODE_VERSION:-v0.99.1}"
CLAUDE_VERSION="${CLAUDE_VERSION:-2.1.317}"
CODEX_VERSION="${CODEX_VERSION:-0.166.0}"
OPENCODE_VERSION="${OPENCODE_VERSION:-1.19.5}"
# pi moved to the @earendil-works scope at 1.x; 1.x ships a built-in MCP client
# (@earendil-works/pi-mcp) and needs Node 22.19 or later.
PI_VERSION="${PI_VERSION:-1.5.0}"
# @oh-my-pi/pi-coding-agent ships its CLI as a Bun script (engines bun>=1.4.4),
# so the compare job installs Bun before running it.
OMP_VERSION="${OMP_VERSION:-18.18.0}"
# omo-ai runs on Node 24+ (or Bun); the job has both.
OMO_VERSION="${OMO_VERSION:-5.1.64}"
# Bun delivered by the npm registry when the runner has none; `bun@1.4.4`
# satisfies the engines constraint @oh-my-pi/pi-coding-agent declares.
BUN_VERSION="${BUN_VERSION:-1.4.4}"

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  MINGW* | MSYS* | CYGWIN*) WINDOWS=1 ;;
  *) WINDOWS=0 ;;
esac

note_fail() {
  echo "install_competitors: $1: $2" >&2
  printf '%s\t%s\n' "$1" "$(printf '%s' "$2" | tr '\t\r\n' '   ')" >>"$FAILURES"
}

have() {
  command -v "$1" >/dev/null 2>&1
}

npm_install_into() {
  # npm_install_into <package@version> <prefix>
  local spec="$1" prefix="$2"
  if ! have npm; then
    return 1
  fi
  npm install --prefix "$prefix" --no-audit --no-fund --loglevel=error "$spec" >&2
}

record_bin() {
  local name="$1" path="$2"
  # Git Bash does not mark a `.cmd` shim executable, so on Windows a regular
  # file is enough; CreateProcess runs `.cmd` and `.exe` either way.
  if [ -x "$path" ] || { [ "$WINDOWS" = 1 ] && [ -f "$path" ]; }; then
    if [ "$WINDOWS" = 1 ] && have cygpath; then
      path="$(cygpath -m "$path")"
    fi
    echo "--bin $name=$path" >>"$BINS_ARGS"
    echo "install_competitors: $name -> $path" >&2
    return 0
  fi
  note_fail "$name" "binary missing at $path"
  return 1
}

JCODE_BASE="https://github.com/1jehuang/jcode/releases/download/${JCODE_VERSION}"
JCODE_API="https://api.github.com/repos/1jehuang/jcode/releases"

curl_get() {
  curl --fail --location --silent --show-error --retry 5 --retry-delay 2 "$@"
}

jcode_api_auth=()
jcode_token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
[ -z "$jcode_token" ] || jcode_api_auth=(--header "Authorization: Bearer $jcode_token")

jcode_fetch() {
  # jcode_fetch <asset name> <out>: the download URL first, then the same asset by
  # its release API id. JCODE_FETCH_ERROR holds the reason when both fail.
  local name="$1" out="$2" release id
  if curl_get --output "$out" "$JCODE_BASE/$name"; then
    return 0
  fi
  echo "install_competitors: jcode: $name download failed, trying the release API" >&2
  release="$(mktemp)"
  if ! curl_get ${jcode_api_auth[@]+"${jcode_api_auth[@]}"} --header 'Accept: application/vnd.github+json' \
    --output "$release" "$JCODE_API/tags/$JCODE_VERSION"; then
    rm -f "$release"
    JCODE_FETCH_ERROR="download failed: $JCODE_BASE/$name and release API $JCODE_API/tags/$JCODE_VERSION"
    return 1
  fi
  # Each asset object lists its "url" before its "name". Tokenize so the JSON
  # layout does not matter, and accept only a numeric asset id of this repository.
  id="$(grep -oE '"(url|name)"[[:space:]]*:[[:space:]]*"[^"]*"' "$release" | awk -F'"' -v want="$name" '
    $2 == "url" { last = "" }
    $2 == "url" && $4 ~ /^https:\/\/api\.github\.com\/repos\/1jehuang\/jcode\/releases\/assets\/[0-9]+$/ {
      last = $4; sub(/.*\//, "", last) }
    $2 == "name" && $4 == want && last != "" { print last; exit }')"
  rm -f "$release"
  if ! [[ "$id" =~ ^[0-9]+$ ]]; then
    JCODE_FETCH_ERROR="download failed: $JCODE_BASE/$name; release API lists no asset named $name"
    return 1
  fi
  if ! curl_get ${jcode_api_auth[@]+"${jcode_api_auth[@]}"} --header 'Accept: application/octet-stream' \
    --output "$out" "$JCODE_API/assets/$id"; then
    JCODE_FETCH_ERROR="download failed: $JCODE_BASE/$name and release API asset $id"
    return 1
  fi
}

sha256_of() {
  # Hash stdin: with a path argument, GNU sha256sum on Git Bash prefixes the line
  # with `\` when the path holds a backslash, which breaks the comparison.
  local digest
  if have sha256sum; then
    digest="$(sha256sum <"$1")" || return 1
  elif have shasum; then
    digest="$(shasum -a 256 <"$1")" || return 1
  else
    return 1
  fi
  printf '%s' "${digest%% *}" | tr '[:upper:]' '[:lower:]'
}

jcode_fetch_verified() {
  # jcode_fetch_verified <asset name> <out>: fetch the asset and SHA256SUMS, and
  # keep the asset only when its digest matches the SHA256SUMS line for its name.
  local name="$1" out="$2" sums expected actual
  JCODE_FETCH_ERROR=""
  if ! jcode_fetch "$name" "$out"; then
    note_fail jcode "$JCODE_FETCH_ERROR"
    return 1
  fi
  sums="$(mktemp)"
  if ! jcode_fetch SHA256SUMS "$sums"; then
    rm -f "$sums" "$out"
    note_fail jcode "checksum $JCODE_FETCH_ERROR"
    return 1
  fi
  # Read only the digest of the exact asset name; the manifest never picks a path.
  expected="$(awk -v want="$name" '$2 == want || $2 == "*" want { print tolower($1); exit }' "$sums")"
  rm -f "$sums"
  if ! [[ "$expected" =~ ^[0-9a-f]{64}$ ]]; then
    rm -f "$out"
    note_fail jcode "SHA256SUMS has no digest for $name"
    return 1
  fi
  if ! actual="$(sha256_of "$out")"; then
    rm -f "$out"
    note_fail jcode "sha256sum or shasum required to verify $name"
    return 1
  fi
  if [ "$actual" != "$expected" ]; then
    rm -f "$out"
    note_fail jcode "SHA256 mismatch for $name: got $actual, SHA256SUMS lists $expected"
    return 1
  fi
}

BUN_BIN_DIR=""
BUN_ATTEMPTED=0

find_bun_bin() {
  # find_bun_bin <npm prefix>: echo the directory that holds a bun executable.
  local prefix="$1" dir
  dir="$prefix/node_modules/.bin"
  if [ -f "$dir/bun" ] || { [ "$WINDOWS" = 1 ] && [ -f "$dir/bun.cmd" ]; }; then
    echo "$dir"
    return 0
  fi
  # The platform binary the npm package unpacks; `.bin` only holds a shim.
  for dir in "$prefix/node_modules/bun/bin" "$prefix"/node_modules/@oven/*/bin; do
    if [ -f "$dir/bun" ] || [ -f "$dir/bun.exe" ]; then
      echo "$dir"
      return 0
    fi
  done
  return 1
}

ensure_bun() {
  # ensure_bun <tool-name>: make `bun` reachable, installing bun@BUN_VERSION
  # from the npm registry when the runner has none. Returns non-zero with the
  # reason already reported when neither the runner nor npm can supply Bun.
  local name="$1" prefix="$DEST/bun"
  if have bun; then
    return 0
  fi
  if [ "$BUN_ATTEMPTED" = 1 ]; then
    note_fail "$name" "bun is required and the earlier npm install of bun@${BUN_VERSION} did not provide it"
    return 1
  fi
  BUN_ATTEMPTED=1
  if ! have npm; then
    note_fail "$name" "bun is required and npm is not available to install bun@${BUN_VERSION}"
    return 1
  fi
  mkdir -p "$prefix"
  if ! npm_install_into "bun@${BUN_VERSION}" "$prefix"; then
    note_fail "$name" "bun is required and npm install bun@${BUN_VERSION} failed"
    return 1
  fi
  if ! BUN_BIN_DIR="$(find_bun_bin "$prefix")"; then
    note_fail "$name" "bun is required and npm package bun@${BUN_VERSION} contained no bun executable"
    return 1
  fi
  PATH="$BUN_BIN_DIR:$PATH"
  export PATH
  echo "install_competitors: bun -> $BUN_BIN_DIR (npm bun@${BUN_VERSION})" >&2
  # Later workflow steps inherit PATH from GITHUB_PATH, not from this process,
  # so record the directory there as well (Windows expects a native path).
  if [ -n "${GITHUB_PATH:-}" ]; then
    if [ "$WINDOWS" = 1 ] && have cygpath; then
      cygpath -w "$BUN_BIN_DIR" >>"$GITHUB_PATH"
    else
      printf '%s\n' "$BUN_BIN_DIR" >>"$GITHUB_PATH"
    fi
  fi
  return 0
}

install_jcode_windows() {
  local asset
  case "$ARCH" in
    x86_64) asset="jcode-windows-x86_64.exe" ;;
    aarch64 | arm64) asset="jcode-windows-aarch64.exe" ;;
    *)
      note_fail jcode "unsupported platform $OS/$ARCH"
      return 1
      ;;
  esac
  local dist="$DEST/jcode-dist"
  rm -rf "$dist"
  mkdir -p "$dist"
  jcode_fetch_verified "$asset" "$dist/jcode.exe" || return 1
  record_bin jcode "$dist/jcode.exe"
}

install_jcode() {
  if [ "$WINDOWS" = 1 ]; then
    install_jcode_windows
    return
  fi
  local asset
  case "$ARCH" in
    arm64 | aarch64) asset="jcode-macos-aarch64.tar.gz" ;;
    x86_64) asset="jcode-macos-x86_64.tar.gz" ;;
    *) asset="" ;;
  esac
  if [ "$OS" = "Linux" ]; then
    case "$ARCH" in
      x86_64) asset="jcode-linux-x86_64.tar.gz" ;;
      aarch64 | arm64) asset="jcode-linux-aarch64.tar.gz" ;;
    esac
  fi
  if [ -z "$asset" ]; then
    note_fail jcode "unsupported platform $OS/$ARCH"
    return 1
  fi
  local tarball="$DEST/jcode-${JCODE_VERSION}.tar.gz"
  jcode_fetch_verified "$asset" "$tarball" || return 1
  # Extract into its own directory: the Linux archive is a sh launcher plus a
  # `<name>.bin` payload it execs from its own directory, so both must stay together.
  local dist="$DEST/jcode-dist"
  rm -rf "$dist"
  mkdir -p "$dist"
  if ! tar xzf "$tarball" -C "$dist"; then
    note_fail jcode "unpack failed: $tarball"
    return 1
  fi
  rm -f "$tarball"
  local extracted
  extracted="$(find "$dist" -maxdepth 1 -type f -name 'jcode-*' ! -name '*.bin' | head -n1)"
  if [ -z "$extracted" ]; then
    note_fail jcode "release archive contained no binary"
    return 1
  fi
  chmod +x "$dist"/jcode-*
  record_bin jcode "$extracted"
}

install_node_tool() {
  # install_node_tool <bench-name> <npm spec> <relative bin path>
  local name="$1" spec="$2" rel="$3"
  local prefix="$DEST/$name"
  mkdir -p "$prefix"
  if ! npm_install_into "$spec" "$prefix"; then
    note_fail "$name" "npm install failed: $spec"
    return 1
  fi
  local bin="$prefix/node_modules/.bin/$rel"
  if [ "$WINDOWS" = 1 ] && [ -e "$bin.cmd" ]; then
    # The extensionless file in .bin is a sh script; CreateProcess runs the shim.
    bin="$bin.cmd"
  fi
  if [ ! -e "$bin" ]; then
    bin="$prefix/node_modules/$rel"
  fi
  record_bin "$name" "$bin"
}

install_claude() {
  # Claude Code ships as an npm package with a platform binary per version.
  install_node_tool claude_code "@anthropic-ai/claude-code@${CLAUDE_VERSION}" "claude"
}

install_codex() {
  install_node_tool codex "@openai/codex@${CODEX_VERSION}" "codex"
}

install_opencode() {
  install_node_tool opencode "opencode-ai@${OPENCODE_VERSION}" "opencode"
}

install_pi() {
  install_node_tool pi "@earendil-works/pi-coding-agent@${PI_VERSION}" "pi"
}

install_omp() {
  # The package's bin is a Bun script; npm does not enforce engines, so the
  # runtime is provided here (from npm when setup-bun lost its download).
  if ! ensure_bun omp; then
    return 1
  fi
  install_node_tool omp "@oh-my-pi/pi-coding-agent@${OMP_VERSION}" "omp"
}

install_omo() {
  # omo-ai runs on Node 24+; Bun is only a fallback when Node is missing.
  if ! have node && ! have bun && ! ensure_bun omo; then
    return 1
  fi
  install_node_tool omo "omo-ai@${OMO_VERSION}" "omo"
}

IFS=',' read -r -a wanted <<<"$TOOLS"
for tool in "${wanted[@]}"; do
  tool="$(echo "$tool" | tr -d '[:space:]')"
  [ -z "$tool" ] && continue
  case "$tool" in
    jcode) install_jcode || true ;;
    claude_code | claude) install_claude || true ;;
    codex) install_codex || true ;;
    opencode) install_opencode || true ;;
    pi) install_pi || true ;;
    omp) install_omp || true ;;
    omo) install_omo || true ;;
    horon) : ;; # provided by the caller
    *)
      note_fail "$tool" "unknown tool"
      ;;
  esac
done

echo "install_competitors: wrote $BINS_ARGS" >&2
# Echo the resolved flags on stdout too so `... | tee competitors.txt` in the
# workflow records which tools and paths the comparison actually used.
cat "$BINS_ARGS"
exit 0
