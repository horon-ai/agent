"""Contract for the change classifier that decides which CI gates run."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from classify_changes import classify  # noqa: E402


def pr(*files):
    return classify("pull_request", list(files))


class ClassifyChangesTests(unittest.TestCase):
    def test_docs_only_pull_request_skips_code_gates_but_keeps_docs_gates(self):
        result = pr("README.md", "docs/runtime/TOOLS.md", "docs/assets/diagram.svg")
        self.assertEqual(result, {"code": False, "docs": True, "docs_only": True})

    def test_any_code_file_forces_the_full_run(self):
        for extra in ("crates/base/src/lib.rs", "Cargo.lock", ".github/workflows/ci.yml",
                      "scripts/ci/check_panics.py", "src/docs/notes.rs"):
            with self.subTest(extra=extra):
                result = pr("docs/README.md", extra)
                self.assertTrue(result["code"])
                self.assertTrue(result["docs"])
                self.assertFalse(result["docs_only"])

    def test_code_without_docs_does_not_run_docs_gates(self):
        self.assertEqual(pr("crates/base/src/lib.rs"),
                         {"code": True, "docs": False, "docs_only": False})

    def test_non_embedded_prose_is_docs_only_but_needs_no_corpus_test(self):
        # AGENTS.md is not compiled into the binary; a template is not documentation
        # the product serves. Both still count as "no code changed".
        result = pr("AGENTS.md", ".github/pull_request_template.md")
        self.assertTrue(result["docs_only"])

    def test_push_dispatch_and_empty_diff_always_run_everything(self):
        full = {"code": True, "docs": False, "docs_only": False}
        self.assertEqual(classify("push", ["README.md"]), full)
        self.assertEqual(classify("workflow_dispatch", ["README.md"]), full)
        self.assertEqual(classify("pull_request", []), full)
        self.assertEqual(classify("pull_request", None), full)


if __name__ == "__main__":
    unittest.main()
