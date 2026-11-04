#!/usr/bin/env bash
# eval_setup_friction.sh - deterministic install / setup / retention friction
# scorecard.
#
# Most first-run friction happens in the installer, in PATH persistence, and in
# whether an upgrade quietly preserves the user's state. This script measures
# that surface deterministically, with no network and no real user data, by
# running the REAL scripts/install/install.sh inside a sandbox with a
# mocked release endpoint, then probing the result with REAL shells.
#
#   Section A  fresh-install PATH resolution - after one `curl | sh`-equivalent
#              install, does `horon-agent` resolve in a brand-new login/interactive
#              shell of every kind we claim to support (bash -l, bash -i,
#              sh -l, fish, zsh)? This is the exact "it wasn't on my PATH"
#              complaint, asked of the real rc files the installer wrote.
#   Section B  idempotency - three installs must leave exactly one PATH line
#              per rc file (no duplicate exports piling up run after run).
#   Section C  retention - an upgrade must preserve ~/.horon-agent config and auth,
#              keep both immutable version binaries (rollback stays possible),
#              and the launcher must serve the new version.
#   Section D  Windows parity - static audit that the Git Bash installer path
#              (install.sh) and the PowerShell installer (install.ps1) both
#              persist the user PATH, dedupe stale entries, and broadcast
#              WM_SETTINGCHANGE. Runtime Windows behavior is covered by
#              scripts/install/test_windows_setup.ps1 in CI; this section
#              stops the two installers drifting apart on POSIX dev machines.
#
# Every case prints PASS/FAIL/SKIP with expected-vs-actual on failure. The
# composite is passed/(passed+failed); SKIPs (shell not installed) don't count
# against the score but are reported. Exits nonzero on any FAIL.
set -u

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
install_sh="$repo_dir/scripts/install/install.sh"
install_ps1="$repo_dir/scripts/install/install.ps1"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

passed=0
failed=0
skipped=0
declare -a failures=()

pass() { passed=$((passed + 1)); printf 'PASS  %s\n' "$1"; }
skip() { skipped=$((skipped + 1)); printf 'SKIP  %s (%s)\n' "$1" "$2"; }
fail() {
  failed=$((failed + 1))
  failures+=("$1")
  printf 'FAIL  %s\n' "$1"
  printf '      expected: %s\n' "$2"
  printf '      actual:   %s\n' "$3"
}

check() { # check <name> <expected-desc> <actual-desc> <condition-exit-status>
  local name="$1" expected="$2" actual="$3" status="$4"
  if [ "$status" -eq 0 ]; then pass "$name"; else fail "$name" "$expected" "$actual"; fi
}

# ---------------------------------------------------------------------------
# Sandbox: mocked release endpoint + tools, identical shape to
# test_install.sh so both exercise the same installer code paths.
# ---------------------------------------------------------------------------
mkdir -p "$work/bin" "$work/assets"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

cat > "$work/bin/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -s) printf '%s\n' "${EVAL_UNAME_S:-Linux}" ;;
  -m) printf '%s\n' "${EVAL_UNAME_M:-x86_64}" ;;
  *) printf '%s\n' "${EVAL_UNAME_S:-Linux}" ;;
esac
EOF

cat > "$work/bin/curl" <<'EOF'
#!/usr/bin/env bash
output=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output="$2"; shift 2 ;;
    -w) shift 2 ;;
    --data) shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  *github.com*/releases/latest)
    printf 'https://github.com/horon-ai/agent/releases/tag/v%s' "${EVAL_VERSION:-1.2.3}"
    ;;
  *github.com*/releases/download/*)
    asset="${url##*/}"
    [ -f "$EVAL_ASSETS/$asset" ] || exit 22
    [ -n "$output" ] || exit 2
    cp "$EVAL_ASSETS/$asset" "$output"
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/uname" "$work/bin/curl"

