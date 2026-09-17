"""Offline installer tests: python3 tests/test_install.py (no dependencies)."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"
RELEASES = "https://github.com/ctxrs/retok/releases"
ASSETS = (
    "retok-linux-x64",
    "retok-linux-aarch64",
    "retok-macos-x64",
    "retok-macos-arm64",
    "retok-windows-x64.exe",
)

# Only these mocks are on PATH for network/platform/checksum commands. The curl
# mock rejects unexpected URLs and copies fixtures; it cannot access a network.
MOCK = r'''
import hashlib
import os
from pathlib import Path
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "uname":
    print(os.environ["MOCK_OS" if args == ["-s"] else "MOCK_ARCH"])
elif name == "curl":
    assert args[:4] == ["-fsSL", "--proto", "=https", "--tlsv1.2"], args
    url, flag, destination = args[4:]
    assert flag == "-o", args
    asset = url.rsplit("/", 1)[1]
    assert url == os.environ["MOCK_BASE"] + "/" + asset, url
    with open(os.environ["MOCK_LOG"], "a") as log:
        log.write(url + "\n")
    if asset == os.environ.get("MOCK_DOWNLOAD_FAILURE"):
        Path(destination).write_bytes(b"partial download")
        sys.exit(22)
    Path(destination).write_bytes((Path(os.environ["MOCK_RELEASE"]) / asset).read_bytes())
elif name == "mv":
    assert args[0] == "-f", args
    source, destination = map(Path, args[1:])
    if destination.name == "retok":
        notices = destination.with_name("retok.third-party-notices.txt")
        expected = Path(os.environ["MOCK_RELEASE"]) / "retok-linux-x64.third-party-notices.txt"
        assert notices.read_bytes() == expected.read_bytes(), "Notices were not replaced first"
        Path(os.environ["MOCK_DENIED_LOG"]).write_text("binary replacement denied")
        print("mv: permission denied replacing executable", file=sys.stderr)
        sys.exit(1)
    os.replace(source, destination)
else:
    assert name in ("sha256sum", "shasum"), name
    if name == "shasum":
        assert args == ["-a", "256"], args
    else:
        assert args == [], args
    if os.environ.get("MOCK_CHECKSUM_FAILURE"):
        sys.exit(1)
    print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest() + "  -")
'''


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="retok-installer-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.release = self.root / "release"
        self.home = self.root / "home with spaces"
        self.downloads = self.root / "temporary downloads"
        for directory in (self.bin, self.release, self.home, self.downloads):
            directory.mkdir()
        for utility in ("awk", "tr", "mktemp", "rm", "mkdir", "chmod", "mv", "cp"):
            (self.bin / utility).symlink_to(shutil.which(utility))
        for command in ("uname", "curl", "sha256sum"):
            self.mock(command)
        sums = []
        for asset in (*ASSETS, *(name + ".third-party-notices.txt" for name in ASSETS)):
            payload = ("synthetic release: " + asset + "\n").encode()
            (self.release / asset).write_bytes(payload)
            sums.append(hashlib.sha256(payload).hexdigest() + "  " + asset + "\n")
        self.sums = self.release / "SHA256SUMS"
        self.sums.write_text("".join(sums))
        self.env = {
            "PATH": str(self.bin),
            "HOME": str(self.home),
            "TMPDIR": str(self.downloads),
            "MOCK_OS": "Linux",
            "MOCK_ARCH": "x86_64",
            "MOCK_BASE": RELEASES + "/latest/download",
            "MOCK_LOG": str(self.root / "requests"),
            "MOCK_RELEASE": str(self.release),
        }
        self.destination = self.home / ".local/bin/retok"

    def mock(self, name):
        command = self.bin / name
        command.write_text("#!" + sys.executable + "\n" + MOCK)
        command.chmod(0o755)

    def run_installer(self, *args):
        result = subprocess.run(
            ["/bin/sh", "-s", "--", *args], input=INSTALLER.read_text(), text=True,
            env=self.env, cwd=self.root, capture_output=True, timeout=15,
        )
        self.assertEqual(list(self.downloads.iterdir()), [], "downloads must be cleaned up")
        if self.destination.parent.exists():
            self.assertEqual(list(self.destination.parent.glob(".retok.*")), [])
        return result

    def assert_installed(self, asset):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.destination.read_bytes(), (self.release / asset).read_bytes())
        self.assertEqual(self.destination.stat().st_mode & 0o777, 0o755)
        notices = self.destination.with_name("retok.third-party-notices.txt")
        self.assertEqual(notices.read_bytes(), (self.release / (asset + ".third-party-notices.txt")).read_bytes())
        self.assertEqual(notices.stat().st_mode & 0o777, 0o644)
        self.assertIn("PATH", result.stdout)
        self.assertEqual(
            (self.root / "requests").read_text().splitlines(),
            [self.env["MOCK_BASE"] + "/" + name
             for name in ("SHA256SUMS", asset, asset + ".third-party-notices.txt")],
        )
        (self.root / "requests").unlink()

    def assert_preserved(self, message=None):
        self.destination.parent.mkdir(parents=True, exist_ok=True)
        self.destination.write_bytes(b"previous working retok")
        notices = self.destination.with_name("retok.third-party-notices.txt")
        notices.write_bytes(b"previous notices")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        if message:
            self.assertIn(message, result.stderr)
        self.assertEqual(self.destination.read_bytes(), b"previous working retok")
        self.assertEqual(notices.read_bytes(), b"previous notices")
        self.assertNotIn("Installed retok", result.stdout)

    def setup_executable(self):
        payload = ("#!" + sys.executable + "\n" + r'''
import json, os, sys
from pathlib import Path
binary = Path(sys.argv[0])
assert binary.is_absolute()
assert binary.with_name("retok.third-party-notices.txt").read_bytes() == (
    Path(os.environ["MOCK_RELEASE"]) / "retok-linux-x64.third-party-notices.txt"
).read_bytes()
Path(os.environ["MOCK_SETUP_LOG"]).write_text(json.dumps(sys.argv))
sys.exit(int(os.environ.get("MOCK_SETUP_EXIT", "0")))
''').encode()
        asset = "retok-linux-x64"
        (self.release / asset).write_bytes(payload)
        lines = self.sums.read_text().splitlines()
        self.sums.write_text("".join(
            (hashlib.sha256(payload).hexdigest() + "  " + asset if line.split()[1] == asset else line) + "\n"
            for line in lines
        ))
        self.env["MOCK_SETUP_LOG"] = str(self.root / "setup-args.json")
        # If setup accidentally uses PATH, fail instead of touching any real binary.
        shadow = self.bin / "retok"
        shadow.write_text("#!/bin/sh\nexit 99\n")
        shadow.chmod(0o755)

    def test_setup_runs_exact_installed_binary_and_arguments(self):
        self.setup_executable()
        self.env["RETOK_INSTALL_DIR"] = "relative install dir/bin"
        self.destination = self.root / "relative install dir/bin/retok"
        for flag, expected in (("--init", ["init"]), ("--replace-rtk", ["init", "--replace-rtk"])):
            with self.subTest(flag=flag):
                result = self.run_installer(flag)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads((self.root / "setup-args.json").read_text()),
                                 [str(self.destination), *expected])

    def test_no_arguments_never_runs_setup(self):
        self.setup_executable()
        self.assert_installed("retok-linux-x64")
        self.assertFalse((self.root / "setup-args.json").exists())

    def test_setup_failure_retains_installed_binary_and_notices(self):
        self.setup_executable()
        self.env["MOCK_SETUP_EXIT"] = "7"
        for flag in ("--init", "--replace-rtk"):
            with self.subTest(flag=flag):
                result = self.run_installer(flag)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Binary installed", result.stderr)
                self.assertIn("integration failed", result.stderr)
                self.assertEqual(self.destination.read_bytes(), (self.release / "retok-linux-x64").read_bytes())
                self.assertEqual(self.destination.with_name("retok.third-party-notices.txt").read_bytes(),
                                 (self.release / "retok-linux-x64.third-party-notices.txt").read_bytes())

    def test_invalid_flags_and_help_do_not_download_or_create_files(self):
        before = set(self.root.rglob("*"))
        for args in (("--unknown",), ("--init", "--replace-rtk"), ("--init", "extra"),
                     ("--init", "--init"), ("",), ("--help", "extra")):
            with self.subTest(args=args):
                result = self.run_installer(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "requests").exists())
                self.assertEqual(set(self.root.rglob("*")), before)
        result = self.run_installer("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--replace-rtk", result.stdout)
        self.assertIn("--init", result.stdout)
        self.assertFalse((self.root / "requests").exists())
        self.assertEqual(set(self.root.rglob("*")), before)

    def test_failed_verification_never_runs_setup(self):
        self.setup_executable()
        (self.release / "retok-linux-x64.third-party-notices.txt").write_bytes(b"corrupt")
        result = self.run_installer("--replace-rtk")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "setup-args.json").exists())
        self.assertFalse(self.destination.exists())

    def test_all_unix_assets_and_upgrade(self):
        for system, arch, asset in (
            ("Linux", "x86_64", "retok-linux-x64"),
            ("Linux", "aarch64", "retok-linux-aarch64"),
            ("Darwin", "x86_64", "retok-macos-x64"),
            ("Darwin", "arm64", "retok-macos-arm64"),
        ):
            with self.subTest(system=system, arch=arch):
                self.env.update(MOCK_OS=system, MOCK_ARCH=arch)
                self.assert_installed(asset)

    def test_pinned_version_custom_directory_and_shasum(self):
        (self.bin / "sha256sum").unlink()
        self.mock("shasum")
        self.env.update(RETOK_VERSION="v0.1.0", RETOK_INSTALL_DIR="relative dir/bin")
        self.env["MOCK_BASE"] = RELEASES + "/download/v0.1.0"
        self.destination = self.root / "relative dir/bin/retok"
        self.assert_installed("retok-linux-x64")

    def test_real_checksum_utilities_with_special_directory_names(self):
        (self.bin / "sha256sum").unlink()
        for utility in ("sha256sum", "shasum"):
            executable = shutil.which(utility)
            self.assertIsNotNone(executable, utility + " is required for this regression check")
            command = self.bin / utility
            command.symlink_to(executable)
            try:
                for special in ("back\\slash", "new\nline"):
                    with self.subTest(utility=utility, directory=special):
                        directory = self.root / special
                        directory.mkdir(exist_ok=True)
                        self.env.update(TMPDIR=str(directory), RETOK_INSTALL_DIR=str(directory / "bin"))
                        self.destination = directory / "bin/retok"
                        self.assert_installed("retok-linux-x64")
            finally:
                command.unlink()

    def test_binary_marker_and_uppercase_digest(self):
        entries = [line.split() for line in self.sums.read_text().splitlines()]
        self.sums.write_text("".join(digest.upper() + " *" + asset + "\n" for digest, asset in entries))
        self.assert_installed("retok-linux-x64")

    def test_corrupt_binary_or_notices_preserves_installation(self):
        for name in ("retok-linux-x64", "retok-linux-x64.third-party-notices.txt"):
            with self.subTest(asset=name):
                path = self.release / name
                original = path.read_bytes()
                path.write_bytes(b"corrupted download")
                self.assert_preserved("SHA-256 mismatch for " + name)
                path.write_bytes(original)

    def test_missing_malformed_duplicate_and_wrong_asset_checksums(self):
        original = self.sums.read_text()
        for name in ("retok-linux-x64", "retok-linux-x64.third-party-notices.txt"):
            entry = next(line + "\n" for line in original.splitlines() if line.split()[1] == name)
            for replacement in ("", entry.replace(name, "other-asset"), entry * 2,
                                "z" * 64 + "  " + name + "\n", "abc  " + name + "\n",
                                entry + "abc  " + name + "\n"):
                with self.subTest(asset=name, replacement=replacement):
                    self.sums.write_text(original.replace(entry, replacement))
                    self.assert_preserved("checksum")

    def test_failed_downloads_preserve_installation(self):
        for asset in ("retok-linux-x64", "retok-linux-x64.third-party-notices.txt", "SHA256SUMS"):
            with self.subTest(asset=asset):
                self.env["MOCK_DOWNLOAD_FAILURE"] = asset
                self.assert_preserved()

    def test_checksum_command_failure_preserves_installation(self):
        self.env["MOCK_CHECKSUM_FAILURE"] = "1"
        self.assert_preserved()

    def test_unsupported_platforms_fail_before_download(self):
        for system, arch in (("FreeBSD", "x86_64"), ("Linux", "i686"), ("Darwin", "ppc")):
            with self.subTest(system=system, arch=arch):
                self.env.update(MOCK_OS=system, MOCK_ARCH=arch)
                self.assert_preserved("Unsupported platform")
                self.assertFalse((self.root / "requests").exists())

    def test_missing_dependencies_fail_before_download(self):
        for name in ("curl", "sha256sum"):
            with self.subTest(name=name):
                (self.bin / name).unlink()
                self.assert_preserved("required")
                self.assertFalse((self.root / "requests").exists())
                self.mock(name)

    def test_invalid_version_fails_before_download(self):
        self.env["RETOK_VERSION"] = "../../other"
        self.assert_preserved("Invalid RETOK_VERSION")
        self.assertFalse((self.root / "requests").exists())

    def test_destination_directory_is_not_treated_as_install_target(self):
        for name in ("retok", "retok.third-party-notices.txt"):
            with self.subTest(name=name):
                target = self.destination.with_name(name)
                target.mkdir(parents=True)
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("is a directory", result.stderr)
                self.assertEqual(list(target.iterdir()), [])
                target.rmdir()

    def test_failed_staging_preserves_installation(self):
        (self.bin / "chmod").unlink()
        (self.bin / "chmod").write_text("#!/bin/sh\nexit 1\n")
        (self.bin / "chmod").chmod(0o755)
        self.assert_preserved()

    def test_replacement_does_not_truncate_open_files(self):
        self.destination.parent.mkdir(parents=True)
        self.destination.write_bytes(b"previous working retok")
        notices = self.destination.with_name("retok.third-party-notices.txt")
        notices.write_bytes(b"previous notices")
        with self.destination.open("rb") as old_binary, notices.open("rb") as old_notices:
            self.assert_installed("retok-linux-x64")
            self.assertEqual(old_binary.read(), b"previous working retok")
            self.assertEqual(old_notices.read(), b"previous notices")

    def test_bad_notices_on_fresh_install_installs_neither_file(self):
        (self.release / "retok-linux-x64.third-party-notices.txt").write_bytes(b"corrupt")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())
        self.assertFalse(self.destination.with_name("retok.third-party-notices.txt").exists())

    def test_denied_binary_replacement_restores_previous_notices_state(self):
        (self.bin / "mv").unlink()
        self.mock("mv")
        denied_log = self.root / "denied"
        self.env["MOCK_DENIED_LOG"] = str(denied_log)
        old_notices = b"previous notices\r\n\x00\xff exact bytes\n"
        # Also cover upgrades from installations that did not ship notices.
        for had_binary, had_notices in ((False, False), (True, False), (True, True), (False, True)):
            with self.subTest(had_binary=had_binary, had_notices=had_notices):
                self.destination.parent.mkdir(parents=True, exist_ok=True)
                if had_binary:
                    self.destination.write_bytes(b"previous working retok")
                notices = self.destination.with_name("retok.third-party-notices.txt")
                if had_notices:
                    notices.write_bytes(old_notices)
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("permission denied replacing executable", result.stderr)
                self.assertEqual(denied_log.read_text(), "binary replacement denied")
                self.assertNotIn("Installed retok", result.stdout)
                if had_binary:
                    self.assertEqual(self.destination.read_bytes(), b"previous working retok")
                    self.destination.unlink()
                else:
                    self.assertFalse(self.destination.exists())
                if had_notices:
                    self.assertEqual(notices.read_bytes(), old_notices)
                    notices.unlink()
                else:
                    self.assertFalse(notices.exists())
                denied_log.unlink()


if __name__ == "__main__":
    unittest.main(verbosity=2)
