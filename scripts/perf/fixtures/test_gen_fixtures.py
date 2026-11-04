#!/usr/bin/env python3
"""Run from the repository root: python3 -m unittest scripts/perf/fixtures/test_gen_fixtures.py -v"""

from __future__ import annotations

import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
GEN_EVENT_LOG = HERE / "gen_event_log.py"
SEED = 20261021


def run(script: pathlib.Path, *args: str) -> dict:
    proc = subprocess.run(
        [sys.executable, str(script), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise AssertionError("%s %s failed: %s" % (script.name, " ".join(args), proc.stderr.strip()))
    return json.loads(proc.stdout)


def digest(path: pathlib.Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            hasher.update(block)
    return hasher.hexdigest()


def canonical(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def records(path: pathlib.Path) -> list[dict]:
    with path.open("rb") as handle:
        return [json.loads(line) for line in handle if line.strip()]


class EventLogFixtureTest(unittest.TestCase):
    def test_same_seed_and_events_produce_identical_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            run(GEN_EVENT_LOG, "--events", "200", "--seed", str(SEED), "--out", first)
            run(GEN_EVENT_LOG, "--events", "200", "--seed", str(SEED), "--out", second)
            a = pathlib.Path(first) / "events.jsonl"
            b = pathlib.Path(second) / "events.jsonl"
            self.assertEqual(a.read_bytes(), b.read_bytes())
            self.assertEqual(digest(a), digest(b))

    def test_same_seed_and_bytes_produce_identical_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            run(GEN_EVENT_LOG, "--bytes", "100000", "--seed", str(SEED), "--out", first)
            run(GEN_EVENT_LOG, "--bytes", "100000", "--seed", str(SEED), "--out", second)
            a = pathlib.Path(first) / "events.jsonl"
            b = pathlib.Path(second) / "events.jsonl"
            self.assertEqual(a.read_bytes(), b.read_bytes())
            self.assertEqual(a.stat().st_size, 100000)

    def test_different_seed_changes_the_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            run(GEN_EVENT_LOG, "--events", "50", "--seed", "1", "--out", first)
            run(GEN_EVENT_LOG, "--events", "50", "--seed", "2", "--out", second)
            self.assertNotEqual(
                digest(pathlib.Path(first) / "events.jsonl"),
                digest(pathlib.Path(second) / "events.jsonl"),
            )

    def test_event_count_is_exact_and_root_is_the_only_parentless_record(self) -> None:
        with tempfile.TemporaryDirectory() as out:
            run(GEN_EVENT_LOG, "--events", "40", "--seed", str(SEED), "--out", out)
            parsed = records(pathlib.Path(out) / "events.jsonl")
        self.assertEqual(len(parsed), 40)
        self.assertEqual(parsed[0]["kind"], "session_created")
        self.assertNotIn("parent_event_id", parsed[0])
        self.assertTrue(all(record["kind"] == "message_appended" for record in parsed[1:]))
        for index, record in enumerate(parsed[1:], start=1):
            self.assertEqual(record["parent_event_id"], parsed[index - 1]["event_id"])

    def test_every_content_hash_recomputes(self) -> None:
        with tempfile.TemporaryDirectory() as out:
            run(GEN_EVENT_LOG, "--events", "30", "--seed", str(SEED), "--out", out)
            parsed = records(pathlib.Path(out) / "events.jsonl")
        for record in parsed:
            stored = record.pop("content_hash")
            recomputed = hashlib.sha256(canonical(record).encode("utf-8")).hexdigest()
            self.assertEqual(recomputed, stored)
            record["content_hash"] = stored

    def test_envelope_carries_the_store_schema_fields(self) -> None:
        with tempfile.TemporaryDirectory() as out:
            run(GEN_EVENT_LOG, "--events", "5", "--seed", str(SEED), "--out", out)
            parsed = records(pathlib.Path(out) / "events.jsonl")
        for record in parsed:
            self.assertEqual(record["schema_version"], 1)
            self.assertEqual(sorted(record), sorted([
                "schema_version", "session_id", "event_id", "parent_event_id",
                "origin_id", "timestamp", "kind", "payload", "content_hash",
            ]) if "parent_event_id" in record else sorted([
                "schema_version", "session_id", "event_id",
                "origin_id", "timestamp", "kind", "payload", "content_hash",
            ]))
            self.assertNotIn("payload_ref", record)
            self.assertRegex(record["timestamp"], r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{9}Z$")


if __name__ == "__main__":
    unittest.main()
