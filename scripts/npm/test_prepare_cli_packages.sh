#!/usr/bin/env bash
# Exercise scripts/npm/prepare_cli_packages.sh end to end against fake release
# archives, one per archive shape the release produces.
#
# usage: test_prepare_cli_packages.sh
#
# The fake entries are shell scripts that print a version, so the host-platform
# `--version` check the prepare script performs runs for real on this machine.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
prepare="$repo_root/scripts/npm/prepare_cli_packages.sh"
version="1.2.3"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
  echo "test_prepare_cli_packages: $*" >&2
  exit 1
}

command -v node >/dev/null 2>&1 || fail "need node"
command -v npm >/dev/null 2>&1 || fail "need npm for the pack check"
command -v tar >/dev/null 2>&1 || fail "need tar"

# A fake entry file. `$1` is the version it reports; the payload form is a
# plain binary stand-in, the wrapper form execs the payload beside it, which is
# what the portable linux-x86_64 archive does.
fake_binary() {
  local file="$1" reported="$2"
  printf '#!/bin/sh\necho "horon-agent %s (fake)"\n' "$reported" >"$file"
  chmod +x "$file"
}

fake_wrapper() {
  local file="$1" payload="$2"
  cat >"$file" <<WRAPPER
#!/bin/sh
exec "\$(dirname "\$0")/$payload" "\$@"
WRAPPER
  chmod +x "$file"
}

