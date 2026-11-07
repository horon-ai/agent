"""Exercise release source preparation against real tagged source in isolated clones."""
import hashlib
import itertools
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/release/prepare_source.py"
HELPERS = ("scripts/ci/install_justrust.sh", "scripts/release/build_linux_compat.sh")


class WorkflowGraphTests(unittest.TestCase):
    """Check real job predicates, including the implicit ancestor success gate.

    This is a deliberately restricted model of the status predicates used here,
    not a GitHub Actions emulator. Matrix results are terminal aggregate results.
    """

    def setUp(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        self.jobs = dict(re.findall(
            r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)",
            workflow.split("\njobs:\n", 1)[1], re.M | re.S))
        self.needs = {}
        self.conditions = {}
        for name, body in self.jobs.items():
            needs = re.search(r"^    needs: (.+)$", body, re.M)
            self.needs[name] = (needs.group(1).strip("[]").split(", ")
                                if needs else [])
            self.conditions[name] = re.search(r"^    if: (.+)$", body, re.M).group(1)

    def ancestors(self, job):
        return set(self.needs[job]).union(
            *(self.ancestors(parent) for parent in self.needs[job]))

    def eligible(self, job, results, cancelled=False):
        expression = self.conditions[job].removeprefix("${{ ").removesuffix(" }}")
        # A status function replaces the implicit success() gate over needs.
        # Force `!cancelled()` to "not cancelled" so the model can prove that a
        # cancelled workflow never starts a gated job.
        has_status_function = "always()" in expression or "!cancelled()" in expression
        if not has_status_function and any(
                results[parent] != "success" for parent in self.ancestors(job)):
            return False
        expression = expression.replace("always()", "True").replace("!cancelled()", "not cancelled")
        expression = re.sub(r"needs\.([\w-]+)\.result",
                            lambda match: repr(results[match.group(1)]), expression)
        expression = expression.replace("&&", "and").replace("||", "or")
        # Reject unexpected syntax rather than executing arbitrary workflow text.
        residue = re.sub(r"'[^']*'|True|and|or|not cancelled|==|[()\s]", "", expression)
        self.assertEqual(residue, "", expression)
        return eval(expression, {"__builtins__": {}}, {"cancelled": cancelled})

    def test_complete_dependency_graph(self):
        self.assertEqual(self.needs, {
            "create-release": [],
            "build-linux-macos": ["create-release"],
            "build-windows": ["create-release"],
            "build-windows-arm64": ["create-release"],
            "publish-windows": ["create-release", "build-windows", "build-windows-arm64"],
            "build-freebsd": ["create-release"],
            "release": ["create-release", "build-linux-macos", "publish-windows", "build-freebsd"],
        })
        self.assertIn("github.repository == 'horon-ai/agent'", self.conditions["create-release"])

    def test_all_terminal_ancestor_results(self):
        for job in ("build-linux-macos", "build-windows", "build-windows-arm64",
                    "build-freebsd", "publish-windows", "release"):
            ancestors = sorted(self.ancestors(job))
            for statuses in itertools.product(("success", "skipped", "failure", "cancelled"),
                                              repeat=len(ancestors)):
                results = dict(zip(ancestors, statuses))
                expected = results["create-release"] == "success"
                if job == "publish-windows":
                    # Only the x64 build gates signing. A failed ARM64 build must
                    # not suppress signing and publication of the x64 assets; the
                    # finalization gate still refuses the partial asset set.
                    expected &= results["build-windows"] == "success"
                # Finalization must run after platform failures to diagnose the
                # incomplete asset set, not silently skip or publish partial sets.
                with self.subTest(job=job, results=results):
                    self.assertEqual(self.eligible(job, results), expected)

    def test_cancelled_workflow_never_starts_gated_jobs(self):
        for job in ("build-linux-macos", "build-windows", "build-windows-arm64",
                    "build-freebsd", "publish-windows", "release"):
            results = {ancestor: "success" for ancestor in self.ancestors(job)}
            with self.subTest(job=job):
                self.assertTrue(self.eligible(job, results))
                self.assertFalse(self.eligible(job, results, cancelled=True))

    def test_every_job_has_a_timeout(self):
        for name, body in self.jobs.items():
            with self.subTest(job=name):
                self.assertRegex(body, r"(?m)^    timeout-minutes: \d+$")

    def test_required_platforms_do_not_continue_on_error(self):
        # The final job requires every platform asset, so a job-level
        # `continue-on-error` would only hide a failure that still keeps the
        # release as a draft. Step-level advisory allowances are unaffected.
        for name in ("build-linux-macos", "build-windows", "build-windows-arm64",
                     "build-freebsd", "publish-windows", "release"):
            with self.subTest(job=name):
                self.assertIsNone(re.search(r"(?m)^    continue-on-error:", self.jobs[name]))

    def test_validation_triggers_and_executes_release_checks(self):
        workflow = (ROOT / ".github/workflows/release-recovery.yml").read_text()
        for event in ("pull_request", "push"):
            section = re.search(rf"^  {event}:\n(.*?)(?=^  \w+:)", workflow, re.M | re.S).group(1)
            for path in (".github/workflows/release.yml", ".github/workflows/release-recovery.yml",
                         "scripts/release/**", "tests/*release*.py",
                         "scripts/install/install.sh", "scripts/ci/install_justrust.sh", "scripts/release/build_linux_compat.sh"):
                self.assertIn(f"- '{path}'", section, (event, path))
        self.assertIn('fetch-depth: 0', workflow)
        # Validation must run on pushes to the branches that exist. `master` is
        # not a branch in this repository, so a push filter naming it never fired.
        push = re.search(r"^  push:\n(.*?)(?=^  \w+:)", workflow, re.M | re.S).group(1)
        self.assertIn("branches: [dev, main]", push)
        self.assertNotIn("master", workflow)
        # Every job needs a bounded runtime.
        self.assertRegex(workflow, r"(?m)^    timeout-minutes: \d+$")
        self.assertIn("python3 -m unittest discover -s tests -p '*release*.py'", workflow)
        self.assertIn("python3 -m unittest discover -s scripts/release -p 'test_*release*.py'", workflow)
        self.assertIn("bash scripts/install/test_install_from_source.sh", workflow)


