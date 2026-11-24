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
    main, parse_attempt, parser, resolve, verified_release_pull_request)

FULL = {
    "code": True,
    "docs": False,
    "docs_only": False,
    "rust": True,
    "docs_site": True,
    "ts_sdk": True,
    "py_sdk": True,
    "npm_pkg": True,
    "install_scripts": True,
    "release_scripts": True,
    "workflows": True,
    "tui_only": False,
    "tui_smoke": True,
    "platform_windows": True,
    "platform_macos": True,
    "full_matrix": True,
}
DOCS_ONLY = {
    "code": False,
    "docs": True,
    "docs_only": True,
    "rust": False,
    "docs_site": True,
    "ts_sdk": False,
    "py_sdk": False,
    "npm_pkg": False,
    "install_scripts": False,
    "release_scripts": False,
    "workflows": False,
    "tui_only": False,
    "tui_smoke": False,
    "platform_windows": False,
    "platform_macos": False,
    "full_matrix": False,
}


def pr(*files, labels=None):
    return classify("pull_request", list(files), labels=labels)


def push(files, ref="refs/heads/dev", attempt=1):
    return classify("push", list(files), ref=ref, attempt=attempt)


def runs_json(sha):
    return json.dumps({"workflow_runs": [{"head_sha": sha}]})


class ClassifyChangesTests(unittest.TestCase):
    def test_docs_only_pull_request_skips_code_gates_but_keeps_docs_gates(self):
        result = pr("README.md", "PHILOSOPHY.md", "PHILOSOPHY.ko.md", "docs/runtime/TOOLS.md", "docs/assets/diagram.svg")
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
        self.assertEqual(
            pr("crates/base/src/lib.rs"),
            {
                "code": True,
                "docs": False,
                "docs_only": False,
                "rust": True,
                "docs_site": False,
                "ts_sdk": False,
                "py_sdk": False,
                "npm_pkg": False,
                "install_scripts": False,
                "release_scripts": False,
                "workflows": False,
                "tui_only": False,
                "tui_smoke": False,
                "platform_windows": False,
                "platform_macos": False,
                "full_matrix": False,
            },
        )

    def test_non_embedded_prose_is_docs_only_but_needs_no_corpus_test(self):
        # AGENTS.md is not compiled into the binary; a template is not documentation
        # the product serves. Both still count as "no code changed".
        result = pr("AGENTS.md", ".github/pull_request_template.md")
        self.assertTrue(result["docs_only"])
        self.assertFalse(result["docs_site"])

    def test_only_pull_requests_and_first_dev_pushes_are_classified(self):
        self.assertEqual(push(["docs/README.md"]), DOCS_ONLY)
        dev_rust = push(["crates/base/src/lib.rs"])
        self.assertTrue(dev_rust["code"])
        self.assertTrue(dev_rust["rust"])
        self.assertTrue(dev_rust["release_scripts"])
        self.assertTrue(dev_rust["platform_windows"])
        self.assertTrue(dev_rust["platform_macos"])
        self.assertTrue(dev_rust["full_matrix"])
        self.assertFalse(dev_rust["docs"])
        self.assertEqual(push(["README.md"], ref="refs/heads/main"), FULL)
        self.assertEqual(push(["README.md"], ref="refs/tags/v1.0.0"), FULL)
        self.assertEqual(push(["README.md"], attempt=2), FULL)
        self.assertEqual(push(["README.md"], attempt=0), FULL)
        self.assertEqual(classify("workflow_dispatch", ["README.md"], ref="refs/heads/dev"), FULL)
        self.assertEqual(classify("push", ["README.md"]), FULL)

    def test_fine_grained_non_rust_and_platform_pull_request_buckets(self):
        docs_site = pr("packages/docs-site/guides/intro.mdx")
        self.assertFalse(docs_site["rust"])
        self.assertTrue(docs_site["docs_site"])
        self.assertFalse(docs_site["docs_only"])

        workflows_only = pr(".github/workflows/ci.yml", ".github/workflows/benchmark-docs.yml", "tests/test_ci_workflow_guards.py")
        self.assertFalse(workflows_only["rust"])
        self.assertTrue(workflows_only["workflows"])
        self.assertTrue(workflows_only["release_scripts"])

        ts_only = pr("packages/sdk/typescript/src/client.ts")
        self.assertFalse(ts_only["rust"])
        self.assertTrue(ts_only["ts_sdk"])
        self.assertFalse(ts_only["py_sdk"])

        py_only = pr("packages/sdk/python/src/horon_agent_sdk/client.py")
        self.assertFalse(py_only["rust"])
        self.assertTrue(py_only["py_sdk"])
        self.assertFalse(py_only["ts_sdk"])

        sh_install = pr("scripts/install/install.sh")
        self.assertFalse(sh_install["rust"])
        self.assertTrue(sh_install["install_scripts"])

        ps_install = pr("scripts/install/install.ps1")
        self.assertTrue(ps_install["rust"])
        self.assertTrue(ps_install["install_scripts"])
        self.assertTrue(ps_install["platform_windows"])

        tui_only = pr("crates/tui/src/app.rs")
        self.assertTrue(tui_only["rust"])
        self.assertTrue(tui_only["tui_only"])
        self.assertTrue(tui_only["tui_smoke"])

        for trigger in (
            "crates/agent-sdk/src/lib.rs",
            "src/cli/tui_handoff.rs",
            "scripts/test/tui_pty_smoke.py",
        ):
            with self.subTest(tui_trigger=trigger):
                triggered = pr(trigger)
                self.assertTrue(triggered["rust"])
                self.assertTrue(triggered["tui_smoke"])

        non_tui = pr("crates/extension-types/src/lib.rs")
        self.assertTrue(non_tui["rust"])
        self.assertFalse(non_tui["tui_smoke"])
        self.assertTrue(push(["crates/extension-types/src/lib.rs"])["tui_smoke"])

        shared_unix = pr("crates/platform-linux/src/unix.rs")
        self.assertTrue(shared_unix["rust"])
        self.assertTrue(shared_unix["platform_macos"])
        self.assertFalse(shared_unix["platform_windows"])

        labeled = pr("packages/docs-site/guides/intro.mdx", labels={"ci:all-os"})
        self.assertEqual(labeled, FULL)
        labeled_docs = pr("docs/runtime/TOOLS.md", labels={"ci:all-os"})
        self.assertEqual(labeled_docs, dict(FULL, docs=True))

        philosophy = pr("PHILOSOPHY.md")
        self.assertTrue(philosophy["docs_site"])
        self.assertTrue(philosophy["docs_only"])

        for readme in (
            "packages/sdk/typescript/README.md",
            "packages/sdk/python/README.md",
            "packages/npm/horon/README.md",
        ):
            self.assertTrue(pr(readme)["docs_site"], readme)
            self.assertTrue(push([readme])["docs_site"], readme)

        docs_script = pr("scripts/ci/check_docs_pairs.py")
        self.assertFalse(docs_script["rust"])
        self.assertTrue(docs_script["docs"])

        for quality_script in ("scripts/ci/check_issue_refs.py", "scripts/ci/package_compat.py"):
            self.assertTrue(pr(quality_script)["rust"], quality_script)

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


