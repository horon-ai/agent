#!/usr/bin/env python3
"""Tag selection for automatic patch releases and the matching source check."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name, HERE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


next_version = load("next_version")
prepare_source = load("prepare_source")


def linear(*commits):
    """Ancestry for one straight history: earlier entries are ancestors of later ones."""
    order = {commit: index for index, commit in enumerate(commits)}
    return lambda commit, descendant: order[commit] <= order[descendant]


class DecideTests(unittest.TestCase):
    def test_first_release_uses_cargo_version(self):
        self.assertEqual(next_version.decide((0, 1, 0), {}, "a", linear("a")), ("tag", "v0.1.0"))

    def test_next_patch_follows_latest_tag_on_the_line(self):
        tags = {"v0.1.0": "a", "v0.1.1": "b", "v0.1.10": "c"}
        self.assertEqual(next_version.decide((0, 1, 0), tags, "d", linear("a", "b", "c", "d")), ("tag", "v0.1.11"))

    def test_cargo_patch_is_a_floor(self):
        self.assertEqual(next_version.decide((0, 1, 5), {"v0.1.2": "a"}, "b", linear("a", "b")), ("tag", "v0.1.5"))

    def test_new_minor_line_starts_at_cargo_version(self):
        tags = {"v0.1.0": "a", "v0.1.1": "b"}
        self.assertEqual(next_version.decide((0, 2, 0), tags, "c", linear("a", "b", "c")), ("tag", "v0.2.0"))

    def test_already_tagged_commit_is_skipped(self):
        kind, reason = next_version.decide((0, 1, 0), {"v0.1.0": "a"}, "a", linear("a"))
        self.assertEqual(kind, "skip")
        self.assertIn("already released", reason)

    def test_commit_older_than_latest_release_is_skipped(self):
        # A late CI result for an older merge must not publish a higher number
        # for older source than the release already out.
        kind, reason = next_version.decide((0, 1, 0), {"v0.1.0": "b"}, "a", linear("a", "b"))
        self.assertEqual(kind, "skip")
        self.assertIn("older than the latest release", reason)

    def test_lowered_cargo_line_is_refused(self):
        # The 0.98 history is not tagged on this repository, but a lowered line
        # below a real release must stop instead of publishing a downgrade.
        with self.assertRaises(ValueError):
            next_version.decide((0, 1, 0), {"v0.98.1": "a"}, "b", linear("a", "b"))

    def test_non_release_tags_are_ignored(self):
        tags = {"bench-1": "a", "v0.1.0-rc1": "a", "v0.1.0": "a"}
        self.assertEqual(next_version.decide((0, 1, 0), tags, "b", linear("a", "b")), ("tag", "v0.1.1"))

    def test_cargo_version_reads_only_the_package_table(self):
        manifest = '[workspace.package]\nversion = "9.9.9"\n\n[package]\nname = "x"\nversion = "0.1.0"\n\n[dependencies]\nversion = "1.0.0"\n'
        self.assertEqual(next_version.cargo_version(manifest), (0, 1, 0))


class GitTests(unittest.TestCase):
    """Run the command line against a real repository with both tag kinds."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get("HORON_AGENT_SCRATCH_DIR"))
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        for args in (("init", "-q", "-b", "main"), ("config", "user.name", "Fixture"),
                     ("config", "user.email", "fixture@example.invalid"), ("config", "tag.gpgsign", "false"),
                     ("config", "commit.gpgsign", "false")):
            self.git(*args)
        self.commits = []
        for index in range(3):
            (self.repo / "Cargo.toml").write_text(f'[package]\nname = "x"\nversion = "0.1.0"\n# {index}\n')
            self.git("add", "Cargo.toml")
            self.git("commit", "-q", "-m", f"commit {index}")
            self.commits.append(self.git("rev-parse", "HEAD"))

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], check=True,
                              capture_output=True, text=True).stdout.strip()

    def run_script(self, sha):
        return subprocess.run(["python3", str(HERE / "next_version.py"), sha], cwd=self.repo,
                              capture_output=True, text=True)

    def test_annotated_and_lightweight_tags_resolve_to_commits(self):
        self.git("tag", "v0.1.0", self.commits[0])
        self.git("tag", "-a", "-m", "annotated", "v0.1.1", self.commits[1])
        result = self.run_script(self.commits[2])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "tag=v0.1.2")
        skipped = self.run_script(self.commits[1])
        self.assertEqual(skipped.returncode, 0, skipped.stderr)
        self.assertTrue(skipped.stdout.startswith("skip="), skipped.stdout)

    def test_short_sha_is_rejected(self):
        self.assertNotEqual(self.run_script(self.commits[0][:7]).returncode, 0)


