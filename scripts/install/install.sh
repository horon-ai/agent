#!/usr/bin/env bash
set -euo pipefail

REPO="horon-ai/agent"
IS_WINDOWS=false
IS_TERMUX=false
tmpdir=""
# Holds the pid of the one optional transfer that runs beside the required
# download. It is the transfer process itself, so it can be stopped directly.
background_fetch_pid=""

info() { printf '\033[1;34m%s\033[0m\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
err()  { printf '\033[1;31merror: %s\033[0m\n' "$*" >&2; exit 1; }

valid_release_tag() {
  printf '%s' "$1" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([+.-][[:alnum:].-]+)?$'
}

sha256_file() {
  file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print tolower($1)}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$file" | awk '{print tolower($NF)}'
  elif command -v sha256 >/dev/null 2>&1; then
    sha256 -q "$file" | awk '{print tolower($1)}'
  else
    return 1
  fi
}

install_exit() {
  status=$?
  trap - EXIT
  set +e
  # A transfer still running would keep writing into the temporary directory
  # removed below, so it is stopped and reaped first. Every other exit path
  # waits on the same pid; this is the one where a failure can arrive while the
  # transfer is still in flight.
  if [ -n "$background_fetch_pid" ]; then
    kill "$background_fetch_pid" 2>/dev/null
    wait "$background_fetch_pid" 2>/dev/null
    background_fetch_pid=""
  fi
  [ -z "$tmpdir" ] || rm -rf "$tmpdir"
  exit "$status"
}
trap install_exit EXIT

OS="$(uname -s)"
ARCH="$(uname -m)"

if [ -n "${TERMUX_VERSION:-}" ] || [ "${PREFIX:-}" = "/data/data/com.termux/files/usr" ] || [ -d "/data/data/com.termux/files/usr" ]; then
  IS_TERMUX=true
fi

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64)  ARTIFACT="horon-agent-linux-x86_64" ;;
      aarch64|arm64) ARTIFACT="horon-agent-linux-aarch64" ;;
      *)       err "Unsupported Linux architecture: $ARCH" ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      arm64)   ARTIFACT="horon-agent-macos-aarch64" ;;
      x86_64)  ARTIFACT="horon-agent-macos-x86_64" ;;
      *)       err "Unsupported macOS architecture: $ARCH" ;;
    esac
    ;;
  FreeBSD)
    case "$ARCH" in
      x86_64|amd64) ARTIFACT="horon-agent-freebsd-x86_64" ;;
      *)            err "Unsupported FreeBSD architecture: $ARCH" ;;
    esac
    ;;
  MINGW*|MSYS*|CYGWIN*)
    IS_WINDOWS=true
    WINDOWS_ARCH=""
    # Git for Windows may itself be an emulated x64 process on Windows ARM64,
    # making `uname -m` report x86_64. Prefer any ARM64 OS environment signal.
    for candidate in "${PROCESSOR_ARCHITEW6432:-}" "${PROCESSOR_ARCHITECTURE:-}" "$ARCH"; do
      case "$candidate" in
        aarch64|AARCH64|arm64|Arm64|ARM64) WINDOWS_ARCH="aarch64"; break ;;
      esac
    done
    if [ -z "$WINDOWS_ARCH" ]; then
      for candidate in "${PROCESSOR_ARCHITEW6432:-}" "${PROCESSOR_ARCHITECTURE:-}" "$ARCH"; do
        case "$candidate" in
          x86_64|X64|AMD64) WINDOWS_ARCH="x86_64"; break ;;
        esac
      done
    fi
    case "$WINDOWS_ARCH" in
      x86_64) ARTIFACT="horon-agent-windows-x86_64" ;;
      aarch64) ARTIFACT="horon-agent-windows-aarch64" ;;
      *) err "Unsupported Windows architecture: $ARCH" ;;
    esac
    ;;
  *)
    err "Unsupported OS: $OS (try building from source: https://github.com/$REPO)"
    ;;
esac


if [ "$IS_WINDOWS" = true ]; then
  EXE=".exe"
else
  EXE=""
fi

# Resolve the launcher directory with the same precedence the executable uses,
# so the PATH setup below always points at the directory that receives it.
if [ -n "${HORON_AGENT_INSTALL_DIR:-}" ]; then
  INSTALL_DIR="$HORON_AGENT_INSTALL_DIR"
