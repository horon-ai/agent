#!/usr/bin/env bash
# Shared loader for HoronAgent remote build defaults.
#
# The config file is intentionally a shell fragment so users can write either:
#   HORON_AGENT_REMOTE_HOST=builder
# or:
#   export HORON_AGENT_REMOTE_HOST=builder
#
# Explicit environment variables take precedence over values loaded from the
# config file. This lets callers temporarily disable remote builds with, for
# example, `HORON_AGENT_REMOTE_CARGO=0 scripts/dev/cargo.sh check`.

horon_agent_remote_config_path() {
  if [[ -n "${HORON_AGENT_REMOTE_CONFIG:-}" ]]; then
    printf '%s\n' "$HORON_AGENT_REMOTE_CONFIG"
  elif [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
    printf '%s\n' "$XDG_CONFIG_HOME/horon-agent/remote-build.env"
  elif [[ -n "${HOME:-}" ]]; then
    printf '%s\n' "$HOME/.config/horon-agent/remote-build.env"
  fi
}

# SSH connect timeout shared by the remote build entry points.
horon_agent_remote_connect_timeout() {
  local value="${HORON_AGENT_REMOTE_CONNECT_TIMEOUT:-5}"
  if [[ ! "$value" =~ ^[0-9]+$ || "$value" -lt 1 ]]; then
    value=5
  fi
  printf '%s\n' "$value"
}

horon_agent_load_remote_config() {
  local config_file
  config_file="$(horon_agent_remote_config_path)"
  [[ -n "$config_file" && -f "$config_file" ]] || return 0

  local had_remote_cargo=0 remote_cargo=""
  local had_remote_host=0 remote_host=""
  local had_remote_dir=0 remote_dir=""
  local had_remote_ssh_bin=0 remote_ssh_bin=""
  local had_remote_rsync_bin=0 remote_rsync_bin=""

  if [[ ${HORON_AGENT_REMOTE_CARGO+x} ]]; then
    had_remote_cargo=1
    remote_cargo="$HORON_AGENT_REMOTE_CARGO"
  fi
  if [[ ${HORON_AGENT_REMOTE_HOST+x} ]]; then
    had_remote_host=1
    remote_host="$HORON_AGENT_REMOTE_HOST"
  fi
  if [[ ${HORON_AGENT_REMOTE_DIR+x} ]]; then
    had_remote_dir=1
    remote_dir="$HORON_AGENT_REMOTE_DIR"
  fi
  if [[ ${HORON_AGENT_REMOTE_SSH_BIN+x} ]]; then
    had_remote_ssh_bin=1
    remote_ssh_bin="$HORON_AGENT_REMOTE_SSH_BIN"
  fi
  if [[ ${HORON_AGENT_REMOTE_RSYNC_BIN+x} ]]; then
    had_remote_rsync_bin=1
    remote_rsync_bin="$HORON_AGENT_REMOTE_RSYNC_BIN"
  fi

  # shellcheck source=/dev/null
  source "$config_file"

  if [[ "$had_remote_cargo" -eq 1 ]]; then
    HORON_AGENT_REMOTE_CARGO="$remote_cargo"
  fi
  if [[ "$had_remote_host" -eq 1 ]]; then
    HORON_AGENT_REMOTE_HOST="$remote_host"
  fi
  if [[ "$had_remote_dir" -eq 1 ]]; then
    HORON_AGENT_REMOTE_DIR="$remote_dir"
  fi
  if [[ "$had_remote_ssh_bin" -eq 1 ]]; then
    HORON_AGENT_REMOTE_SSH_BIN="$remote_ssh_bin"
  fi
  if [[ "$had_remote_rsync_bin" -eq 1 ]]; then
    HORON_AGENT_REMOTE_RSYNC_BIN="$remote_rsync_bin"
  fi
}
