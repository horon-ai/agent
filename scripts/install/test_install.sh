#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/home" "$tmp/install"

# The installer keeps a per-user download cache. Every scenario below gets its
# own HOME, so dropping an inherited XDG_CACHE_HOME keeps every run's cache
# inside $tmp and away from the real user cache.
unset XDG_CACHE_HOME

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

cat > "$tmp/bin/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -s) printf '%s\n' "${TEST_UNAME_S:-Linux}" ;;
  -m) printf '%s\n' "${TEST_UNAME_M:-x86_64}" ;;
  *) printf '%s\n' "${TEST_UNAME_S:-Linux}" ;;
esac
EOF

cat > "$tmp/bin/curl" <<'EOF'
#!/usr/bin/env bash
output=""
payload=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output="$2"; shift 2 ;;
    --data) payload="$2"; shift 2 ;;
    -w) shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
[ -z "${DOWNLOAD_URL_LOG:-}" ] || printf '%s\n' "$url" >> "$DOWNLOAD_URL_LOG"
case "$url" in
  https://github.com/horon-ai/agent/releases/latest)
    [ "${FAIL_RELEASE:-0}" != "1" ] || exit 22
    printf 'https://github.com/horon-ai/agent/releases/tag/v1.2.3'
    ;;
  https://github.com/horon-ai/agent/releases/download/v1.2.3/*)
    name="${url##*/}"
    [ "${FAIL_ASSET:-}" != "$name" ] || exit 22
    [ -f "$ASSET_SOURCE_DIR/$name" ] || exit 22
    [ -n "$output" ] || exit 2
    # The interactive client's transfer must already be under way when the
    # required archive is requested: the installer starts it first, so the two
    # overlap instead of running one after the other. Waiting here makes that
    # ordering a deterministic check rather than a timing race.
    if [ "${REQUIRE_PARALLEL_TUI:-0}" = "1" ] && [ "${name#horon-agent-}" != "$name" ]; then
      for _attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        grep -q 'horon-tui' "$REQUIRE_PARALLEL_TUI_LOG" 2>/dev/null && break
        sleep 0.05
      done
      grep -q 'horon-tui' "$REQUIRE_PARALLEL_TUI_LOG" 2>/dev/null || exit 97
    fi
    if [ "${CORRUPT_ASSET:-}" = "$name" ]; then
      printf 'corrupted bytes' > "$output"
    else
      cp "$ASSET_SOURCE_DIR/$name" "$output"
    fi
    # A transfer that finishes late is what proves a background transfer was
    # stopped: the marker only appears when the process ran to completion.
    if [ "${SLOW_ASSET:-}" = "$name" ]; then
      sleep "${SLOW_ASSET_SECONDS:-0.4}"
      [ -z "${SLOW_ASSET_DONE_LOG:-}" ] || printf '%s\n' "$name" >> "$SLOW_ASSET_DONE_LOG"
    fi
    ;;
  *) exit 22 ;;
esac
EOF
chmod +x "$tmp/bin/uname" "$tmp/bin/curl"


# The release CLI the installer bootstraps. It records the arguments it is given
# and reproduces what the real `install-release` leaves behind: an immutable
# version directory and the PATH launcher.
cli_body='#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then
  printf "version\tv1.2.3 (abc1234)\n"
  exit 0
fi
if [ "${1:-}" = "install-release" ]; then
  printf "%s\n" "$*" >> "$CLI_ARG_LOG"
  previous=""
  for argument in "$@"; do
    if [ "$previous" = "--assets" ]; then
      for entry in "$argument"/*; do
        printf "asset:%s\n" "${entry##*/}" >> "$CLI_ARG_LOG"
        # The verified downloads are published only after this executable has
        # staged them, so an entry it is handed from a download must not be in
        # the cache yet.
        if [ -n "${CLI_CACHE_PROBE:-}" ] && [ -e "$CLI_CACHE_PROBE/${entry##*/}" ]; then
          printf "cached-during-install:%s\n" "${entry##*/}" >> "$CLI_ARG_LOG"
        fi
      done
    fi
    previous="$argument"
  done
  [ "${CLI_FAIL:-0}" != "1" ] || exit 1
  install_dir="${HORON_AGENT_INSTALL_DIR:?}"
  mkdir -p "$HORON_HOME/builds/versions/1.2.3" "$install_dir"
  printf "%s\n" "1.2.3" > "$HORON_HOME/builds/current-version"
  cp "$0" "$install_dir/horon-agent"
  chmod +x "$install_dir/horon-agent"
  exit 0
fi
# Desktop setup verbs (hotkeys, launchers) are no longer part of installation.
# Anything other than `install-release` is recorded so the test can assert the
# installer never asks the binary to configure the desktop again.
if [ -n "${1:-}" ]; then
  [ -z "${DESKTOP_SETUP_LOG:-}" ] || printf "%s\n" "$*" >> "$DESKTOP_SETUP_LOG"
  exit 0
fi
exit 0
'