elif [ -n "${HORON_HOME:-}" ]; then
  INSTALL_DIR="$HORON_HOME/bin"
elif [ "$IS_WINDOWS" = true ]; then
  INSTALL_DIR="$LOCALAPPDATA/horon-agent/bin"
else
  INSTALL_DIR="$HOME/.local/bin"
fi
launcher_path="$INSTALL_DIR/horon-agent${EXE}"

# The immutable version store the installed executable activates into.
if [ -n "${HORON_HOME:-}" ]; then
  builds_dir="$HORON_HOME/builds"
elif [ "$IS_WINDOWS" = true ]; then
  builds_dir="$LOCALAPPDATA/horon-agent/builds"
else
  builds_dir="$HOME/.horon/builds"
fi

# Resolve the release from the canonical repository through GitHub's stable
# redirect rather than the rate-limited unauthenticated API. No other host is
# consulted: an upstream mirror must never supply assets for this repository.
VERSION="${HORON_AGENT_VERSION:-}"
if [ -z "$VERSION" ]; then
  LATEST_RELEASE_URL=$(curl -fsSIL --retry 2 --connect-timeout 10 \
    -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null || true)
  case "$LATEST_RELEASE_URL" in
    */releases/tag/*) GITHUB_VERSION="${LATEST_RELEASE_URL##*/}" ;;
    *) GITHUB_VERSION="" ;;
  esac
  if valid_release_tag "$GITHUB_VERSION"; then
    VERSION="$GITHUB_VERSION"
  fi
fi
valid_release_tag "$VERSION" || err "Failed to determine latest version"
version="${VERSION#v}"

GITHUB_RELEASE_BASE="https://github.com/$REPO/releases/download/$VERSION"

EXISTING=""
if [ -x "$launcher_path" ]; then
  EXISTING=$("$launcher_path" --version 2>/dev/null | head -1 || echo "unknown")
fi

if [ -n "$EXISTING" ]; then
  if echo "$EXISTING" | grep -qF "$version"; then
    info "horon-agent $VERSION is already installed — reinstalling"
  else
    info "Updating horon-agent $EXISTING → $VERSION"
  fi
else
  info "Installing horon-agent $VERSION"
fi
info "  launcher: $launcher_path"

tmpdir=$(mktemp -d)
assets_dir="$tmpdir/assets"
bootstrap_dir="$tmpdir/bootstrap"
mkdir -p "$assets_dir" "$bootstrap_dir"

bin_name="horon-agent${EXE}"

fetch_release_asset() {
  curl -fsSL --retry 2 --connect-timeout 10 "$GITHUB_RELEASE_BASE/$1" -o "$2" 2>/dev/null
}

# Prints the manifest digest for one asset, and succeeds only when `SHA256SUMS`
# names it with a well-formed SHA-256 value. That exit status is what separates
# "the release publishes this file" from "it does not", so an optional asset is
# never requested when its bytes could not be verified anyway.
manifest_digest() {
  digest=$(awk -v asset="$1" '$2 == asset || $2 == "*" asset { print tolower($1); exit }' \
    "$assets_dir/SHA256SUMS") || return 1
  printf '%s' "$digest" | grep -Eq '^[0-9a-f]{64}$' || return 1
  printf '%s' "$digest"
}

# Prints why an asset fails verification and returns 1, so an optional asset can
# be skipped instead of aborting the install.
checksum_problem() {
  asset_name="$1"
  file="$2"
  if ! expected=$(manifest_digest "$asset_name"); then
    printf 'Could not find a trusted SHA-256 checksum for %s in %s' "$asset_name" "$VERSION"
    return 1
  fi
  if ! actual=$(sha256_file "$file"); then
    printf 'sha256sum, shasum, sha256 or openssl is required to verify the download'
    return 1
  fi
  if [ "$actual" != "$expected" ]; then
    printf 'SHA-256 verification failed for %s' "$asset_name"
    return 1
  fi
}

# The executable repeats every check in Rust before it stages anything; this
# pass exists so an unverified file is never executed and the failure is clear.
verify_asset_checksum() {
  problem=$(checksum_problem "$1" "$2") \
    || err "${problem:-SHA-256 verification failed for $1}"
}

