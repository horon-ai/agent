#!/usr/bin/env bash
# Install the comparison tools into one throwaway directory.
#
# Usage:
#   scripts/perf/install_competitors.sh <dest_dir> [comma-list]
#
# <comma-list> defaults to "jcode,claude_code,codex,opencode,pi"; "horon" is
# supplied by the caller as a locally built binary and is never installed here.
# Every tool lands in <dest_dir> and <dest_dir>/bins.args receives one line:
#
#   --bin jcode=/path --bin codex=/path ...
#
# Tools that fail to install are omitted from that line and the reason goes to
# stderr, so a missing competitor degrades the report instead of failing CI.
#
# Versions are pinned so a workflow rerun measures the same tools:
#   JCODE_VERSION, CLAUDE_VERSION, CODEX_VERSION, OPENCODE_VERSION, PI_VERSION
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
: >"$BINS_ARGS"

JCODE_VERSION="${JCODE_VERSION:-v0.99.1}"
CLAUDE_VERSION="${CLAUDE_VERSION:-2.1.317}"
CODEX_VERSION="${CODEX_VERSION:-0.166.0}"
OPENCODE_VERSION="${OPENCODE_VERSION:-1.19.5}"
PI_VERSION="${PI_VERSION:-0.73.1}"

OS="$(uname -s)"
ARCH="$(uname -m)"

note_fail() {
  echo "install_competitors: $1: $2" >&2
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
  if [ -x "$path" ]; then
    echo "--bin $name=$path" >>"$BINS_ARGS"
    echo "install_competitors: $name -> $path" >&2
    return 0
  fi
  note_fail "$name" "binary missing at $path"
  return 1
}

install_jcode() {
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
  local url="https://github.com/1jehuang/jcode/releases/download/${JCODE_VERSION}/${asset}"
  local tarball="$DEST/jcode-${JCODE_VERSION}.tar.gz"
  if ! curl -fsSL -o "$tarball" "$url"; then
    note_fail jcode "download failed: $url"
    return 1
  fi
  if ! tar xzf "$tarball" -C "$DEST"; then
    note_fail jcode "unpack failed: $tarball"
    return 1
  fi
  rm -f "$tarball"
  local extracted
  extracted="$(find "$DEST" -maxdepth 1 -type f -name 'jcode-*' ! -name '*.tar.gz' | head -n1)"
  if [ -z "$extracted" ]; then
    note_fail jcode "release archive contained no binary"
    return 1
  fi
  chmod +x "$extracted"
  if [ "$extracted" != "$DEST/jcode" ]; then
    mv -f "$extracted" "$DEST/jcode"
  fi
  record_bin jcode "$DEST/jcode"
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
  install_node_tool pi "@mariozechner/pi-coding-agent@${PI_VERSION}" "pi"
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