build_assets() {
  dest="$1"
  agent_artifact="$2"
  archive_name="${agent_artifact%.exe}"
  rm -rf "$dest"
  mkdir -p "$dest/stage"
  printf '%s\n' "$cli_body" > "$dest/stage/$agent_artifact"
  chmod +x "$dest/stage/$agent_artifact"
  # Every published archive carries the licence beside the binary, and the
  # installer must accept that member without treating it as unexpected.
  printf 'MIT License\n' > "$dest/stage/LICENSE"
  local members=("$agent_artifact")
  if [[ "$agent_artifact" == horon-agent-linux-x86_64 ]]; then
    mv "$dest/stage/$agent_artifact" "$dest/stage/$agent_artifact.bin"
    printf '%s\n' '#!/bin/sh' 'exec "${0%/*}/horon-agent-linux-x86_64.bin" "$@"' > "$dest/stage/$agent_artifact"
    chmod +x "$dest/stage/$agent_artifact"
    members+=("$agent_artifact.bin")
  fi
  tar -czf "$dest/$archive_name.tar.gz" -C "$dest/stage" "${members[@]}" LICENSE
  (
    cd "$dest"
    printf '%s  %s\n' "$(sha256 "$archive_name.tar.gz")" "$archive_name.tar.gz"
  ) > "$dest/SHA256SUMS"
  rm -rf "$dest/stage"
}

# The interactive client ships as a second archive beside the agent. Linux
# x86_64 uses the compatibility layout: a wrapper plus its `.bin` payload.
# Other platforms publish the executable alone.
build_tui_asset() {
  dest="$1"
  tui_artifact="$2"
  layout="${3:-compat}"
  archive="${tui_artifact%.exe}"
  mkdir -p "$dest/tui-stage"
  printf '%s\n' 'tui executable' > "$dest/tui-stage/$tui_artifact"
  chmod +x "$dest/tui-stage/$tui_artifact"
  printf 'MIT License\n' > "$dest/tui-stage/LICENSE"
  if [ "$layout" = compat ]; then
    printf '%s\n' 'tui payload' > "$dest/tui-stage/$tui_artifact.bin"
    tar -czf "$dest/$archive.tar.gz" -C "$dest/tui-stage" "$tui_artifact" "$tui_artifact.bin" LICENSE
  else
    tar -czf "$dest/$archive.tar.gz" -C "$dest/tui-stage" "$tui_artifact" LICENSE
  fi
  (
    cd "$dest"
    printf '%s  %s\n' "$(sha256 "$archive.tar.gz")" "$archive.tar.gz"
  ) >> "$dest/SHA256SUMS"
  rm -rf "$dest/tui-stage"
}

assets="$tmp/assets"
build_assets "$assets" horon-agent-linux-x86_64
build_tui_asset "$assets" horon-tui-linux-x86_64

desktop_setup_log="$tmp/desktop-setup.log"
url_log="$tmp/urls.log"
cli_arg_log="$tmp/cli-args.log"

run_install() {
  PATH="$tmp/bin:$PATH" \
    HOME="$tmp/home" \
    HORON_HOME="$tmp/home/.horon" \
    HORON_AGENT_INSTALL_DIR="$tmp/install" \
    ASSET_SOURCE_DIR="$assets" \
    CLI_ARG_LOG="$cli_arg_log" \
    DESKTOP_SETUP_LOG="$desktop_setup_log" \
    DOWNLOAD_URL_LOG="$url_log" \
    HORON_AGENT_SKIP_SERVER_RELOAD=1 \
    bash "$repo_dir/scripts/install/install.sh"
}

PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home" \
  HORON_HOME="$tmp/home/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install" \
  ASSET_SOURCE_DIR="$assets" \
  CLI_ARG_LOG="$cli_arg_log" \
  CLI_CACHE_PROBE="$tmp/home/.cache/horon-agent/release" \
  DESKTOP_SETUP_LOG="$desktop_setup_log" \
  DOWNLOAD_URL_LOG="$url_log" \
  REQUIRE_PARALLEL_TUI=1 \
  REQUIRE_PARALLEL_TUI_LOG="$url_log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >"$tmp/full.out" 2>"$tmp/full.err"

# Cache scenarios reuse the shared environment but need their own HOME, and they
# add per-scenario variables. `env` turns every NAME=VALUE argument into the
# child's environment; prefix assignments cannot do that from an expansion.
run_install_home() {
  install_home="$1"
  shift
  env PATH="$tmp/bin:$PATH" \
    HOME="$install_home" \
    HORON_HOME="$install_home/.horon" \
    HORON_AGENT_INSTALL_DIR="$tmp/install-scenarios" \
    ASSET_SOURCE_DIR="$assets" \
    HORON_AGENT_SKIP_SERVER_RELOAD=1 \
    "$@" \
    bash "$repo_dir/scripts/install/install.sh"
}

test ! -e "$desktop_setup_log"
test "$(cat "$tmp/home/.horon/builds/current-version")" = "1.2.3"

# The installer hands the executable a local asset directory holding exactly the
# same release, and never asks the executable to download anything itself.
grep -Fq -- "--agent-version 1.2.3" "$cli_arg_log"
grep -Fq -- "install-release --assets " "$cli_arg_log"
test "$(grep -c -- '--source-binary' "$cli_arg_log")" = 0
for required in SHA256SUMS horon-agent-linux-x86_64.tar.gz horon-tui-linux-x86_64.tar.gz; do
  grep -qx -- "asset:$required" "$cli_arg_log" \
    || { echo "installer did not hand $required to the release executable" >&2; exit 1; }
done
# A verified client is reported, and nothing warns about it.
grep -Fq 'Verified SHA-256: horon-tui-linux-x86_64.tar.gz' "$tmp/full.out"
if grep -q '^warning: .*horon-tui' "$tmp/full.err"; then
  echo "installer warned about a complete release" >&2
  cat "$tmp/full.err" >&2
  exit 1
fi

# Every asset comes from the canonical horon-ai/agent release, and no other host
# is consulted for release bytes.
grep -q '^https://github.com/horon-ai/agent/releases/download/v1.2.3/horon-agent-linux-x86_64.tar.gz$' "$url_log"
grep -q '^https://github.com/horon-ai/agent/releases/download/v1.2.3/SHA256SUMS$' "$url_log"
grep -q '^https://github.com/horon-ai/agent/releases/download/v1.2.3/horon-tui-linux-x86_64.tar.gz$' "$url_log"
if grep -v -e '^https://github.com/horon-ai/agent/' "$url_log" | grep -q .; then
  echo "installer contacted a non-canonical release host" >&2
  exit 1
