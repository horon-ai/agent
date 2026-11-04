"""The SDK's error type.

Lives in its own module because both the client and the launcher raise it, and
importing it from the client would make ``launch`` and ``client`` circular.
"""

from __future__ import annotations


class HarnessError(Exception):
    """A failure with a stable ``code``, so callers branch on cause not message text."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message

    def __str__(self) -> str:
        return f"{self.code}: {self.message}"


def os_error_code(error: BaseException) -> str | None:
    """The errno name of an OS-level failure, e.g. ``ENOENT``, or ``None``.

    The launcher and the binary resolver both need to tell one filesystem
    failure from another, and only the errno distinguishes them.
    """
    if isinstance(error, OSError) and error.errno is not None:
        return _errno_name(error.errno)
    code = getattr(error, "code", None)
    return code if isinstance(code, str) else None


def _errno_name(errno: int) -> str | None:
    import errno as errno_module

    for name in dir(errno_module):
        if name.startswith("E") and getattr(errno_module, name) == errno:
            return name
    return None
