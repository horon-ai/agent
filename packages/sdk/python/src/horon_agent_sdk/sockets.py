"""Socket path resolution, mirroring ``horon-agent-harness-api::sockets``.

The bridge and every client must resolve the same directory or nothing can
connect, so the rules here follow the Rust module exactly.
"""

from __future__ import annotations

import hashlib
import os
import sys
import tempfile
from pathlib import PurePosixPath, PureWindowsPath


def runtime_dir() -> str:
    explicit = os.environ.get("HORON_AGENT_RUNTIME_DIR")
    if explicit:
        return explicit
    xdg = os.environ.get("XDG_RUNTIME_DIR")
    if xdg:
        return xdg
    if sys.platform == "darwin" and os.environ.get("TMPDIR"):
        return os.environ["TMPDIR"]
    return os.path.join(tempfile.gettempdir(), f"horon-agent-{_user_discriminator()}")


def _user_discriminator() -> str:
    raw = (
        os.environ.get("USERNAME") or os.environ.get("USER")
        if sys.platform == "win32"
        else os.environ.get("UID") or os.environ.get("USER")
    )
    return _sanitize(raw or "user")


def _sanitize(raw: str) -> str:
    out = "".join(ch for ch in raw if ch.isascii() and (ch.isalnum() or ch in "-_"))[:64]
    return out or "user"


def transport_endpoint(socket_path: str) -> str:
    """Endpoint the SDK dials for a given socket path.

    On Unix that is the path itself. On Windows there is no socket file: the
    bridge publishes a named pipe whose name is derived from the path, so a
    client that dialled the path would find nothing there. This mirrors
    ``horon-agent-transport``'s derivation exactly, and the two are pinned
    together by a test on each side.
    """
    if sys.platform != "win32":
        return socket_path

    # Same rule as the Rust side: a readable stem for diagnosis, plus a hash of
    # the normalized path so two different paths never collide.
    name = PureWindowsPath(socket_path).stem
    stem = "".join(ch for ch in name if ch.isascii() and (ch.isalnum() or ch in "-_"))[:32] or "horon-agent"
    normalized = socket_path.replace("\\", "/").lower()
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()[:16]
    return "\\\\.\\pipe\\" + f"{stem}-{digest}"


def api_socket_path() -> str:
    """Path of the versioned harness API socket. ``HORON_AGENT_API_SOCKET`` overrides."""
    return os.environ.get("HORON_AGENT_API_SOCKET") or os.path.join(runtime_dir(), "horon-agent-api.sock")


def legacy_socket_path() -> str:
    """Path of the internal daemon socket. ``HORON_AGENT_SOCKET`` overrides."""
    return os.environ.get("HORON_AGENT_SOCKET") or os.path.join(runtime_dir(), "horon-agent.sock")


#: Re-exported for tests that need a platform-independent stem.
def pipe_stem(socket_path: str) -> str:
    """The readable part of a Windows pipe name, derived like the bridge does."""
    name = PureWindowsPath(socket_path).stem
    return "".join(ch for ch in name if ch.isascii() and (ch.isalnum() or ch in "-_"))[:32] or "horon-agent"


def posix_stem(socket_path: str) -> str:
    """The POSIX spelling of :func:`pipe_stem`, for the same drift test."""
    name = PurePosixPath(socket_path).stem
    return "".join(ch for ch in name if ch.isascii() and (ch.isalnum() or ch in "-_"))[:32] or "horon-agent"