# A reinstall normally pays for the same download as the first install. The
# last verified copy of each asset is kept in a per-user cache, one file per
# asset name, and reused only when the SHA256SUMS fetched in this run lists that
# same digest for that exact name. SHA256SUMS is never cached: every run fetches
# it from the canonical release over HTTPS. A cache entry is only a source: its
# bytes are copied into the temporary asset directory and that isolated copy is
# the one hashed and installed, so a file that changes underneath cannot reach
# the executable. A damaged copy, a name this release does not list, or a cache
# location that cannot be shown to belong to this user alone falls through to
# the existing download path; HORON_AGENT_NO_DOWNLOAD_CACHE=1 disables the cache
# outright. An entry is published only after the release executable has staged
# the assets, so on one filesystem the verified archive is moved into the cache
# instead of copied a second time, and no entry is ever visible half-written.
# Windows keeps its current path; install.ps1 is unchanged.

# Read the owner and octal permissions from one stat of the same object.
cache_metadata() {
  case "$OS" in
    Darwin|FreeBSD) stat -f '%u %Lp' "$1" 2>/dev/null ;;
    *) stat -c '%u %a' "$1" 2>/dev/null ;;
  esac
}


path_private() {
  local metadata mode
  metadata=$(cache_metadata "$1") || return 1
  mode=${metadata#* }
  [[ ${metadata%% *} == "$EUID" && $mode =~ ^[0-7]{3,4}$ ]] && (( (8#$mode & 0022) == 0 ))
}

# Enables the cache at "$1/horon-agent/release" only when every directory on the
# way is owned by this user and private to them. A shared location such as /tmp,
# or one whose owner or mode cannot be read, is skipped rather than written to:
# an uncertain environment then costs a download and never a new risk.
prepare_asset_cache() {
  local cache_root=${1%/} parent ancestor metadata mode owner directory
  [[ $cache_root == /* && ${cache_root##*/} != . && ${cache_root##*/} != .. ]] || return 0
  parent=${cache_root%/*}
  [[ -d $parent && ! -L $parent ]] && path_private "$parent" || return 0
  parent=$(cd -P -- "$parent" && pwd -P) || return 0
  cache_root="$parent/${cache_root##*/}"
  ancestor=${parent%/*}
  while [[ -n $ancestor ]]; do
    metadata=$(cache_metadata "$ancestor") || return 0
    owner=${metadata%% *}
    mode=${metadata#* }
    [[ $owner == "$EUID" || $owner == 0 ]] && [[ $mode =~ ^[0-7]{3,4}$ ]] || return 0
    # A root-owned sticky temporary directory protects each private child from replacement.
    (( (8#$mode & 0022) == 0 || (owner == 0 && (8#$mode & 01000) != 0) )) || return 0
    ancestor=${ancestor%/*}
  done
  for directory in "$cache_root" "$cache_root/horon-agent" "$cache_root/horon-agent/release"; do
    [[ ! -L $directory ]] || return 0
    if [[ ! -d $directory ]]; then
      (umask 077 && mkdir -- "$directory" 2>/dev/null) || [[ -d $directory ]] || return 0
    fi
    [[ ! -L $directory ]] && path_private "$directory" || return 0
  done
  asset_cache_dir=$directory
}

# Copies one cache entry into the asset directory, verifies that copy against
# the manifest fetched in this run, and only then renames it into place. Returns
# 1 and leaves nothing behind when the entry is absent, is not a private regular
# file, or no longer matches this release, so the caller downloads instead.
use_cached_asset() {
  [ -n "$asset_cache_dir" ] || return 1
  cache_entry="$asset_cache_dir/$1"
  if [ ! -f "$cache_entry" ] || [ -L "$cache_entry" ]; then
    return 1
  fi
  path_private "$cache_entry" || return 1
  cache_copy="$assets_dir/.$1.cached"
  rm -f "$cache_copy"
  if ! cp "$cache_entry" "$cache_copy" 2>/dev/null; then
    rm -f "$cache_copy"
    return 1
  fi
  if ! problem=$(checksum_problem "$1" "$cache_copy"); then
    rm -f "$cache_copy"
    return 1
  fi
  mv -f "$cache_copy" "$assets_dir/$1" 2>/dev/null || { rm -f "$cache_copy"; return 1; }
  return 0
}

# Move the consumed archive to a private staging name. Across filesystems mv
# copies to that name first. The final rename stays on the cache filesystem.
cache_asset() {
  local cache_source="$assets_dir/$1" cache_entry="$asset_cache_dir/$1" cache_temp
  [ -n "$asset_cache_dir" ] || return 0
  [[ ! -L $asset_cache_dir ]] && path_private "$asset_cache_dir" || return 0
  if [ ! -f "$cache_source" ] || [ -L "$cache_source" ]; then
    return 0
  fi
  # Portable mv treats an existing directory (including a link to one) as a destination directory.
  [[ ! -d $cache_entry ]] || return 0
  cache_temp=$(mktemp "$asset_cache_dir/.$1.XXXXXX" 2>/dev/null) || return 0
  if ! chmod 600 "$cache_source" || ! mv -f "$cache_source" "$cache_temp" 2>/dev/null; then
    rm -f "$cache_temp"
    return 0
  fi
  if ! mv -f "$cache_temp" "$cache_entry" 2>/dev/null; then
    rm -f "$cache_temp"
    return 0
  fi
  return 0
}

asset_cache_dir=""
if [ "$IS_WINDOWS" = false ] && [ "${HORON_AGENT_NO_DOWNLOAD_CACHE:-}" != "1" ]; then
  if [ -n "${XDG_CACHE_HOME:-}" ]; then
    cache_root="$XDG_CACHE_HOME"
  elif [ "$OS" = "Darwin" ]; then
    cache_root="$HOME/Library/Caches"
  else
    cache_root="$HOME/.cache"
  fi
  prepare_asset_cache "$cache_root"
fi

# Validate the complete listing before writing any bootstrap file. Only file
# contents go into the private directory; tar never extracts an archive tree.
extract_bootstrap_executable() {
  archive="$1"
  members=$(tar -tzf "$archive" 2>/dev/null) \
    || err "Downloaded agent archive is not a readable tar.gz file"
  [ -n "$members" ] || err "Downloaded agent archive is empty"
  extracted=0
  while IFS= read -r member; do
    name="${member#./}"
    [ -n "$name" ] || continue
    case "$name" in
      */*|.|..) err "Downloaded agent archive contains an unexpected path: $member" ;;
    esac
    case "$name" in
      "$ARTIFACT"|"$ARTIFACT$EXE"|"$ARTIFACT.bin"|libssl.so*|libcrypto.so*)
        tar -xOf "$archive" "$member" > "$bootstrap_dir/$name" \
          || err "Failed to extract $member from the downloaded agent archive"
        extracted=$((extracted + 1))
        ;;
      LICENSE)
        # The licence travels with every archive; the executable that stages the
        # version keeps it. Nothing here needs a copy, so this member is accepted
        # and skipped rather than treated as an unexpected name.
        ;;
      *)
        err "Downloaded agent archive contains an unexpected member: $member"
        ;;
    esac
  done <<EOF
$members
EOF
  [ "$extracted" -gt 0 ] || err "Downloaded agent archive did not contain ${ARTIFACT}${EXE}"
}

prepare_bootstrap_executable() {
  extract_bootstrap_executable "$assets_dir/$downloaded_asset"
  bootstrap_bin="$bootstrap_dir/$bin_name"
  if [ -f "$bootstrap_dir/${ARTIFACT}${EXE}" ]; then
    mv "$bootstrap_dir/${ARTIFACT}${EXE}" "$bootstrap_bin"
  elif [ -f "$bootstrap_dir/$ARTIFACT" ]; then
    mv "$bootstrap_dir/$ARTIFACT" "$bootstrap_bin"
  else
    err "Downloaded agent archive did not contain ${ARTIFACT}${EXE}"
  fi
  chmod +x "$bootstrap_bin" 2>/dev/null || true
  if [ -f "$bootstrap_dir/$ARTIFACT.bin" ]; then
    chmod +x "$bootstrap_dir/$ARTIFACT.bin" 2>/dev/null || true
  fi
}

# Termux has no glibc loader of its own, so the temp executable must be patched
# before it can run here. The Linux x64 wrapper is a script, so the ELF that
# gets patched is the `.bin` payload beside it. The installed copy is patched
# again by the executable itself when it stages the immutable version.
patch_termux_bootstrap() {
  [ "$IS_TERMUX" = true ] || return 0
  [ "$IS_WINDOWS" = false ] || return 0
  termux_glibc_dir="/data/data/com.termux/files/usr/glibc/lib"
  termux_glibc_linker=""
  case "$ARCH" in
    aarch64|arm64) termux_glibc_linker="$termux_glibc_dir/ld-linux-aarch64.so.1" ;;
    x86_64) termux_glibc_linker="$termux_glibc_dir/ld-linux-x86-64.so.2" ;;
  esac
  [ "$OS" = "Linux" ] && [ -n "$termux_glibc_linker" ] || return 0
  if [ ! -x "$termux_glibc_linker" ]; then
    info "Termux detected: install glibc with 'pkg install glibc' if horon-agent fails due to a missing dynamic linker."
    return 0
  fi
  if ! command -v patchelf >/dev/null 2>&1; then
    info "Termux detected: install patchelf with 'pkg install patchelf' and rerun this installer if horon-agent fails to start."
    return 0
  fi
  target="$bootstrap_bin"
  [ ! -f "$bootstrap_dir/$ARTIFACT.bin" ] || target="$bootstrap_dir/$ARTIFACT.bin"
  patchelf --set-interpreter "$termux_glibc_linker" "$target" \
    || err "Failed to patch horon-agent ELF interpreter for Termux glibc"
  info "Patched Termux glibc ELF interpreter: $termux_glibc_linker"
}

# Every install path needs the same release's checksum list. An install that
# cannot prove the bytes it executes is not a successful install, so the list is
# required before anything is downloaded or run.
fetch_release_asset "SHA256SUMS" "$assets_dir/SHA256SUMS" \
  || err "Release $VERSION does not publish SHA256SUMS"

# >>> horon-tui
# The interactive client ships as its own archive beside the agent. It is
# optional: a release that predates it, or a platform that does not publish it,
# installs the headless agent alone. Its transfer starts here — after
# `SHA256SUMS`, which every asset is verified against anyway, and before the
# required archive — so the client arrives while the agent archive is still in
# flight instead of adding its own round trip after it. Two conditions decide
# that: the platform publishes the client, and the manifest names both archives
# with valid digests. An archive the manifest does not name could never be
# verified, so it is not requested at all. The transfer is settled and checked
# below, before anything is handed to the release executable.
tui_archive=""
tui_fetch_state="unpublished"
if [ "$OS" != "FreeBSD" ]; then
  tui_archive="horon-tui${ARTIFACT#horon-agent}.tar.gz"
  if manifest_digest "$tui_archive" >/dev/null \
    && { manifest_digest "$ARTIFACT.tar.gz" >/dev/null || manifest_digest "$ARTIFACT$EXE" >/dev/null; }; then
    if use_cached_asset "$tui_archive"; then
      tui_fetch_state="cached"
    else
      # Track curl itself so cleanup can stop it before removing its output.
      curl -fsSL --retry 2 --connect-timeout 10 \
        "$GITHUB_RELEASE_BASE/$tui_archive" -o "$assets_dir/$tui_archive" 2>/dev/null &
      background_fetch_pid=$!
      tui_fetch_state="pending"
    fi
  fi
fi
# <<< horon-tui

download_mode=""
downloaded_asset=""
downloaded_from_cache=false
# Assets this run downloaded and verified. They are published to the cache after
# the release executable has staged them, so the publish can move the file
# instead of copying bytes the install still has to read.
verified_assets=()
for candidate in "$ARTIFACT.tar.gz" "$ARTIFACT$EXE"; do
  case "$candidate" in
    *.tar.gz) candidate_mode="tar" ;;
    *) candidate_mode="bin" ;;
  esac
  if use_cached_asset "$candidate"; then
    downloaded_asset="$candidate"
    download_mode="$candidate_mode"
    downloaded_from_cache=true
    info "Using cached, SHA-256 verified: $candidate"
    break
  fi
  if fetch_release_asset "$candidate" "$assets_dir/$candidate"; then
    downloaded_asset="$candidate"
    download_mode="$candidate_mode"
    break
  fi
  rm -f "$assets_dir/$candidate"