# The installer installs through the release executable's own `install-release`,
# so this sandbox serves a release asset set (checksum list, agent archive) and
# the agent archive carries a stub that reproduces what that command leaves
# behind: the immutable version directory, both channel markers and the PATH
# launcher.
# build_release_assets <version> <agent-artifact>
build_release_assets() {
  local version="$1" artifact="$2" archive_name stage
  archive_name="${artifact%.exe}"
  stage="$work/assets-stage"
  rm -rf "$stage"
  mkdir -p "$stage"
  cat > "$stage/stub-template" <<'STUB'
#!/usr/bin/env bash
version="__VERSION__"
if [ "${1:-}" = "--version" ]; then printf 'horon-agent %s\n' "$version"; exit 0; fi
if [ "${1:-}" = "install-release" ]; then
  install_dir="${HORON_AGENT_INSTALL_DIR:?}"
  version_dir="$HORON_AGENT_HOME/builds/versions/$version"
  mkdir -p "$version_dir" "$HORON_AGENT_HOME/builds" "$install_dir"
  printf '#!/usr/bin/env bash\nprintf "horon-agent %s\\n"\n' "$version" > "$version_dir/horon-agent"
  chmod +x "$version_dir/horon-agent"
  printf '%s\n' "$version" > "$HORON_AGENT_HOME/builds/stable-version"
  printf '%s\n' "$version" > "$HORON_AGENT_HOME/builds/current-version"
  cp "$version_dir/horon-agent" "$install_dir/horon-agent"
  chmod +x "$install_dir/horon-agent"
  exit 0
fi
exit 0
STUB
  sed "s|__VERSION__|$version|" "$stage/stub-template" > "$stage/$artifact"
  rm -f "$stage/stub-template"
  chmod +x "$stage/$artifact"
  tar -czf "$work/assets/$archive_name.tar.gz" -C "$stage" "$artifact"
  (
    cd "$work/assets" || exit 1
    printf '%s  %s\n' "$(sha256_of "$archive_name.tar.gz")" "$archive_name.tar.gz"
  ) > "$work/assets/SHA256SUMS"
  rm -rf "$stage"
}

build_release_assets "1.2.3" horon-agent-linux-x86_64

# Run the real installer into an isolated HOME. $1 = home dir, $2 = version.
run_install() {
  local home="$1" version="$2"
  mkdir -p "$home"
  build_release_assets "$version" horon-agent-linux-x86_64
  EVAL_VERSION="$version" \
  EVAL_ASSETS="$work/assets" \
  PATH="$work/bin:/usr/bin:/bin" \
  HOME="$home" \
  XDG_CONFIG_HOME="$home/.config" \
  HORON_AGENT_HOME="$home/.horon-agent" \
  HORON_AGENT_INSTALL_DIR="$home/.local/bin" \
  HORON_AGENT_SKIP_SERVER_RELOAD=1 \
  bash "$install_sh" 2>&1
}

# Probe: does `horon-agent` resolve and run in a fresh shell of the given kind, with
# only the sandbox HOME's rc files to set it up? PATH starts minimal (no
# ~/.local/bin) so resolution can only come from what the installer wrote.
probe_shell() { # probe_shell <home> <shell-cmd...>
  local home="$1"; shift
  HOME="$home" \
  XDG_CONFIG_HOME="$home/.config" \
  ENV="$home/.profile" \
  PATH="/usr/bin:/bin" \
  "$@" 'command -v horon-agent >/dev/null 2>&1 && horon-agent --version' 2>/dev/null </dev/null
}

echo "================ SETUP FRICTION SCORECARD ================"

# ---------------------------------------------------------------------------
# Section A: fresh install, then PATH resolution in real new shells.
# ---------------------------------------------------------------------------
echo ""
echo "-- Section A: fresh-install PATH resolution (real shells) --"
home_a="$work/home-a"
install_out=$(run_install "$home_a" "1.2.3")
install_status=$?
check "installer completes on a fresh home" \
  "exit 0" "exit $install_status" "$install_status"

launcher="$home_a/.local/bin/horon-agent"
[ -x "$launcher" ]; check "launcher exists and is executable" \
  "executable at ~/.local/bin/horon-agent" "missing or not executable: $launcher" "$?"

ver=$("$launcher" --version 2>/dev/null || true)
[ "$ver" = "horon-agent 1.2.3" ]; check "launcher runs and reports the installed version" \
  "horon-agent 1.2.3" "${ver:-<no output>}" "$?"

# The success message must not dead-end the user: either horon-agent is already
# resolvable or the copy explicitly says future shells will have it.
printf '%s' "$install_out" | grep -q "Run 'horon-agent' to get started\|Future terminal sessions will have horon-agent on PATH automatically"
check "install output gives a working next step (no dead end)" \
  "a 'run horon-agent' or 'future sessions' line" "neither line found in installer output" "$?"

