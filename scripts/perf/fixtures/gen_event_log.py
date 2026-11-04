#!/usr/bin/env python3
"""Generate a canonical session event log fixture (`events.jsonl`).

The output is the real on-disk form, not a toy format: one canonical JSON
`EventEnvelope` per line, exactly as `crates/session-store/src/schema.rs`
writes it. That means

  * keys sorted by UTF-8 byte order at every level, no whitespace between
    tokens (`schema.rs::canonicalize_value`, `canonical_json`),
  * `timestamp` in fixed UTC RFC3339 nanosecond form (`canonical_timestamp`),
  * `content_hash` = lowercase hex SHA-256 over the canonical JSON of the
    envelope with `content_hash` removed (`EventEnvelope::content_hash_of`),
  * `parent_event_id` present on every record but the session root, and
  * `payload` inline, so a later reader can decode the typed payload without a
    blob lookup.

A log is one `session_created` record followed by `message_appended` records
that chain parent-to-child, which is what `EventGraph` expects to replay.

Determinism: the generator uses its own SplitMix64 PRNG and a fixed word list,
so the same `--seed` and the same size argument always produce byte-identical
output. Nothing reads the clock, the environment, or a random source.

Usage:
    python3 scripts/perf/fixtures/gen_event_log.py --events 1000 --seed 20261021 --out DIR
    python3 scripts/perf/fixtures/gen_event_log.py --bytes 209715200 --seed 20261021 --out DIR

`--events N` writes exactly N records (1 session_created + N-1 message_appended).
`--bytes N` writes exactly N bytes, padding the last message's text so the file
lands on the requested size to the byte.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import pathlib
import sys
import uuid

EVENT_SCHEMA_VERSION = 1
LOG_FILENAME = "events.jsonl"

# Part of the fixture format: changing it changes every recorded hash.
FIXTURE_NAMESPACE = uuid.UUID("5f2b9c1e-0f3a-5b7d-8c41-2d6a9e0b7c33")

# 2026-01-01T00:00:00Z, the fixed epoch every fixture timestamp counts from.
BASE_SECONDS = 1_767_225_600
NS_PER_SECOND = 1_000_000_000
NS_PER_MS = 1_000_000
BASE_NS = BASE_SECONDS * NS_PER_SECOND
BASE_MILLIS = BASE_SECONDS * 1000

# Byte-budget mode stops adding regular records while more than this much
# budget remains, so the final padded record always has room.
MAX_MESSAGE_LINE_BYTES = 4096

MASK64 = (1 << 64) - 1

WORDS = (
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    "india", "juliet", "kilo", "lima", "mike", "november", "oscar", "papa",
    "quebec", "romeo", "sierra", "tango", "uniform", "victor", "whiskey",
    "xray", "yankee", "zulu", "perf", "fixture", "session", "store", "event",
    "log",
)


class Rng:
    """SplitMix64 with a fixed algorithm, so output never depends on the Python version."""

    def __init__(self, seed: int) -> None:
        self.state = seed & MASK64

    def next_u64(self) -> int:
        self.state = (self.state + 0x9E3779B97F4A7C15) & MASK64
        z = self.state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK64
        return z ^ (z >> 31)

    def below(self, bound: int) -> int:
        return self.next_u64() % bound


def canonical_json(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def rfc3339_nanos(total_ns: int) -> str:
    """UTC RFC3339 with fixed nanosecond precision, as canonical_timestamp writes it."""
    seconds, nanos = divmod(total_ns, NS_PER_SECOND)
    moment = datetime.datetime.fromtimestamp(seconds, datetime.timezone.utc)
    return moment.strftime("%Y-%m-%dT%H:%M:%S") + ".%09dZ" % nanos


def event_id(seed: int, index: int) -> str:
    return str(uuid.uuid5(FIXTURE_NAMESPACE, "event:%d:%d" % (seed, index)))


def origin_id(seed: int) -> str:
    return str(uuid.uuid5(FIXTURE_NAMESPACE, "origin:%d" % seed))


def message_seed(seed: int, index: int) -> int:
    return (seed * 0x9E3779B97F4A7C15 + index * 0x100000001B3) & MASK64


def session_created_payload(seed: int, session_id: str) -> dict:
    stamp = rfc3339_nanos(BASE_NS)
    return {
        "branch_root": False,
        "metadata": {
            "created_at": stamp,
            "updated_at": stamp,
            "title": "perf fixture event log %s" % session_id,
            "model": "gpt-5.1-codex",
            "provider_key": "openai",
            "working_dir": "/tmp/horon-perf-fixture",
            "is_canary": False,
            "saved": True,
            "migration_epoch": 0,
        },
    }


def message_appended_payload(seed: int, index: int, pad: int = 0) -> dict:
    """One `message_appended` payload for a 1-based message index.

    `pad` appends that many ASCII bytes to the first text block, which is how
    byte-budget mode lands on an exact file size. Padding never consumes PRNG
    draws, so padded and unpadded records share every other field.
    """
    rng = Rng(message_seed(seed, index))
    role = "user" if index % 2 == 1 else "assistant"
    word_count = 4 + rng.below(9)
    body = " ".join(WORDS[rng.below(len(WORDS))] for _ in range(word_count))
    text = "fixture message %d: %s" % (index, body)
    if pad:
        text = text + "x" * pad
    content = [{"type": "text", "text": text}]
    if index % 5 == 0:
        content.append({"type": "text", "text": "streamed continuation %d" % index})
    return {
        "message": {
            "id": "message_%d_%d" % (BASE_MILLIS + index, rng.next_u64()),
            "role": role,
            "content": content,
            "timestamp": rfc3339_nanos(BASE_NS + index * NS_PER_MS),
        }
    }


def build_line(
    session_id: str,
    seed: int,
    index: int,
    parent: str | None,
    kind: str,
    payload: dict,
) -> bytes:
    envelope = {
        "schema_version": EVENT_SCHEMA_VERSION,
        "session_id": session_id,
        "event_id": event_id(seed, index),
        "origin_id": origin_id(seed),
        "timestamp": rfc3339_nanos(BASE_NS + index * NS_PER_MS),
        "kind": kind,
        "payload": payload,
    }
    if parent is not None:
        envelope["parent_event_id"] = parent
    envelope["content_hash"] = sha256_hex(canonical_json(envelope).encode("utf-8"))
    return canonical_json(envelope).encode("utf-8") + b"\n"


def message_line(session_id: str, seed: int, index: int, pad: int = 0) -> bytes:
    return build_line(
        session_id,
        seed,
        index,
        event_id(seed, index - 1) if index > 1 else event_id(seed, 0),
        "message_appended",
        message_appended_payload(seed, index, pad),
    )


def header_line(session_id: str, seed: int) -> bytes:
    return build_line(
        session_id,
        seed,
        0,
        None,
        "session_created",
        session_created_payload(seed, session_id),
    )


def write_events(path: pathlib.Path, count: int, seed: int, session_id: str) -> tuple[int, int]:
    if count < 1:
        raise SystemExit("--events must be at least 1 (the session_created root)")
    written = 0
    with path.open("wb") as handle:
        line = header_line(session_id, seed)
        handle.write(line)
        written += len(line)
        for index in range(1, count):
            line = message_line(session_id, seed, index)
            if len(line) > MAX_MESSAGE_LINE_BYTES:
                raise SystemExit("message line %d is %d bytes, over the %d-byte bound" % (index, len(line), MAX_MESSAGE_LINE_BYTES))
            handle.write(line)
            written += len(line)
    return written, count


def write_bytes(path: pathlib.Path, target: int, seed: int, session_id: str) -> tuple[int, int]:
    written = 0
    records = 0
    with path.open("wb") as handle:
        line = header_line(session_id, seed)
        handle.write(line)
        written += len(line)
        records += 1
        if written > target:
            raise SystemExit("--bytes %d is smaller than the session_created root (%d bytes)" % (target, written))
        index = 1
        while target - written > MAX_MESSAGE_LINE_BYTES:
            line = message_line(session_id, seed, index)
            if len(line) > MAX_MESSAGE_LINE_BYTES:
                raise SystemExit("message line %d is %d bytes, over the %d-byte bound" % (index, len(line), MAX_MESSAGE_LINE_BYTES))
            handle.write(line)
            written += len(line)
            records += 1
            index += 1
        probe = message_line(session_id, seed, index)
        pad = (target - written) - len(probe)
        if pad < 0:
            raise SystemExit(
                "cannot land on %d bytes: %d bytes remain and the next record needs at least %d"
                % (target, target - written, len(probe))
            )
        final = message_line(session_id, seed, index, pad=pad)
        if len(final) != target - written:
            raise SystemExit(
                "padded record is %d bytes, expected %d; the size is not affine in padding"
                % (len(final), target - written)
            )
        handle.write(final)
        written += len(final)
        records += 1
    if written != target:
        raise SystemExit("wrote %d bytes, expected %d" % (written, target))
    return written, records


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Generate a canonical session event log fixture.")
    size = parser.add_mutually_exclusive_group(required=True)
    size.add_argument("--events", type=int, help="write exactly N records (1 session_created + N-1 message_appended)")
    size.add_argument("--bytes", type=int, help="write exactly N bytes, padding the last message")
    parser.add_argument("--seed", type=int, required=True, help="PRNG seed; the same seed reproduces the same bytes")
    parser.add_argument("--out", required=True, help="output directory (created if missing)")
    args = parser.parse_args(argv)

    out_dir = pathlib.Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / LOG_FILENAME

    if args.events is not None:
        session_id = "perf-events-%d" % args.events
        written, records = write_events(path, args.events, args.seed, session_id)
    else:
        session_id = "perf-bytes-%d" % args.bytes
        written, records = write_bytes(path, args.bytes, args.seed, session_id)

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)

    summary = {
        "path": str(path),
        "session_id": session_id,
        "seed": args.seed,
        "records": records,
        "bytes": written,
        "sha256": digest.hexdigest(),
    }
    print(canonical_json(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
