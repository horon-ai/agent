#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fixture="$tmp/repo"; fake_bin="$tmp/bin"
mkdir -p "$fixture/scripts/install/lib" "$fake_bin"
cp "$repo_root/scripts/install/install_from_source.sh" "$fixture/scripts/install/"
printf '%s\n' 'horon_agent_configure_path() { :; }' > "$fixture/scripts/install/lib/configure_path.sh"
cat > "$fixture/Cargo.toml" <<'EOF'
[package]
name = "horon-agent"
version = "0.98.1"

[workspace]
members = []
EOF

cat > "$fake_bin/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -s) printf '%s\n' "${TEST_UNAME_S:-Darwin}" ;;
  -m) printf '%s\n' "${TEST_UNAME_M:-arm64}" ;;
  *) printf '%s\n' "${TEST_UNAME_S:-Darwin}" ;;
esac
EOF
cat > "$fake_bin/git" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *"rev-parse --show-toplevel"*) printf '%s\n' "$TEST_REPO" ;;
  *"rev-parse --git-dir"*) printf '%s\n' .git ;;
  *"rev-parse --short HEAD"*) printf '%s\n' abc123def ;;
  *"log -1 --format=%ci"*) printf '%s\n' '2026-08-09 20:00:00 +0000' ;;
  *"status --porcelain"*) [[ "${TEST_DIRTY:-0}" == 1 ]] && printf '%s\n' ' M file' ;;
  *) exit 1 ;;
esac
EOF
cat > "$fake_bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p "$TEST_REPO/target/release"
# install_from_source.sh builds the CLI and then the interactive client as
# separate cargo invocations. Only the CLI build carries the embedded git
# metadata, so only that invocation asserts it and writes the version-reporting
# stub; the client build just has to produce its own binary.
if [[ " $* " == *" -p horon-tui "* ]]; then
  printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$TEST_REPO/target/release/horon-tui"
  chmod +x "$TEST_REPO/target/release/horon-tui"
  exit 0
fi
test "${HORON_AGENT_BUILD_GIT_HASH:-}" = abc123def
test "${HORON_AGENT_BUILD_GIT_DATE:-}" = '2026-08-09 20:00:00 +0000'
test "${HORON_AGENT_BUILD_GIT_DIRTY:-}" = "${TEST_DIRTY:-0}"
cat > "$TEST_REPO/target/release/horon-agent" <<BIN
#!/usr/bin/env bash
if [[ "\${1:-}" == --version ]]; then
  identity="\${TEST_BINARY_HASH:-$HORON_AGENT_BUILD_GIT_HASH}"
  if [[ "${TEST_DIRTY:-0}" == 1 ]]; then identity="\$identity, dirty"; fi
  printf '%s\\n' "version\tv0.98.1 (\$identity)"
  exit 0
fi
if [[ "\${1:-}" == install-release ]]; then
  printf '%s\\n' "\$*" >> "\$CLI_ARG_LOG"
  [[ "\${CLI_FAIL:-0}" != 1 ]] || exit 1
  install_dir="\${HORON_AGENT_INSTALL_DIR:?}"
  mkdir -p "\$install_dir"
  cp "\$0" "\$install_dir/horon-agent"
  chmod +x "\$install_dir/horon-agent"
  printf '%s\\n' "installed complete release at \$install_dir/horon-agent"
  exit 0
fi
exit 0
BIN
chmod +x "$TEST_REPO/target/release/horon-agent"
# The installer copies the interactive client beside the installed binary, so
# the fixture build has to produce that sibling too.
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$TEST_REPO/target/release/horon-tui"
chmod +x "$TEST_REPO/target/release/horon-tui"
EOF
chmod +x "$fake_bin/uname" "$fake_bin/git" "$fake_bin/cargo"

assets="$tmp/assets"
mkdir -p "$assets"
printf '%s  %s\n' \
  '0000000000000000000000000000000000000000000000000000000000000000' 'horon-agent-macos-aarch64.tar.gz' \
  > "$assets/SHA256SUMS"

run_install() {
  HOME="$tmp/home" PATH="$fake_bin:/usr/bin:/bin" TEST_REPO="$fixture" HORON_AGENT_RELEASE_PROFILE=release \
    HORON_AGENT_INSTALL_DIR="$tmp/launcher" HORON_AGENT_SKIP_SERVER_RELOAD=1 \
    HORON_AGENT_RELEASE_ASSETS="$assets" CLI_ARG_LOG="$tmp/cli-args.log" \
    TEST_DIRTY="${TEST_DIRTY:-0}" TEST_BINARY_HASH="${TEST_BINARY_HASH:-}" \
    CLI_FAIL="${CLI_FAIL:-0}" \
    "$fixture/scripts/install/install_from_source.sh"
}

