#!/bin/sh
# Pack a release archive in POSIX ustar, then install it back to prove it works.
#
# The FreeBSD agent archive has two top-level regular files: the executable and
# LICENSE. The archiver's default format cannot be trusted to deliver that.
# GNU sparse archives can expose a different member name to FreeBSD tar:
# `GNUSparseFile.0/<name>`, and the installer refuses any member with a path
# separator before it looks at the executable (`scripts/install/install.sh`,
# `crates/app-core/src/update/release_asset.rs`). v0.1.14's FreeBSD archive was
# listed as `GNUSparseFile.0/horon-agent-freebsd-x86_64`, so the published asset
# could not be installed. This script always writes ustar and checks the archive
# it produced before anything uploads it.
#
# Usage:
#   scripts/release/agent_archive.sh pack <dist-dir> <binary>
#   scripts/release/agent_archive.sh install-verify <dist-dir> <binary> [version] [samples]
#
# install-verify feeds the archive to the binary's own `install-release` with
# a fresh HORON_HOME for each pair, then reinstalls into the same home. It
# compares the installation with its source and starts the launcher each time.
# The SHA256SUMS it uses lives in a temporary directory, so the published dist
# directory never carries a second checksum manifest next to the release assets.
set -eu

prog=$(basename "$0")
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
tmp_root=${TMPDIR:-/tmp}
tmp_root=${tmp_root%/}
# Member listings and checksums are byte-oriented; a locale must not reorder them.
LC_ALL=C
export LC_ALL

die() {
  printf '%s: %s\n' "$prog" "$*" >&2
  exit 1
}

usage() {
  printf 'usage: %s pack <dist-dir> <binary>\n' "$prog" >&2
  printf '       %s install-verify <dist-dir> <binary> [version] [samples]\n' "$prog" >&2
  exit 2
}

# Same names the updater resolves at runtime (`crates/update-core/src/lib.rs`).
asset_name() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os/$arch" in
    FreeBSD/amd64 | FreeBSD/x86_64) printf 'horon-agent-freebsd-x86_64' ;;
    *) die "unsupported host for a release archive: $os $arch" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v sha256 >/dev/null 2>&1; then
    sha256 -q "$1"
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

absolute_path() {
  case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s' "$(CDPATH= cd -- "$(dirname -- "$1")" && pwd)/$(basename -- "$1")" ;;
  esac
}

# The install directory name must satisfy `release_asset::validate_version`: the
# version names a directory, so only alphanumerics and `.`, `-`, `+` are allowed
# and a leading `v` (the tag form) is not part of it.
resolve_version() {
  version=$1
  version=${version#v}
  case "$version" in
    '' | . | ..) die 'release version is not a safe version directory name' ;;
  esac
  printf '%s' "$version" | grep -Eq '^[A-Za-z0-9.+-]+$' \
    || die "release version is not a safe version directory name: $version"
  printf '%s' "$version"
}

# Refuse anything other than the two expected top-level regular files, so an
# archiver that rewrites the members fails here instead of at the user's install.
check_archive() {
  archive=$1
  asset=$2
  work=$3

  tar -tzf "$archive" >"$work/names" || die "archive is not a readable tar.gz: $archive"
  entries=$(grep -c '' "$work/names" || true)
  [ "$entries" = 2 ] || die "archive holds $entries entries, expected exactly 2"
  if grep -q '/' "$work/names"; then
    die "archive member has a path separator: $(grep '/' "$work/names" | head -n 1)"
  fi
  printf '%s\n%s\n' "$asset" LICENSE | sort >"$work/expected"
  sort "$work/names" >"$work/names.sorted"
  cmp -s "$work/names.sorted" "$work/expected" \
    || die "archive members are $(tr '\n' ' ' <"$work/names") instead of '$asset LICENSE'"

  tar -tvzf "$archive" >"$work/listing" || die "archive listing failed: $archive"
  irregular=$(awk 'substr($0, 1, 1) != "-"' "$work/listing")
  [ -z "$irregular" ] || die "archive member is not a regular file: $irregular"
  awk -v want="$asset" 'substr($0, 1, 4) == "-rwx" && $NF == want { found = 1 } END { exit !found }' \
    "$work/listing" || die "archive member '$asset' is not executable"

  # A POSIX ustar header carries `ustar\0` and version `00` at byte 257.
  magic=$(gzip -dc "$archive" | dd bs=1 skip=257 count=8 2>/dev/null | od -An -tx1 | tr -d ' \n')
  [ "$magic" = 7573746172003030 ] || die "archive is not POSIX ustar (header magic: $magic), refusing to publish it"
}

