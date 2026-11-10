"""Contract for the change classifier that decides which CI gates run."""
import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import classify_changes  # noqa: E402
from classify_changes import (  # noqa: E402
    changed_files, classify, fetch_runs, head_in_origin_main, last_green_sha,
    main, parse_attempt, parser, resolve)

FULL = {"code": True, "docs": False, "docs_only": False}
DOCS_ONLY = {"code": False, "docs": True, "docs_only": True}


def pr(*files):
    return classify("pull_request", list(files))


def push(files, ref="refs/heads/dev", attempt=1):
    return classify("push", list(files), ref=ref, attempt=attempt)


def runs_json(sha):
    return json.dumps({"workflow_runs": [{"head_sha": sha}]})


class ClassifyChangesTests(unittest.TestCase):
    def test_docs_only_pull_request_skips_code_gates_but_keeps_docs_gates(self):
        result = pr("README.md", "docs/runtime/TOOLS.md", "docs/assets/diagram.svg")
        self.assertEqual(result, DOCS_ONLY)

    def test_any_code_file_forces_the_full_run(self):
        for extra in ("crates/base/src/lib.rs", "Cargo.lock", ".github/workflows/ci.yml",
                      "scripts/ci/check_panics.py", "src/docs/notes.rs"):
            with self.subTest(extra=extra):
                result = pr("docs/README.md", extra)
                self.assertTrue(result["code"])
                self.assertTrue(result["docs"])
                self.assertFalse(result["docs_only"])

    def test_code_without_docs_does_not_run_docs_gates(self):
        self.assertEqual(pr("crates/base/src/lib.rs"), FULL)

    def test_non_embedded_prose_is_docs_only_but_needs_no_corpus_test(self):
        # AGENTS.md is not compiled into the binary; a template is not documentation
        # the product serves. Both still count as "no code changed".
        result = pr("AGENTS.md", ".github/pull_request_template.md")
        self.assertTrue(result["docs_only"])

    def test_only_pull_requests_and_first_dev_pushes_are_classified(self):
        self.assertEqual(push(["docs/README.md"]), DOCS_ONLY)
        self.assertEqual(push(["crates/base/src/lib.rs"]), FULL)
        self.assertEqual(push(["README.md"], ref="refs/heads/main"), FULL)
        self.assertEqual(push(["README.md"], ref="refs/tags/v1.0.0"), FULL)
        self.assertEqual(push(["README.md"], attempt=2), FULL)
        self.assertEqual(push(["README.md"], attempt=0), FULL)
        self.assertEqual(classify("workflow_dispatch", ["README.md"], ref="refs/heads/dev"), FULL)
        self.assertEqual(classify("push", ["README.md"]), FULL)

    def test_empty_or_unknown_diff_runs_everything(self):
        self.assertEqual(classify("pull_request", []), FULL)
        self.assertEqual(classify("pull_request", None), FULL)
        self.assertEqual(push([]), FULL)
        self.assertEqual(classify("push", None, ref="refs/heads/dev"), FULL)

    def test_last_green_sha_accepts_only_a_full_hex_sha(self):
        good = "0123456789abcdef0123456789abcdef01234567"
        self.assertEqual(last_green_sha("o/r", "ci.yml", lambda r, w: runs_json(good)), good)
        bad = {
            "fetch failed": lambda r, w: None,
            "fetch raised": lambda r, w: (_ for _ in ()).throw(subprocess.TimeoutExpired("gh", 30)),
            "empty runs": lambda r, w: json.dumps({"workflow_runs": []}),
            "null sha": lambda r, w: runs_json(None),
            "short sha": lambda r, w: runs_json("abc123"),
            "upper-case sha": lambda r, w: runs_json(good.upper()),
            "non-hex sha": lambda r, w: runs_json("z" * 40),
            "invalid json": lambda r, w: "{not json",
            "bare sha text": lambda r, w: good,
            "wrong shape": lambda r, w: json.dumps([good]),
        }
        for name, fetch in bad.items():
            with self.subTest(name):
                self.assertIsNone(last_green_sha("o/r", "ci.yml", fetch))
        self.assertIsNone(last_green_sha("", "ci.yml", lambda r, w: runs_json(good)))
        self.assertIsNone(last_green_sha("o/r", "", lambda r, w: runs_json(good)))


