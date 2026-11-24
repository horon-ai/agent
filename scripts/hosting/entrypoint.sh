#!/bin/sh
# Entrypoint for the hosted per-tenant image.
#
# Validates the hosting variables before the daemon can bind a socket, then
# execs it in the foreground so the container runtime's stop signal reaches the
# daemon directly and its session drain runs. The checks mirror the daemon's own
# startup boundary; this layer exists so a missing tenant id or a volume mounted
# in the wrong place fails in seconds with a sentence instead of a startup loop.
set -eu

fail() {
    printf 'horon hosting entrypoint: %s\n' "$*" >&2
    exit 64
}

mode=${HORON_AGENT_HOSTING_MODE:-}
tenant=${HORON_AGENT_TENANT_ID:-}
home=${HORON_HOME:-}
runtime=${HORON_AGENT_RUNTIME_DIR:-}

if [ "$mode" != "per-user-container" ]; then
    fail "HORON_AGENT_HOSTING_MODE must be per-user-container for this image, got '${mode}'"
fi

if [ -z "$tenant" ]; then
    fail "HORON_AGENT_TENANT_ID is required"
fi

case "$tenant" in
    *[!a-z0-9-]* | -* | *-)
        fail "HORON_AGENT_TENANT_ID '${tenant}' must use lowercase letters, digits and inner hyphens"
        ;;
esac
if [ "${#tenant}" -gt 63 ]; then
    fail "HORON_AGENT_TENANT_ID '${tenant}' is longer than 63 bytes"
fi

case "$home" in
    /*) ;;
    *) fail "HORON_HOME must be an absolute path, got '${home}'" ;;
esac
case "$home" in
    /data/*) ;;
    *) fail "HORON_HOME must live on the /data volume, got '${home}'" ;;
esac

case "$runtime" in
    /*) ;;
    *) fail "HORON_AGENT_RUNTIME_DIR must be an absolute path, got '${runtime}'" ;;
esac
case "$runtime" in
    "$home" | "$home"/*) fail "HORON_AGENT_RUNTIME_DIR must stay outside HORON_HOME" ;;
esac

mkdir -p "$home" "$runtime"
chmod 0700 "$home" "$runtime"

# The gateway binds an address outside the container's socket boundary. The
# environment override wins over any [gateway] section a tenant wrote.
HORON_AGENT_GATEWAY_ENABLED=0
export HORON_AGENT_GATEWAY_ENABLED

if [ "$#" -eq 0 ]; then
    set -- serve
fi
exec horon-agent --no-update "$@"
