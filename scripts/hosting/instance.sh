#!/usr/bin/env bash
# Reference lifecycle for one hosted tenant.
#
# The control plane owns the user -> instance -> volume mapping; this script is
# the smallest honest implementation of the verbs it calls, for a local Docker
# or Podman runtime or a plain supervised process. It never touches a tenant
# other than the one named, and it refuses a tenant directory that is a symlink
# or that resolves outside the volume root.
#
#   scripts/hosting/instance.sh create <tenant>
#   scripts/hosting/instance.sh start  <tenant>      # foreground; the supervisor runs this
#   scripts/hosting/instance.sh stop   <tenant>
#   scripts/hosting/instance.sh delete <tenant>
#   scripts/hosting/instance.sh status <tenant>
#
# Options: --driver process|container, --root DIR, --runtime-dir DIR,
#          --image IMAGE, --binary PATH. Each also has HORON_HOSTING_* default.
set -euo pipefail

ROOT=${HORON_HOSTING_ROOT:-/data}
RUNTIME_BASE=${HORON_HOSTING_RUNTIME_DIR:-/run/horon}
DRIVER=${HORON_HOSTING_DRIVER:-process}
IMAGE=${HORON_HOSTING_IMAGE:-ghcr.io/horon-ai/agent-hosting:local}
BINARY=${HORON_AGENT_BIN:-horon-agent}

usage() {
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
}

die() {
    printf 'instance: %s\n' "$*" >&2
    exit 1
}

[ "$#" -ge 1 ] || {
    usage >&2
    exit 2
}
verb=$1
shift
[ "$#" -ge 1 ] || {
    usage >&2
    exit 2
}
tenant=$1
shift

while [ "$#" -gt 0 ]; do
    case "$1" in
        --driver)
            DRIVER=${2:?--driver needs a value}
            shift 2
            ;;
        --root)
            ROOT=${2:?--root needs a value}
            shift 2
            ;;
        --runtime-dir)
            RUNTIME_BASE=${2:?--runtime-dir needs a value}
            shift 2
            ;;
        --image)
            IMAGE=${2:?--image needs a value}
            shift 2
            ;;
        --binary)
            BINARY=${2:?--binary needs a value}
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

case "$tenant" in
    "" | -* | *-)
        die "invalid tenant id '${tenant}': lowercase letters, digits and inner hyphens only"
        ;;
    *[!a-z0-9-]*)
        die "invalid tenant id '${tenant}': lowercase letters, digits and inner hyphens only"
        ;;
esac
[ "${#tenant}" -le 63 ] || die "invalid tenant id '${tenant}': longer than 63 bytes"