done

if [ -n "$download_mode" ] && [ "$downloaded_from_cache" = false ]; then
  verify_asset_checksum "$downloaded_asset" "$assets_dir/$downloaded_asset"
  info "Verified SHA-256: $downloaded_asset"
  verified_assets+=("$downloaded_asset")
fi

# >>> horon-tui
# Settle the client before the handoff, so the release executable only ever sees
# a complete, verified archive. A transfer still running is waited on here, and
# a failed one is a normal reported outcome — the release does not publish the
# archive after all — never an error for the agent itself.
if [ -n "$background_fetch_pid" ]; then
  tui_fetch_status=0
  wait "$background_fetch_pid" || tui_fetch_status=$?
  background_fetch_pid=""
  if [ "$tui_fetch_status" -eq 0 ]; then
    tui_fetch_state="downloaded"
  else
    tui_fetch_state="unavailable"
  fi
fi

if [ -n "$tui_archive" ]; then
  if [ -z "$download_mode" ]; then
    # A source build hands over the locally built binary only, so a client
    # transferred beside a prebuilt agent that turned out to be unavailable is
    # discarded exactly as a serial download never fetched it.
    rm -f "$assets_dir/$tui_archive"
  elif [ "$tui_fetch_state" = "cached" ]; then
    info "Using cached, SHA-256 verified: $tui_archive"
  elif [ "$tui_fetch_state" = "downloaded" ]; then
    if problem=$(checksum_problem "$tui_archive" "$assets_dir/$tui_archive"); then
      info "Verified SHA-256: $tui_archive"
      verified_assets+=("$tui_archive")
    else
      warn "skipped the interactive client: $problem"
      rm -f "$assets_dir/$tui_archive"
    fi
  else
    rm -f "$assets_dir/$tui_archive"
    warn "this release does not publish $tui_archive; installed horon-agent without the interactive client"
  fi
