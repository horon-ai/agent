#!/usr/bin/env python3
"""Tests for scripts/perf/ab_perf.py.

Run with `python3 -m unittest scripts/perf/test_ab_perf.py`. The refusal cases matter
more than the happy path: a runner that reports a number it cannot support is
the defect this file exists to catch.
"""

import json
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import ab_perf  # noqa: E402


def write_fake_binary(directory, name, version, body=None):
    """A shell stand-in that answers `version --json` and then does `body`."""
    path = os.path.join(directory, name)
    payload = json.dumps(version)
    script = textwrap.dedent(
        """\
        #!/bin/sh
        if [ "$1" = "version" ]; then
          cat <<'JSON'
        %s
        JSON
          exit 0
        fi
        %s
        """
        % (payload, body or "exit 0")
    )
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(script)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return path


def version_payload(profile="release", features=("pdf",), git_hash="abc1234"):
    return {
        "version": "v0.98.1-dev (%s)" % git_hash,
        "semver": "0.98.1",
        "git_hash": git_hash,
        "build_profile": profile,
        "build_features": list(features),
        "release_build": profile == "release",
    }


class ProfileMismatchTest(unittest.TestCase):
    def test_refuses_a_candidate_built_with_a_different_profile(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = write_fake_binary(directory, "b0", version_payload("release"))
            candidate = write_fake_binary(directory, "cand", version_payload("debug"))
            emit = os.path.join(directory, "report.json")

            completed = subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_perf.py"),
                    "--baseline",
                    baseline,
                    "--candidate",
                    candidate,
                    "--iterations",
                    "1",
                    "--warmup",
                    "0",
                    "--emit",
                    emit,
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

            self.assertNotEqual(completed.returncode, 0, "a profile mismatch must not exit 0")
            stderr = completed.stderr.decode()
            self.assertIn("profile differs", stderr)
            self.assertIn("release", stderr)
            self.assertIn("debug", stderr)
            self.assertFalse(os.path.exists(emit), "no report may be written for a refused pair")

    def test_refuses_a_candidate_built_with_a_different_feature_set(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = write_fake_binary(directory, "b0", version_payload(features=("pdf",)))
            candidate = write_fake_binary(
                directory, "cand", version_payload(features=("pdf", "jemalloc"))
            )
            completed = subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_perf.py"),
                    "--baseline",
                    baseline,
                    "--candidate",
                    candidate,
                    "--iterations",
                    "1",
                    "--warmup",
                    "0",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("features differs", completed.stderr.decode())

    def test_require_comparable_names_the_differing_field(self):
        baseline = {"profile": "release", "features": ["pdf"]}
        candidate = {"profile": "debug", "features": ["pdf"]}
        with self.assertRaises(ab_perf.RunnerError) as raised:
            ab_perf.require_comparable(baseline, candidate)
        self.assertIn("profile", str(raised.exception))

    def test_missing_profile_is_a_refusal_not_a_pass(self):
        with self.assertRaises(ab_perf.RunnerError):
            ab_perf.require_comparable({"profile": None, "features": []}, {"profile": None, "features": []})


class ControlRunTest(unittest.TestCase):
    def test_same_binary_twice_reports_no_improvement(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = write_fake_binary(directory, "b0", version_payload())
            emit = os.path.join(directory, "report.json")
            completed = subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_perf.py"),
                    "--baseline",
                    binary,
                    "--candidate",
                    binary,
                    "--iterations",
                    "6",
                    "--warmup",
                    "1",
                    "--emit",
                    emit,
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr.decode())
            with open(emit, encoding="utf-8") as handle:
                report = json.load(handle)
            self.assertTrue(report["same_binary"])
            # Timing on a shared runner decides between the two control outcomes;
            # only the facts that do not depend on timing are asserted here.
            self.assertIn(report["adoption_decision"], ("control-no-change", "control-drift"))
            self.assertNotIn(
                report["adoption_decision"], ("adopt-pending-correctness", "reject-regression")
            )
            self.assertIn("no improvement", report["interpretation"].lower())

    def test_same_binary_beyond_the_band_is_drift_in_both_directions(self):
        counts = {"failure": 0, "timeout": 0}
        for delta_ms in (-5.0, 5.0):
            text, decision = ab_perf.interpret(
                delta_ms, delta_ms * 10, 1.0, False, True, counts, 6
            )
            self.assertEqual(decision, "control-drift", "delta %+.1f ms" % delta_ms)
            self.assertIn("runner noise", text)

    def test_different_binaries_keep_their_decisions(self):
        counts = {"failure": 0, "timeout": 0}
        cases = (
            ((-5.0, -50.0, 1.0, False), "adopt-pending-correctness"),
            ((5.0, 50.0, 1.0, False), "reject-regression"),
            ((0.5, 5.0, 1.0, True), "not-adopted-no-measurable-change"),
        )
        for (delta_ms, delta_pct, band_ms, within), expected in cases:
            _, decision = ab_perf.interpret(delta_ms, delta_pct, band_ms, within, False, counts, 6)
            self.assertEqual(decision, expected)


class ReportShapeTest(unittest.TestCase):
    def test_report_carries_every_required_key(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = write_fake_binary(directory, "b0", version_payload())
            emit = os.path.join(directory, "report.json")
            subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_perf.py"),
                    "--baseline",
                    binary,
                    "--candidate",
                    binary,
                    "--iterations",
                    "2",
                    "--warmup",
                    "0",
                    "--work-id",
                    "unit-test",
                    "--rollback-commit",
                    "deadbee",
                    "--emit",
                    emit,
                ],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            with open(emit, encoding="utf-8") as handle:
                report = json.load(handle)
            for key in (
                "work_id",
                "baseline",
                "candidate",
                "machine",
                "toolchain",
                "source",
                "workload",
                "fixtures",
                "iterations",
                "counts",
                "timing",
                "resources",
                "correctness",
                "interpretation",
                "adoption_decision",
                "rollback_commit",
            ):
                self.assertIn(key, report, "report is missing %s" % key)
            self.assertEqual(report["work_id"], "unit-test")
            self.assertEqual(report["rollback_commit"], "deadbee")
            self.assertEqual(report["baseline"]["sha256"], report["candidate"]["sha256"])
            self.assertIn("median_ms", report["timing"]["baseline"])
            self.assertIn("p95_ms", report["timing"]["baseline"])
            self.assertIn("samples", report["timing"]["baseline"])
            for key in ("failure", "timeout", "missing"):
                self.assertIn(key, report["counts"])
            self.assertIsNotNone(report["baseline"]["profile"])

    def test_missing_metric_is_named_and_never_becomes_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = write_fake_binary(directory, "b0", version_payload())
            emit = os.path.join(directory, "report.json")
            subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_perf.py"),
                    "--baseline",
                    binary,
                    "--candidate",
                    binary,
                    "--iterations",
                    "1",
                    "--warmup",
                    "0",
                    "--emit",
                    emit,
                ],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            with open(emit, encoding="utf-8") as handle:
                report = json.load(handle)
            self.assertIn("read_bytes", report["resources"]["missing_metrics"])
            self.assertIsNone(report["resources"]["baseline"]["read_bytes"])
            self.assertGreater(report["counts"]["missing"], 0)

    def test_require_counters_fails_when_the_workload_reports_none(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = write_fake_binary(directory, "b0", version_payload())
            completed = subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_perf.py"),
                    "--baseline",
                    binary,
                    "--candidate",
                    binary,
                    "--iterations",
                    "1",
                    "--warmup",
                    "0",
                    "--require-counters",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("no stage counters", completed.stderr.decode())


class PercentileTest(unittest.TestCase):
    def test_p99_is_refused_below_twenty_samples(self):
        summary = ab_perf.summarize([1.0] * 19, extra_p99=True)
        self.assertIsNone(summary["p99_ms"])
        self.assertIn("requires at least 20 samples", summary["p99_refused"])

    def test_p99_keys_are_always_present_and_never_silently_absent(self):
        # A consumer reads report["p99_ms"] or report["p99_refused"]; dropping
        # either key makes the metric silently absent instead of refused.
        small = [1.0, 2.0, 3.0]
        cases = (
            (small, False, "3"),
            (small, True, "3"),
            ([float(value) for value in range(25)], False, None),
        )
        for samples, extra_p99, refused_count in cases:
            with self.subTest(samples=len(samples), extra_p99=extra_p99):
                summary = ab_perf.summarize(samples, extra_p99=extra_p99)
                self.assertIn("p99_ms", summary)
                self.assertIn("p99_refused", summary)
                if refused_count is None:
                    self.assertIsNotNone(summary["p99_ms"])
                    self.assertIsNone(summary["p99_refused"])
                else:
                    self.assertIsNone(summary["p99_ms"])
                    self.assertIn(
                        "got %s" % refused_count,
                        summary["p99_refused"],
                        "the refusal must name the sample count it actually had",
                    )

    def test_p99_is_reported_at_twenty_samples(self):
        summary = ab_perf.summarize([float(value) for value in range(20)], extra_p99=True)
        self.assertIsNotNone(summary["p99_ms"])
        self.assertIsNone(summary["p99_refused"])

    def test_percentiles_interpolate(self):
        self.assertEqual(ab_perf.percentile([1.0, 2.0, 3.0], 0.5), 2.0)
        self.assertEqual(ab_perf.percentile([], 0.5), None)
        self.assertEqual(ab_perf.percentile([5.0], 0.99), 5.0)


class NoiseBandTest(unittest.TestCase):
    def test_band_has_a_floor_so_tight_spread_cannot_fake_a_win(self):
        band, method = ab_perf.noise_band([100.0, 100.0, 100.0], 100.0)
        self.assertAlmostEqual(band, 1.0)
        self.assertIn("MAD", method)

    def test_band_widens_with_real_spread(self):
        band, _ = ab_perf.noise_band([80.0, 100.0, 120.0], 100.0)
        self.assertGreater(band, 1.0)

    def test_delta_inside_the_band_is_not_an_improvement(self):
        interpretation, decision = ab_perf.interpret(-0.2, -0.2, 5.0, True, False, {}, 10)
        self.assertEqual(decision, "not-adopted-no-measurable-change")
        self.assertIn("inside the noise band", interpretation)

    def test_delta_outside_the_band_downward_is_a_candidate_gain(self):
        _, decision = ab_perf.interpret(-40.0, -20.0, 5.0, False, False, {}, 10)
        self.assertEqual(decision, "adopt-pending-correctness")

    def test_delta_outside_the_band_upward_is_a_regression(self):
        _, decision = ab_perf.interpret(40.0, 20.0, 5.0, False, False, {}, 10)
        self.assertEqual(decision, "reject-regression")


class TimeoutTest(unittest.TestCase):
    def test_a_hanging_child_is_killed_and_reported_as_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = write_fake_binary(
                directory, "b0", version_payload(), body="sleep 30\n"
            )
            emit = os.path.join(directory, "report.json")
            completed = subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_perf.py"),
                    "--baseline",
                    binary,
                    "--candidate",
                    binary,
                    "--iterations",
                    "1",
                    "--warmup",
                    "0",
                    "--timeout",
                    "1",
                    "--workload",
                    '["{binary}", "hang"]',
                    "--emit",
                    emit,
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=120,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr.decode())
            with open(emit, encoding="utf-8") as handle:
                report = json.load(handle)
            self.assertGreater(report["counts"]["timeout"], 0)
            self.assertEqual(report["adoption_decision"], "blocked")
            self.assertEqual(report["timing"]["baseline"]["samples"], 0)


class CounterParsingTest(unittest.TestCase):
    def test_reads_the_counter_object_out_of_mixed_output(self):
        text = 'log line\n{"enabled": true, "io_read_bytes": 4096, "queue_depth_high_water": 7}\ntrailing\n'
        counters = ab_perf.parse_counters(text)
        self.assertEqual(counters["io_read_bytes"], 4096)
        self.assertEqual(counters["queue_depth_high_water"], 7)

    def test_no_counter_object_returns_none(self):
        self.assertIsNone(ab_perf.parse_counters("just a log line"))
        self.assertIsNone(ab_perf.parse_counters(""))


if __name__ == "__main__":
    unittest.main()