class GitRepo:
    """A throwaway repository with an `origin/main` ref and a `dev` history."""

    def __init__(self, root):
        self.root = root
        for args in (["init", "-q"], ["config", "user.email", "t@example.com"],
                     ["config", "user.name", "t"], ["config", "commit.gpgsign", "false"]):
            self.git(*args)

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, check=True,
                              capture_output=True, text=True).stdout.strip()

    def commit(self, path, text="x\n"):
        target = Path(self.root, path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", path)
        return self.git("rev-parse", "HEAD")


class DevPushResolutionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = GitRepo(self.tmp.name)
        self.released = self.repo.commit("crates/a/src/lib.rs")
        self.repo.git("update-ref", "refs/remotes/origin/main", self.released)
        self.green = self.repo.commit("crates/a/src/more.rs")

    def tearDown(self):
        self.tmp.cleanup()

    def resolve(self, head, *extra, fetch=None, attempt=1):
        args = parser().parse_args(["--event", "push", "--ref", "refs/heads/dev",
                                    "--workflow", "ci.yml", "--repo", "o/r",
                                    "--head", head, *extra])
        fetch = fetch or (lambda r, w: runs_json(self.green))
        return resolve(args, attempt, fetch=fetch, cwd=self.tmp.name)

    def test_docs_only_push_is_compared_with_the_last_green_run(self):
        head = self.repo.commit("docs/guide.md")
        files, notice = self.resolve(head)
        self.assertEqual(files, ["docs/guide.md"])
        self.assertIn(f"compared with {self.green}", notice)
        self.assertEqual(push(files), DOCS_ONLY)

    def test_code_pushed_since_the_last_green_run_is_not_hidden_by_a_docs_tip(self):
        self.repo.commit("crates/a/src/third.rs")
        head = self.repo.commit("docs/guide.md")
        files, _ = self.resolve(head)
        self.assertTrue(push(files)["code"])

    def test_explicit_base_wins_over_the_lookup(self):
        head = self.repo.commit("docs/guide.md")
        files, notice = self.resolve(head, "--base", self.released,
                                     fetch=lambda r, w: runs_json(head))
        self.assertEqual(sorted(files), ["crates/a/src/more.rs", "docs/guide.md"])
        self.assertIn("(--base)", notice)

    def test_every_uncertainty_runs_everything(self):
        head = self.repo.commit("docs/guide.md")
        cases = {
            "lookup failed": dict(head=head, fetch=lambda r, w: None),
            "unknown base": dict(head=head, fetch=lambda r, w: runs_json("f" * 40)),
            "head already in origin/main": dict(head=self.released),
            "re-run": dict(head=head, attempt=2),
            "no head": dict(head=""),
        }
        for name, case in cases.items():
            with self.subTest(name):
                files, notice = self.resolve(case["head"], fetch=case.get("fetch"),
                                             attempt=case.get("attempt", 1))
                self.assertIsNone(files)
                self.assertIn("running every gate", notice)

    def test_empty_diff_runs_everything(self):
        files, _ = self.resolve(self.green)
        self.assertEqual(files, [])
        self.assertEqual(push(files), FULL)

    def test_a_code_file_renamed_into_docs_still_counts_as_code(self):
        Path(self.tmp.name, "docs").mkdir()
        self.repo.git("mv", "crates/a/src/more.rs", "docs/more.rs")
        self.repo.git("commit", "-q", "-m", "move")
        head = self.repo.git("rev-parse", "HEAD")
        for three_dot in (True, False):
            files = changed_files(self.green, head, three_dot=three_dot, cwd=self.tmp.name)
            self.assertIn("crates/a/src/more.rs", files)
            self.assertFalse(classify("pull_request", files)["docs_only"])


class CommandLineTests(unittest.TestCase):
    def run_main(self, *argv, fetch=lambda r, w: None):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = main(list(argv), fetch=fetch)
        return code, out.getvalue().splitlines()

    def test_one_notice_and_success_exit_on_every_path(self):
        with tempfile.NamedTemporaryFile("w", suffix=".txt") as listing:
            listing.write("docs/a.md\n")
            listing.flush()
            cases = [
                ("--event", "pull_request", "--files-from", listing.name),
                ("--event", "push", "--ref", "refs/heads/main", "--attempt", "1",
                 "--head", "HEAD"),
                ("--event", "push", "--ref", "refs/heads/dev", "--attempt", "x",
                 "--head", "HEAD"),
                ("--event", "workflow_dispatch", "--ref", "refs/heads/dev"),
                ("--event", "pull_request", "--base", "", "--head", ""),
                ("--event", "pull_request", "--base", "0" * 40, "--head", "1" * 40),
            ]
            for argv in cases:
                with self.subTest(argv=argv):
                    code, lines = self.run_main(*argv)
                    self.assertEqual(code, 0)
                    self.assertEqual(sum(line.startswith("::notice::") for line in lines), 1)
                    self.assertEqual(len([l for l in lines if l.startswith("code=")]), 1)
            _, lines = self.run_main(*cases[0])
            self.assertIn("docs_only=true", lines)
            for argv in cases[1:]:
                _, lines = self.run_main(*argv)
                self.assertIn("code=true", lines)


class EdgeCaseTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = GitRepo(self.tmp.name)
        self.released = self.repo.commit("crates/a/src/lib.rs")

    def tearDown(self):
        self.tmp.cleanup()

    def resolve(self, *argv):
        args = parser().parse_args(["--event", "push", "--ref", "refs/heads/dev",
                                    "--workflow", "ci.yml", "--repo", "o/r", *argv])
        return resolve(args, 1, fetch=lambda r, w: None, cwd=self.tmp.name)

    def test_lookup_asks_for_the_last_successful_dev_push_run(self):
        done = subprocess.CompletedProcess([], 0, stdout="{}", stderr="")
        with mock.patch.object(classify_changes.subprocess, "run", return_value=done) as run:
            self.assertEqual(fetch_runs("o/r", "ci.yml"), "{}")
        self.assertEqual(run.call_args.args[0], [
            "gh", "api", "repos/o/r/actions/workflows/ci.yml/runs"
            "?branch=dev&event=push&status=success&per_page=1"])
        failed = subprocess.CompletedProcess([], 1, stdout="{}", stderr="")
        with mock.patch.object(classify_changes.subprocess, "run", return_value=failed):
            self.assertIsNone(fetch_runs("o/r", "ci.yml"))

    def test_unknown_origin_main_runs_everything(self):
        head = self.repo.commit("docs/guide.md")
        self.assertIsNone(head_in_origin_main(head, cwd=self.tmp.name))
        files, notice = self.resolve("--base", self.released, "--head", head)
        self.assertIsNone(files)
        self.assertIn("could not be checked against origin/main", notice)

    def test_a_diverged_base_reports_its_code_under_the_push_diff(self):
        # A base that is not an ancestor of the head: the two-dot tree diff
        # still reports the base's code, where a three-dot diff would hide it.
        self.repo.git("update-ref", "refs/remotes/origin/main", self.released)
        self.repo.git("checkout", "-q", "-b", "side")
        side = self.repo.commit("crates/a/src/side.rs")
        self.repo.git("checkout", "-q", "-")
        head = self.repo.commit("docs/guide.md")
        files, _ = self.resolve("--base", side, "--head", head)
        self.assertTrue(classify("push", files, ref="refs/heads/dev")["code"])
        hidden = changed_files(side, head, three_dot=True, cwd=self.tmp.name)
        self.assertEqual(hidden, ["docs/guide.md"])

    def test_a_pull_request_ignores_code_that_only_its_base_moved_ahead(self):
        # The base branch gained code after the pull request branched off; the
        # three-dot diff keeps that out of a documentation-only pull request.
        self.repo.git("checkout", "-q", "-b", "base")
        base = self.repo.commit("crates/a/src/base_only.rs")
        self.repo.git("checkout", "-q", "-")
        head = self.repo.commit("docs/guide.md")
        args = parser().parse_args(["--event", "pull_request", "--base", base, "--head", head])
        files, _ = resolve(args, 1, cwd=self.tmp.name)
        self.assertEqual(files, ["docs/guide.md"])
        self.assertEqual(classify("pull_request", files), DOCS_ONLY)

    def test_a_non_utf8_path_is_classified_instead_of_crashing(self):
        blob = subprocess.run(["git", "hash-object", "-w", "--stdin"], input="x\n",
                              cwd=self.tmp.name, check=True, capture_output=True,
                              text=True).stdout.strip()
        subprocess.run([b"git", b"update-index", b"--add", b"--cacheinfo",
                        b"100644," + blob.encode() + b",docs/caf\xe9.md"],
                       cwd=self.tmp.name, check=True, capture_output=True)
        self.repo.git("commit", "-q", "-m", "latin-1 name")
        files = changed_files(self.released, "HEAD", three_dot=False, cwd=self.tmp.name)
        self.assertEqual(len(files), 1)
        self.assertEqual(classify("pull_request", files), DOCS_ONLY)

    def test_attempt_numbers_other_than_positive_integers_count_as_re_runs(self):
        self.assertEqual(parse_attempt("1"), 1)
        self.assertEqual(parse_attempt("3"), 3)
        for value in ("", None, "x", "0", "-1", "1.0"):
            with self.subTest(value=value):
                self.assertEqual(parse_attempt(value), 0)

    def test_command_line_classifies_a_docs_only_dev_push(self):
        self.repo.git("update-ref", "refs/remotes/origin/main", self.released)
        green = self.repo.commit("crates/a/src/more.rs")
        self.repo.commit("docs/guide.md")
        good = json.dumps({"workflow_runs": [{"head_sha": green}]})
        out = io.StringIO()
        cwd = os.getcwd()
        os.chdir(self.tmp.name)
        try:
            with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": ""}), \
                    contextlib.redirect_stdout(out):
                code = main(["--event", "push", "--ref", "refs/heads/dev", "--attempt", "1",
                             "--workflow", "ci.yml", "--repo", "o/r", "--base", "",
                             "--head", "HEAD"], fetch=lambda r, w: good)
        finally:
            os.chdir(cwd)
        self.assertEqual(code, 0)
        lines = out.getvalue().splitlines()
        self.assertIn("docs_only=true", lines)
        self.assertIn(f"compared with {green}", lines[-1])


if __name__ == "__main__":
    unittest.main()
