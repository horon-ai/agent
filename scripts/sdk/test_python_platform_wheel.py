#!/usr/bin/env python3
"""Tests for the platform-wheel retagging that carries the runtime binary.

`build_python_wheels.sh` bakes each release archive into a platform wheel, so the
archive's members become files inside an installed package. The licence has to
make that trip: the wheel ships a copy of the binary outside the pure wheel's
`dist-info`, and MIT requires the notice to travel with it. A release archive
without `LICENSE` is a packaging defect, and the test below proves the script
stops instead of publishing it.
"""

from __future__ import annotations

import base64
import hashlib
import io
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import python_platform_wheel as helper

VERSION = "0.1.8"


def record_hash(data: bytes) -> str:
    digest = hashlib.sha256(data).hexdigest()
    return "sha256=" + base64.urlsafe_b64encode(bytes.fromhex(digest)).rstrip(b"=").decode("ascii")


def write_pure_wheel(path: Path) -> Path:
    dist_info = f"horon_agent_sdk-{VERSION}.dist-info"
    entries = {
        "horon_agent_sdk/__init__.py": b"",
        f"{dist_info}/WHEEL": (
            "Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
        ).encode(),
        f"{dist_info}/METADATA": f"Metadata-Version: 2.1\nName: horon-agent-sdk\nVersion: {VERSION}\n".encode(),
    }
    with zipfile.ZipFile(path, "w") as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
        record = "".join(f"{name},{record_hash(data)},{len(data)}\n" for name, data in entries.items())
        record += f"{dist_info}/RECORD,,\n"
        archive.writestr(f"{dist_info}/RECORD", record.encode())
    return path


def write_archive(path: Path, members: dict[str, bytes]) -> Path:
    with tarfile.open(path, "w:gz") as archive:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o755
            archive.addfile(info, io.BytesIO(data))
    return path


class PlatformWheelTests(unittest.TestCase):
    PRIMARY = "horon-agent-linux-x86_64"

    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.addCleanup(self._temp.cleanup)
        self.root = Path(self._temp.name)
        self.pure = write_pure_wheel(self.root / f"horon_agent_sdk-{VERSION}-py3-none-any.whl")
        self.out = self.root / "out"

    def build(self, members: dict[str, bytes]) -> Path:
        archive = write_archive(self.root / f"{self.PRIMARY}.tar.gz", members)
        helper.main(
            [
                "--pure-wheel",
                str(self.pure),
                "--asset",
                str(archive),
                "--primary",
                self.PRIMARY,
                "--tag",
                "manylinux2014_x86_64",
                "--out-dir",
                str(self.out),
            ]
        )
        return next(self.out.glob("*manylinux2014_x86_64.whl"))

    def test_the_binary_and_its_licence_both_reach_the_wheel(self) -> None:
        wheel = self.build({self.PRIMARY: b"binary", "LICENSE": b"MIT License\n"})
        names = set(zipfile.ZipFile(wheel).namelist())
        self.assertIn("horon_agent_sdk/_bin/horon-agent", names)
        self.assertIn("horon_agent_sdk/_bin/LICENSE", names)

    def test_an_archive_without_the_licence_is_refused(self) -> None:
        archive = write_archive(self.root / f"{self.PRIMARY}.tar.gz", {self.PRIMARY: b"binary"})
        with self.assertRaises(SystemExit) as caught:
            helper.main(
                [
                    "--pure-wheel",
                    str(self.pure),
                    "--asset",
                    str(archive),
                    "--primary",
                    self.PRIMARY,
                    "--tag",
                    "manylinux2014_x86_64",
                    "--out-dir",
                    str(self.out),
                ]
            )
        self.assertIn("carries no LICENSE", str(caught.exception))

    def test_the_retagged_wheel_claims_no_pure_install_root(self) -> None:
        wheel = self.build({self.PRIMARY: b"binary", "LICENSE": b"MIT License\n"})
        with zipfile.ZipFile(wheel) as archive:
            wheel_text = archive.read(f"horon_agent_sdk-{VERSION}.dist-info/WHEEL").decode()
        self.assertIn("Tag: py3-none-manylinux2014_x86_64", wheel_text)
        self.assertIn("Root-Is-Purelib: false", wheel_text)


if __name__ == "__main__":
    unittest.main()