class ReleasePullRequestTests(unittest.TestCase):
    """A `dev` -> `main` pull request that the `dev` push run already verified."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = GitRepo(self.tmp.name)
        self.repo.commit("crates/a/src/lib.rs")
        # Not named `main`: CI sets init.defaultBranch=main for `git init`.
        self.repo.git("checkout", "-q", "-b", "released")
        self.main = self.repo.commit("docs/performance/RESULTS.md")
        self.repo.git("checkout", "-q", "-")
        self.head = self.repo.commit("crates/a/src/feature.rs")

    def tearDown(self):
        self.tmp.cleanup()

    def verify(self, *, base=None, head=None, base_ref="main", green=None, labels=()):
        args = parser().parse_args([
            "--event", "pull_request", "--base-ref", base_ref,
            "--base", base or self.main, "--head", head or self.head,
            "--workflow", "ci.yml", "--repo", "o/r"])
        sha = green if green is not None else self.head
        return verified_release_pull_request(
            args, set(labels), fetch=lambda r, w: runs_json(sha), cwd=self.tmp.name)

    def test_head_verified_on_dev_with_docs_only_base_runs_only_docs_gates(self):
        lanes, notice = self.verify()
        self.assertEqual(lanes, DOCS_ONLY)
        self.assertIn("documentation gates only", notice)
        # `main` already in the head: nothing on the base side, still docs gates.
        self.repo.git("merge", "-q", "--no-edit", self.main)
        merged = self.repo.git("rev-parse", "HEAD")
        lanes, _ = self.verify(head=merged, green=merged)
        self.assertEqual(lanes, dict(DOCS_ONLY, docs_site=False))

    def test_anything_short_of_a_verified_head_is_classified_as_usual(self):
        self.repo.git("checkout", "-q", "released")
        main_code = self.repo.commit("crates/a/src/hotfix.rs")
        self.repo.git("checkout", "-q", "-")
        cases = {
            "head is not the last green dev push": dict(green="f" * 40),
            "dev push lookup failed": dict(green=""),
            "base changed code": dict(base=main_code),
            "pull request into dev": dict(base_ref="dev"),
            "ci:full label": dict(labels=("ci:full",)),
            "unknown base commit": dict(base="e" * 40),
        }
        for name, case in cases.items():
            with self.subTest(name):
                self.assertIsNone(self.verify(**case))


class CommandLineTests(unittest.TestCase):
    def run_main(self, *argv, fetch=lambda r, w: None):
        # Outside any repository and without $GITHUB_OUTPUT, so the CI step that
        # runs these tests neither diffs its own checkout nor writes outputs.
        out = io.StringIO()
        cwd = os.getcwd()
        with tempfile.TemporaryDirectory() as empty:
            os.chdir(empty)
            try:
                with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": ""}), \
                        contextlib.redirect_stdout(out):
                    code = main(list(argv), fetch=fetch)
            finally:
                os.chdir(cwd)
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
