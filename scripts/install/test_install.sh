#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/home" "$tmp/install"

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
    if [ "${CORRUPT_ASSET:-}" = "$name" ]; then
      printf 'corrupted bytes' > "$output"
    else
      cp "$ASSET_SOURCE_DIR/$name" "$output"
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
  tar -czf "$dest/$archive_name.tar.gz" -C "$dest/stage" "$agent_artifact"
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
  if [ "$layout" = compat ]; then
    printf '%s\n' 'tui payload' > "$dest/tui-stage/$tui_artifact.bin"
    tar -czf "$dest/$archive.tar.gz" -C "$dest/tui-stage" "$tui_artifact" "$tui_artifact.bin"
  else
    tar -czf "$dest/$archive.tar.gz" -C "$dest/tui-stage" "$tui_artifact"
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
  DESKTOP_SETUP_LOG="$desktop_setup_log" \
  DOWNLOAD_URL_LOG="$url_log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >"$tmp/full.out" 2>"$tmp/full.err"

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

# A release that predates the interactive client installs the headless agent
# after exactly one warning, and never hands the missing archive to the
# executable.
legacy_release="$tmp/assets-no-tui"
build_assets "$legacy_release" horon-agent-linux-x86_64
mkdir -p "$tmp/home-no-tui" "$tmp/install-no-tui"
PATH="$tmp/bin:$PATH" \
  HOME="$tmp/home-no-tui" \
  HORON_HOME="$tmp/home-no-tui/.horon" \
  HORON_AGENT_INSTALL_DIR="$tmp/install-no-tui" \
  ASSET_SOURCE_DIR="$legacy_release" \
  CLI_ARG_LOG="$tmp/cli-args-no-tui.log" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$repo_dir/scripts/install/install.sh" >/dev/null 2>"$tmp/no-tui.err"
test "$(cat "$tmp/home-no-tui/.horon/builds/current-version")" = "1.2.3"
grep -Fq 'does not publish horon-tui-linux-x86_64.tar.gz' "$tmp/no-tui.err"
test "$(grep -c '^warning: ' "$tmp/no-tui.err")" = 1
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
