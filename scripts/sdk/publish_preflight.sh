#!/usr/bin/env bash
# The local half of the release check, in one place.
#
# `.github/workflows/publish-typescript-sdk.yml` owns publishing: it stamps the
# release tag's version, prepares the six runtime packages and publishes all
# seven from a runner. This script answers what can be answered before that run:
# do the SDK's own gates pass, is the tarball clean, and does the registry
# already know the seven names. Nothing here publishes, and nothing here can
# see the repository variable or the Trusted Publisher configuration.
#
# Usage: bash scripts/sdk/publish_preflight.sh
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sdk_dir="$repo_root/packages/sdk/typescript"
name="$(node -p "require('$sdk_dir/package.json').name")"
version="$(node -p "require('$sdk_dir/package.json').version")"
problems=0

note_problem() {
  echo "  BLOCKED: $1"
  problems=$((problems + 1))
}

echo "== $name@$version =="

echo
echo "-- gates --"
if npm --prefix "$sdk_dir" run check --silent >/tmp/sdk-preflight-check.log 2>&1; then
  echo "  ok: typecheck, build, unit tests"
else
  note_problem "npm run check failed; see /tmp/sdk-preflight-check.log"
fi

if bash "$repo_root/scripts/sdk/test_package.sh" >/tmp/sdk-preflight-pack.log 2>&1; then
  echo "  ok: the packed tarball installs and imports as a consumer"
else
  note_problem "the package check failed; see /tmp/sdk-preflight-pack.log"
fi

echo
echo "-- tarball --"
# `prepack` rebuilds dist/ from a clean slate, so every `npm pack --dry-run` is
# a full build. Both checks below read the same listing, so take it once.
pack="$(cd "$sdk_dir" && npm pack --dry-run 2>&1 || true)"
files="$(printf '%s\n' "$pack" | grep -c 'npm notice.*dist/' || true)"
if [ "$files" -gt 0 ]; then
  echo "  ok: $files built files included"
else
  note_problem "the tarball contains no dist/ files; prepack may have failed"
fi
if printf '%s\n' "$pack" | grep -qE 'npm notice.*(src/|test/)'; then
  note_problem "sources or tests are leaking into the tarball; check \`files\`"
else
  echo "  ok: no sources or tests in the tarball"
fi

echo
echo "-- registry --"
# npm has no way to reserve a name, so a name npm does not know yet is expected
# before the owner publishes its `0.0.0` placeholder. A missing placeholder is
# not a blocker for this checkout, but the publish run cannot land without it.
missing_placeholders=0
while IFS= read -r package; do
  if npm view "$package" version >/dev/null 2>&1; then
    echo "  ok: $package exists on the registry"
  else
    echo "  note: $package is unpublished; its 0.0.0 placeholder is a one-time owner step"
    missing_placeholders=$((missing_placeholders + 1))
  fi
done < <(python3 "$repo_root/scripts/sdk/sdk_release.py" package-names)
if [ "$missing_placeholders" -gt 0 ]; then
  echo "        see the first-release section of packages/sdk/typescript/RELEASING.md"
fi

echo
if [ "$problems" -eq 0 ]; then
  echo "Preflight clean. Publishing runs in .github/workflows/publish-typescript-sdk.yml"
  echo "on a release tag; a pull request exercises the same flow as a dry run."
else
  echo "$problems blocker(s) above. Nothing was published."
  exit 1
fi