fi

# The first install into an empty cache downloads and keeps exactly the assets
# it verified, one file per asset name. The manifest is fetched every run and is
# never stored.
cache_dir="$tmp/home/.cache/horon-agent/release"
for cached in horon-agent-linux-x86_64.tar.gz horon-tui-linux-x86_64.tar.gz; do
  test -f "$cache_dir/$cached" \
    || { echo "installer did not cache $cached" >&2; exit 1; }
  test ! -L "$cache_dir/$cached"
  mode=$(stat -c '%a' "$cache_dir/$cached" 2>/dev/null || stat -f '%Lp' "$cache_dir/$cached")
  [ "$mode" = "600" ] \
    || { echo "cached $cached has mode ${mode:-unknown}, not 600" >&2; exit 1; }
done
test ! -e "$cache_dir/SHA256SUMS"
if grep -q 'Using cached' "$tmp/full.out"; then
  echo "installer reported a cache hit on an empty cache" >&2
  exit 1
fi
# A verified download becomes a cache entry only after the release executable
# has staged it, so the publish can move the file instead of copying bytes the
# install still needs and a failed install leaves no entry behind.
if grep -q '^cached-during-install:' "$cli_arg_log"; then
  echo "installer published a verified download before the install consumed it" >&2
  grep '^cached-during-install:' "$cli_arg_log" >&2
  exit 1
fi

# A rerun with a warm cache downloads only the manifest. The cached bytes are
# copied into the temporary asset directory and hashed there, so the check runs
# against this run's manifest and not against the cache file.
warm_url_log="$tmp/warm-urls.log"
run_install_home "$tmp/home" \
  CLI_ARG_LOG="$tmp/cli-args-warm.log" \
  DOWNLOAD_URL_LOG="$warm_url_log" >"$tmp/warm.out" 2>"$tmp/warm.err"
grep -Fq 'Using cached, SHA-256 verified: horon-agent-linux-x86_64.tar.gz' "$tmp/warm.out"
grep -Fq 'Using cached, SHA-256 verified: horon-tui-linux-x86_64.tar.gz' "$tmp/warm.out"
test -s "$warm_url_log"
grep -q '/v1.2.3/SHA256SUMS$' "$warm_url_log"
if grep -q '/releases/download/[^/]*/h' "$warm_url_log"; then
  echo "installer downloaded an asset that was already cached" >&2
  cat "$warm_url_log" >&2
  exit 1
fi
if grep -q 'Verified SHA-256: horon-agent-linux-x86_64.tar.gz' "$tmp/warm.out"; then
  echo "installer re-verified a download it never made" >&2
  exit 1
fi

# A damaged cache entry is refused: the asset is downloaded again, the verified
# bytes replace the entry, and the damaged file never reaches the executable.
for damaged_asset in horon-agent-linux-x86_64.tar.gz horon-tui-linux-x86_64.tar.gz; do
  printf 'corrupted cache bytes' > "$cache_dir/$damaged_asset"
  recovery_urls="$tmp/recover-$damaged_asset.urls"
  recovery_args="$tmp/recover-$damaged_asset.args"
  run_install_home "$tmp/home" \
    CLI_ARG_LOG="$recovery_args" DOWNLOAD_URL_LOG="$recovery_urls" \
    >"$tmp/recover-$damaged_asset.out" 2>"$tmp/recover-$damaged_asset.err"
  grep -Fxq "https://github.com/horon-ai/agent/releases/download/v1.2.3/$damaged_asset" "$recovery_urls"
  grep -Fxq "asset:$damaged_asset" "$recovery_args"
  test "$(sha256 "$cache_dir/$damaged_asset")" \
    = "$(awk -v asset="$damaged_asset" '$2 == asset { print $1 }' "$assets/SHA256SUMS")"
done

# A cache entry that does not match this release's manifest is refused the same
# way even when the bytes are a complete, readable file.
printf 'bytes from another release' > "$cache_dir/horon-agent-linux-x86_64.tar.gz"
stale_cache_url_log="$tmp/stale-cache-urls.log"
run_install_home "$tmp/home" \
  CLI_ARG_LOG="$tmp/cli-args-stale-cache.log" \
  DOWNLOAD_URL_LOG="$stale_cache_url_log" >"$tmp/stale-cache.out" 2>"$tmp/stale-cache.err"
if grep -q 'Using cached, SHA-256 verified: horon-agent-linux-x86_64.tar.gz' "$tmp/stale-cache.out"; then
  echo "installer reused a cache entry the manifest does not list" >&2
  exit 1
fi
grep -q '/v1.2.3/horon-agent-linux-x86_64.tar.gz$' "$stale_cache_url_log"

# A cache entry replaced by a symbolic link is neither followed nor written
# through: the link target keeps its bytes and the entry becomes a regular file
# holding the freshly verified download.
link_target="$tmp/cache-link-target"
printf 'link target bytes\n' > "$link_target"
rm -f "$cache_dir/horon-agent-linux-x86_64.tar.gz"
ln -s "$link_target" "$cache_dir/horon-agent-linux-x86_64.tar.gz"
link_url_log="$tmp/link-cache-urls.log"
run_install_home "$tmp/home" \
  CLI_ARG_LOG="$tmp/cli-args-link-cache.log" \
  DOWNLOAD_URL_LOG="$link_url_log" >"$tmp/link-cache.out" 2>"$tmp/link-cache.err"