TEST_DIRTY=1 run_install >/dev/null
grep -Fq -- "install-release --assets $assets --agent-version 0.98.1 --source-binary $fixture/target/release/horon-agent" \
  "$tmp/cli-args.log"
test -x "$tmp/launcher/horon-agent"
# The CLI hands a terminal launch to the sibling client, so a complete install
# puts it beside the installed binary.
test -x "$tmp/launcher/horon-tui"

if TEST_BINARY_HASH=stale999 TEST_DIRTY=0 run_install >"$tmp/out" 2>"$tmp/err"; then
  echo "installer accepted a binary with stale metadata" >&2
  exit 1
fi
grep -Fq 'does not report expected git identity: (abc123def)' "$tmp/err"

# A local install without the release checksum list is refused instead of
# installing a binary with no verifiable release.
rm -rf "${tmp:?}/home" "${tmp:?}/launcher"
if HOME="$tmp/home" PATH="$fake_bin:/usr/bin:/bin" TEST_REPO="$fixture" HORON_AGENT_RELEASE_PROFILE=release \
  HORON_AGENT_INSTALL_DIR="$tmp/launcher" HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  HORON_AGENT_RELEASE_ASSETS="" CLI_ARG_LOG="$tmp/cli-args-none.log" \
  "$fixture/scripts/install/install_from_source.sh" >/dev/null 2>"$tmp/no-assets.err"; then
  echo "installer accepted a local build without release assets" >&2
  exit 1
fi
grep -Fq 'needs the release assets' "$tmp/no-assets.err"
test ! -e "$tmp/launcher/horon-agent"
test ! -e "$tmp/launcher/horon-tui"

# An assets directory without SHA256SUMS is refused before the build is installed.
incomplete="$tmp/assets-incomplete"
mkdir -p "$incomplete"
if HOME="$tmp/home" PATH="$fake_bin:/usr/bin:/bin" TEST_REPO="$fixture" HORON_AGENT_RELEASE_PROFILE=release \
  HORON_AGENT_INSTALL_DIR="$tmp/launcher" HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  HORON_AGENT_RELEASE_ASSETS="$incomplete" CLI_ARG_LOG="$tmp/cli-args-incomplete.log" \
  "$fixture/scripts/install/install_from_source.sh" >/dev/null 2>"$tmp/incomplete.err"; then
  echo "installer accepted assets without SHA256SUMS" >&2
  exit 1
fi
grep -Fq 'missing SHA256SUMS' "$tmp/incomplete.err"
test ! -e "$tmp/cli-args-incomplete.log"

# A failed release install leaves the previous launcher alone.
mkdir -p "$tmp/launcher"
printf '%s\n' 'prior launcher' > "$tmp/launcher/horon-agent"
if TEST_DIRTY=0 CLI_FAIL=1 run_install >/dev/null 2>"$tmp/failed.err"; then
  echo "installer reported success after the release install failed" >&2
  exit 1
fi
test "$(cat "$tmp/launcher/horon-agent")" = 'prior launcher'
# The client is only copied after a release install succeeds, so a failed
# install must not leave one behind.
test ! -e "$tmp/launcher/horon-tui"

# FreeBSD x64 is a supported host.
freebsd_assets="$tmp/assets-freebsd"
mkdir -p "$freebsd_assets"
printf '%s  %s\n' \
  '0000000000000000000000000000000000000000000000000000000000000000' 'horon-agent-freebsd-x86_64.tar.gz' \
  > "$freebsd_assets/SHA256SUMS"
HOME="$tmp/home-freebsd" PATH="$fake_bin:/usr/bin:/bin" TEST_REPO="$fixture" HORON_AGENT_RELEASE_PROFILE=release \
  HORON_AGENT_INSTALL_DIR="$tmp/launcher-freebsd" HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  HORON_AGENT_RELEASE_ASSETS="$freebsd_assets" CLI_ARG_LOG="$tmp/cli-args-freebsd.log" \
  TEST_UNAME_S=FreeBSD TEST_UNAME_M=amd64 \
  "$fixture/scripts/install/install_from_source.sh" >/dev/null
grep -Fq -- "--assets $freebsd_assets" "$tmp/cli-args-freebsd.log"

echo "install_from_source metadata regression test passed"