probe_case() { # probe_case <label> <binary> <shell-cmd...>
  local label="$1" binary="$2"; shift 2
  if ! command -v "$binary" >/dev/null 2>&1; then
    skip "$label" "$binary not installed on this machine"
    return
  fi
  local out
  out=$(probe_shell "$home_a" "$@")
  [ "$out" = "horon-agent 1.2.3" ]
  check "$label" "horon-agent resolves and prints 'horon-agent 1.2.3'" "${out:-<not found on PATH>}" "$?"
}

probe_case "bash login shell (bash -lc) finds horon-agent"        bash bash -lc
probe_case "bash interactive shell (bash -ic) finds horon-agent"  bash bash -ic
probe_case "sh login shell (sh -lc) finds horon-agent"            sh   sh -lc
probe_case "fish shell (fish -c) finds horon-agent"               fish fish -c
probe_case "zsh login shell (zsh -lc) finds horon-agent"          zsh  zsh -lc

# ---------------------------------------------------------------------------
# Section B: idempotency - reinstalling must not stack PATH lines.
# ---------------------------------------------------------------------------
echo ""
echo "-- Section B: idempotency (3x install) --"
run_install "$home_a" "1.2.3" >/dev/null 2>&1
run_install "$home_a" "1.2.3" >/dev/null 2>&1

for rc in .bashrc .profile .zshenv .config/fish/config.fish; do
  file="$home_a/$rc"
  [ -f "$file" ] || continue
  # Each install appends one "# Added by horon-agent installer" stanza when missing;
  # a correct idempotency guard leaves exactly one after any number of runs.
  count=$(grep -cF "# Added by horon-agent installer" "$file" || true)
  [ "$count" -le 1 ]
  check "~/$rc has at most one horon-agent PATH stanza after 3 installs" \
    "<= 1 installer stanza" "$count installer stanzas" "$?"
done

# ---------------------------------------------------------------------------
# Section C: retention - upgrade preserves user state and rollback stays
# possible.
# ---------------------------------------------------------------------------
echo ""
echo "-- Section C: retention across upgrade --"
home_c="$work/home-c"
run_install "$home_c" "1.2.3" >/dev/null 2>&1

# Simulate accumulated user state between installs.
mkdir -p "$home_c/.horon-agent"
printf 'model = "kept"\n' > "$home_c/.horon-agent/config.toml"
printf '{"kept":true}\n' > "$home_c/.horon-agent/auth.json"

run_install "$home_c" "1.3.0" >/dev/null 2>&1

ver=$("$home_c/.local/bin/horon-agent" --version 2>/dev/null || true)
[ "$ver" = "horon-agent 1.3.0" ]; check "upgrade switches the launcher to the new version" \
  "horon-agent 1.3.0" "${ver:-<no output>}" "$?"

[ "$(cat "$home_c/.horon-agent/config.toml" 2>/dev/null)" = 'model = "kept"' ]
check "upgrade preserves ~/.horon-agent/config.toml" \
  "file unchanged" "missing or modified" "$?"

[ "$(cat "$home_c/.horon-agent/auth.json" 2>/dev/null)" = '{"kept":true}' ]
check "upgrade preserves ~/.horon-agent/auth.json" \
  "file unchanged" "missing or modified" "$?"

[ -x "$home_c/.horon-agent/builds/versions/1.2.3/horon-agent" ] && [ -x "$home_c/.horon-agent/builds/versions/1.3.0/horon-agent" ]
check "both immutable version binaries kept (rollback possible)" \
  "versions/1.2.3 and versions/1.3.0 both executable" \
  "$(ls "$home_c/.horon-agent/builds/versions" 2>/dev/null | tr '\n' ' ')" "$?"

stable_ver=$(cat "$home_c/.horon-agent/builds/stable-version" 2>/dev/null || true)
[ "$stable_ver" = "1.3.0" ]; check "stable channel marker points at the new version" \
  "1.3.0" "${stable_ver:-<missing>}" "$?"

# A second post-upgrade login shell must still resolve horon-agent (PATH survives
# upgrades, not just fresh installs).
out=$(probe_shell "$home_c" bash -lc)
[ "$out" = "horon-agent 1.3.0" ]; check "post-upgrade login shell still finds horon-agent" \
  "horon-agent 1.3.0" "${out:-<not found on PATH>}" "$?"