grep -q '/v1.2.3/horon-agent-linux-x86_64.tar.gz$' "$link_url_log"
test "$(cat "$link_target")" = 'link target bytes'
test ! -L "$cache_dir/horon-agent-linux-x86_64.tar.gz"
test "$(sha256 "$cache_dir/horon-agent-linux-x86_64.tar.gz")" \
  = "$(awk '$2 == "horon-agent-linux-x86_64.tar.gz" { print $1 }' "$assets/SHA256SUMS")"

# A cache directory the group can write into is not trusted, because another
# member of the group could replace an entry. The installer downloads instead
# and leaves the directory alone.
group_home="$tmp/home-group-cache"
group_cache="$group_home/.cache/horon-agent/release"
mkdir -p "$group_cache"
cp "$assets/horon-agent-linux-x86_64.tar.gz" "$group_cache/"
cp "$assets/horon-tui-linux-x86_64.tar.gz" "$group_cache/"
chmod 0775 "$group_cache"
group_url_log="$tmp/group-cache-urls.log"
run_install_home "$group_home" \
  CLI_ARG_LOG="$tmp/cli-args-group-cache.log" \
  DOWNLOAD_URL_LOG="$group_url_log" >"$tmp/group-cache.out" 2>"$tmp/group-cache.err"
if grep -q 'Using cached' "$tmp/group-cache.out"; then
  echo "installer trusted a group-writable cache directory" >&2
  exit 1
fi
grep -q '/v1.2.3/horon-agent-linux-x86_64.tar.gz$' "$group_url_log"

# A failed install publishes nothing, so the next run downloads instead of
# trusting bytes the release executable never staged.
failed_home="$tmp/home-failed-install"
if run_install_home "$failed_home" \
  CLI_FAIL=1 \
  CLI_ARG_LOG="$tmp/cli-args-install-failed.log" \
  >"$tmp/publish-failed.out" 2>"$tmp/publish-failed.err"; then
  echo "installer reported success for a failed install" >&2
  exit 1
fi
test ! -e "$failed_home/.cache/horon-agent/release/horon-agent-linux-x86_64.tar.gz"
test ! -e "$failed_home/.cache/horon-agent/release/horon-tui-linux-x86_64.tar.gz"

# Check publication across real filesystems when the Linux runner has tmpfs.
if [[ -d /dev/shm && -w /dev/shm ]] && [[ $(stat -c %d /dev/shm) != "$(stat -c %d "$tmp")" ]]; then
  (
    split_parent=$(mktemp -d /dev/shm/horon-cache.XXXXXX)
    trap 'rm -rf "$split_parent"' EXIT
    split_home="$tmp/home-split-device"
    split_cache="$split_parent/cache"
    run_install_home "$split_home" XDG_CACHE_HOME="$split_cache" \
      CLI_ARG_LOG="$tmp/cli-args-split.log" >"$tmp/split.out" 2>"$tmp/split.err"
    for archive in horon-agent-linux-x86_64.tar.gz horon-tui-linux-x86_64.tar.gz; do
      entry="$split_cache/horon-agent/release/$archive"
      cmp "$assets/$archive" "$entry"
      test "$(stat -c %a "$entry")" = 600
    done
    run_install_home "$split_home" XDG_CACHE_HOME="$split_cache" \
      FAIL_ASSET=horon-agent-linux-x86_64.tar.gz CLI_ARG_LOG="$tmp/cli-args-split-warm.log" \
      >"$tmp/split-warm.out" 2>"$tmp/split-warm.err"
    grep -Fq 'Using cached, SHA-256 verified: horon-agent-linux-x86_64.tar.gz' "$tmp/split-warm.out"
  )
fi

# An explicit XDG_CACHE_HOME is used when it is private to this user...
xdg_private="$tmp/xdg-private"
mkdir -p "$xdg_private"
chmod 0700 "$xdg_private"
run_install_home "$tmp/home-xdg-private" \
  XDG_CACHE_HOME="$xdg_private" \
  CLI_ARG_LOG="$tmp/cli-args-xdg-private.log" \
  >"$tmp/xdg-private.out" 2>"$tmp/xdg-private.err"
test -f "$xdg_private/horon-agent/release/horon-agent-linux-x86_64.tar.gz"

# ... and is skipped without being written into when it is a shared,
# world-writable location.
xdg_shared="$tmp/xdg-shared"
mkdir -p "$xdg_shared"
chmod 0777 "$xdg_shared"
run_install_home "$tmp/home-xdg-shared" \
  XDG_CACHE_HOME="$xdg_shared" \
  CLI_ARG_LOG="$tmp/cli-args-xdg-shared.log" \
  >"$tmp/xdg-shared.out" 2>"$tmp/xdg-shared.err"
test "$(cat "$tmp/home-xdg-shared/.horon/builds/current-version")" = "1.2.3"
test ! -e "$xdg_shared/horon-agent"

# A private directory inside a shared parent is refused too, because the parent
# could be replaced by another user.
mkdir -p "$xdg_shared/nested"
chmod 0700 "$xdg_shared/nested"
run_install_home "$tmp/home-xdg-nested" \
  XDG_CACHE_HOME="$xdg_shared/nested" \
  CLI_ARG_LOG="$tmp/cli-args-xdg-nested.log" \
  >"$tmp/xdg-nested.out" 2>"$tmp/xdg-nested.err"
test "$(cat "$tmp/home-xdg-nested/.horon/builds/current-version")" = "1.2.3"
test ! -e "$xdg_shared/nested/horon-agent"