fi
# <<< horon-tui

# Hand the verified assets to the release executable's own installer. It
# re-verifies every asset in Rust, extracts the archive itself, checks the
# version against the staged executable, and only then advances the immutable
# version, channels, launcher and metadata with rollback. A failure before that
# point leaves the previous installation, its settings and its metadata exactly
# as they were.
source_binary=""
bootstrap_bin=""
if [ "$download_mode" = "tar" ]; then
  prepare_bootstrap_executable
  patch_termux_bootstrap
elif [ "$download_mode" = "bin" ]; then
  bootstrap_bin="$bootstrap_dir/$bin_name"
  cp "$assets_dir/$downloaded_asset" "$bootstrap_bin"
  chmod +x "$bootstrap_bin" 2>/dev/null || true
  patch_termux_bootstrap
else
  info "No prebuilt asset found for $ARTIFACT in $VERSION; building from source..."
  command -v git >/dev/null 2>&1 || err "git is required to build from source"
  command -v cargo >/dev/null 2>&1 || err "cargo is required to build from source"

  src_dir="$tmpdir/horon-agent-src"
  git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$src_dir" \
    || err "Failed to clone $REPO at $VERSION"
  cargo build --release --locked --manifest-path "$src_dir/Cargo.toml" \
    || err "cargo build failed while building $REPO from source"

  bootstrap_bin="$src_dir/target/release/$bin_name"
  [ -f "$bootstrap_bin" ] || err "Built binary not found at $bootstrap_bin"
  chmod +x "$bootstrap_bin" 2>/dev/null || true
  # A locally compiled executable is trusted as source rather than compared with
  # published bytes. The release checksum list downloaded above stays mandatory.
  source_binary="$bootstrap_bin"
