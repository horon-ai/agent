"""Locating the runtime binary bundled in a platform wheel.

The npm SDK resolves the runtime through an optional per-platform package. A
Python wheel cannot express that, so the platform wheels built by
``scripts/sdk/build_python_wheels.sh`` carry the binary inside the package at
``horon_agent_sdk/_bin/``. The pure wheel and the sdist contain no binary at
all: ``launch`` then falls back to ``horon-agent`` on ``PATH``.
"""

from __future__ import annotations

import sys
from pathlib import Path

__all__ = ["BIN_DIRNAME", "bundled_horon_agent_binary"]

BIN_DIRNAME = "_bin"


def bundled_horon_agent_binary() -> str | None:
    """Path of the runtime shipped with this installation, or ``None``.

    ``None`` means this is the pure wheel (or a source checkout): callers fall
    back to ``horon-agent`` on ``PATH``.
    """
    name = "horon-agent.exe" if sys.platform == "win32" else "horon-agent"
    candidate = Path(__file__).resolve().parent / BIN_DIRNAME / name
    return str(candidate) if candidate.is_file() else None