# A cache location that cannot be written to costs a download and nothing else.
# A root-run suite ignores the mode bits, so this scenario is skipped there.
if [ "$(id -u)" != "0" ]; then
  readonly_root="$tmp/readonly-cache"
  mkdir -p "$readonly_root"
  chmod 0500 "$readonly_root"
  run_install_home "$tmp/home-readonly-cache" \
    XDG_CACHE_HOME="$readonly_root" \
    CLI_ARG_LOG="$tmp/cli-args-readonly-cache.log" \
    >"$tmp/readonly-cache.out" 2>"$tmp/readonly-cache.err"
  test "$(cat "$tmp/home-readonly-cache/.horon/builds/current-version")" = "1.2.3"
  test ! -e "$readonly_root/horon-agent"
  chmod 0700 "$readonly_root"
fi

# The documented switch disables the cache outright and installs normally.
disabled_home="$tmp/home-cache-disabled"
run_install_home "$disabled_home" \
  HORON_AGENT_NO_DOWNLOAD_CACHE=1 \
  CLI_ARG_LOG="$tmp/cli-args-cache-disabled.log" \
  >"$tmp/cache-disabled.out" 2>"$tmp/cache-disabled.err"
test "$(cat "$disabled_home/.horon/builds/current-version")" = "1.2.3"
test ! -e "$disabled_home/.cache"

# A release without its checksum list is refused before anything is installed.
missing_assets="$tmp/assets-missing-checksums"
build_assets "$missing_assets" horon-agent-linux-x86_64
rm -f "$missing_assets/SHA256SUMS"
mkdir -p "$tmp/home-checksumless/builds" "$tmp/install-checksumless"
printf '%s\n' 'prior-version' > "$tmp/home-checksumless/builds/current-version"
if PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-checksumless" \
  HORON_HOME="$tmp/home-checksumless/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-checksumless" \
  ASSET_SOURCE_DIR="$missing_assets" \
  CLI_ARG_LOG="$tmp/cli-args-checksumless.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/checksumless.err"; then
  echo "installer accepted a release without SHA256SUMS" >&2
  exit 1
fi
grep -q 'does not publish SHA256SUMS' "$tmp/checksumless.err"
test ! -e "$tmp/cli-args-checksumless.log"
test "$(cat "$tmp/home-checksumless/builds/current-version")" = 'prior-version'
test ! -e "$tmp/install-checksumless/horon-agent"

# A corrupted asset fails verification before the executable is run.
if PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-corrupt" \
  HORON_HOME="$tmp/home-corrupt/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-corrupt" \
  ASSET_SOURCE_DIR="$assets" \
  CORRUPT_ASSET=horon-agent-linux-x86_64.tar.gz \
  CLI_ARG_LOG="$tmp/cli-args-corrupt.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/corrupt.err"; then
  echo "installer accepted a corrupted archive" >&2
  exit 1
fi
grep -q 'SHA-256 verification failed for horon-agent-linux-x86_64.tar.gz' "$tmp/corrupt.err"
test ! -e "$tmp/cli-args-corrupt.log"

# A failed install leaves the previous launcher, channels and metadata alone.
mkdir -p "$tmp/home-failed/builds" "$tmp/install-failed"
printf '%s\n' 'prior-version' > "$tmp/home-failed/builds/current-version"
printf '%s\n' 'prior launcher' > "$tmp/install-failed/horon-agent"
if PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-failed" \
  HORON_HOME="$tmp/home-failed/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-failed" \
  ASSET_SOURCE_DIR="$assets" \
  CLI_FAIL=1 \
  CLI_ARG_LOG="$tmp/cli-args-failed.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/failed.err"; then
  echo "installer reported success after the release install failed" >&2
  exit 1
fi
grep -q 'the previous installation is unchanged' "$tmp/failed.err"
test "$(cat "$tmp/home-failed/builds/current-version")" = 'prior-version'
test "$(cat "$tmp/install-failed/horon-agent")" = 'prior launcher'

# A release lookup failure fails the install.
if PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-lookup" \
  HORON_HOME="$tmp/home-lookup/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-lookup" \
  ASSET_SOURCE_DIR="$assets" \
  FAIL_RELEASE=1 \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>&1; then
  echo "expected release lookup failure" >&2
  exit 1
fi

# Git for Windows can be x64-emulated on Windows ARM64. In that case uname -m
# reports x86_64 while PROCESSOR_ARCHITEW6432 exposes the native ARM64 OS, and the
# installer must fetch that platform's executable.
windows_assets="$tmp/assets-windows"
build_assets "$windows_assets" horon-agent-windows-aarch64.exe
build_tui_asset "$windows_assets" horon-tui-windows-aarch64.exe single
windows_url_log="$tmp/windows-arm64-urls.log"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-windows" \
  LOCALAPPDATA="$tmp/localappdata-windows" \
  HORON_HOME="$tmp/home-windows/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-windows" \
  ASSET_SOURCE_DIR="$windows_assets" \
  CLI_ARG_LOG="$tmp/cli-args-windows.log" \
  DOWNLOAD_URL_LOG="$windows_url_log" \
  TEST_UNAME_S=MINGW64_NT-10.0 \
  TEST_UNAME_M=x86_64 \
  PROCESSOR_ARCHITECTURE=AMD64 \
  PROCESSOR_ARCHITEW6432=ARM64 \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null
grep -q '/horon-agent-windows-aarch64.tar.gz$' "$windows_url_log"