fi

set -- install-release --assets "$assets_dir" --agent-version "$version"
if [ -n "$source_binary" ]; then
  set -- "$@" --source-binary "$source_binary"
fi
HORON_AGENT_NO_AUTO_UPDATE=1 \
  HORON_AGENT_INSTALL_DIR="$INSTALL_DIR" \
  "$bootstrap_bin" "$@" \
  || err "Failed to install $VERSION; the previous installation is unchanged"
info "Installed horon-agent $VERSION"

# Only completed installs may publish their verified downloads.
for verified_asset in ${verified_assets[@]+"${verified_assets[@]}"}; do
  cache_asset "$verified_asset"
done

if [ "$IS_TERMUX" = true ] && [ "$IS_WINDOWS" = false ]; then
  # Termux exports a preload that the glibc build must not inherit, so the
  # launcher stays a small wrapper that clears it before exec.
  rm -f "$launcher_path"
  cat > "$launcher_path" <<EOF
#!/usr/bin/env bash
unset LD_PRELOAD
exec "$builds_dir/current/$bin_name" "\$@"
EOF
  chmod +x "$launcher_path"
  # Aliases must go through the same wrapper so LD_PRELOAD is cleared for them too.
  for alias in horon hrn; do
    alias_path="$INSTALL_DIR/$alias"
    if [ ! -e "$alias_path" ] || [ -L "$alias_path" ]; then
      ln -sf "$launcher_path" "$alias_path"
    fi
  done
