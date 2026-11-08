"""Offline installer acceptance tests using real local tarballs and stubbed hosts/curl."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/ci/install_justrust.sh"


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        # The installer records the physical path (`pwd -P`); on macOS the temp
        # directory sits behind the /var -> /private/var symlink.
        self.root = Path(self.tmp.name).resolve()
        self.stubs = self.root / "stubs"
        self.stubs.mkdir()
        self.install = self.root / "install space"
        self.install.mkdir()
        self.pathfile = self.root / "github_path"
        self.env = dict(os.environ, PATH=f"{self.stubs}:{os.environ['PATH']}",
                        HOME=str(self.root), JUSTRUST_INSTALL_DIR=str(self.install),
                        GITHUB_PATH=str(self.pathfile), JUSTRUST_ALLOW_TEST_URL="1",
                        JUSTRUST_TEST_BASE_URL="https://trusted.invalid/fixtures",
                        FIXTURES=str(self.root), CURL_LOG=str(self.root / "curl_log"),
                        HOST_OS="Linux", HOST_ARCH="x86_64")
        self.stub("uname", '#!/bin/bash\ncase "$1" in -s) echo "$HOST_OS";; -m) echo "$HOST_ARCH";; esac\n')
        self.stub("cygpath", "#!/bin/bash\n[ \"$1\" = -w ] || exit 99\nprintf '%s\\n' 'C:\\fixture\\bin'\n")
        self.stub("curl", '''#!/bin/bash
printf '%s\\n' "$*" >> "$CURL_LOG"
while [[ $# -gt 0 ]]; do
    case "$1" in --output) out=$2; shift 2;; *) url=$1; shift;; esac
done
cp "$FIXTURES/${url##*/}" "$out"
''')

    def stub(self, name, text):
        path = self.stubs / name
        path.write_text(text)
        path.chmod(0o755)

    def fixture(self, target="x86_64-unknown-linux-musl", status=0, bad=False,
                missing=False):
        binary = "justrust.exe" if "windows" in target else "justrust"
        archive = self.root / f"justrust-{target}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            data = f'#!/bin/sh\n[ "$1" = --version ] || exit 99\necho justrust-fixture\nexit {status}\n'.encode()
            info = tarfile.TarInfo(f"justrust-{target}/{'other' if missing else binary}")
            info.size = len(data)
            info.mode = 0o755
            tar.addfile(info, io.BytesIO(data))
            # This unselected traversal member must never reach the filesystem.
            info = tarfile.TarInfo("../escaped")
            info.size = 4
            tar.addfile(info, io.BytesIO(b"oops"))
        digest = "0" * 64 if bad else hashlib.sha256(archive.read_bytes()).hexdigest()
        Path(str(archive) + ".sha256").write_text(f"{digest}  {archive.name}\n")
        return binary

    def run_install(self):
        return subprocess.run(["bash", str(SCRIPT)], env=self.env,
                              capture_output=True, text=True)

    def test_all_targets(self):
        cases = [("Linux", "x86_64", "x86_64-unknown-linux-musl"),
                 ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
                 ("Linux", "arm64", "aarch64-unknown-linux-musl"),
                 ("Darwin", "x86_64", "x86_64-apple-darwin"),
                 ("Darwin", "arm64", "aarch64-apple-darwin"),
                 ("MINGW64_NT-10.0", "x86_64", "x86_64-pc-windows-msvc")]
        for system, arch, target in cases:
            with self.subTest(target=target, arch=arch):
                self.env.update(HOST_OS=system, HOST_ARCH=arch)
                binary = self.fixture(target)
                result = self.run_install()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue((self.install / binary).is_file())
                self.assertIn("justrust-fixture", result.stdout)
                self.assertIn(target, result.stdout)
                self.assertFalse((self.root / "escaped").exists())
        self.assertEqual(self.pathfile.read_text().splitlines(),
                         [str(self.install)] * (len(cases) - 1) + [r"C:\fixture\bin"])
        log = (self.root / "curl_log").read_text()
        self.assertIn("--retry 5", log)
        self.assertNotIn("--retry-all-errors", log)

    def assert_preserved(self, **fixture_args):
        self.fixture(**fixture_args)
        existing = self.install / "justrust"
        existing.write_text("original")
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(existing.read_text(), "original")
        self.assertFalse(self.pathfile.exists())
        self.assertEqual(list(self.install.iterdir()), [existing])
        return result

    def test_checksum_failure_preserves_existing(self):
        result = self.assert_preserved(bad=True)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertNotIn("justrust-fixture", result.stdout)

    def test_version_failure_preserves_existing(self):
        self.assert_preserved(status=7)

    def test_missing_member_preserves_existing(self):
        self.assert_preserved(missing=True)

    def test_unsupported_host(self):
        self.env["HOST_ARCH"] = "riscv64"
        self.assertNotEqual(self.run_install().returncode, 0)
        self.assertFalse((self.root / "curl_log").exists())

    def test_override_requires_opt_in(self):
        self.env.pop("JUSTRUST_ALLOW_TEST_URL")
        self.assertNotEqual(self.run_install().returncode, 0)
        self.assertFalse((self.root / "curl_log").exists())

    def test_default_directory_without_github_path(self):
        self.fixture()
        self.env.pop("JUSTRUST_INSTALL_DIR")
        self.env.pop("GITHUB_PATH")
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / ".local/bin/justrust").exists())

    def test_shasum_fallback(self):
        # Isolate PATH so sha256sum is genuinely unavailable.
        import shutil
        for name in ("bash", "cp", "awk", "tr", "tar", "gzip", "chmod",
                     "mkdir", "mktemp", "mv", "rm", "shasum"):
            executable = shutil.which(name)
            self.assertIsNotNone(executable, name)
            (self.stubs / name).symlink_to(executable)
        self.env["PATH"] = str(self.stubs)
        self.fixture()
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_malformed_checksum_preserves_existing(self):
        self.fixture()
        manifest = self.root / "justrust-x86_64-unknown-linux-musl.tar.gz.sha256"
        manifest.write_text("not-a-digest\n")
        existing = self.install / "justrust"
        existing.write_text("original")
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid SHA256", result.stderr)
        self.assertEqual(existing.read_text(), "original")
        self.assertFalse(self.pathfile.exists())

    def test_pinned_production_url(self):
        self.fixture()
        self.env.pop("JUSTRUST_TEST_BASE_URL")
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        log = (self.root / "curl_log").read_text()
        self.assertIn("/releases/download/v0.1.0-alpha.17/", log)
        self.assertNotIn("/latest/", log)

    def test_download_failure_falls_back_to_release_api(self):
        # The download URL answers 504; the same assets come from the API by id,
        # and an asset of another repository or name is never picked.
        self.fixture()
        name = "justrust-x86_64-unknown-linux-musl.tar.gz"
        (self.root / "release.json").write_text(
            '{"assets": [\n'
            '{"url": "https://api.github.com/repos/other/repo/releases/assets/9", "name": "%s"},\n'
            '{"url": "https://api.github.com/repos/1jehuang/justrust/releases/assets/11", "name": "%s"},\n'
            '{"url": "https://api.github.com/repos/1jehuang/justrust/releases/assets/12", "name": "%s.sha256"}]}'
            % (name, name, name))
        (self.root / "11").write_bytes((self.root / name).read_bytes())
        (self.root / "12").write_bytes((self.root / f"{name}.sha256").read_bytes())
        self.env["JUSTRUST_TEST_API_URL"] = "https://api.invalid/releases"
        self.stub("curl", '''#!/bin/bash
printf '%s\\n' "$*" >> "$CURL_LOG"
while [[ $# -gt 0 ]]; do
    case "$1" in --output) out=$2; shift 2;; --header) shift 2;; *) url=$1; shift;; esac
done
case "$url" in
    https://trusted.invalid/*) echo "curl: (22) The requested URL returned error: 504" >&2; exit 22;;
    */tags/*) cp "$FIXTURES/release.json" "$out";;
    *) cp "$FIXTURES/${url##*/}" "$out";;
esac
''')
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("justrust-fixture", result.stdout)
        log = (self.root / "curl_log").read_text()
        self.assertIn("https://api.invalid/releases/assets/11", log)
        self.assertIn("https://api.invalid/releases/assets/12", log)
        self.assertNotIn("assets/9", log)


if __name__ == "__main__":
    unittest.main()