# FreeBSD x64 is a supported install target.
freebsd_assets="$tmp/assets-freebsd"
build_assets "$freebsd_assets" horon-agent-freebsd-x86_64
freebsd_url_log="$tmp/freebsd-urls.log"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-freebsd" \
  HORON_HOME="$tmp/home-freebsd/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-freebsd" \
  ASSET_SOURCE_DIR="$freebsd_assets" \
  CLI_ARG_LOG="$tmp/cli-args-freebsd.log" \
  DOWNLOAD_URL_LOG="$freebsd_url_log" \
  TEST_UNAME_S=FreeBSD \
  TEST_UNAME_M=amd64 \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null
grep -q '/horon-agent-freebsd-x86_64.tar.gz$' "$freebsd_url_log"
test "$(cat "$tmp/home-freebsd/.horon/builds/current-version")" = "1.2.3"

# An archive carrying a member the installer does not expect is refused instead
# of being unpacked wholesale.
odd_assets="$tmp/assets-odd-member"
build_assets "$odd_assets" horon-agent-linux-x86_64
printf 'unexpected\n' > "$odd_assets/stage-extra"
mkdir -p "$odd_assets/stage"
cp "$odd_assets/horon-agent-linux-x86_64.tar.gz" "$odd_assets/agent.tar.gz"
printf '%s\n' 'unexpected member' > "$odd_assets/stage/surprise.txt"
printf '%s\n' "$cli_body" > "$odd_assets/stage/horon-agent-linux-x86_64"
tar -czf "$odd_assets/horon-agent-linux-x86_64.tar.gz" -C "$odd_assets/stage" horon-agent-linux-x86_64 surprise.txt
(
  cd "$odd_assets"
  printf '%s  %s\n' "$(sha256 horon-agent-linux-x86_64.tar.gz)" 'horon-agent-linux-x86_64.tar.gz'
) > "$odd_assets/SHA256SUMS"
rm -rf "$odd_assets/stage" "$odd_assets/stage-extra" "$odd_assets/agent.tar.gz"
if PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-odd" \
  HORON_HOME="$tmp/home-odd/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-odd" \
  ASSET_SOURCE_DIR="$odd_assets" \
  CLI_ARG_LOG="$tmp/cli-args-odd.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/odd.err"; then
  echo "installer unpacked an archive with an unexpected member" >&2
  exit 1
fi
grep -q 'unexpected member: surprise.txt' "$tmp/odd.err"
test ! -e "$tmp/cli-args-odd.log"

# On Linux one uncompressed image of the verified archive serves both the
# listing check and the extraction, so the release bytes are expanded once per
# install. The gzip wrapper records that expansion — the installer ends its
# options before the archive path, and tar's own decompression calls gzip
# without that marker. The tar wrapper fails an extraction out of the image the
# way a full filesystem does, leaving one allowed member half written. Both
# forward to the real tools, so the bytes installed are the release bytes.
real_gzip=$(command -v gzip)
real_tar=$(command -v tar)
mkdir -p "$tmp/bin-inflate" "$tmp/bin-retry"
cat > "$tmp/bin-inflate/gzip" <<EOF
#!/usr/bin/env bash
marker=""
for argument in "\$@"; do
  if [ "\$marker" = "--" ] && [ -n "\${GZIP_ARCHIVE_LOG:-}" ]; then
    printf '%s\n' "\$argument" >> "\$GZIP_ARCHIVE_LOG"
  fi
  marker="\$argument"
done
exec "$real_gzip" "\$@"
EOF
cat > "$tmp/bin-retry/tar" <<EOF
#!/usr/bin/env bash
mode=""
archive=""
previous=""
for argument in "\$@"; do
  case "\$previous" in
    -xf|-xOf|-xzf) mode="\$previous"; archive="\$argument" ;;
  esac
  previous="\$argument"
done
if [ -n "\${RETRY_FAIL_IMAGE:-}" ] && [ "\${archive##*/}" = "bootstrap.tar" ]; then
  [ -z "\${RETRY_TAR_LOG:-}" ] || printf '%s\n' "\$mode" >> "\$RETRY_TAR_LOG"
  case "\$mode" in
    -xf) printf 'partial' > "\${HORON_BOOTSTRAP_DIR:?}/\${RETRY_PARTIAL_NAME:?}"; exit 2 ;;
    -xOf) printf 'partial'; exit 2 ;;
  esac
fi
if [ -n "\${RETRY_TAR_LOG:-}" ]; then
  case "\$mode" in -xf|-xOf|-xzf) printf '%s\n' "\$mode" >> "\$RETRY_TAR_LOG" ;; esac
fi
exec "$real_tar" "\$@"
EOF
chmod +x "$tmp/bin-inflate/gzip" "$tmp/bin-retry/tar"

# One install per scenario: the case's own home, tool path and asset directory,
# with its output and log kept under the home's name.
run_inflate_case() {
  case_home="$1"
  case_path="$2"
  case_gzip_log="$3"
  case_assets="$4"
  shift 4
  env PATH="$case_path" \
    HOME="$case_home" \
    HORON_HOME="$case_home/.horon" \
    HORON_AGENT_INSTALL_DIR="$tmp/install-${case_home##*/}" \
    ASSET_SOURCE_DIR="$case_assets" \
    GZIP_ARCHIVE_LOG="$case_gzip_log" \
    CLI_ARG_LOG="$tmp/cli-args-${case_home##*/}.log" \
    HORON_AGENT_SKIP_SERVER_RELOAD=1 \
    "$@" \
    bash "$repo_dir/scripts/install/install.sh" \
    >"$tmp/${case_home##*/}.out" 2>"$tmp/${case_home##*/}.err"
}
inflate_path="$tmp/bin-inflate:$tmp/bin:$PATH"