class TagFitsVersionTests(unittest.TestCase):
    def test_same_line_at_or_above_floor(self):
        for tag, version, fits in (("v0.1.0", "0.1.0", True), ("v0.1.7", "0.1.0", True),
                                   ("v0.1.0", "0.1.1", False), ("v0.2.0", "0.1.0", False),
                                   ("v1.1.0", "0.1.0", False), ("v0.1.0", "0.1.0-dev", False)):
            with self.subTest(tag=tag, version=version):
                self.assertEqual(prepare_source.tag_fits_version(tag, version), fits)

    def test_a_prerelease_keeps_the_stable_rule_on_its_own_line(self):
        for tag, version, fits in (("v0.1.0-alpha", "0.1.0", True),
                                   ("v0.1.7-alpha.1", "0.1.0", True),
                                   ("v0.1.0-alpha", "0.1.1", False)):
            with self.subTest(tag=tag, version=version):
                self.assertEqual(prepare_source.tag_fits_version(tag, version), fits)

    def test_a_prerelease_may_open_the_next_minor_line_at_patch_zero(self):
        # The alpha announces the line `Cargo.toml` has not taken yet, which is
        # how `v0.2.0-alpha` is cut from a commit that still reads 0.1.x.
        self.assertTrue(prepare_source.tag_fits_version("v0.2.0-alpha", "0.1.0"))
        self.assertTrue(prepare_source.tag_fits_version("v0.2.0-alpha.3", "0.1.24"))
        # Anything else still needs the root version bumped first.
        for tag in ("v0.2.1-alpha", "v0.3.0-alpha", "v1.0.0-alpha"):
            with self.subTest(tag=tag):
                self.assertFalse(prepare_source.tag_fits_version(tag, "0.1.0"))

    def test_build_metadata_is_not_a_release_tag(self):
        for tag in ("v0.1.0+build", "v0.2.0-alpha+build.1", "v0.1", "0.1.0", "v0.1.0-"):
            with self.subTest(tag=tag):
                self.assertFalse(prepare_source.tag_fits_version(tag, "0.1.0"))


class ReleaseTagShapeTests(unittest.TestCase):
    """The one tag shape every release gate validates, and its semver order."""

    def test_a_prerelease_suffix_is_a_release_tag(self):
        for tag in ("v0.1.0", "v0.2.0-alpha", "v0.2.0-alpha.1", "v10.0.0-rc.2"):
            with self.subTest(tag=tag):
                self.assertIsNotNone(prepare_source.parse_release_tag(tag))
        for tag in ("v0.1.0+build", "v0.2.0-alpha+build.1", "v0.1.0-", "v0.2.0-",
                    "0.2.0-alpha", "v0.2-alpha", "v0.2.0-alpha..1"):
            with self.subTest(tag=tag):
                self.assertIsNone(prepare_source.parse_release_tag(tag))

    def test_prerelease_order_follows_semver(self):
        ordered = ["v0.2.0-alpha", "v0.2.0-alpha.1", "v0.2.0-alpha.2",
                   "v0.2.0-alpha.10", "v0.2.0-beta", "v0.2.0", "v0.2.1-alpha", "v0.10.0"]
        keys = [prepare_source.semver_key(tag) for tag in ordered]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(len(set(keys)), len(keys))

    def test_a_name_that_is_not_a_release_tag_has_no_order(self):
        with self.assertRaises(ValueError):
            prepare_source.semver_key("bench-1")


if __name__ == "__main__":
    unittest.main()
