#!/usr/bin/env bash
# Reclaim the disk a hosted Linux runner spends on toolchains this repository
# never uses, without making the job wait for the deletion to finish.
#
# The five paths below add up to a lot of files, mostly in
# /usr/local/lib/android. Deleting them inline took 1.0 to 2.2 minutes per job in
# runs 42373575639 and 42371014571, and every one of those jobs then started its
# build late. Renaming a path into a trash directory on the same filesystem is
# O(1), so the step returns within seconds; one `rm -rf` of that directory runs
# in the background while checkout and toolchain installation proceed. A job
# that measures wall-clock time (Benchmark) waits for it first with `--wait` so
# deletion IO never overlaps a measurement.
#
# Usage:
#   bash scripts/ci/free_disk_space.sh          # rename, start the background rm
#   bash scripts/ci/free_disk_space.sh --wait   # block until that rm has ended
#
# Environment:
#   FREE_DISK_ROOT  prefix for both the targets and the trash directory, so the
#                   script can be exercised in a temporary directory
#   FREE_DISK_SUDO  privilege command, default `sudo`; an empty value runs
#                   without it
#   RUNNER_TEMP     directory for the background process id
set -euo pipefail

TARGETS=(
  /usr/share/dotnet
  /usr/local/lib/android
  /opt/ghc
  /opt/hostedtoolcache/CodeQL
  /usr/local/share/boost
)
ROOT="${FREE_DISK_ROOT:-/}"
ROOT="${ROOT%/}"
SUDO="${FREE_DISK_SUDO-sudo}"
STATE_DIR="${RUNNER_TEMP:-/tmp}"
PID_FILE="$STATE_DIR/free-disk-space.pid"
WAIT_LIMIT=600

run_root() {
  if [[ -n "$SUDO" ]]; then
    "$SUDO" "$@"
  else
    "$@"
  fi
}

wait_for_deletion() {
  if [[ ! -f "$PID_FILE" ]]; then
    return 0
  fi
  local pid deadline
  pid="$(cat "$PID_FILE")"
  if [[ -z "$pid" ]]; then
    rm -f "$PID_FILE"
    return 0
  fi
  deadline=$((SECONDS + WAIT_LIMIT))
  while kill -0 "$pid" 2>/dev/null; do
    if (( SECONDS >= deadline )); then
      echo "free_disk_space: deletion pid $pid is still running after ${WAIT_LIMIT}s" >&2
      exit 1
    fi
    sleep 1
  done
  rm -f "$PID_FILE"
  df -h /
}

MODE="${1:-}"
case "$MODE" in
  --wait)
    wait_for_deletion
    exit 0
    ;;
  "")
    ;;
  *)
    echo "free_disk_space: unknown argument '$MODE' (only --wait is supported)" >&2
    exit 2
    ;;
esac

# A stale pid from an earlier invocation must not make a later `--wait` block on
# an unrelated process.
rm -f "$PID_FILE"

trash="$ROOT/.free-disk-trash-$$"
created_trash=0
moved=0
for target in "${TARGETS[@]}"; do
  path="$ROOT$target"
  [[ -e "$path" ]] || continue
  if (( created_trash == 0 )); then
    run_root mkdir -p -- "$trash"
    created_trash=1
  fi
  if run_root mv -- "$path" "$trash/"; then
    moved=1
  else
    # The rename failed, which means this path is on another filesystem. Delete
    # it synchronously rather than leave its space unreclaimed.
    run_root rm -rf -- "$path"
  fi
done

if (( moved == 1 )); then
  # `setsid` moves the deletion into its own session so the runner's per-step
  # process cleanup cannot cancel it. The recorded pid is the process this
  # script spawned (setsid with `--wait`, or `rm` itself), which lives until the
  # deletion ends, so `--wait` waits for the real work and not for a launcher.
  detach=(nohup)
  if command -v setsid >/dev/null 2>&1 && setsid --help 2>&1 | grep -q -- '--wait'; then
    detach=(nohup setsid --wait)
  fi
  if [[ -n "$SUDO" ]]; then
    "${detach[@]}" "$SUDO" rm -rf -- "$trash" </dev/null >/dev/null 2>&1 &
  else
    "${detach[@]}" rm -rf -- "$trash" </dev/null >/dev/null 2>&1 &
  fi
  echo "$!" > "$PID_FILE"
elif (( created_trash == 1 )); then
  run_root rmdir -- "$trash" 2>/dev/null || true
fi

df -h /