case "$ROOT" in
    /*) ;;
    *) die "--root must be an absolute path, got '${ROOT}'" ;;
esac
case "$RUNTIME_BASE" in
    /*) ;;
    *) die "--runtime-dir must be an absolute path, got '${RUNTIME_BASE}'" ;;
esac
while [ "$ROOT" != "/" ] && [ "${ROOT%/}" != "$ROOT" ]; do
    ROOT=${ROOT%/}
done
while [ "$RUNTIME_BASE" != "/" ] && [ "${RUNTIME_BASE%/}" != "$RUNTIME_BASE" ]; do
    RUNTIME_BASE=${RUNTIME_BASE%/}
done

if [ "$ROOT" = "/" ]; then
    instance="/$tenant"
else
    instance="$ROOT/$tenant"
fi
home="$instance/home"
# A short digest keeps the API socket path under the Unix socket limit, the same
# derivation horon-agent-sdk uses for a hosted launch.
short_digest() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 | cut -c1-12
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum | cut -c1-12
    else
        die "need shasum or sha256sum to derive the runtime directory"
    fi
}
if [ "$RUNTIME_BASE" = "/" ]; then
    runtime="/horon-tenant-$(printf '%s' "$home" | short_digest)"
else
    runtime="$RUNTIME_BASE/horon-tenant-$(printf '%s' "$home" | short_digest)"
fi
legacy_socket="$runtime/horon-agent.sock"
container="horon-tenant-$tenant"

require_no_symlink() {
    if [ -L "$1" ]; then
        die "refusing to use a symbolic link at $1"
    fi
}

prepare() {
    require_no_symlink "$instance"
    mkdir -p "$home" "$runtime"
    chmod 0700 "$instance" "$home" "$runtime"
}

tenant_env() {
    # A hosted process never inherits the operator's logins or home: HOME points
    # at the tenant volume, the gateway is forced off, and the mode and tenant
    # are fixed here.
    export HORON_AGENT_GATEWAY_ENABLED=0
    export HOME="$home"
    export HORON_HOME="$home"
    export HORON_AGENT_RUNTIME_DIR="$runtime"
    export HORON_AGENT_SOCKET="$legacy_socket"
    export HORON_AGENT_HOSTING_MODE=per-user-container
    export HORON_AGENT_TENANT_ID="$tenant"
    export HORON_AGENT_NO_AUTO_UPDATE=1
}

container_cmd() {
    if command -v docker >/dev/null 2>&1; then
        printf 'docker'
    elif command -v podman >/dev/null 2>&1; then
        printf 'podman'
    else
        die "the container driver needs docker or podman on PATH"
    fi
}

case "$verb" in
    create)
        prepare
        printf 'created %s\n  home: %s\n  runtime: %s\n' "$instance" "$home" "$runtime"
        ;;
    start)
        if [ "$DRIVER" = "container" ]; then
            runtime_cli=$(container_cmd)
            prepare
            "$runtime_cli" run --detach --init --name "$container" \
                --user "$(id -u):$(id -g)" \
                --volume "$instance:/data" \
                --env "HORON_AGENT_TENANT_ID=$tenant" \
                --env "HORON_AGENT_HOSTING_MODE=per-user-container" \
                --env "HORON_AGENT_RUNTIME_DIR=/run/horon" \
                --env "HORON_HOME=/data/home" \
                "$IMAGE"
            printf 'started %s\n' "$container"
            exit 0
        fi
        prepare
        tenant_env
        exec "$BINARY" --no-update --socket "$legacy_socket" serve
        ;;
    stop)
        if [ "$DRIVER" = "container" ]; then
            runtime_cli=$(container_cmd)
            "$runtime_cli" stop --time 20 "$container" >/dev/null
            printf 'stopped %s\n' "$container"
            exit 0
        fi
        [ -d "$home" ] || die "tenant '${tenant}' has no home at $home"
        tenant_env
        # SIGTERM reaches the daemon's session drain before it exits; --force
        # only bounds the wait, it does not skip the drain.
        "$BINARY" server stop --force >/dev/null 2>&1 || true
        printf 'stopped tenant %s\n' "$tenant"
        ;;
    delete)
        if [ "$DRIVER" = "container" ]; then
            runtime_cli=$(container_cmd)
            "$runtime_cli" rm --force "$container" >/dev/null 2>&1 || true
        else
            if [ -d "$home" ]; then
                tenant_env
                "$BINARY" server stop --force >/dev/null 2>&1 || true
            fi
        fi
        [ -e "$instance" ] || [ -L "$instance" ] || {
            printf 'tenant %s is already absent\n' "$tenant"
            exit 0
        }
        require_no_symlink "$instance"
        resolved_root=$(cd "$ROOT" && pwd -P)
        resolved_instance=$(cd "$instance" && pwd -P)
        case "$resolved_instance" in
            "$resolved_root"/*) ;;
            *) die "refusing to delete $instance: it does not resolve inside $ROOT" ;;
        esac
        rm -rf -- "$resolved_instance"
        if [ -d "$runtime" ] && [ ! -L "$runtime" ]; then
            rm -rf -- "$runtime"
        fi
        printf 'deleted tenant %s\n' "$tenant"
        ;;
    status)
        if [ -d "$instance" ]; then
            printf 'tenant %s: present\n  home: %s\n  runtime: %s\n' "$tenant" "$home" "$runtime"
        else
            printf 'tenant %s: absent\n' "$tenant"
        fi
        ;;
    *)
        usage >&2
        die "unknown verb: $verb"
        ;;
esac