# A normal install expands the archive once and still hands the downloaded
# archive itself to the executable that re-verifies it.
mkdir -p "$tmp/home-inflate"
run_inflate_case "$tmp/home-inflate" "$inflate_path" "$tmp/gzip-archive.log" "$assets"
test "$(cat "$tmp/home-inflate/.horon/builds/current-version")" = "1.2.3"
[ "$(grep -c . "$tmp/gzip-archive.log" 2>/dev/null)" = 1 ] \
  || { echo "installer did not expand the archive exactly once" >&2; exit 1; }
grep -q '/horon-agent-linux-x86_64\.tar\.gz$' "$tmp/gzip-archive.log"
grep -qx -- "asset:horon-agent-linux-x86_64.tar.gz" "$tmp/cli-args-home-inflate.log"

# The whole listing is still checked before a bootstrap file is written, and an
# unexpected member is refused out of that same image.
mkdir -p "$tmp/home-inflate-odd"
if run_inflate_case "$tmp/home-inflate-odd" "$inflate_path" \
  "$tmp/gzip-archive-odd.log" "$odd_assets"; then
  echo "installer unpacked an unexpected member from the expanded image" >&2
  exit 1
fi
grep -q 'unexpected member: surprise.txt' "$tmp/home-inflate-odd.err"
[ "$(grep -c . "$tmp/gzip-archive-odd.log" 2>/dev/null)" = 1 ] \
  || { echo "the listing check did not read the expanded image" >&2; exit 1; }
test ! -e "$tmp/cli-args-home-inflate-odd.log"

# A temporary filesystem too small to hold that image beside what is extracted
# from it keeps the previous path: the install still succeeds, and the extra
# copy is never written there.
mkdir -p "$tmp/bin-tight"
cat > "$tmp/bin-tight/df" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'Filesystem 1024-blocks Used Available Capacity Mounted on'
printf '%s\n' 'tmpfs 65536 65535 1 100% /tmp'
EOF
chmod +x "$tmp/bin-tight/df"
mkdir -p "$tmp/home-tight"
run_inflate_case "$tmp/home-tight" "$tmp/bin-tight:$inflate_path" \
  "$tmp/gzip-archive-tight.log" "$assets"
test "$(cat "$tmp/home-tight/.horon/builds/current-version")" = "1.2.3"
[ ! -s "$tmp/gzip-archive-tight.log" ] \
  || { echo "installer expanded the archive although the temporary filesystem was full" >&2; exit 1; }
grep -qx -- "asset:horon-agent-linux-x86_64.tar.gz" "$tmp/cli-args-home-tight.log"

# An extraction out of the image that fails is recovered from the download: the
# image and its partial member are discarded, and the same validated members are
# written once more from the archive the executable re-verifies.
mkdir -p "$tmp/home-retry"
run_inflate_case "$tmp/home-retry" "$tmp/bin-retry:$inflate_path" \
  "$tmp/gzip-archive-retry.log" "$assets" \
  RETRY_FAIL_IMAGE=1 \
  RETRY_PARTIAL_NAME=horon-agent-linux-x86_64 \
  RETRY_TAR_LOG="$tmp/tar-retry.log"
test "$(cat "$tmp/home-retry/.horon/builds/current-version")" = "1.2.3"
[ "$(grep -c . "$tmp/gzip-archive-retry.log" 2>/dev/null)" = 1 ] \
  || { echo "the retried install did not start from the expanded image" >&2; exit 1; }
[ "$(grep -cE -- '^-x(Of|f)$' "$tmp/tar-retry.log")" = 1 ] \
  || { echo "the failing image extraction was not attempted exactly once" >&2; exit 1; }
[ "$(grep -c -- '^-xzf$' "$tmp/tar-retry.log")" = 1 ] \
  || { echo "the install did not fall back to the downloaded archive exactly once" >&2; exit 1; }
grep -qx -- "asset:horon-agent-linux-x86_64.tar.gz" "$tmp/cli-args-home-retry.log"

# A release that predates the interactive client installs the headless agent
# after exactly one warning, and never hands the missing archive to the
# executable.
legacy_release="$tmp/assets-no-tui"
build_assets "$legacy_release" horon-agent-linux-x86_64
legacy_url_log="$tmp/no-tui-urls.log"
mkdir -p "$tmp/home-no-tui" "$tmp/install-no-tui"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-no-tui" \
  HORON_HOME="$tmp/home-no-tui/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-no-tui" \
  ASSET_SOURCE_DIR="$legacy_release" \
  CLI_ARG_LOG="$tmp/cli-args-no-tui.log" \
  DOWNLOAD_URL_LOG="$legacy_url_log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/no-tui.err"
test "$(cat "$tmp/home-no-tui/.horon/builds/current-version")" = "1.2.3"
grep -Fq 'does not publish horon-tui-linux-x86_64.tar.gz' "$tmp/no-tui.err"
test "$(grep -c '^warning: ' "$tmp/no-tui.err")" = 1
# An archive the manifest does not list can never be verified, so it is not even
# requested: the warning above costs no round trip.
if grep -q 'horon-tui' "$legacy_url_log"; then
  echo "installer requested a client archive the manifest does not list" >&2
  exit 1
fi
if grep -q 'asset:horon-tui' "$tmp/cli-args-no-tui.log"; then
  echo "installer handed an absent interactive client to the executable" >&2
  exit 1
fi
grep -qx -- "asset:horon-agent-linux-x86_64.tar.gz" "$tmp/cli-args-no-tui.log"

