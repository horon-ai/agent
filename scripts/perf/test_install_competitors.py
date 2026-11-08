#!/usr/bin/env python3
"""Contract tests for the jcode download in install_competitors.sh.

A stub `curl` on PATH stands in for GitHub, so these run offline:

- a transient 503 on the release download URL falls back to the same asset by
  id through the release API and still yields `--bin jcode=...`;
- bytes whose SHA-256 differs from SHA256SUMS are rejected with a reason;
- when every source fails, the reason lands in failures.txt and bench_compare
  reports it instead of the generic "binary not found".
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

INSTALLER = HERE / "install_competitors.sh"
VERSION = "v0.99.1"
ASSET_ID = 4242

STUB_CURL = textwrap.dedent(
    """\
    #!/usr/bin/env bash
    # Serves files from $STUB_ROOT. STUB_DOWNLOAD / STUB_API = ok | fail.
    out=""
    url=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --output | -o) out="$2"; shift 2 ;;
        --header | --retry | --retry-delay) shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
      esac
    done
    echo "$url" >>"$STUB_ROOT/requests.log"
    fail() { echo "curl: (22) The requested URL returned error: $1" >&2; exit 22; }
    case "$url" in
      https://github.com/1jehuang/jcode/releases/download/*)
        [ "$STUB_DOWNLOAD" = ok ] || fail 503
        src="$STUB_ROOT/files/${url##*/}" ;;
      https://api.github.com/repos/1jehuang/jcode/releases/tags/*)
        [ "$STUB_API" = ok ] || fail 504
        src="$STUB_ROOT/release.json" ;;
      https://api.github.com/repos/1jehuang/jcode/releases/assets/*)
        [ "$STUB_API" = ok ] || fail 504
        src="$STUB_ROOT/assets/${url##*/}" ;;
      *) fail 404 ;;
    esac
    [ -f "$src" ] || fail 404
    cp "$src" "$out"
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


def jcode_tarball(binary_name: str) -> bytes:
    script = b"#!/bin/sh\necho jcode stub\n"
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        info = tarfile.TarInfo(binary_name)
        info.size = len(script)
        info.mode = 0o755
        archive.addfile(info, io.BytesIO(script))
    return buffer.getvalue()


@unittest.skipIf(jcode_asset() is None or shutil.which("bash") is None,
                 "needs bash on Linux or macOS")
class JcodeDownloadTests(unittest.TestCase):
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
        tarball = jcode_tarball(self.asset.removesuffix(".tar.gz"))
        self.write_release(tarball, hashlib.sha256(tarball).hexdigest())
        self.dest = self.tmp / "dest"

    def write_release(self, tarball: bytes, digest: str) -> None:
        (self.stub / "files" / self.asset).write_bytes(tarball)
        (self.stub / "assets" / str(ASSET_ID)).write_bytes(tarball)
        sums = f"{digest}  {self.asset}\n{'0' * 64}  other.tar.gz\n"
        (self.stub / "files" / "SHA256SUMS").write_text(sums)
        (self.stub / "assets" / str(ASSET_ID + 1)).write_text(sums)
        api = "https://api.github.com/repos/1jehuang/jcode/releases/assets"
        release = {"tag_name": VERSION, "assets": [
            {"url": f"{api}/{ASSET_ID}", "id": ASSET_ID, "name": self.asset},
            {"url": f"{api}/{ASSET_ID + 1}", "id": ASSET_ID + 1, "name": "SHA256SUMS"},
        ]}
        (self.stub / "release.json").write_text(json.dumps(release, indent=2))

    def run_installer(self, download: str, api: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env.update({
            "PATH": f"{self.stub / 'bin'}{os.pathsep}{env.get('PATH', '')}",
            "STUB_ROOT": str(self.stub),
            "STUB_DOWNLOAD": download,
            "STUB_API": api,
            "JCODE_VERSION": VERSION,
        })
        env.pop("GH_TOKEN", None)
        env.pop("GITHUB_TOKEN", None)
        return subprocess.run(["bash", str(INSTALLER), str(self.dest), "jcode"],
                              capture_output=True, text=True, env=env, timeout=60)

    def failures(self) -> str:
        return (self.dest / "failures.txt").read_text()

    def test_download_5xx_falls_back_to_release_api(self) -> None:
        result = self.run_installer(download="fail", api="ok")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--bin jcode=", result.stdout)
        self.assertEqual(self.failures(), "")
        requests = (self.stub / "requests.log").read_text()
        self.assertIn(f"/releases/assets/{ASSET_ID}\n", requests)
        bin_path = result.stdout.split("--bin jcode=", 1)[1].split()[0]
        self.assertEqual(subprocess.run([bin_path], capture_output=True, text=True).stdout.strip(),
                         "jcode stub")

    def test_digest_mismatch_is_rejected(self) -> None:
        tarball = (self.stub / "files" / self.asset).read_bytes()
        self.write_release(tarball, "f" * 64)
        result = self.run_installer(download="ok", api="ok")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--bin jcode=", result.stdout)
        self.assertIn(f"jcode\tSHA256 mismatch for {self.asset}", self.failures())
        self.assertFalse((self.dest / "jcode-dist").exists())

    def test_every_source_failing_reaches_the_report(self) -> None:
        result = self.run_installer(download="fail", api="fail")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--bin jcode=", result.stdout)
        reasons = bench.read_install_failures(str(self.dest / "failures.txt"))
        self.assertTrue(reasons["jcode"].startswith(
            f"download failed: https://github.com/1jehuang/jcode/releases/download/{VERSION}/{self.asset}"))
        reason = bench.missing_binary_reason("jcode", reasons)
        self.assertTrue(reason.startswith("install failed: download failed: "), reason)
        self.assertNotIn("binary not found", reason)


class InstallFailureReasonTests(unittest.TestCase):
    def test_tool_without_recorded_reason_keeps_the_generic_hint(self) -> None:
        self.assertEqual(bench.missing_binary_reason("codex", {"jcode": "download failed: x"}),
                         "binary not found (pass --bin name=path)")

    def test_missing_failures_file_reads_as_empty(self) -> None:
        self.assertEqual(bench.read_install_failures(None), {})
        self.assertEqual(bench.read_install_failures("/nonexistent/failures.txt"), {})


if __name__ == "__main__":
    unittest.main()
