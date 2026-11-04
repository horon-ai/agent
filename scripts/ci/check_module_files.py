#!/usr/bin/env python3
"""Fail when a `mod` declaration has no corresponding file.

A tree can declare `mod foo;` and not commit `foo.rs`. That tree cannot be
formatted or built, and the first gate to notice is rustfmt, whose
"Error writing files: failed to resolve mod" reads like a formatting problem and
hides every gate behind it. `cargo check` reports the same break as E0583, but
only after a full dependency build.

The cause recurs: a commit assembled by copying files out of a worktree picks up
some files and not their siblings. This runs in well under a second over the
tracked tree with no compiler, so it can also serve as a pre-commit/pre-push
check.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

# `mod foo;` only. A `mod foo { ... }` block is inline and needs no file, and
# `#[path = "..."]` overrides resolution, so both are handled by the caller
# below rather than by this pattern.
MOD_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
PATH_ATTR = re.compile(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]')


def tracked_rust_files(repo_root: Path) -> list[Path]:
    """Every tracked .rs file, so untracked scratch files never fail the gate."""
    out = subprocess.run(
        ["git", "ls-files", "-z", "*.rs"],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [repo_root / name for name in out.split("\0") if name]


def resolves(source: Path, module: str, path_override: str | None) -> bool:
    """Whether `mod module;` in `source` has a file behind it."""
    parent = source.parent
    if path_override is not None:
        return (parent / path_override).exists()

    # `foo.rs` beside the declaring file, or `foo/mod.rs`. For a non-mod.rs
    # parent, Rust also looks in a directory named after the parent module.
    candidates = [parent / f"{module}.rs", parent / module / "mod.rs"]
    if source.name not in ("mod.rs", "lib.rs", "main.rs"):
        stem = source.stem
        candidates += [
            parent / stem / f"{module}.rs",
            parent / stem / module / "mod.rs",
        ]
    return any(c.exists() for c in candidates)


def repo_root_from_git() -> Path:
    """Resolve the repo from git rather than from this file's location, so the
    script works when copied elsewhere (e.g. into a hook directory)."""
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    return Path(out)


def main() -> int:
    repo_root = repo_root_from_git()
    missing: list[str] = []

    for source in tracked_rust_files(repo_root):
        try:
            lines = source.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeDecodeError):
            continue

        pending_path: str | None = None
        for line in lines:
            attr = PATH_ATTR.search(line)
            if attr:
                pending_path = attr.group(1)
                continue

            decl = MOD_DECL.match(line)
            if not decl:
                # Any other non-blank, non-attribute line ends the attribute's
                # scope, so a `#[path]` cannot leak onto an unrelated `mod`.
                stripped = line.strip()
                if stripped and not stripped.startswith(("#", "//", "/*", "*")):
                    pending_path = None
                continue

            module = decl.group(1)
            if not resolves(source, module, pending_path):
                rel = source.relative_to(repo_root)
                missing.append(f"{rel}: mod {module}; has no file")
            pending_path = None

    if missing:
        print("Module declarations without files:")
        for entry in sorted(missing):
            print(f"  - {entry}")
        print(
            "\nThis tree cannot be formatted or compiled. The usual cause is a "
            "commit that\npicked up a file declaring the module but not the "
            "module's own file."
        )
        return 1

    print("Module declarations OK: every `mod x;` resolves to a file")
    return 0


if __name__ == "__main__":
    sys.exit(main())