fi

if [ "$OS" = "Darwin" ]; then
  xattr -d com.apple.quarantine "$launcher_path" 2>/dev/null || true
fi

# Retire any background server still running the old binary so the freshly
# installed version is picked up without the user having to kill a daemon by
# hand (issue #291). We use the graceful `server reload` path, which hands live
# headless/swarm sessions to a newly-exec'd server instead of dropping them, and
# only reloads when the running server is genuinely older than what we just
# installed (so a newer/dev daemon is never downgraded). This is best-effort:
# it must never fail the install, and it is skipped when no server is running.
if [ "${HORON_AGENT_SKIP_SERVER_RELOAD:-}" != "1" ]; then
  reload_bin="$launcher_path"
  [ -x "$reload_bin" ] || reload_bin="$builds_dir/current/$bin_name"
  if [ -x "$reload_bin" ]; then
    if "$reload_bin" server reload </dev/null >/dev/null 2>&1; then
      info "Reloaded the running horon-agent server onto $VERSION (if one was active)."
    fi
  fi
fi

if [ "$IS_WINDOWS" = true ]; then
  win_install_dir=$(cygpath -w "$INSTALL_DIR" 2>/dev/null || echo "$INSTALL_DIR")

  # Persist the launcher dir on the USER PATH so every future shell (PowerShell,
  # cmd, Git Bash, Windows Terminal) finds horon-agent without manual setup. This is
  # the Git Bash (`curl | sh`) counterpart of install.ps1's Set-HoronAgentUserPath:
  # read the user PATH, drop stale horon-agent launcher entries (case- and trailing-
  # slash-insensitive), prepend the canonical dir, and broadcast
  # WM_SETTINGCHANGE so already-open apps can pick up the change.
  win_path_persisted=false
  _win_path_key() { printf '%s' "$1" | sed 's|[\\/]*$||' | tr '[:upper:]' '[:lower:]'; }
  if command -v powershell.exe >/dev/null 2>&1; then
    current_user_path=$(powershell.exe -NoProfile -NonInteractive -Command \
      "[Environment]::GetEnvironmentVariable('Path','User')" 2>/dev/null | tr -d '\r' || true)
    target_key=$(_win_path_key "$win_install_dir")
    new_user_path="$win_install_dir"
    set -f
    IFS=';'
    for entry in $current_user_path; do
      [ -n "$entry" ] || continue
      [ "$(_win_path_key "$entry")" = "$target_key" ] && continue
      new_user_path="$new_user_path;$entry"
    done
    unset IFS
    set +f
    if [ "$new_user_path" = "$current_user_path" ]; then
      win_path_persisted=true
    elif HORON_AGENT_NEW_USER_PATH="$new_user_path" powershell.exe -NoProfile -NonInteractive -Command \
      '[Environment]::SetEnvironmentVariable("Path", $env:HORON_AGENT_NEW_USER_PATH, "User")' >/dev/null 2>&1; then
      win_path_persisted=true
      # Broadcast WM_SETTINGCHANGE (0x001A) with the "Environment" lParam to
      # HWND_BROADCAST so running shells learn about the new PATH. Best-effort.
      # The script lives in a quoted heredoc so no bash expansion touches it;
      # eval_setup_friction.sh parse-checks this exact block with real pwsh.
      win_broadcast_ps=$(cat <<'HORON_AGENT_PS_BROADCAST_EOF'
$sig = '[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);'
$type = Add-Type -MemberDefinition $sig -Name 'HoronAgentEnvBroadcast' -Namespace Win32 -PassThru
[UIntPtr]$result = [UIntPtr]::Zero
$type::SendMessageTimeout([IntPtr]0xffff, 0x001A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result) | Out-Null
HORON_AGENT_PS_BROADCAST_EOF
)
      powershell.exe -NoProfile -NonInteractive -Command "$win_broadcast_ps" >/dev/null 2>&1 || true
    fi
  fi

  echo ""
  info "✅ horon-agent $VERSION installed successfully!"
  echo ""
  if [ "$win_path_persisted" = true ]; then
    info "Added $win_install_dir to your user PATH. New terminals will find horon-agent automatically."
  fi
  if command -v horon-agent >/dev/null 2>&1; then
    info "Run 'horon-agent' to get started."
  else
    echo "  To start using horon-agent in THIS terminal right now, run:"
    echo ""
    printf '    \033[1;32mexport PATH="%s:$PATH" && horon-agent\033[0m\n' "$INSTALL_DIR"
    if [ "$win_path_persisted" != true ]; then
      echo ""
      echo "  To add horon-agent to PATH permanently (PowerShell):"
      echo ""
      printf '    \033[1;32m[Environment]::SetEnvironmentVariable("Path", "%s;" + [Environment]::GetEnvironmentVariable("Path", "User"), "User")\033[0m\n' "$win_install_dir"
    fi
  fi