pack() {
  [ "$#" -eq 2 ] || usage
  dist=$1
  binary=$(absolute_path "$2")
  [ -f "$binary" ] || die "release binary not found: $2"
  [ -f "$repo_root/LICENSE" ] || die "LICENSE not found at $repo_root/LICENSE"
  asset=$(asset_name)

  work=$(mktemp -d "${tmp_root}/horon-archive.XXXXXX")
  trap 'rm -rf "$work"' EXIT

  # tar reads the staged copies, so the member names are the published names and
  # no host path can leak into the header. tar and gzip run as separate steps so a
  # failure in either one is reported instead of being hidden by a pipeline.
  cp "$binary" "$work/$asset"
  cp "$repo_root/LICENSE" "$work/LICENSE"
  chmod 755 "$work/$asset"
  chmod 644 "$work/LICENSE"
  ( cd "$work" && tar -c --format=ustar -f "$asset.tar" "$asset" LICENSE ) \
    || die "tar failed to write the ustar archive for $asset"
  gzip -c "$work/$asset.tar" >"$work/$asset.tar.gz" || die "gzip failed for $asset.tar"

  check_archive "$work/$asset.tar.gz" "$asset" "$work"

  mkdir "$work/unpacked"
  tar -xzf "$work/$asset.tar.gz" -C "$work/unpacked" || die "archive does not extract: $asset.tar.gz"
  cmp -s "$work/unpacked/$asset" "$binary" || die "archive member '$asset' differs from the packaged binary"

  mkdir -p "$dist"
  mv "$work/$asset.tar.gz" "$dist/$asset.tar.gz" || die "could not write $dist/$asset.tar.gz"
  printf 'packed %s (%s bytes)\n' "$dist/$asset.tar.gz" "$(wc -c <"$dist/$asset.tar.gz" | tr -d ' ')"
}

install_verify() {
  [ "$#" -ge 2 ] && [ "$#" -le 4 ] || usage
  dist=$1
  binary=$(absolute_path "$2")
  source_version=$("$binary" --version) || die "could not read the source version: $binary"
  reported=${source_version#horon-agent }
  reported=${reported%% *}
  version=$(resolve_version "${3:-$reported}")
  [ "${reported#v}" = "$version" ] || die "expected $version, source reports $source_version"
  samples=${4:-1}
  case "$samples" in
    '' | *[!0-9]*) die "sample count must be a positive integer: $samples" ;;
  esac
  [ "$samples" -ge 1 ] || die "sample count must be a positive integer: $samples"
  [ -f "$binary" ] || die "release binary not found: $2"
  asset=$(asset_name)
  [ -f "$dist/$asset.tar.gz" ] || die "archive not found: $dist/$asset.tar.gz"

  work=$(mktemp -d "${tmp_root}/horon-install.XXXXXX")
  trap 'rm -rf "$work"' EXIT
  check_archive "$dist/$asset.tar.gz" "$asset" "$work"
  # The assets directory is a copy: it carries the checksum manifest this check
  # needs, which must never appear in the published dist directory.
  mkdir -p "$work/assets"
  cp "$dist/$asset.tar.gz" "$work/assets/"
  printf '%s  %s\n' "$(sha256_of "$work/assets/$asset.tar.gz")" "$asset.tar.gz" >"$work/assets/SHA256SUMS"

  tar -xOzf "$dist/$asset.tar.gz" "$asset" > "$work/member" || die 'could not read the packaged executable'
  cmp -s "$work/member" "$binary" || die 'packaged executable differs from the source binary'

  sample=1
  while [ "$sample" -le "$samples" ]; do
    pair_dir="$work/pair-$sample"
    mkdir "$pair_dir"
    for state in cold warm; do
      HORON_HOME="$pair_dir" HORON_AGENT_INSTALL_DIR="$pair_dir/bin" HORON_AGENT_NO_AUTO_UPDATE=1 \
        "$binary" install-release --assets "$work/assets" --agent-version "$version"
      installed="$pair_dir/builds/versions/$version/horon-agent"
      [ -f "$installed" ] && [ ! -L "$installed" ] || die "missing version executable: $installed"
      cmp -s "$installed" "$binary" || die 'installed bytes differ from the original'
      started=$(HORON_HOME="$pair_dir" "$pair_dir/bin/horon-agent" --version) || die 'installed launcher failed'
      [ "$started" = "$source_version" ] || die "unexpected installed version: $started"
      printf 'pair %s/%s %s: %s\n' "$sample" "$samples" "$state" "$started"
    done
    # The next pair starts empty and this build needs only one installed copy on disk.
    rm -r "$pair_dir"
    sample=$((sample + 1))
  done
}

case "${1:-}" in
  pack) shift; pack "$@" ;;
  install-verify) shift; install_verify "$@" ;;
  *) usage ;;
esac
