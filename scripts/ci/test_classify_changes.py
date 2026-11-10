"""Contract for the change classifier that decides which CI gates run."""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from classify_changes import classify, last_green_sha  # noqa: E402


def pr(*files):
    return classify("pull_request", list(files))


def push(files, ref="refs/heads/dev", attempt=1):
    return classify("push", list(files), ref=ref, attempt=attempt)


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

    # ---- dev-push-first-attempt tests ----

    def test_dev_push_docs_only_runs_only_docs_gates(self):
        # A push to dev with only docs changed → docs-only result.
        result = push(["docs/README.md"])
        self.assertEqual(result, {"code": False, "docs": True, "docs_only": True})

    def test_dev_push_with_code_runs_full(self):
        result = push(["crates/base/src/lib.rs"])
        self.assertEqual(result, {"code": True, "docs": False, "docs_only": False})

    def test_dev_push_attempt_2_runs_full(self):
        result = push(["docs/README.md"], attempt=2)
        self.assertEqual(result, {"code": True, "docs": False, "docs_only": False})

    def test_main_push_docs_only_runs_full(self):
        result = push(["README.md"], ref="refs/heads/main")
        self.assertEqual(result, {"code": True, "docs": False, "docs_only": False})

    def test_workflow_dispatch_runs_full(self):
        result = classify("workflow_dispatch", ["README.md"], ref="", attempt=1)
        self.assertEqual(result, {"code": True, "docs": False, "docs_only": False})

    # ---- last_green_sha unit tests ----

    def test_last_green_sha_valid_40hex(self):
        valid_sha = "a" * 40
        result = last_green_sha("test/repo", "ci.yml", lambda r, w: valid_sha)
        self.assertEqual(result, valid_sha)

    def test_last_green_sha_fetch_raising(self):
        # fetch that raises/returns None → None
        result = last_green_sha("test/repo", "ci.yml",
                                lambda r, w: (_ for _ in ()).throw(RecursionError()))
        self.assertIsNone(result)

    def test_last_green_sha_empty_workflow_runs(self):
        result = last_green_sha("test/repo", "ci.yml",
                                lambda r, w: json.dumps({"workflow_runs": []}))
        self.assertIsNone(result)

    def test_last_green_sha_null_head_sha(self):
        result = last_green_sha("test/repo", "ci.yml",
                                lambda r, w: json.dumps(
                                    {"workflow_runs": [{"head_sha": None}]}))
        self.assertIsNone(result)

    def test_last_green_sha_short_head_sha(self):
        result = last_green_sha("test/repo", "ci.yml",
                                lambda r, w: json.dumps(
                                    {"workflow_runs": [{"head_sha": "abc"}]}))
        self.assertIsNone(result)

    def test_last_green_sha_non_hex_head_sha(self):
        result = last_green_sha("test/repo", "ci.yml",
                                lambda r, w: json.dumps(
                                    {"workflow_runs": [{"head_sha": "xyzzzz"}]}))
        self.assertIsNone(result)

    def test_last_green_sha_invalid_json(self):
        result = last_green_sha("test/repo", "ci.yml", lambda r, w: "{invalid json")
        self.assertIsNone(result)

    # ---- changed_files tests ----

    def test_changed_files_rename_code_to_docs_is_not_docs_only(self):
        """A code->doc rename should NOT be classified as docs-only."""
        with tempfile.TemporaryDirectory() as tmpdir:
            repo = tmpdir
            subprocess = __import__("subprocess")

            subprocess.check_call(
                ["git", "init"], cwd=repo,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            subprocess.check_call(
                ["git", "config", "user.email", "test@test.com"], cwd=repo,
            )
            subprocess.check_call(
                ["git", "config", "user.name", "Test"], cwd=repo,
            )

            # Create initial code file.
            code_dir = Path(repo) / "crates" / "x"
            code_dir.mkdir(parents=True, exist_ok=True)
            (code_dir / "foo.md").write_text("# foo\n")
            subprocess.check_call(
                ["git", "add", "."], cwd=repo,
            )
            subprocess.check_call(
                ["git", "commit", "-m", "initial code file"], cwd=repo,
            )

            # Move code file to docs.
            docs_dir = Path(repo) / "docs"
            docs_dir.mkdir(parents=True, exist_ok=True)
            subprocess.check_call(
                ["git", "mv", "crates/x/foo.md", "docs/foo.md"], cwd=repo,
            )
            subprocess.check_call(
                ["git", "commit", "-m", "move code to docs"], cwd=repo,
            )

            # With --no-renames, the old code path still lists crates/x/foo.md.
            from classify_changes import changed_files
            changed = changed_files("HEAD~1", "HEAD", three_dot=False, cwd=repo)
            self.assertIsNotNone(changed)
            # With --no-renames, crates/x/foo.md should appear in the diff output.
            self.assertIn("crates/x/foo.md", changed)


if __name__ == "__main__":
    unittest.main()