# Uninstall (default, no --purge) must remove binaries but KEEP user data, so
# a returning user's reinstall lands on their old config/auth. This is the
# retention contract of leaving: coming back is cheap.
uninstall_sh="$repo_dir/scripts/install/uninstall.sh"
# uninstall.sh pkills running horon-agent servers; neuter that inside the sandbox so
# the eval never touches real processes on the machine running it.
printf '#!/usr/bin/env bash\nexit 0\n' > "$work/bin/pkill"
chmod +x "$work/bin/pkill"
PATH="$work/bin:/usr/bin:/bin" \
HOME="$home_c" \
bash "$uninstall_sh" --yes >/dev/null 2>&1
uninstall_status=$?
check "uninstall (no --purge) completes" "exit 0" "exit $uninstall_status" "$uninstall_status"

[ ! -e "$home_c/.local/bin/horon-agent" ] && [ ! -d "$home_c/.horon-agent/builds" ]
check "uninstall removes launcher and build channels" \
  "launcher and builds gone" "still present" "$?"

[ "$(cat "$home_c/.horon-agent/config.toml" 2>/dev/null)" = 'model = "kept"' ] \
  && [ "$(cat "$home_c/.horon-agent/auth.json" 2>/dev/null)" = '{"kept":true}' ]
check "uninstall keeps config/auth for a cheap return" \
  "config.toml and auth.json intact" "user data lost" "$?"

# ---------------------------------------------------------------------------
# Section D: Windows parity (static audit of both installers).
# ---------------------------------------------------------------------------
echo ""
echo "-- Section D: Windows PATH parity (static audit) --"
sh_text=$(cat "$install_sh")
ps1_text=$(cat "$install_ps1")

printf '%s' "$sh_text" | grep -q 'SetEnvironmentVariable.*"User"'
check "install.sh (Git Bash) persists the Windows user PATH" \
  "SetEnvironmentVariable(...,'User') present" "no user-PATH persistence found" "$?"

printf '%s' "$sh_text" | grep -q 'SendMessageTimeout(\[IntPtr\]0xffff' || printf '%s' "$sh_text" | grep -q '0x001A'
check "install.sh broadcasts WM_SETTINGCHANGE after PATH change" \
  "0x001A broadcast present" "no WM_SETTINGCHANGE broadcast" "$?"

printf '%s' "$sh_text" | grep -q '_win_path_key'
check "install.sh dedupes stale Windows PATH entries" \
  "case/slash-insensitive dedupe helper present" "no dedupe logic" "$?"

# The WM_SETTINGCHANGE broadcast is PowerShell source embedded in bash. Parse
# that exact block with a REAL PowerShell parser so a quoting/escaping bug
# (bash-escaped quotes are not PowerShell-escaped quotes) fails here instead
# of silently no-oping on end-user machines.
if command -v pwsh >/dev/null 2>&1; then
  broadcast_block=$(awk "/<<'HORON_AGENT_PS_BROADCAST_EOF'/{inblock=1; next} /^HORON_AGENT_PS_BROADCAST_EOF\$/{inblock=0} inblock" "$install_sh")
  printf '%s' "$broadcast_block" > "$work/broadcast.ps1"
  parse_errors=$(HORON_AGENT_EVAL_PS_FILE="$work/broadcast.ps1" pwsh -NoProfile -NonInteractive -Command '
    $errors = $null
    [System.Management.Automation.Language.Parser]::ParseFile($env:HORON_AGENT_EVAL_PS_FILE, [ref]$null, [ref]$errors) | Out-Null
    $errors.Count
  ' 2>/dev/null | tr -d '[:space:]')
  [ -n "$broadcast_block" ] && [ "$parse_errors" = "0" ]
  check "embedded WM_SETTINGCHANGE PowerShell parses cleanly (real pwsh)" \
    "0 parse errors" "${parse_errors:-<block not found>} parse errors" "$?"
else
  skip "embedded WM_SETTINGCHANGE PowerShell parses cleanly (real pwsh)" "pwsh not installed"
fi

# Runtime: drive the REAL install.sh Windows (Git Bash) branch with a mocked
# powershell.exe and confirm it PERSISTS the user PATH (deduping a stale
# uppercase duplicate) and broadcasts WM_SETTINGCHANGE. This is the regression
# that shipped: the Git Bash installer printed manual PATH instructions but
# never wrote the user PATH itself.
home_w="$work/home-w"
mkdir -p "$home_w" "$work/localappdata"
win_state="$work/win-env"
mkdir -p "$win_state"
# Seed a user PATH that already contains a stale (case/slash variant) entry.
printf 'C:\\Tools;%s\\' "$(printf '%s' "$work/localappdata/horon-agent/bin" | tr '[:lower:]' '[:upper:]')" > "$win_state/user-path"
cat > "$work/bin/powershell.exe" <<'EOF'
#!/usr/bin/env bash
# Minimal mock of the two PowerShell invocations install.sh makes on Windows:
#   1. read the user PATH   -> print the stored value
#   2. write the user PATH  -> persist $HORON_AGENT_NEW_USER_PATH
#   3. broadcast            -> record that a broadcast happened
args="$*"
case "$args" in
  *GetEnvironmentVariable*) cat "$EVAL_WIN_STATE/user-path" ;;
  *SetEnvironmentVariable*) printf '%s' "$HORON_AGENT_NEW_USER_PATH" > "$EVAL_WIN_STATE/user-path" ;;
  *SendMessageTimeout*) touch "$EVAL_WIN_STATE/broadcasted" ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$work/bin/powershell.exe"