# A corrupted interactive-client archive is dropped: the agent still installs,
# the bad bytes never reach the executable, and the warning names the file.
mkdir -p "$tmp/home-tui-corrupt" "$tmp/install-tui-corrupt"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-tui-corrupt" \
  HORON_HOME="$tmp/home-tui-corrupt/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-tui-corrupt" \
  ASSET_SOURCE_DIR="$assets" \
  CORRUPT_ASSET=horon-tui-linux-x86_64.tar.gz \
  CLI_ARG_LOG="$tmp/cli-args-tui-corrupt.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/tui-corrupt.err"
test "$(cat "$tmp/home-tui-corrupt/.horon/builds/current-version")" = "1.2.3"
grep -Fq 'skipped the interactive client: SHA-256 verification failed for horon-tui-linux-x86_64.tar.gz' \
  "$tmp/tui-corrupt.err"
test "$(grep -c '^warning: ' "$tmp/tui-corrupt.err")" = 1
if grep -q 'asset:horon-tui' "$tmp/cli-args-tui-corrupt.log"; then
  echo "installer handed a corrupted interactive client to the executable" >&2
  exit 1
fi

# A client the manifest lists but the release does not actually serve fails its
# transfer, which is the same one-line skip: reported, never raised as an install
# error, and the agent installs alone.
mkdir -p "$tmp/home-tui-absent" "$tmp/install-tui-absent"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-tui-absent" \
  HORON_HOME="$tmp/home-tui-absent/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-tui-absent" \
  ASSET_SOURCE_DIR="$assets" \
  FAIL_ASSET=horon-tui-linux-x86_64.tar.gz \
  CLI_ARG_LOG="$tmp/cli-args-tui-absent.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/tui-absent.err"
test "$(cat "$tmp/home-tui-absent/.horon/builds/current-version")" = "1.2.3"
grep -Fq 'does not publish horon-tui-linux-x86_64.tar.gz' "$tmp/tui-absent.err"
test "$(grep -c '^warning: ' "$tmp/tui-absent.err")" = 1
if grep -q 'asset:horon-tui' "$tmp/cli-args-tui-absent.log"; then
  echo "installer handed an unavailable interactive client to the executable" >&2
  exit 1
fi

# A required archive that fails verification aborts at once, and the client
# transfer running beside it is killed and reaped before the temporary directory
# goes away. A transfer left running would finish after the installer exited and
# write into a path that no longer exists.
tui_transfer_done="$tmp/tui-transfer-done.log"
if PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-tui-killed" \
  HORON_HOME="$tmp/home-tui-killed/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-tui-killed" \
  ASSET_SOURCE_DIR="$assets" \
  CORRUPT_ASSET=horon-agent-linux-x86_64.tar.gz \
  SLOW_ASSET=horon-tui-linux-x86_64.tar.gz \
  SLOW_ASSET_SECONDS=0.4 \
  SLOW_ASSET_DONE_LOG="$tui_transfer_done" \
  CLI_ARG_LOG="$tmp/cli-args-tui-killed.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/tui-killed.err"; then
  echo "installer reported success after the required archive failed" >&2
  exit 1
fi
grep -q 'SHA-256 verification failed for horon-agent-linux-x86_64.tar.gz' "$tmp/tui-killed.err"
test ! -e "$tmp/cli-args-tui-killed.log"
for _attempt in 1 2 3 4 5 6 7 8 9 10; do
  [ ! -e "$tui_transfer_done" ] || break
  sleep 0.1
done
if [ -e "$tui_transfer_done" ]; then
  echo "installer left the client transfer running after the agent archive failed" >&2
  exit 1
fi


# The installer from before this change ignores the extra archive entirely: it
# installs and uninstalls a new-format release without touching the client.
old_installer="$tmp/install-before-tui.sh"
sed '/# >>> horon-tui/,/# <<< horon-tui/d' "$repo_dir/scripts/install/install.sh" > "$old_installer"
if grep -q 'horon-tui' "$old_installer"; then
  echo "the pre-TUI installer fixture still mentions the interactive client" >&2
  exit 1
fi
mkdir -p "$tmp/home-old-installer" "$tmp/install-old-installer"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-old-installer" \
  HORON_HOME="$tmp/home-old-installer/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-old-installer" \
  ASSET_SOURCE_DIR="$assets" \
  CLI_ARG_LOG="$tmp/cli-args-old-installer.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$old_installer" >/dev/null 2>"$tmp/old-installer.err"
test "$(cat "$tmp/home-old-installer/.horon/builds/current-version")" = "1.2.3"
grep -qx -- "asset:horon-agent-linux-x86_64.tar.gz" "$tmp/cli-args-old-installer.log"
if grep -q 'asset:horon-tui' "$tmp/cli-args-old-installer.log"; then
  echo "the pre-TUI installer fetched the interactive client" >&2
  exit 1
fi
# Uninstall still works on that layout, including the version directory the
# interactive client would live in.
mkdir -p "$tmp/home-old-installer/.horon/builds/versions/1.2.3"
printf '#!/bin/sh\n' > "$tmp/home-old-installer/.horon/builds/versions/1.2.3/horon-tui"
printf '#!/bin/sh\n' > "$tmp/install-old-installer/horon-tui"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-old-installer" \
  HORON_HOME="$tmp/home-old-installer/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-old-installer" \
  bash "$repo_dir/scripts/install/uninstall.sh" --yes >/dev/null 2>&1
test ! -e "$tmp/home-old-installer/.horon/builds"
test ! -e "$tmp/install-old-installer/horon-agent"
test ! -e "$tmp/install-old-installer/horon-tui"

if grep -q 'api.github.com' "$url_log"; then
  echo "installer must not depend on the rate-limited unauthenticated GitHub API" >&2
  exit 1
fi

echo "installer conversion tests passed"
