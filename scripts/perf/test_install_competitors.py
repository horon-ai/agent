#!/usr/bin/env python3
"""Contract tests for the jcode release install in install_competitors.sh.

A stub `curl` on PATH stands in for GitHub, so these run offline. The tests pin
the behaviour a benchmark run depends on now that jcode is resolved instead of
pinned in the script:

- no JCODE_VERSION resolves the newest stable release, and the explicit pin
  goes through the same download, checksum and `--version` verification;
- the installed binary is the release asset for this platform, matched against
  SHA256SUMS, and `--version` must name the resolved tag;
- a transient 503 on the release download URL falls back to the same asset by id
  through the release API;
- a missing release, a missing platform asset, a bad digest or a wrong version
  fails the run (nonzero exit) with the reason in failures.txt;
- the run metadata (tag, peeled tag commit, asset, URL, SHA-256) lands in
  `jcode-release.json`, and the GitHub token never reaches the release binary;
- a non-jcode tool that fails to install still only degrades the comparison.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import bench_compare as bench  # noqa: E402
import jcode_release  # noqa: E402

INSTALLER = HERE / "install_competitors.sh"
VERSION = "v1.0.1"
TAG_OBJECT = "b" * 40
COMMIT = "a" * 40
ASSET_ID = 4242
SUMS_ID = 4243

STUB_CURL = textwrap.dedent(
    """\
    #!/usr/bin/env bash
    # Serves files from $STUB_ROOT. STUB_LATEST / STUB_DOWNLOAD / STUB_API = ok | fail.
    out=""
    url=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --output | -o) out="$2"; shift 2 ;;
        --header) shift 2 ;;
        --retry | --retry-delay) shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
      esac
    done
    echo "$url" >>"$STUB_ROOT/requests.log"
    fail() { echo "curl: (22) The requested URL returned error: $1" >&2; exit 22; }
    case "$url" in
      https://api.github.com/repos/1jehuang/jcode/releases/latest)
        [ "$STUB_LATEST" = ok ] || fail 503
        src="$STUB_ROOT/latest.json" ;;
      https://api.github.com/repos/1jehuang/jcode/releases\\?per_page=*)
        [ "$STUB_LATEST" = ok ] || fail 503
        src="$STUB_ROOT/releases.json" ;;
      https://api.github.com/repos/1jehuang/jcode/releases/tags/*)
        [ "$STUB_LATEST" = ok ] || fail 503
        tag_file="$STUB_ROOT/tags/${url##*/}.json"
        if [ -f "$tag_file" ]; then src="$tag_file"; else src="$STUB_ROOT/latest.json"; fi ;;
      https://api.github.com/repos/1jehuang/jcode/git/ref/tags/*)
        [ "$STUB_LATEST" = ok ] || fail 503
        src="$STUB_ROOT/ref.json" ;;
      https://api.github.com/repos/1jehuang/jcode/git/tags/*)
        [ "$STUB_LATEST" = ok ] || fail 503
        src="$STUB_ROOT/tag.json" ;;
      https://github.com/1jehuang/jcode/releases/download/*)
        [ "$STUB_DOWNLOAD" = ok ] || fail 503
        src="$STUB_ROOT/files/${url##*/}" ;;
      https://api.github.com/repos/1jehuang/jcode/releases/assets/*)
        [ "$STUB_API" = ok ] || fail 504
        src="$STUB_ROOT/assets/${url##*/}" ;;
      https://registry.npmjs.org/*)
        [ -n "${GH_TOKEN:-}${GITHUB_TOKEN:-}" ] && echo "LEAKED_TOKEN:$url" >>"$STUB_ROOT/leaked.log"
        npm_rel="${url#https://registry.npmjs.org/}"
        npm_safe="${npm_rel//\\//__}"
        src="$STUB_ROOT/npm/${npm_safe}.json"
        if [ ! -f "$src" ]; then
          [ "$STUB_NPM_FAIL" = 1 ] && fail 503
          fail 404
        fi ;;
      *) fail 404 ;;
    esac
    [ -f "$src" ] || fail 404
    if [ -n "$out" ]; then cp "$src" "$out"; else cat "$src"; fi
    """
)


def jcode_asset() -> str | None:
    system, machine = platform.system(), platform.machine().lower()
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(machine)
    if arch is None:
        return None
    if system == "Linux":
        return f"jcode-linux-{arch}.tar.gz"
    if system == "Darwin":
        return f"jcode-macos-{arch}.tar.gz"
    return None


def jcode_tarball(binary_name: str, version: str = "1.0.1", env_dump: Path | None = None) -> bytes:
    lines = ["#!/bin/sh"]
    if env_dump is not None:
        lines.append(f'env > "{env_dump}"')
    lines.append(f'echo "jcode {version}"')
    script = ("\n".join(lines) + "\n").encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        info = tarfile.TarInfo(binary_name)
        info.size = len(script)
        info.mode = 0o755
        archive.addfile(info, io.BytesIO(script))
    return buffer.getvalue()


@unittest.skipIf(jcode_asset() is None or shutil.which("bash") is None,
                 "needs bash on Linux or macOS")
class JcodeReleaseInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="install-competitors-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.asset = jcode_asset() or ""
        self.stub = self.tmp / "stub"
        (self.stub / "bin").mkdir(parents=True)
        (self.stub / "files").mkdir()
        (self.stub / "assets").mkdir()
        curl = self.stub / "bin" / "curl"
        curl.write_text(STUB_CURL)
        curl.chmod(0o755)
        self.tarball = jcode_tarball(self.asset.removesuffix(".tar.gz"))
        self.write_release(self.tarball, hashlib.sha256(self.tarball).hexdigest())
        self.dest = self.tmp / "dest"

    def write_release(self, tarball: bytes, digest: str, *, tag: str = VERSION,
                      assets: list[dict] | None = None, prerelease: bool = False) -> None:
        (self.stub / "files" / self.asset).write_bytes(tarball)
        (self.stub / "assets" / str(ASSET_ID)).write_bytes(tarball)
        sums = f"{digest}  {self.asset}\n{'0' * 64}  other.tar.gz\n"
        (self.stub / "files" / "SHA256SUMS").write_text(sums)
        (self.stub / "assets" / str(SUMS_ID)).write_text(sums)
        api = "https://api.github.com/repos/1jehuang/jcode/releases/assets"
        web = f"https://github.com/1jehuang/jcode/releases/download/{tag}"
        listed = assets if assets is not None else [
            {"id": ASSET_ID, "name": self.asset, "browser_download_url": f"{web}/{self.asset}"},
            {"id": SUMS_ID, "name": "SHA256SUMS", "browser_download_url": f"{web}/SHA256SUMS"},
        ]
        release = {"tag_name": tag, "draft": False, "prerelease": prerelease,
                   "assets": [dict(a, url=f"{api}/{a['id']}") for a in listed]}
        (self.stub / "latest.json").write_text(json.dumps(release, indent=2))
        (self.stub / "ref.json").write_text(json.dumps({
            "ref": f"refs/tags/{tag}",
            "object": {"type": "tag", "sha": TAG_OBJECT, "url": "x"},
        }))
        (self.stub / "tag.json").write_text(json.dumps({
            "tag": tag, "object": {"type": "commit", "sha": COMMIT, "url": "x"},
        }))

    def run_installer(self, *, tools: str = "jcode", version: str | None = None,
                      download: str = "ok", api: str = "ok", latest: str = "ok",
                      extra_env: dict | None = None) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env.update({
            "PATH": f"{self.stub / 'bin'}{os.pathsep}{env.get('PATH', '')}",
            "STUB_ROOT": str(self.stub),
            "STUB_DOWNLOAD": download,
            "STUB_API": api,
            "STUB_LATEST": latest,
            "NPM_INSTALL_RETRY_DELAY": "0",
        })
        env.pop("JCODE_VERSION", None)
        env.pop("JCODE_RELEASE_COMMIT", None)
        env.pop("JCODE_RELEASE_SOURCE", None)
        env.pop("JCODE_INSTALL_PLAN", None)
        env.pop("GH_TOKEN", None)
        env.pop("GITHUB_TOKEN", None)
        if version is not None:
            env["JCODE_VERSION"] = version
        env.update(extra_env or {})
        return subprocess.run(["bash", str(INSTALLER), str(self.dest), tools],
                              capture_output=True, text=True, env=env, timeout=120)

    def failures(self) -> str:
        path = self.dest / "failures.txt"
        return path.read_text() if path.is_file() else ""

    def metadata(self) -> dict:
        return json.loads((self.dest / "jcode-release.json").read_text())

    def bin_path(self, result: subprocess.CompletedProcess[str]) -> str:
        self.assertIn("--bin jcode=", result.stdout)
        return result.stdout.split("--bin jcode=", 1)[1].split()[0]

    def test_latest_release_installs_and_records_provenance(self) -> None:
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.failures(), "")
        binary = self.bin_path(result)
        self.assertEqual(subprocess.run([binary], capture_output=True, text=True).stdout.strip(), "jcode 1.0.1")
        meta = self.metadata()
        expected_digest = hashlib.sha256(self.tarball).hexdigest()
        self.assertEqual(meta["tag"], VERSION)
        self.assertEqual(meta["version"], "1.0.1")
        self.assertEqual(meta["commit"], COMMIT)
        self.assertEqual(meta["asset"], self.asset)
        self.assertEqual(meta["sha256"], expected_digest)
        self.assertEqual(meta["download_url"],
                         f"https://github.com/1jehuang/jcode/releases/download/{VERSION}/{self.asset}")
        self.assertEqual(meta["selection"], "latest")
        self.assertEqual(meta["served_by"], "release-download")
        self.assertEqual(meta["served_url"], meta["download_url"])
        self.assertTrue(meta["verified_sha256"])
        self.assertTrue(meta["verified_version"])
        requests = (self.stub / "requests.log").read_text()
        self.assertIn("/releases/latest\n", requests)
        self.assertNotIn("/releases/tags/", requests)
        self.assertIn("/git/ref/tags/", requests)
        self.assertIn("/git/tags/", requests)

    def test_explicit_pin_uses_the_same_verification(self) -> None:
        result = self.run_installer(version=VERSION)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.bin_path(result)
        meta = self.metadata()
        self.assertEqual(meta["selection"], "pin")
        self.assertEqual(meta["tag"], VERSION)
        self.assertEqual(meta["sha256"], hashlib.sha256(self.tarball).hexdigest())
        self.assertIn(f"/releases/tags/{VERSION}\n", (self.stub / "requests.log").read_text())

    def test_expected_commit_from_the_matrix_is_enforced(self) -> None:
        result = self.run_installer(extra_env={"JCODE_RELEASE_COMMIT": COMMIT})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.metadata()["commit"], COMMIT)
        moved = self.run_installer(extra_env={"JCODE_RELEASE_COMMIT": "c" * 40})
        self.assertNotEqual(moved.returncode, 0)
        self.assertIn("release ref moved", self.failures())

    def test_missing_release_fails_the_run(self) -> None:
        result = self.run_installer(latest="fail")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--bin jcode=", result.stdout)
        self.assertIn("jcode\t", self.failures())
        self.assertIn("release", self.failures())

    def test_missing_platform_asset_fails_the_run(self) -> None:
        self.write_release(self.tarball, "0" * 64, assets=[
            {"id": SUMS_ID, "name": "SHA256SUMS",
             "browser_download_url": f"https://github.com/1jehuang/jcode/releases/download/{VERSION}/SHA256SUMS"},
        ])
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--bin jcode=", result.stdout)
        self.assertIn(self.asset, self.failures())
        self.assertIn("no asset", self.failures())

    def test_prerelease_is_not_eligible(self) -> None:
        self.write_release(self.tarball, hashlib.sha256(self.tarball).hexdigest(), prerelease=True)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("stable", self.failures())

    def test_digest_mismatch_fails_the_run(self) -> None:
        self.write_release(self.tarball, "f" * 64)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--bin jcode=", result.stdout)
        self.assertIn(f"jcode\tSHA-256 mismatch for {self.asset}", self.failures())
        self.assertFalse((self.dest / "jcode-dist" / self.asset.removesuffix(".tar.gz")).exists())

    def test_version_mismatch_fails_the_run(self) -> None:
        tarball = jcode_tarball(self.asset.removesuffix(".tar.gz"), version="9.9.9")
        self.write_release(tarball, hashlib.sha256(tarball).hexdigest())
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--bin jcode=", result.stdout)
        self.assertIn("--version", self.failures())
        self.assertIn("9.9.9", self.failures())

    def test_a_resolved_tag_keeps_the_selection_that_chose_it(self) -> None:
        # benchmark.yml resolves "latest" once and hands the OS jobs only the tag;
        # the record must still say the tag came from the newest-release lookup.
        result = self.run_installer(version=VERSION, extra_env={"JCODE_RELEASE_SOURCE": "latest"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.metadata()["selection"], "latest")

    def test_transient_download_error_falls_back_to_the_asset_id(self) -> None:
        result = self.run_installer(download="fail", api="ok")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.bin_path(result)
        self.assertEqual(self.failures(), "")
        requests = (self.stub / "requests.log").read_text()
        self.assertIn(f"/releases/assets/{ASSET_ID}\n", requests)
        meta = self.metadata()
        self.assertEqual(meta["served_by"], "asset-api")
        self.assertTrue(meta["served_url"].endswith(f"/releases/assets/{ASSET_ID}"))
        self.assertEqual(meta["download_url"],
                         f"https://github.com/1jehuang/jcode/releases/download/{VERSION}/{self.asset}")

    def test_deferred_version_check_does_not_execute_the_binary(self) -> None:
        # The install suite owns the first --version call: the installer must not
        # warm the binary, and the record says the version was not verified here.
        dump = self.tmp / "version-env.log"
        tarball = jcode_tarball(self.asset.removesuffix(".tar.gz"), env_dump=dump)
        self.write_release(tarball, hashlib.sha256(tarball).hexdigest())
        result = self.run_installer(extra_env={"JCODE_VERIFY_VERSION": "0"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.bin_path(result)
        meta = self.metadata()
        self.assertFalse(meta["verified_version"])
        self.assertIsNone(meta["version_output"])
        self.assertFalse(dump.exists())
        self.assertEqual(meta["sha256"], hashlib.sha256(tarball).hexdigest())

    def test_the_timed_installer_reuses_the_resolved_platform_release(self) -> None:
        plan = {
            "tag": VERSION, "commit": COMMIT, "selection": "latest", "name": self.asset,
            "api_url": "https://api.github.com/repos/1jehuang/jcode/releases/latest",
            "asset_id": ASSET_ID, "sums_id": SUMS_ID,
        }
        result = self.run_installer(latest="fail", extra_env={"JCODE_INSTALL_PLAN": json.dumps(plan)})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.metadata()["commit"], COMMIT)
        self.assertEqual(self.metadata()["sha256"], hashlib.sha256(self.tarball).hexdigest())
        requests = (self.stub / "requests.log").read_text()
        self.assertNotIn("api.github.com", requests)
        self.assertIn("/releases/download/", requests)

    def test_github_token_never_reaches_the_release_binary(self) -> None:
        dump = self.tmp / "version-env.log"
        tarball = jcode_tarball(self.asset.removesuffix(".tar.gz"), env_dump=dump)
        self.write_release(tarball, hashlib.sha256(tarball).hexdigest())
        result = self.run_installer(extra_env={"GH_TOKEN": "secret-token", "GITHUB_TOKEN": "other-secret"})
        self.assertEqual(result.returncode, 0, result.stderr)
        seen = dump.read_text()
        self.assertNotIn("secret-token", seen)
        self.assertNotIn("other-secret", seen)
        self.assertIn("PATH=", seen)

    def test_a_failing_npm_tool_degrades_the_comparison_without_the_api_token(self) -> None:
        npm = self.stub / "bin" / "npm"
        npm.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "${GH_TOKEN-unset}" "${GITHUB_TOKEN-unset}" >"$STUB_ROOT/npm-env"\nexit 1\n')
        npm.chmod(0o755)
        result = self.run_installer(tools="jcode,codex", extra_env={"GH_TOKEN": "fixture-token", "GITHUB_TOKEN": "fixture-token"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.bin_path(result)
        self.assertIn("codex\tnpm install failed", self.failures())
        self.assertEqual((self.stub / "npm-env").read_text().splitlines(), ["unset", "unset"])

    def test_transient_npm_install_failure_retries_and_succeeds(self) -> None:
        npm = self.stub / "bin" / "npm"
        npm.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            count_file="$STUB_ROOT/npm-calls"
            calls=0
            [ -f "$count_file" ] && calls="$(cat "$count_file")"
            calls=$((calls + 1))
            echo "$calls" >"$count_file"
            prefix=""
            while [ $# -gt 0 ]; do
              case "$1" in
                --prefix) prefix="$2"; shift 2 ;;
                *) shift ;;
              esac
            done
            if [ "$calls" -eq 1 ]; then
              mkdir -p "$prefix/node_modules"
              touch "$prefix/node_modules/stale.txt"
              exit 1
            fi
            mkdir -p "$prefix/node_modules/.bin"
            printf '#!/bin/sh\\necho "codex 0.166.0"\\n' >"$prefix/node_modules/.bin/codex"
            chmod +x "$prefix/node_modules/.bin/codex"
            exit 0
            """))
        npm.chmod(0o755)
        result = self.run_installer(tools="jcode,codex")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--bin codex=", result.stdout)
        self.assertEqual(self.failures(), "")
        self.assertFalse((self.dest / "codex" / "node_modules" / "stale.txt").exists())
        self.assertEqual((self.stub / "npm-calls").read_text().strip(), "2")

    def test_resolve_platforms_skips_single_os_latest_when_other_oses_requested(self) -> None:
        def rel(tag: str, asset_names: list[str], *, prerelease: bool = False) -> dict:
            return {
                "tag_name": tag,
                "draft": False,
                "prerelease": prerelease,
                "assets": [{"id": idx + 1, "name": name} for idx, name in enumerate(asset_names)],
            }

        linux_only = rel("v1.1.2", ["jcode-linux-x86_64.tar.gz", "SHA256SUMS"])
        rc = rel("v1.1.0-rc1", ["jcode-linux-x86_64.tar.gz", "jcode-macos-aarch64.tar.gz", "jcode-windows-x86_64.exe", "SHA256SUMS"], prerelease=True)
        full = rel("v1.0.3", ["jcode-linux-x86_64.tar.gz", "jcode-macos-aarch64.tar.gz", "jcode-windows-x86_64.exe", "SHA256SUMS"])

        (self.stub / "latest.json").write_text(json.dumps(linux_only))
        (self.stub / "releases.json").write_text(json.dumps([linux_only, rc, full]))
        (self.stub / "tags").mkdir(exist_ok=True)
        (self.stub / "tags" / "v1.1.2.json").write_text(json.dumps(linux_only))
        (self.stub / "tags" / "v1.0.3.json").write_text(json.dumps(full))

        env = dict(os.environ)
        env["PATH"] = f"{self.stub / 'bin'}{os.pathsep}{env.get('PATH', '')}"
        env["STUB_ROOT"] = str(self.stub)
        env["STUB_LATEST"] = "ok"
        env["STUB_DOWNLOAD"] = "ok"
        env["STUB_API"] = "ok"
        env.pop("JCODE_VERSION", None)

        # Single Linux request keeps latest v1.1.2 without fetching the list endpoint.
        meta_linux = self.tmp / "meta-linux.json"
        res_linux = subprocess.run(
            [sys.executable, str(HERE / "jcode_release.py"), "resolve", "--platforms", "linux", "--metadata", str(meta_linux)],
            capture_output=True, text=True, env=env, timeout=30,
        )
        self.assertEqual(res_linux.returncode, 0, res_linux.stderr)
        self.assertEqual(json.loads(meta_linux.read_text())["tag"], "v1.1.2")

        # Multi-OS request skips Linux-only v1.1.2 and prerelease v1.1.0-rc1, choosing v1.0.3.
        meta_all = self.tmp / "meta-all.json"
        res_all = subprocess.run(
            [sys.executable, str(HERE / "jcode_release.py"), "resolve", "--platforms", "all", "--metadata", str(meta_all)],
            capture_output=True, text=True, env=env, timeout=30,
        )
        self.assertEqual(res_all.returncode, 0, res_all.stderr)
        rec_all = json.loads(meta_all.read_text())
        self.assertEqual(rec_all["tag"], "v1.0.3")
        self.assertEqual(rec_all["selection"], "latest")

        # Explicit pin refuses to fall back when requested platform assets are absent.
        res_pin = subprocess.run(
            [sys.executable, str(HERE / "jcode_release.py"), "resolve", "--tag", "v1.1.2", "--platforms", "all"],
            capture_output=True, text=True, env=env, timeout=30,
        )
        self.assertNotEqual(res_pin.returncode, 0)
        self.assertIn("pinned release v1.1.2 has no asset(s)", res_pin.stderr)

        # Per-platform resolution selects newest release per OS in one pass.
        meta_per_os = self.tmp / "meta-per-os.json"
        res_per_os = subprocess.run(
            [
                sys.executable,
                str(HERE / "jcode_release.py"),
                "resolve",
                "--per-platform",
                "--platforms",
                "all",
                "--metadata",
                str(meta_per_os),
            ],
            capture_output=True, text=True, env=env, timeout=30,
        )
        self.assertEqual(res_per_os.returncode, 0, res_per_os.stderr)
        per_os = json.loads(meta_per_os.read_text())["platforms"]
        self.assertEqual(per_os["linux"]["tag"], "v1.1.2")
        self.assertEqual(per_os["macos"]["tag"], "v1.0.3")
        self.assertEqual(per_os["windows"]["tag"], "v1.0.3")

    def test_npm_latest_skips_prereleases_strips_tokens_and_records_provenance(self) -> None:
        npm_dir = self.stub / "npm"
        npm_dir.mkdir(exist_ok=True)
        (npm_dir / "@oh-my-pi__pi-coding-agent__latest.json").write_text(json.dumps({
            "version": "19.2.0-beta.1",
            "dist": {"shasum": "deadbeef", "integrity": "sha512-beta"},
        }))
        (npm_dir / "@oh-my-pi__pi-coding-agent.json").write_text(json.dumps({
            "versions": {
                "19.2.0-beta.1": {"version": "19.2.0-beta.1"},
                "19.1.1": {
                    "version": "19.1.1",
                    "gitHead": "abc123def456",
                    "dist": {
                        "tarball": "https://registry.npmjs.org/@oh-my-pi/pi-coding-agent/-/pi-coding-agent-19.1.1.tgz",
                        "shasum": "111122223333",
                        "integrity": "sha512-stable1911==",
                    },
                },
                "19.0.0": {"version": "19.0.0"},
            },
        }))
        (npm_dir / "@oh-my-pi__pi-coding-agent__19.1.1.json").write_text(json.dumps({
            "version": "19.1.1",
            "gitHead": "abc123def456",
            "dist": {
                "tarball": "https://registry.npmjs.org/@oh-my-pi/pi-coding-agent/-/pi-coding-agent-19.1.1.tgz",
                "shasum": "111122223333",
                "integrity": "sha512-stable1911==",
            },
        }))
        npm_stub = self.stub / "bin" / "npm"
        npm_stub.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            prefix="" spec=""
            while [ $# -gt 0 ]; do
              case "$1" in
                --prefix) prefix="$2"; shift 2 ;;
                -*) shift ;;
                *) spec="$1"; shift ;;
              esac
            done
            echo "$spec" >>"$STUB_ROOT/npm-specs.log"
            ver="${spec##*@}"
            mkdir -p "$prefix/node_modules/@oh-my-pi/pi-coding-agent" "$prefix/node_modules/.bin"
            printf '{"name":"@oh-my-pi/pi-coding-agent","version":"%s"}\\n' "$ver" \
              >"$prefix/node_modules/@oh-my-pi/pi-coding-agent/package.json"
            printf '#!/bin/sh\\necho "omp/%s"\\n' "$ver" >"$prefix/node_modules/.bin/omp"
            chmod +x "$prefix/node_modules/.bin/omp"
            """))
        npm_stub.chmod(0o755)
        bun_stub = self.stub / "bin" / "bun"
        bun_stub.write_text("#!/bin/sh\necho 1.4.5\n")
        bun_stub.chmod(0o755)

        result = self.run_installer(
            tools="omp",
            extra_env={"GH_TOKEN": "secret-token", "GITHUB_TOKEN": "other-secret", "OMP_VERSION": ""},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.stub / "leaked.log").exists())
        specs = (self.stub / "npm-specs.log").read_text().splitlines()
        self.assertEqual(specs, ["@oh-my-pi/pi-coding-agent@19.1.1"])
        prov = json.loads((self.dest / "competitor-provenance.json").read_text())["tools"]["omp"]
        self.assertEqual(prov["version"], "19.1.1")
        self.assertEqual(prov["installed_version"], "19.1.1")
        self.assertEqual(prov["selection"], "npm highest-stable")
        self.assertEqual(prov["git_head"], "abc123def456")
        self.assertEqual(prov["integrity"], "sha512-stable1911==")


class HoronTargetResolutionTests(unittest.TestCase):
    def test_resolve_horon_defaults_to_main_and_stamps_next_patch_semver(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            env = {
                **os.environ,
                "GIT_AUTHOR_NAME": "Test",
                "GIT_AUTHOR_EMAIL": "t@example.com",
                "GIT_COMMITTER_NAME": "Test",
                "GIT_COMMITTER_EMAIL": "t@example.com",
            }
            def git(*args: str) -> str:
                return subprocess.check_output(["git", *args], cwd=repo, env=env, text=True).strip()

            git("init", "-b", "main")
            (repo / "Cargo.toml").write_text('[package]\nname = "horon-agent"\nversion = "0.1.0"\n')
            git("add", "Cargo.toml")
            git("commit", "-m", "base")
            git("tag", "v0.1.22")
            (repo / "Cargo.toml").write_text('[package]\nname = "horon-agent"\nversion = "0.1.0"\n# m\n')
            git("add", "Cargo.toml")
            git("commit", "-m", "main tip")
            main_sha = git("rev-parse", "HEAD")
            git("branch", "dev")
            git("checkout", "-q", "dev")
            (repo / "Cargo.toml").write_text('[package]\nname = "horon-agent"\nversion = "0.1.0"\n# d\n')
            git("add", "Cargo.toml")
            git("commit", "-m", "dev tip")
            dev_sha = git("rev-parse", "HEAD")

            res_main = subprocess.run(
                [sys.executable, str(HERE / "competitor_versions.py"), "resolve-horon",
                 "--repo", str(repo), "--main-ref", "main", "--release-tag", "v0.1.22"],
                capture_output=True, text=True, check=True,
            )
            info = json.loads(res_main.stdout)
            self.assertEqual(info["sha"], main_sha)
            self.assertEqual(info["semver"], "0.1.23")
            self.assertFalse(info["release_build"])
            self.assertTrue(info["on_main"])

            res_dev = subprocess.run(
                [sys.executable, str(HERE / "competitor_versions.py"), "resolve-horon",
                 "--repo", str(repo), "--ref", dev_sha, "--main-ref", "main",
                 "--release-tag", "v0.1.22", "--require-main"],
                capture_output=True, text=True,
            )
            self.assertNotEqual(res_dev.returncode, 0)
            self.assertIn("not an ancestor of main", res_dev.stderr)


class ReleaseHelpers(unittest.TestCase):
    def test_every_measured_platform_has_an_exact_asset(self) -> None:
        expected = {
            ("Darwin", "arm64"): "jcode-macos-aarch64.tar.gz",
            ("Darwin", "x86_64"): "jcode-macos-x86_64.tar.gz",
            ("Linux", "x86_64"): "jcode-linux-x86_64.tar.gz",
            ("Linux", "aarch64"): "jcode-linux-aarch64.tar.gz",
            ("Linux", "arm64"): "jcode-linux-aarch64.tar.gz",
            ("FreeBSD", "amd64"): "jcode-freebsd-x86_64.tar.gz",
            ("FreeBSD", "x86_64"): "jcode-freebsd-x86_64.tar.gz",
            ("Windows", "AMD64"): "jcode-windows-x86_64.exe",
            ("Windows", "ARM64"): "jcode-windows-aarch64.exe",
        }
        for (system, machine), asset in expected.items():
            with self.subTest(platform=f"{system}/{machine}"):
                self.assertEqual(jcode_release.asset_name(system, machine), asset)
        self.assertIsNone(jcode_release.asset_name("Plan9", "mips"))
        self.assertIsNone(jcode_release.asset_name("Linux", "riscv64"))

    def test_only_a_stable_semver_tag_is_accepted(self) -> None:
        for tag in ("v1.0.1", "v10.20.300"):
            with self.subTest(tag=tag):
                self.assertEqual(jcode_release.validate_tag(tag), tag)
        for tag in ("1.0.1", "v1.0", "v1.0.1-rc1", "v1.0.1; rm -rf /", "", "latest"):
            with self.subTest(tag=tag), self.assertRaises(jcode_release.ReleaseError):
                jcode_release.validate_tag(tag)

    def test_version_output_must_match_the_resolved_tag(self) -> None:
        self.assertTrue(jcode_release.version_matches("v1.0.1", "jcode 1.0.1"))
        self.assertTrue(jcode_release.version_matches("v1.0.1", "jcode v1.0.1 (abc123)"))
        self.assertFalse(jcode_release.version_matches("v1.0.1", "jcode 1.0.2"))
        self.assertFalse(jcode_release.version_matches("v1.0.1", "jcode 1.0.10"))
        self.assertFalse(jcode_release.version_matches("v1.0.1", ""))

    def test_checksum_manifest_reads_only_the_exact_asset(self) -> None:
        manifest = f"{'a' * 64}  jcode-linux-x86_64.tar.gz\n{'b' * 64}  jcode-linux-x86_64.tar.gz.sig\n"
        self.assertEqual(jcode_release.manifest_digest(manifest, "jcode-linux-x86_64.tar.gz"), "a" * 64)
        self.assertIsNone(jcode_release.manifest_digest(manifest, "jcode-macos-x86_64.tar.gz"))


class InstallFailureReasonTests(unittest.TestCase):
    def test_tool_without_recorded_reason_keeps_the_generic_hint(self) -> None:
        self.assertEqual(bench.missing_binary_reason("codex", {"jcode": "download failed: x"}),
                         "binary not found (pass --bin name=path)")

    def test_missing_failures_file_reads_as_empty(self) -> None:
        self.assertEqual(bench.read_install_failures(None), {})
        self.assertEqual(bench.read_install_failures("/nonexistent/failures.txt"), {})


if __name__ == "__main__":
    unittest.main()
