#!/usr/bin/env bash
# Noninteractive CI install. Update this literal only after inspecting published assets.
set -euo pipefail
readonly version='v0.1.0-alpha.17'
fail() { printf 'install_justrust: %s\n' "$*" >&2; exit 1; }
case "$(uname -s):$(uname -m)" in
    Linux:x86_64) target=x86_64-unknown-linux-musl ;;
    Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
    MINGW*:x86_64) target=x86_64-pc-windows-msvc ;;
    *) fail 'unsupported host' ;;
esac
binary=justrust
[[ "$target" != *windows* ]] || binary=justrust.exe
base="https://github.com/1jehuang/justrust/releases/download/$version"
# The same release through the REST API. github.com's download redirect has
# answered 504 for hours while this endpoint served the same assets (#31).
api="https://api.github.com/repos/1jehuang/justrust/releases"
# Explicit opt-in for trusted, isolated fixture tests only. Never set in CI.
if [[ -n "${JUSTRUST_TEST_BASE_URL:-}" ]]; then
    [[ "${JUSTRUST_ALLOW_TEST_URL:-}" == 1 ]] || fail 'test URL requires JUSTRUST_ALLOW_TEST_URL=1'
    base=${JUSTRUST_TEST_BASE_URL%/}
    api=${JUSTRUST_TEST_API_URL:-}
fi
install_dir=${JUSTRUST_INSTALL_DIR:-"$HOME/.local/bin"}
[[ "$install_dir" != *$'\n'* && "$install_dir" != *$'\r'* ]] || fail 'invalid install directory'
archive="justrust-$target.tar.gz"
work=$(mktemp -d)
staged=''
cleanup() { rm -rf -- "$work"; [[ -z "$staged" ]] || rm -f -- "$staged"; }
trap cleanup EXIT
curl_get() {
    curl --fail --location --silent --show-error --retry 5 --retry-delay 2 "$@"
}
api_auth=()
token=${GH_TOKEN:-${GITHUB_TOKEN:-}}
[[ -z "$token" ]] || api_auth=(--header "Authorization: Bearer $token")
# Fetch one release asset: the download URL first, then the same asset by its
# API id. Both copies are checked against the one digest below, so the second
# path changes where the bytes come from, never what is accepted.
fetch() {
    local name=$1 out=$2 id
    curl_get --output "$out" "$base/$name" && return 0
    [[ -n "$api" ]] || return 1
    printf 'install_justrust: %s failed, trying the release API\n' "$name" >&2
    curl_get ${api_auth[@]+"${api_auth[@]}"} --header 'Accept: application/vnd.github+json' \
        --output "$work/release.json" "$api/tags/$version" || return 1
    # Each asset object lists its "url" before its "name". Tokenize so the JSON
    # layout does not matter, take the id of the exact name only, and accept
    # nothing but a numeric asset id of this repository.
    id=$(grep -oE '"(url|name)"[[:space:]]*:[[:space:]]*"[^"]*"' "$work/release.json" | awk -F'"' -v want="$name" '
        $2 == "url" { last = "" }
        $2 == "url" && $4 ~ /^https:\/\/api\.github\.com\/repos\/1jehuang\/justrust\/releases\/assets\/[0-9]+$/ {
            last = $4; sub(/.*\//, "", last) }
        $2 == "name" && $4 == want && last != "" { print last; exit }')
    [[ "$id" =~ ^[0-9]+$ ]] || fail "no release asset named $name"
    curl_get ${api_auth[@]+"${api_auth[@]}"} --header 'Accept: application/octet-stream' \
        --output "$out" "$api/assets/$id"
}
fetch "$archive" "$work/$archive" || fail "could not download $archive"
fetch "$archive.sha256" "$work/checksum" || fail "could not download $archive.sha256"
# Parse only a single digest. Do not let a downloaded manifest select filesystem paths.
expected=$(awk 'NR == 1 {print $1} END {if (NR != 1) exit 1}' "$work/checksum") || fail 'invalid checksum manifest'
[[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || fail 'invalid SHA256 digest'
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/$archive")
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$work/$archive")
else
    fail 'sha256sum or shasum required'
fi
actual=$(printf '%s' "${actual%% *}" | tr '[:upper:]' '[:lower:]')
expected=$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')
[[ "$actual" == "$expected" ]] || fail 'SHA256 checksum mismatch'
# Stream only the exact selected member, never extract archive paths or symlinks.
member="justrust-$target/$binary"
tar -xOzf "$work/$archive" -- "$member" > "$work/$binary"
[[ -s "$work/$binary" ]] || fail 'empty binary'
chmod 755 "$work/$binary"
# Validate before touching an existing installation.
"$work/$binary" --version
mkdir -p -- "$install_dir"
staged=$(mktemp "$install_dir/.justrust-install.XXXXXX")
cp -- "$work/$binary" "$staged"
chmod 755 "$staged"
mv -f -- "$staged" "$install_dir/$binary"
staged=''
if [[ -n "${GITHUB_PATH:-}" ]]; then
    path_dir=$(cd -- "$install_dir" && pwd -P)
    if [[ "$target" == *windows* ]]; then
        path_dir=$(cygpath -w "$path_dir")
    fi
    printf '%s\n' "$path_dir" >> "$GITHUB_PATH"
fi
printf 'Installed justrust %s (%s) into %s\n' "$version" "$target" "$install_dir"