class ReleaseWorkflowContractTests(unittest.TestCase):
    """Checks that only need the workflow text, not the historical tag history."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get("HORON_AGENT_SCRATCH_DIR"))
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)

    def test_public_state_refuses_public_newer_and_api_failure(self):
        directory = self.directory / "commands"
        directory.mkdir()
        gh = directory / "gh"
        gh.write_text('#!/bin/sh\n[ "${GH_FAIL:-0}" = 0 ] || exit 1\nif [ "$2" = view ]; then printf "%s" "$GH_DRAFT"; else printf "%s" "$GH_RELEASES"; fi\n')
        gh.chmod(0o755)
        for draft, releases, fail, succeeds in (("true", [], "0", True), ("false", [], "0", False),
                 ("true", [{"tagName": "v0.97.0"}], "0", False),
                 ("true", [{"tagName": "v0.95.0"}, {"tagName": "v0.96.0"}], "0", True),
                 ("true", [], "1", False)):
            with self.subTest(draft=draft, releases=releases, fail=fail):
                env = dict(os.environ, PATH=f"{directory}:{os.environ['PATH']}", GH_DRAFT=draft,
                           GH_RELEASES=json.dumps(releases), GH_FAIL=fail)
                result = subprocess.run(["python3", str(SCRIPT), "--check-public-state", "v0.96.0"],
                                        cwd=self.directory, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, succeeds, result.stderr)

    def test_windows_signing_predicate(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        signing = workflow.split("  publish-windows:\n", 1)[1].split("  build-freebsd:\n", 1)[0]
        condition = next(line.strip().split("if: ", 1)[1] for line in signing.splitlines()
                         if line.startswith("    if: "))
        condition = condition.removeprefix("${{ ").removesuffix(" }}")
        self.assertEqual(condition, "!cancelled() && needs.create-release.result == 'success' && needs.build-windows.result == 'success'")
        self.assertIn("needs: [create-release, build-windows, build-windows-arm64]", signing)
        # A failed x64 build must stay blocked. The ARM64 build is not part of
        # the predicate so its failure cannot suppress x64 publication.
        for create in ("success", "failure", "cancelled", "skipped"):
            for build in ("success", "failure", "cancelled", "skipped"):
                with self.subTest(create=create, build=build):
                    expression = condition.replace("!cancelled()", "True")
                    expression = expression.replace("needs.create-release.result", repr(create))
                    expression = expression.replace("needs.build-windows.result", repr(build))
                    expression = expression.replace("&&", "and")
                    self.assertEqual(eval(expression, {"__builtins__": {}}),
                                     create == "success" and build == "success")

    def test_release_notes_disclose_actual_windows_signing(self):
        import textwrap

        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        notes = workflow.split("      - name: Add platform availability to release notes\n", 1)[1]
        self.assertIn("WINDOWS_SIGNING_ENABLED: ${{ needs.publish-windows.outputs.signing_enabled }}", notes)
        script = textwrap.dedent(notes.split("python3 - << 'PY'\n", 1)[1].split("\n          PY", 1)[0])
        platforms = ("LINUX_X86_64", "LINUX_AARCH64", "MACOS_AARCH64", "MACOS_X86_64",
                     "WINDOWS_X86_64", "WINDOWS_AARCH64", "FREEBSD_X86_64")
        for enabled in ("true", "false", ""):
            with self.subTest(enabled=enabled):
                (self.directory / "release_notes.md").write_text("Release highlights\n")
                env = dict(os.environ, WINDOWS_SIGNING_ENABLED=enabled,
                           **{f"{platform}_AVAILABLE": "true" for platform in platforms})
                for _ in range(2):
                    subprocess.run(["python3", "-c", script], cwd=self.directory, env=env, check=True)
                body = (self.directory / "release_notes.md").read_text()
                self.assertEqual(body.count("## Platform availability"), 1)
                self.assertIn("Release highlights", body)
                if enabled == "true":
                    self.assertIn("Authenticode signatures verified", body)
                    self.assertNotIn("**unsigned**", body)
                else:
                    self.assertIn("**unsigned**", body)
                    self.assertNotIn("Authenticode signatures verified", body)

    def test_workflow_identity_and_publication_gates(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertNotIn("GITHUB_REF_NAME", workflow)
        self.assertEqual(workflow.count("ref: ${{ needs.create-release.outputs.tag_sha }}"), 6)
        self.assertEqual(workflow.count("name: Prepare release source"), 6)
        self.assertIn("Require successful exact-commit CI", workflow)
        self.assertIn("Release tag no longer matches the triggering commit", workflow)
        self.assertNotIn("recovery", workflow)
        self.assertNotIn("companion", workflow)
        self.assertEqual(workflow.count("Verify release build preserved Cargo.lock"), 3)
        self.assertIn("cancel-in-progress: false", workflow)
        self.assertEqual(workflow.count('--check-public-state "$RELEASE_TAG"'), 2)
        self.assertIn("sha256sum release-provenance.json >> SHA256SUMS", workflow)
        self.assertIn('SHA256SUMS release-provenance.json --clobber', workflow)
        self.assertIn("Get-AuthenticodeSignature", workflow)
        windows = workflow.split("  build-windows:\n", 1)[1].split("  publish-windows:\n", 1)[0]
        self.assertEqual(workflow.count('JUSTRUST_DISABLE: "1"'), 1)
        self.assertIn('JUSTRUST_DISABLE: "1"', windows)
        self.assertIn('& justrust @cargoArgs', windows)
        self.assertIn('@("build", "--local", "--locked", "--release"', windows)
        # The ARM64 Windows build is native cargo: justrust has no
        # aarch64-pc-windows-msvc client.
        self.assertIn("& cargo build --locked --release --target aarch64-pc-windows-msvc --no-default-features --features pdf", windows)
        self.assertNotIn("install_justrust", windows.split("  build-windows-arm64:\n", 1)[1])
        # The deploy key and the SSH agent it fed are gone: the only git
        # dependency is a public HTTPS repository and there are no submodules.
        self.assertNotIn("DEPLOY_KEY", workflow)
        self.assertNotIn("ssh-agent", workflow)
        # Matrix targets are installed into `stable`, which is also the
        # toolchain every build must use inside the checkout.
        self.assertIn("RUSTUP_TOOLCHAIN: stable", workflow)
        # Signing is opt-in: an unset WINDOWS_SIGNING_REQUIRED skips with a
        # notice, only the literal `true` makes it a hard failure.
        self.assertIn("$signingRequired = $env:WINDOWS_SIGNING_REQUIRED -eq 'true'", workflow)
        self.assertIn("::notice::Windows code signing skipped", workflow)
        self.assertIn("Validate complete platform asset set", workflow)
        self.assertIn("Keeping the release as a draft until every platform succeeds", workflow)
        self.assertIn("if: github.event_name != 'workflow_dispatch'", workflow)


HISTORICAL_TAGS = ("v0.96.0", "v0.97.0")


def tag_available(tag):
    """True when this clone carries the archived release tag.

    The public repository history starts at a single snapshot commit and has no
    tags, so the preparation contract can only be exercised where the historical
    release tags exist. Skipping keeps the workflow green there without
    weakening any assertion for a clone that does have them.
    """
    result = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
        capture_output=True)
    return result.returncode == 0


class PrepareSourceTests(unittest.TestCase):
    def setUp(self):
        if not tag_available("v0.96.0"):
            self.skipTest(
                "historical release tags v0.96.0/v0.97.0 are not present in this clone; "
                "release preparation tests need the archived release history")
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get("HORON_AGENT_SCRATCH_DIR"))
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "repo"
        subprocess.run(["git", "clone", "--quiet", "--shared", "--no-checkout", str(ROOT), str(self.repo)], check=True)
        self.workflow_sha = self.git("rev-parse", "HEAD")
        self.git("checkout", "--quiet", "--detach", "v0.96.0")
        self.tag_sha = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], stderr=subprocess.PIPE).decode().strip()

    def run_prepare(self, tag="v0.96.0", workflow_sha=None):
        return subprocess.run(["python3", str(SCRIPT), tag, workflow_sha or self.workflow_sha],
                              cwd=self.repo, capture_output=True, text=True)

    def test_real_tags_preserve_identity_and_record_provenance(self):
        for tag in HISTORICAL_TAGS:
            with self.subTest(tag=tag):
                if not tag_available(tag):
                    self.skipTest(f"historical release tag {tag} is not present in this clone")
                self.git("checkout", "--quiet", "--force", "--detach", tag)
                before = self.git("rev-parse", "HEAD")
                result = self.run_prepare(tag=tag)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.git("rev-parse", "HEAD"), before)
                self.assertEqual(self.git("rev-parse", f"refs/tags/{tag}^{{commit}}"), before)
                changed = set(self.git("diff", "--name-only").splitlines())
                self.assertLessEqual(changed, set(HELPERS))
                data = json.loads((self.repo / "release-provenance.json").read_text())
                self.assertEqual(data["tag"], tag)
                self.assertEqual(data["tag_commit"], before)
                self.assertEqual(data["workflow_commit"], self.workflow_sha)
                self.assertEqual(data["eligibility_policy"], "exact-sha-main-ci")
                for helper in HELPERS:
                    self.assertEqual(data["release_helpers"][helper],
                                     hashlib.sha256((self.repo / helper).read_bytes()).hexdigest())
                    self.assertTrue(os.access(self.repo / helper, os.X_OK))
                # A second run starts from a dirty checkout and must refuse.
                self.assertNotEqual(self.run_prepare(tag=tag).returncode, 0)

    def test_external_provenance_path(self):
        if not tag_available("v0.97.0"):
            self.skipTest("historical release tag v0.97.0 is not present in this clone")
        self.git("checkout", "--quiet", "--detach", "v0.97.0")
        output = Path(self.temp.name) / "provenance.json"
        tag_sha = self.git("rev-parse", "HEAD")
        result = subprocess.run(["python3", str(SCRIPT), "v0.97.0", tag_sha], cwd=self.repo,
                                env=dict(os.environ, RELEASE_PROVENANCE_PATH=str(output)), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(output.is_file())
        # Release-only tooling comes from the same commit as the tag, so the
        # helper files are unchanged.
        self.assertEqual(self.git("status", "--porcelain"), "")

    def test_invalid_input_and_wrong_checkout_rejected(self):
        for tag, sha in (("master", None), ("v0.96.0;touch nope", None),
                         ("v0.96.0", "master"), ("v0.96.0", "b" * 40),
                         ("v0.97.0", self.tag_sha)):
            with self.subTest(tag=tag, sha=sha):
                self.assertNotEqual(self.run_prepare(tag, sha).returncode, 0)
                self.assertEqual(self.git("diff", "--name-only"), "")

    def test_dirty_checkout_rejected(self):
        source = self.repo / "Cargo.toml"
        content = source.read_bytes() + b"\n# unrelated change\n"
        source.write_bytes(content)
        self.assertNotEqual(self.run_prepare().returncode, 0)
        self.assertEqual(source.read_bytes(), content)

    def test_windows_crlf_checkout(self):
        self.git("config", "core.autocrlf", "true")
        self.git("checkout", "--quiet", "--force", "--detach", "v0.96.0")
        self.assertIn(b"\r\n", (self.repo / "Cargo.toml").read_bytes())
        result = self.run_prepare()
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
