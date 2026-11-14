#!/usr/bin/env python3
"""Tests for the issue-reference gate.

The gate's value is that it separates "every reference resolves" from "could not
check", so both halves are proved here: the tree really carries the numbers the
gate extracts, a missing number is reported with the sites that mention it, and
the two allowances (the illustrative workflow, an exhausted API budget) behave as
documented rather than silently passing everything.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_issue_refs as gate


def ref(number: int) -> str:
    """Build a reference without writing one in this file.

    The gate reads every `#NNN` under `scripts/`, and this file lives there, so a
    literal fixture would be reported as a dead reference in the tree.
    """
    return "#" + str(number)


class ReferenceExtractionTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / "scripts/ci").mkdir(parents=True)

    def test_a_reference_is_recorded_with_its_site(self) -> None:
        (self.root / "scripts/ci/x.sh").write_text(
            "# why\n# because of " + ref(412) + "\n", encoding="utf-8"
        )
        self.assertEqual(gate.references(self.root), {412: ["scripts/ci/x.sh:2"]})

    def test_the_illustrative_workflow_is_skipped(self) -> None:
        path = self.root / ".github/workflows/require-issue.yml"
        path.write_text('# e.g. "Closes ' + ref(123) + '"\n', encoding="utf-8")
        self.assertEqual(gate.references(self.root), {})

    def test_the_tree_carries_no_illustrative_numbers(self) -> None:
        # Anchors the allowance to the file it was written for, so a copied
        # example elsewhere is still checked.
        self.assertEqual(gate.ILLUSTRATIVE, {".github/workflows/require-issue.yml"})


class ResolvabilityTests(unittest.TestCase):
    def test_a_missing_number_names_every_site(self) -> None:
        found = {416: ["a.yml:20"], 435: ["b.sh:4", "c.yml:155"]}
        self.assertEqual(
            gate.problems(found, {435}),
            [ref(416) + " does not exist in this repository: a.yml:20"],
        )

    def test_all_resolving_reports_nothing(self) -> None:
        self.assertEqual(gate.problems({226: ["ci.yml:314"]}, {226}), [])


class RepositoryCheckTests(unittest.TestCase):
    def test_the_real_tree_resolves_every_reference(self) -> None:
        root = Path(__file__).resolve().parents[2]
        found = gate.references(root)
        # The sweep in the commit that added this gate removed eleven dead
        # numbers; a failure here means one came back or a new one was written.
        self.assertTrue(found, "expected the tree to still cite issues")
        with mock.patch.object(gate, "existing_numbers", return_value=set(found)):
            self.assertEqual(gate.main(["--root", str(root), "--repo", "horon-ai/agent"]), 0)

    def test_a_rate_limit_is_reported_rather_than_failing_the_job(self) -> None:
        error = gate.subprocess.CalledProcessError(1, "gh", stderr="API rate limit exceeded for user")
        with mock.patch.object(gate.subprocess, "run", side_effect=error):
            self.assertEqual(gate.existing_numbers("horon-ai/agent"), set())

    def test_any_other_api_error_fails(self) -> None:
        error = gate.subprocess.CalledProcessError(1, "gh", stderr="HTTP 401: Bad credentials")
        with mock.patch.object(gate.subprocess, "run", side_effect=error):
            with self.assertRaises(SystemExit):
                gate.existing_numbers("horon-ai/agent")


if __name__ == "__main__":
    unittest.main()
