"""Tests for the growth ratchet gate.

Each case builds a synthetic repository in a temporary directory, so the gate
can be proved to fail on growth without adding a real dependency or a legacy
marker to this tree.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_growth_ratchets as gate


LOCK_COUNTER = {"kind": "cargo_lock_packages"}
MARKER_COUNTER = {
    "kind": "regex_lines",
    "pattern": "legacy|deprecated|compat_alias|back.?compat",
}


class GrowthRatchetTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        (self.root / "scripts" / "ci").mkdir(parents=True)
        for root in ("src", "crates"):
            (self.root / root).mkdir()

    # -- builders ---------------------------------------------------------

    def write_ratchet(
        self,
        filename: str = "count.ratchet.json",
        ceiling: int = 1,
        counter: dict | None = None,
        name: str = "test-ratchet",
        description: str = "what this protects",
        **extra: object,
    ) -> Path:
        path = self.root / "scripts" / "ci" / filename
        body = {
            "version": 1,
            "name": name,
            "description": description,
            "counter": counter if counter is not None else LOCK_COUNTER,
            "ceiling": ceiling,
            **extra,
        }
        path.write_text(json.dumps(body, indent=2) + "\n", encoding="utf-8")
        return path

    def write_lockfile(self, packages: int) -> None:
        lines = ["# generated", "version = 4", ""]
        for index in range(packages):
            lines += ["[[package]]", f'name = "crate{index}"', 'version = "0.1.0"', ""]
        (self.root / "Cargo.lock").write_text("\n".join(lines), encoding="utf-8")

    def ensure_roots(self) -> None:
        """The default scan roots must exist; the gate fails rather than narrow."""
        for root in ("src", "crates"):
            (self.root / root).mkdir(exist_ok=True)

    def write_source(self, relative: str, text: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def run_gate(self, *extra: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gate.main(["--root", str(self.root), *extra])
        return code, out.getvalue(), err.getvalue()

    # -- lockfile packages ------------------------------------------------

    def test_lockfile_ceiling_holds_at_the_recorded_count(self):
        self.write_lockfile(3)
        self.write_ratchet(ceiling=3)
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("test-ratchet OK: 3/3", out)

    def test_one_more_dependency_fails_the_check(self):
        self.write_ratchet(ceiling=3)
        self.write_lockfile(4)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("test-ratchet: 4 > ceiling 3 (+1)", err)

    def test_counting_ignores_comments_and_duplicates_count_twice(self):
        self.write_lockfile(2)
        lockfile = self.root / "Cargo.lock"
        lockfile.write_text(
            "# [[package]] in a comment is not a package\n"
            "[[package]]\n"
            'name = "a"\nversion = "0.1.0"\n'
            "[[package]]\n"
            'name = "a"\nversion = "0.2.0"\n',
            encoding="utf-8",
        )
        self.write_ratchet(ceiling=2)
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("2/2", out)

    def test_a_dropped_count_passes_and_is_reported(self):
        # A cleanup in another change must not be forced to edit the ceiling,
        # and a space between the count and the old ceiling still fails as soon
        # as anything grows back.
        self.write_lockfile(2)
        self.write_ratchet(ceiling=3)
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("test-ratchet: 2 < ceiling 3", out)
        self.assertIn("--update", out)
        self.write_lockfile(4)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("4 > ceiling 3", err)

    def test_update_re_pins_only_the_ceiling(self):
        self.write_lockfile(4)
        path = self.write_ratchet(ceiling=3)
        before = json.loads(path.read_text(encoding="utf-8"))
        code, out, err = self.run_gate("--update")
        self.assertEqual(code, 0, err)
        after = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(after["ceiling"], 4)
        for key in ("version", "name", "description", "counter"):
            self.assertEqual(after[key], before[key])
        self.assertIn("ceiling 3 -> 4", out)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 0, err)

    def test_missing_lockfile_is_a_failure_not_a_zero(self):
        self.write_ratchet(ceiling=0)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("Cargo.lock missing", err)

    # -- legacy markers ---------------------------------------------------

    def test_marker_ceiling_counts_matching_lines_case_insensitively(self):
        self.write_source("src/lib.rs", "fn a() {}\n")
        self.write_source("crates/base/src/x.rs", "// Legacy path\n// back-compat\n")
        self.write_ratchet(ceiling=2, counter=MARKER_COUNTER)
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("2/2", out)

    def test_one_new_internal_marker_fails_the_check(self):
        self.write_source("crates/base/src/x.rs", "// legacy path\n")
        self.write_ratchet(ceiling=1, counter=MARKER_COUNTER)
        self.write_source("crates/base/src/y.rs", "fn deprecated_thing() {}\n")
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("test-ratchet: 2 > ceiling 1 (+1)", err)
        self.assertIn("crates/base/src/y.rs", err)

    def test_excluded_external_contract_subtree_is_not_counted(self):
        self.write_source("src/lib.rs", "// legacy\n")
        self.write_source("crates/harness-api-server/src/translate.rs", "// legacy legacy\n")
        counter = dict(MARKER_COUNTER, exclude=[
            {"path": "crates/harness-api-server", "reason": "harness API v1 contract"},
        ])
        self.write_ratchet(ceiling=1, counter=counter)
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("1/1", out)

        # Growth inside the excluded contract stays out of the count.
        self.write_source("crates/harness-api-server/src/more.rs", "// legacy\n" * 10)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 0, err)

    def test_exclude_without_a_reason_fails(self):
        self.write_source("src/lib.rs", "// legacy\n")
        counter = dict(MARKER_COUNTER, exclude=[{"path": "crates/harness-api-server"}])
        (self.root / "crates" / "harness-api-server").mkdir(parents=True)
        self.write_ratchet(ceiling=1, counter=counter)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("needs a reason", err)

    def test_exclude_of_a_missing_path_fails(self):
        self.write_source("src/lib.rs", "// legacy\n")
        counter = dict(MARKER_COUNTER, exclude=[
            {"path": "crates/deleted-crate", "reason": "stale entry"},
        ])
        self.write_ratchet(ceiling=1, counter=counter)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("does not exist", err)

    def test_root_that_does_not_exist_is_a_failure(self):
        (self.root / "crates").rmdir()
        self.write_ratchet(ceiling=1, counter=MARKER_COUNTER)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("scan root missing", err)

    def test_extensions_narrow_the_scan(self):
        self.write_source("src/lib.rs", "// legacy\n")
        self.write_source("src/notes.txt", "// legacy\n// legacy\n")
        counter = dict(MARKER_COUNTER, extensions=[".txt"])
        self.write_ratchet(ceiling=2, counter=counter)
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("2/2", out)

    # -- discovery by naming rule -----------------------------------------

    def test_a_second_ratchet_is_picked_up_without_a_workflow_edit(self):
        self.write_lockfile(2)
        self.write_ratchet("lockfile.ratchet.json", ceiling=2, name="lockfile-packages")
        self.write_source("crates/base/src/x.rs", "fn f() -> Result<(), String> {}\n")
        # A later ratchet such as #156's `Result<_, String>` count: the suffix is
        # not `*.ratchet.json`, and it is still discovered.
        self.write_ratchet(
            "result_string_budget.json",
            ceiling=1,
            name="result-strings",
            counter={"kind": "regex_lines", "pattern": r"Result<[^>]*,\s*String>"},
        )
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("lockfile-packages OK: 2/2", out)
        self.assertIn("result-strings OK: 1/1", out)

    def test_sibling_budget_files_are_not_treated_as_growth_ratchets(self):
        self.write_lockfile(1)
        self.write_ratchet(ceiling=1)
        sibling = self.root / "scripts" / "ci" / "panic_budget.json"
        sibling.write_text(
            json.dumps({"version": 1, "total": 3, "tracked_files": {"src/a.rs": 3}}),
            encoding="utf-8",
        )
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("All 1 growth ratchets hold", out)

    def test_duplicate_ratchet_names_fail(self):
        self.write_lockfile(1)
        self.write_ratchet("a.json", ceiling=1, name="same")
        self.write_ratchet("b.json", ceiling=1, name="same")
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("duplicate ratchet name(s): same", err)

    def test_malformed_ratchet_file_fails_instead_of_being_skipped(self):
        self.write_lockfile(1)
        self.write_ratchet(ceiling=1)
        (self.root / "scripts" / "ci" / "broken.json").write_text("{", encoding="utf-8")
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("cannot read JSON", err)

    def test_invalid_ceiling_and_unknown_counter_fail(self):
        self.write_lockfile(1)
        self.write_ratchet(ceiling=-1)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("ceiling must be a non-negative integer", err)

        self.write_ratchet(ceiling=1, counter={"kind": "magic"})
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("unknown counter kind", err)

    def test_bad_pattern_fails_with_the_regex_error(self):
        self.write_ratchet(ceiling=1, counter={"kind": "regex_lines", "pattern": "("})
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("does not compile", err)

    def test_missing_description_fails(self):
        self.write_lockfile(1)
        self.write_ratchet(ceiling=1, description="")
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("description", err)

    def test_empty_ratchet_set_fails(self):
        self.write_lockfile(1)
        code, _out, err = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("no growth ratchet file", err)

    def test_list_prints_the_ceiling_and_each_exclusion_reason(self):
        self.write_source("crates/harness-api-server/src/lib.rs", "")
        counter = dict(MARKER_COUNTER, exclude=[
            {"path": "crates/harness-api-server", "reason": "harness API v1 contract"},
        ])
        self.write_ratchet("markers.ratchet.json", ceiling=1, counter=counter)
        code, out, err = self.run_gate("--list")
        self.assertEqual(code, 0, err)
        self.assertIn("test-ratchet  ceiling=1", out)
        self.assertIn("exclude crates/harness-api-server: harness API v1 contract", out)


class RealRepositoryRatchetTests(unittest.TestCase):
    """The committed ratchet files must match this tree."""

    def setUp(self) -> None:
        self.root = Path(__file__).resolve().parents[2]

    def run_gate(self, *extra: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gate.main(["--root", str(self.root), *extra])
        return code, out.getvalue(), err.getvalue()

    def test_committed_ratchets_hold_on_this_tree(self):
        code, out, err = self.run_gate()
        self.assertEqual(code, 0, err)
        self.assertIn("lockfile-packages OK", out)
        self.assertIn("internal-legacy-markers OK", out)

    def test_every_exclusion_names_a_reason_and_an_internal_scope(self):
        ratchets = gate.load_ratchets(self.root)
        self.assertGreaterEqual(len(ratchets), 2)
        for ratchet in ratchets:
            for entry in ratchet["counter"].get("exclude", []):
                with self.subTest(ratchet=ratchet["name"], path=entry["path"]):
                    self.assertGreater(len(entry["reason"]), 20)
                    self.assertTrue((self.root / entry["path"]).exists())


if __name__ == "__main__":
    unittest.main()