# Build a full fake asset directory: six suffixes, both stems, plus SHA256SUMS.
make_assets() {
  local dir="$1" reported="$2"
  local stage="$work/stage"
  rm -rf "$dir" "$stage"
  mkdir -p "$dir" "$stage"
  for suffix in linux-x86_64 linux-aarch64 macos-x86_64 macos-aarch64 windows-x86_64 windows-aarch64; do
    for stem in horon-agent horon-tui; do
      rm -rf "$stage/$stem-$suffix"
      mkdir -p "$stage/$stem-$suffix"
      printf 'MIT License\n' >"$stage/$stem-$suffix/LICENSE"
      case "$suffix" in
        linux-x86_64)
          fake_binary "$stage/$stem-$suffix/$stem-$suffix.bin" "$reported"
          fake_wrapper "$stage/$stem-$suffix/$stem-$suffix" "$stem-$suffix.bin"
          tar -czf "$dir/$stem-$suffix.tar.gz" -C "$stage/$stem-$suffix" "$stem-$suffix" "$stem-$suffix.bin" LICENSE
          ;;
        windows-*)
          fake_binary "$stage/$stem-$suffix/$stem-$suffix.exe" "$reported"
          tar -czf "$dir/$stem-$suffix.tar.gz" -C "$stage/$stem-$suffix" "$stem-$suffix.exe" LICENSE
          ;;
        *)
          fake_binary "$stage/$stem-$suffix/$stem-$suffix" "$reported"
          tar -czf "$dir/$stem-$suffix.tar.gz" -C "$stage/$stem-$suffix" "$stem-$suffix" LICENSE
          ;;
      esac
    done
  done
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$dir" && sha256sum ./*.tar.gz | sed 's#\./##' >SHA256SUMS)
  else
    (cd "$dir" && shasum -a 256 ./*.tar.gz | sed 's#\./##' >SHA256SUMS)
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

json_report() {
  local dir="$1" expression="$2"
  shift 2
  node -e "$expression" "$dir" "$@"
}

# --- happy path -------------------------------------------------------------
assets="$work/assets"
out="$work/out"
make_assets "$assets" "$version"
bash "$prepare" "$out" "$assets" "$version" >"$work/happy.log" 2>&1 ||
  fail "prepare failed on valid assets: $(cat "$work/happy.log")"

for platform in linux-x64 linux-arm64 darwin-x64 darwin-arm64 win32-x64 win32-arm64; do
  [ -f "$out/$platform/package.json" ] || fail "missing package.json for $platform"
  [ -f "$out/$platform/README.md" ] || fail "missing README.md for $platform"
  [ -f "$out/$platform/LICENSE" ] || fail "missing LICENSE for $platform"
  [ -x "$out/$platform/bin/horon-agent" ] || [ -x "$out/$platform/bin/horon-agent.exe" ] ||
    fail "missing entry binary for $platform"
  [ -f "$out/$platform/bin/horon-tui" ] || [ -f "$out/$platform/bin/horon-tui.exe" ] ||
    fail "missing horon-tui for $platform"
done

listing() {
  (cd "$1" && find bin -type f | LC_ALL=C sort | tr '\n' ' ')
}

[ "$(listing "$out/linux-x64")" = "bin/horon-agent bin/horon-agent-linux-x86_64.bin bin/horon-tui bin/horon-tui-linux-x86_64.bin " ] ||
  fail "unexpected linux-x64 bin layout: $(listing "$out/linux-x64")"
[ "$(listing "$out/linux-arm64")" = "bin/horon-agent bin/horon-tui " ] ||
  fail "unexpected linux-arm64 bin layout: $(listing "$out/linux-arm64")"
[ "$(listing "$out/darwin-arm64")" = "bin/horon-agent bin/horon-tui " ] ||
  fail "unexpected darwin-arm64 bin layout: $(listing "$out/darwin-arm64")"
[ "$(listing "$out/win32-x64")" = "bin/horon-agent.exe bin/horon-tui.exe " ] ||
  fail "unexpected win32-x64 bin layout: $(listing "$out/win32-x64")"

# The wrapper entry must still resolve the payload the archive shipped.
"$out/linux-x64/bin/horon-agent" --version | grep -F "$version" >/dev/null ||
  fail "linux-x64 wrapper does not run its payload"

# Manifests: platform packages declare os/cpu, no scripts, and the version they
# were built for.
json_report "$out" '
  const fs = require("node:fs");
  const [dir, version] = [process.argv[1], process.argv[2]];
  const expected = {
    "linux-x64": ["linux", "x64"],
    "linux-arm64": ["linux", "arm64"],
    "darwin-x64": ["darwin", "x64"],
    "darwin-arm64": ["darwin", "arm64"],
    "win32-x64": ["win32", "x64"],
    "win32-arm64": ["win32", "arm64"],
  };
  for (const [platform, [os, cpu]] of Object.entries(expected)) {
    const manifest = JSON.parse(fs.readFileSync(`${dir}/${platform}/package.json`, "utf8"));
    if (manifest.name !== `@horon-ai/horon-${platform}`) throw new Error(`bad name in ${platform}: ${manifest.name}`);
    if (manifest.version !== version) throw new Error(`bad version in ${platform}: ${manifest.version}`);
    if (JSON.stringify(manifest.os) !== JSON.stringify([os])) throw new Error(`bad os in ${platform}: ${manifest.os}`);
    if (JSON.stringify(manifest.cpu) !== JSON.stringify([cpu])) throw new Error(`bad cpu in ${platform}: ${manifest.cpu}`);
    if (JSON.stringify(manifest.files) !== JSON.stringify(["bin", "LICENSE", "README.md"])) throw new Error(`bad files in ${platform}`);
    for (const script of ["preinstall", "install", "postinstall"]) {
      if (manifest.scripts && manifest.scripts[script]) throw new Error(`${platform} declares ${script}`);
    }
    if (manifest.dependencies) throw new Error(`${platform} declares dependencies`);
  }
' "$version" || fail "platform manifests are wrong"

json_report "$out/horon" '
  const fs = require("node:fs");
  const [dir, version] = [process.argv[1], process.argv[2]];
  const manifest = JSON.parse(fs.readFileSync(`${dir}/package.json`, "utf8"));
  if (manifest.name !== "@horon-ai/horon") throw new Error(`bad name: ${manifest.name}`);
  if (manifest.version !== version) throw new Error(`bad version: ${manifest.version}`);
  if (JSON.stringify(manifest.bin) !== JSON.stringify({ horon: "bin/horon.js", hrn: "bin/horon.js" })) throw new Error(`bad bin: ${JSON.stringify(manifest.bin)}`);
  if (manifest.dependencies) throw new Error("wrapper declares dependencies");
  for (const script of ["preinstall", "install", "postinstall"]) {
    if (manifest.scripts && manifest.scripts[script]) throw new Error(`wrapper declares ${script}`);
  }
  const pinned = Object.entries(manifest.optionalDependencies ?? {});
  if (pinned.length !== 6) throw new Error(`expected 6 optionalDependencies, found ${pinned.length}`);
  for (const [name, pinnedVersion] of pinned) {
    if (pinnedVersion !== version) throw new Error(`${name} pinned to ${pinnedVersion}, expected ${version}`);
  }
  if (!fs.existsSync(`${dir}/bin/horon.js`)) throw new Error("wrapper has no launcher");
' "$version" || fail "wrapper manifest is wrong"

# `npm pack` is what publishing would upload, so assert the exact file list.
packed="$(cd "$out/linux-x64" && npm pack --dry-run --json 2>/dev/null)" ||
  fail "npm pack failed for linux-x64"
node -e '
  const parsed = JSON.parse(process.argv[1]);
  // npm 10 prints an array of package reports; npm 11 and later print an
  // object keyed by package name. Accept both so the assertion holds on either.
  const packed = Array.isArray(parsed) ? parsed[0] : Object.values(parsed)[0];
  const files = packed.files.map((file) => file.path).sort();
  const expected = ["LICENSE", "README.md", "bin/horon-agent", "bin/horon-agent-linux-x86_64.bin", "bin/horon-tui", "bin/horon-tui-linux-x86_64.bin", "package.json"];
  if (JSON.stringify(files) !== JSON.stringify(expected)) throw new Error(`linux-x64 pack is ${JSON.stringify(files)}`);
' "$packed" || fail "linux-x64 pack contents are wrong"

packed="$(cd "$out/horon" && npm pack --dry-run --json 2>/dev/null)" ||
  fail "npm pack failed for the wrapper"
node -e '
  const parsed = JSON.parse(process.argv[1]);
  // npm 10 prints an array of package reports; npm 11 and later print an
  // object keyed by package name. Accept both so the assertion holds on either.
  const packed = Array.isArray(parsed) ? parsed[0] : Object.values(parsed)[0];
  const files = packed.files.map((file) => file.path).sort();
  const expected = ["LICENSE", "README.md", "bin/horon.js", "package.json"];
  if (JSON.stringify(files) !== JSON.stringify(expected)) throw new Error(`wrapper pack is ${JSON.stringify(files)}`);
' "$packed" || fail "wrapper pack contents are wrong"

# --- failure: checksum mismatch names the archive ---------------------------
bad="$work/bad-sum"
make_assets "$bad" "$version"
grep -v 'horon-tui-macos-x86_64.tar.gz' "$bad/SHA256SUMS" >"$bad/SHA256SUMS.new"
mv "$bad/SHA256SUMS.new" "$bad/SHA256SUMS"
if bash "$prepare" "$work/out-bad-sum" "$bad" "$version" >"$work/bad-sum.log" 2>&1; then
  fail "prepare accepted a missing SHA256SUMS entry"
fi
grep -F "horon-tui-macos-x86_64.tar.gz" "$work/bad-sum.log" >/dev/null ||
  fail "failure did not name the unsummed archive: $(cat "$work/bad-sum.log")"
[ ! -d "$work/out-bad-sum/horon" ] || fail "prepare wrote packages despite a checksum failure"

corrupt="$work/corrupt"
make_assets "$corrupt" "$version"
printf 'tampered' >>"$corrupt/horon-agent-linux-aarch64.tar.gz"
if bash "$prepare" "$work/out-corrupt" "$corrupt" "$version" >"$work/corrupt.log" 2>&1; then
  fail "prepare accepted a tampered archive"
fi
grep -F "checksum mismatch: horon-agent-linux-aarch64.tar.gz" "$work/corrupt.log" >/dev/null ||
  fail "failure did not name the tampered archive: $(cat "$work/corrupt.log")"

# --- failure: an archive with an unexpected layout is named ----------------
shape="$work/bad-shape"
make_assets "$shape" "$version"
stage="$work/shape-stage"
rm -rf "$stage" && mkdir -p "$stage"
tar -xzf "$shape/horon-tui-macos-aarch64.tar.gz" -C "$stage"
printf 'stray\n' >"$stage/README.txt"
tar -czf "$shape/horon-tui-macos-aarch64.tar.gz" -C "$stage" .
digest="$(sha256_of "$shape/horon-tui-macos-aarch64.tar.gz")"
awk -v name="horon-tui-macos-aarch64.tar.gz" -v sum="$digest" \
  '($2 == name) { $1 = sum } { print }' "$shape/SHA256SUMS" >"$shape/SHA256SUMS.new"
mv "$shape/SHA256SUMS.new" "$shape/SHA256SUMS"
if bash "$prepare" "$work/out-shape" "$shape" "$version" >"$work/shape.log" 2>&1; then
  fail "prepare accepted an archive with an unexpected layout"
fi
grep -F "unexpected members in horon-tui-macos-aarch64.tar.gz" "$work/shape.log" >/dev/null ||
  fail "failure did not name the mis-shaped archive: $(cat "$work/shape.log")"
[ ! -d "$work/out-shape/horon" ] || fail "prepare wrote packages despite a mis-shaped archive"

# --- failure: missing platform archive is named ----------------------------
missing="$work/missing"
make_assets "$missing" "$version"
rm "$missing/horon-tui-windows-aarch64.tar.gz"
if bash "$prepare" "$work/out-missing" "$missing" "$version" >"$work/missing.log" 2>&1; then
  fail "prepare accepted a missing archive"
fi
grep -F "missing release archive: horon-tui-windows-aarch64.tar.gz" "$work/missing.log" >/dev/null ||
  fail "failure did not name the missing archive: $(cat "$work/missing.log")"
[ ! -d "$work/out-missing/horon" ] || fail "prepare wrote packages despite a missing archive"

# --- failure: wrong version on the host package ----------------------------
wrong="$work/wrong-version"
make_assets "$wrong" "9.9.9"
if bash "$prepare" "$work/out-wrong" "$wrong" "$version" >"$work/wrong.log" 2>&1; then
  fail "prepare accepted a binary that does not report $version"
fi
grep -F "does not report $version" "$work/wrong.log" >/dev/null ||
  fail "wrong-version failure message is missing: $(cat "$work/wrong.log")"
[ ! -d "$work/out-wrong/horon" ] || fail "prepare wrote packages despite a version mismatch"

echo "ok: prepare_cli_packages happy path and five failure paths"
