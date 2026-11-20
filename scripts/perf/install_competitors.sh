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
# reason. A missing npm competitor degrades the report instead of failing CI.
#
# jcode is different: its release is resolved and verified, so a jcode failure
# is a failed run. `scripts/perf/jcode_release.py` resolves the newest stable
# GitHub release (or the JCODE_VERSION pin), requires this platform's exact
# asset, verifies SHA-256 against the release SHA256SUMS with the same
# download-URL-then-asset-id fallback as before, and runs `--version` once to
# require the resolved tag. It writes <dest_dir>/jcode-release.json with the
# tag, peeled tag commit, asset, download URL and SHA-256. `resolve` picks the
# release once per benchmark round; the workflows pass that tag and commit here
# so every OS measures the same release.
#
# On Windows (Git Bash) jcode comes as a release `.exe`, npm tools are recorded
# by their `.cmd` shim, and every path is written in `C:/...` form so the Python
# benchmark can open it.
#
# The npm tools stay pinned so a workflow rerun measures the same packages:
#   CLAUDE_VERSION, CODEX_VERSION, OPENCODE_VERSION, PI_VERSION,
#   OMP_VERSION, OMO_VERSION, BUN_VERSION
# Override any of them, or JCODE_VERSION to pin jcode, through the environment.
# JCODE_RELEASE_COMMIT refuses a release whose tag moved; JCODE_RELEASE_SOURCE
# records whether the tag came from the newest-release lookup or a manual pin;
# JCODE_VERIFY_VERSION=0 skips the `--version` execution so the install suite's
# timed first call is the executable's first run.
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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Empty means "newest stable release"; the resolver validates a pin before use.
JCODE_VERSION="${JCODE_VERSION:-}"
# The peeled tag commit resolved once for the whole benchmark round. When set,
# the installer refuses a tag that now points at another commit.
JCODE_RELEASE_COMMIT="${JCODE_RELEASE_COMMIT:-}"
# The workflows pass "latest" when the tag came from their one resolve step, so
# the record separates that choice from a manual pin. Empty lets the resolver
# classify a tag-only invocation as a pin.
JCODE_RELEASE_SOURCE="${JCODE_RELEASE_SOURCE:-}"
# 0 leaves the first `--version` execution to the install suite's timed call.
JCODE_VERIFY_VERSION="${JCODE_VERIFY_VERSION:-1}"
JCODE_METADATA="${JCODE_METADATA:-$DEST/jcode-release.json}"
JCODE_LOG="$DEST/jcode-release.log"
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

jcode_python() {
  # The resolver is Python; every benchmark runner (including the FreeBSD guest)
  # already installs it for bench_compare/bench_install.
  if have python3; then
    command -v python3
  elif have python; then
    command -v python
  else
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

install_jcode() {
  local py binary args
  if ! py="$(jcode_python)"; then
    note_fail jcode "python3 is required to resolve the jcode release"
    return 1
  fi
  if ! command -v curl >/dev/null 2>&1; then
    note_fail jcode "curl is required to reach the jcode release"
    return 1
  fi
  # The Python resolver owns resolution, the checkout-free download, the
  # SHA256SUMS check, extraction and (unless the caller hands that to the
  # timed first --version) the version check. Everything it logs goes to both
  # the caller's stderr and <dest>/jcode-release.log so the run artifact keeps
  # the real installer output.
  : >"$JCODE_LOG"
  args=(materialize --dest "$DEST/jcode-dist" --metadata "$JCODE_METADATA")
  [ -z "$JCODE_VERSION" ] || args+=(--tag "$JCODE_VERSION")
  [ -z "$JCODE_RELEASE_COMMIT" ] || args+=(--expected-commit "$JCODE_RELEASE_COMMIT")
  [ -z "$JCODE_RELEASE_SOURCE" ] || args+=(--selection "$JCODE_RELEASE_SOURCE")
  [ "$JCODE_VERIFY_VERSION" = "0" ] && args+=(--skip-version-exec)
  if ! binary="$("$py" "$SCRIPT_DIR/jcode_release.py" "${args[@]}" 2>"$JCODE_LOG")"; then
    cat "$JCODE_LOG" >&2
    note_fail jcode "$(tail -n 1 "$JCODE_LOG" | sed 's/^jcode_release: //')"
    return 1
  fi
  cat "$JCODE_LOG" >&2
  record_bin jcode "$binary"
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
JCODE_REQUESTED=0
JCODE_FAILED=0
for tool in "${wanted[@]}"; do
  tool="$(echo "$tool" | tr -d '[:space:]')"
  [ -z "$tool" ] && continue
  case "$tool" in
    jcode) JCODE_REQUESTED=1; install_jcode || JCODE_FAILED=1 ;;
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

if [ "$JCODE_REQUESTED" = 1 ] && [ "$JCODE_FAILED" = 1 ]; then
  # Resolution, checksum or version verification is a measurement precondition:
  # falling back to an older release or a source build would change what the
  # comparison claims, so the run fails and the reason stays in failures.txt.
  echo "install_competitors: jcode release verification failed; see $FAILURES and $JCODE_LOG" >&2
  exit 1
fi

echo "install_competitors: wrote $BINS_ARGS" >&2
# Echo the resolved flags on stdout too so `... | tee competitors.txt` in the
# workflow records which tools and paths the comparison actually used.
cat "$BINS_ARGS"
exit 0
