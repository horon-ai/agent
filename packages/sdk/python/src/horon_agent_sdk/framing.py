"""NDJSON line framing over a byte stream."""

from __future__ import annotations

import codecs
import json
from typing import Any, Generic, Literal, TypeVar

MAX_FRAME_BYTES = 64 * 1024 * 1024
"""Largest single frame this decoder accepts, in bytes.

The bridge caps an inbound frame at 64 MiB (``MAX_FRAME_BYTES`` in
``harness-api-server``) because a 32 MiB aggregate PDF budget expands to
~43 MiB of base64 plus metadata. Outbound frames are the same traffic in the
other direction: a session-history or side-panel event carrying that PDF is
the largest legitimate event a client ever sees, so the ceiling is the same
64 MiB. A lower cap would refuse a frame the server considers valid, and a
higher one would leave a stream that never sends a newline free to grow.
The two constants must stay equal.
"""

MAX_RESIDUAL_BYTES = MAX_FRAME_BYTES
"""Largest unterminated remainder the decoder holds, in bytes.

The remainder is a frame whose newline has not arrived yet, so it cannot
legitimately be larger than a whole frame. Keeping the two equal means the
decoder refuses an oversized frame as soon as it can tell, whether or not the
newline has arrived.
"""

RELEASE_FRAME_BYTES = 256 * 1024
"""Frames above this size are transient (history, transcripts, file reads)."""

FrameLimit = Literal["frame", "remainder"]

T = TypeVar("T")


class FrameTooLargeError(Exception):
    """A frame, or the remainder of one, grew past its byte cap.

    A named error rather than a generic one: a caller has to tell an oversized
    frame (the peer is broken or hostile; drop the connection) from a JSON parse
    failure (one bad frame) without matching on message text.
    """

    def __init__(self, limit: FrameLimit, limit_bytes: int, observed_bytes: int) -> None:
        message = (
            f"NDJSON frame of {observed_bytes} bytes exceeds the {limit_bytes} byte frame limit"
            if limit == "frame"
            else f"NDJSON remainder of {observed_bytes} bytes exceeds the {limit_bytes} byte buffer limit"
        )
        super().__init__(message)
        self.limit: FrameLimit = limit
        self.limit_bytes = limit_bytes
        self.observed_bytes = observed_bytes


class NdjsonDecoder(Generic[T]):
    """Incremental newline-delimited JSON decoder.

    Sockets split frames anywhere, so decoding per chunk is wrong; this keeps a
    remainder buffer and only emits complete lines. Blank lines are skipped,
    matching the Rust reader.

    Bytes are decoded incrementally as well. Decoding each chunk on its own turns
    a character whose UTF-8 bytes straddle two chunks into U+FFFD, which silently
    corrupts every non-ASCII payload that lands on a chunk boundary; the
    incremental decoder holds the incomplete sequence until its remaining bytes
    arrive, so the character is emitted whole.
    """

    def __init__(self, *, max_frame_bytes: int = MAX_FRAME_BYTES, max_residual_bytes: int = MAX_RESIDUAL_BYTES) -> None:
        self._buffer = ""
        # Bytes of the decoded buffer, tracked incrementally so the cap check
        # costs O(chunk) rather than O(buffer).
        self._buffer_bytes = 0
        self._text = codecs.getincrementaldecoder("utf-8")("replace")
        self._max_frame_bytes = max_frame_bytes
        self._max_residual_bytes = max_residual_bytes

    @property
    def buffered_bytes(self) -> int:
        return self._buffer_bytes

    def push(self, chunk: bytes | bytearray | memoryview | str) -> list[T]:
        text = chunk if isinstance(chunk, str) else self._text.decode(bytes(chunk))
        if text == "":
            return []
        text_bytes = len(text.encode("utf-8"))
        # Refuse a chunk that cannot belong to an acceptable frame before
        # concatenating it: the concatenation would allocate a copy of a buffer
        # that is already too large. A chunk carrying a newline is let through,
        # because its frames are bounded by the per-frame check below.
        if self._buffer_bytes + text_bytes > self._max_residual_bytes and "\n" not in text:
            observed = self._buffer_bytes + text_bytes
            self._discard()
            raise FrameTooLargeError("remainder", self._max_residual_bytes, observed)
        self._buffer += text
        self._buffer_bytes += text_bytes

        out: list[T] = []
        index = self._buffer.find("\n")
        while index != -1:
            frame = self._buffer[:index]
            line = frame.strip()
            frame_bytes = len(frame.encode("utf-8"))
            self._buffer = self._buffer[index + 1 :]
            # The newline goes with the frame, so it leaves the buffer too.
            self._buffer_bytes -= frame_bytes + 1
            if frame_bytes > self._max_frame_bytes:
                self._discard()
                raise FrameTooLargeError("frame", self._max_frame_bytes, frame_bytes)
            if line != "":
                out.append(json.loads(line))
            index = self._buffer.find("\n")

        # The newline never arrived: the remainder is a frame that is still growing.
        if self._buffer_bytes > self._max_residual_bytes:
            observed = self._buffer_bytes
            self._discard()
            raise FrameTooLargeError("remainder", self._max_residual_bytes, observed)
        return out

    def _discard(self) -> None:
        """Drop the partial frame, its bytes, and any half-decoded character.

        A frame that broke a cap can never be completed, and what follows it on
        the wire cannot be trusted to start on a frame boundary. Starting clean
        means a caller that keeps reading sees the next frame rather than the
        tail of the rejected one, and no half-character from the rejected stream
        can leak into the next frame.
        """
        self._buffer = ""
        self._buffer_bytes = 0
        self._text.reset()


def encode_frame(frame: Any) -> str:
    """Encode one frame as an NDJSON line, preserving non-ASCII text as UTF-8."""
    return json.dumps(frame, ensure_ascii=False, separators=(",", ":")) + "\n"
