#!/usr/bin/env bash
# Formatting gate for this workspace.
#
# Every workspace member is checked at the repository's own style (rustfmt.toml: edition 2024,
# max_width 120) through `cargo fmt --all`, so a member added or renamed later is covered without
# editing this script. The workspace metadata is still validated first: every member id must
# resolve against the packages map and no id may repeat.
set -euo pipefail

cd "$(dirname "$0")/../.."

metadata() {
  cargo metadata --locked --no-deps --format-version 1
}

reported=$(metadata | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["workspace_members"]))')

members=$(metadata | python3 -c '
import json, sys

meta = json.load(sys.stdin)
by_id = {package["id"]: package["name"] for package in meta["packages"]}
members = meta["workspace_members"]
unresolved = [member for member in members if member not in by_id]
if unresolved:
    raise SystemExit("workspace_members did not resolve against packages: " + ", ".join(unresolved))
if len(set(members)) != len(members):
    raise SystemExit("workspace_members repeats an id")
print(len(members))
')

if [ "$members" -ne "$reported" ]; then
  echo "the resolved member list did not match the metadata: $members of $reported" >&2
  exit 1
fi
if [ "$members" -lt 2 ]; then
  echo "the workspace reported $members members, which cannot be the whole workspace" >&2
  exit 1
fi

echo "formatting $members workspace members at the repository style"
cargo fmt --all -- --check