build_release_assets "1.2.3" horon-agent-windows-x86_64.exe

EVAL_VERSION="1.2.3" \
EVAL_ASSETS="$work/assets" \
EVAL_UNAME_S="MINGW64_NT-10.0" \
EVAL_WIN_STATE="$win_state" \
PATH="$work/bin:/usr/bin:/bin" \
HOME="$home_w" \
LOCALAPPDATA="$work/localappdata" \
HORON_AGENT_HOME="$home_w/.horon-agent" \
HORON_AGENT_INSTALL_DIR="$work/localappdata/horon-agent/bin" \
HORON_AGENT_SKIP_SERVER_RELOAD=1 \
bash "$install_sh" >/dev/null 2>&1
win_install_status=$?
check "Git Bash Windows install completes" "exit 0" "exit $win_install_status" "$win_install_status"

win_path_now=$(cat "$win_state/user-path" 2>/dev/null || true)
case "$win_path_now" in
  "$work/localappdata/horon-agent/bin;C:\\Tools") win_ok=0 ;;
  *) win_ok=1 ;;
esac
check "Git Bash install persists + dedupes the Windows user PATH" \
  "$work/localappdata/horon-agent/bin;C:\\Tools" "${win_path_now:-<unset>}" "$win_ok"

[ -f "$win_state/broadcasted" ]
check "Git Bash install broadcasts WM_SETTINGCHANGE" \
  "broadcast recorded" "no broadcast" "$?"

printf '%s' "$ps1_text" | grep -q 'Resolve-HoronAgentPathUpdate'
check "install.ps1 persists + dedupes the user PATH" \
  "Resolve-HoronAgentPathUpdate present" "helper missing" "$?"

printf '%s' "$ps1_text" | grep -q 'SendMessageTimeout(\[IntPtr\]0xffff'
check "install.ps1 broadcasts WM_SETTINGCHANGE to HWND_BROADCAST" \
  "SendMessageTimeout(HWND_BROADCAST, ...) present" "broadcast missing" "$?"

# Both installers must maintain the same channel layout so retention behaves
# identically across platforms. The `stable-version` marker file is the shared
# contract written by both.
printf '%s' "$sh_text" | grep -q 'install-release' && printf '%s' "$ps1_text" | grep -q 'install-release'
check "both installers maintain the stable/versions build channels" \
  "both delegate channel activation to install-release" "delegation missing from one installer" "$?"

# ---------------------------------------------------------------------------
# Scorecard.
# ---------------------------------------------------------------------------
echo ""
echo "-- SCORE --"
total=$((passed + failed))
if [ "$total" -gt 0 ]; then
  composite=$((passed * 100 / total))
else
  composite=0
fi
echo "cases passed  : $passed"
echo "cases failed  : $failed"
echo "cases skipped : $skipped (shell not installed; not scored)"
echo "COMPOSITE     : $composite / 100"
echo "=========================================================="

if [ "$failed" -gt 0 ]; then
  echo ""
  echo "failures:"
  for f in "${failures[@]}"; do echo "  - $f"; done
  exit 1
fi
