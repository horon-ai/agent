from __future__ import annotations

import json

import pytest

from horon_agent_sdk.framing import (
    MAX_FRAME_BYTES,
    MAX_RESIDUAL_BYTES,
    FrameTooLargeError,
    NdjsonDecoder,
)

#: Korean + emoji + a CJK character: every one is multi-byte in UTF-8.
SPLIT_PAYLOAD = "가나다😀한글"


def frame_for(text: str) -> bytes:
    return (
        json.dumps({"v": 1, "ev": "text_delta", "session_id": "s1", "text": text}, ensure_ascii=False).encode("utf-8")
        + b"\n"
    )


def rejection(run, limit: str) -> FrameTooLargeError:
    with pytest.raises(FrameTooLargeError) as caught:
        run()
    assert caught.value.limit == limit
    return caught.value


def test_decoder_reassembles_frames_split_across_chunks() -> None:
    decoder: NdjsonDecoder[dict] = NdjsonDecoder()
    assert decoder.push('{"v":1,"ev":"p') == []
    assert decoder.push('ong"}\n\n{"v":1,"ev":"ok"}\n') == [{"v": 1, "ev": "pong"}, {"v": 1, "ev": "ok"}]


def test_a_frame_split_at_every_byte_offset_decodes_to_the_original_text() -> None:
    wire = frame_for(SPLIT_PAYLOAD)
    expected = [{"v": 1, "ev": "text_delta", "session_id": "s1", "text": SPLIT_PAYLOAD}]
    for split in range(1, len(wire)):
        decoder: NdjsonDecoder[dict] = NdjsonDecoder()
        frames = [*decoder.push(wire[:split]), *decoder.push(wire[split:])]
        assert frames == expected, f"split at byte {split} of {len(wire)}"
        assert frames[0]["text"] == SPLIT_PAYLOAD
        assert "\ufffd" not in json.dumps(frames, ensure_ascii=False)


def test_a_frame_split_byte_by_byte_decodes_to_the_original_text() -> None:
    wire = frame_for(SPLIT_PAYLOAD)
    decoder: NdjsonDecoder[dict] = NdjsonDecoder()
    frames: list[dict] = []
    for byte in wire:
        frames.extend(decoder.push(bytes([byte])))
    assert frames == [{"v": 1, "ev": "text_delta", "session_id": "s1", "text": SPLIT_PAYLOAD}]


def test_the_frame_cap_is_the_bridges_64_mib() -> None:
    assert MAX_FRAME_BYTES == 64 * 1024 * 1024
    assert MAX_RESIDUAL_BYTES <= MAX_FRAME_BYTES


def test_an_oversized_frame_is_rejected_and_the_next_valid_frame_still_decodes() -> None:
    decoder: NdjsonDecoder[dict] = NdjsonDecoder(max_frame_bytes=64)
    oversized = (json.dumps({"ev": "text_delta", "text": "x" * 200}) + "\n").encode()
    error = rejection(lambda: decoder.push(oversized), "frame")
    assert error.limit_bytes == 64
    assert error.observed_bytes > 64
    assert decoder.buffered_bytes == 0
    assert decoder.push(b'{"ev":"pong"}\n') == [{"ev": "pong"}]


def test_the_residual_buffer_never_exceeds_its_cap_and_refuses_an_unterminated_frame() -> None:
    cap = 1024
    decoder: NdjsonDecoder[dict] = NdjsonDecoder(max_frame_bytes=cap, max_residual_bytes=cap)
    chunk = b"y" * 100

    def flood() -> None:
        for _ in range(100):
            decoder.push(chunk)

    error = rejection(flood, "remainder")
    assert error.observed_bytes <= cap + len(chunk), "refused before concatenating past one chunk"
    assert decoder.buffered_bytes <= cap
    assert decoder.push(b'{"ev":"ok"}\n') == [{"ev": "ok"}]


def test_a_rejected_frame_does_not_leak_its_half_character_into_the_next_stream() -> None:
    decoder: NdjsonDecoder[dict] = NdjsonDecoder(max_frame_bytes=64, max_residual_bytes=64)
    korean = '{"text":"가'.encode()
    decoder.push(korean[:-2])
    rejection(lambda: decoder.push(b"z" * 128), "remainder")
    following = '{"text":"나"}\n'.encode()
    frames = [*decoder.push(following[:3]), *decoder.push(following[3:])]
    assert frames == [{"text": "나"}]


def test_a_large_frame_keeps_the_trailing_remainder_intact() -> None:
    decoder: NdjsonDecoder[dict] = NdjsonDecoder()
    big = {"ev": "text_delta", "text": "가" * 200_000}
    wire = (json.dumps(big, ensure_ascii=False) + "\n").encode() + b'{"ev":"po'
    assert decoder.push(wire) == [big]
    assert decoder.buffered_bytes == len(b'{"ev":"po')
    assert decoder.push(b'ng"}\n') == [{"ev": "pong"}]