else
  PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""
  added_to=""

  _have() { command -v "$1" >/dev/null 2>&1; }

  # Append the POSIX (bash/zsh/sh) PATH line to an rc file, idempotently.
  #   ensure_posix_rc <rc-file> <create:yes|no>
  # With create=yes the file (and parent dir) is created if missing; with
  # create=no we only touch files that already exist, so we never change how a
  # login shell resolves its startup files (e.g. creating ~/.bash_profile would
  # stop bash from reading ~/.profile).
  ensure_posix_rc() {
    rc="$1"; create="$2"
    if [ ! -f "$rc" ]; then
      [ "$create" = "yes" ] || return 0
      mkdir -p "$(dirname "$rc")"
    fi
    if ! grep -qF "$INSTALL_DIR" "$rc" 2>/dev/null; then
      printf '\n# Added by horon-agent installer\n%s\n' "$PATH_LINE" >> "$rc"
      added_to="$added_to $rc"
    fi
  }

  # fish uses its own syntax and does not read POSIX rc files.
  ensure_fish_rc() {
    create="$1"
    rc="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
    if [ ! -f "$rc" ]; then
      [ "$create" = "yes" ] || return 0
      mkdir -p "$(dirname "$rc")"
    fi
    if ! grep -qF "$INSTALL_DIR" "$rc" 2>/dev/null; then
      {
        printf '\n# Added by horon-agent installer\n'
        printf 'if not contains "%s" $PATH\n' "$INSTALL_DIR"
        printf '    set -gx PATH "%s" $PATH\n' "$INSTALL_DIR"
        printf 'end\n'
      } >> "$rc"
      added_to="$added_to $rc"
    fi
  }

  # zsh: ~/.zshenv is read for every zsh invocation (login, interactive and
  # scripts), so it is the most reliable single place to export PATH.
  if _have zsh || [ "$OS" = "Darwin" ] || [ -f "$HOME/.zshenv" ] || [ -f "$HOME/.zshrc" ]; then
    ensure_posix_rc "$HOME/.zshenv" yes
  fi

  # bash: ~/.bashrc for interactive shells, ~/.profile for login shells (macOS
  # Terminal, ssh, etc.). We only create ~/.profile, never ~/.bash_profile, so
  # we don't override an existing login-file lookup order.
  if _have bash || [ -f "$HOME/.bashrc" ] || [ -f "$HOME/.bash_profile" ]; then
    ensure_posix_rc "$HOME/.bashrc" yes
  fi
  ensure_posix_rc "$HOME/.profile" yes

  # fish: only set it up when fish is installed or already configured.
  if _have fish || [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish" ]; then
    ensure_fish_rc yes
  fi

  # Also patch other common startup files when they already exist, so we cover
  # users with custom login-shell setups without creating new files.
  for rc in "$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bash_profile"; do
    ensure_posix_rc "$rc" no
  done

  if [ -n "$added_to" ]; then
    info "Added $INSTALL_DIR to PATH in:$added_to"
  fi

  echo ""
  info "✅ horon-agent $VERSION installed successfully!"
  echo ""

  if command -v horon-agent >/dev/null 2>&1; then
    info "Run 'horon-agent' to get started."
  else
    echo "  To start using horon-agent right now, run:"
    echo ""
    printf '    \033[1;32mexport PATH="%s:\$PATH" && horon-agent\033[0m\n' "$INSTALL_DIR"
    echo ""
    echo "  Future terminal sessions will have horon-agent on PATH automatically."
  fi
fi

