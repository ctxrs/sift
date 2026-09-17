"""Exercise the shell verifier with synthetic executables and mocked codesign."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class MacosVersionTests(unittest.TestCase):
    def test_version_requires_exact_stdout_empty_stderr_and_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scripts = root / "scripts"
            contracts = root / "contracts"
            tools = root / "tools"
            for directory in (scripts, contracts, tools):
                directory.mkdir()
            verifier = scripts / "verify_macos_release.sh"
            shutil.copyfile(Path(__file__).with_name(verifier.name), verifier)
            team = "TESTTEAM01"
            team_hash = hashlib.sha256(team.encode()).hexdigest()
            (contracts / "release-signing-v1.json").write_text(json.dumps({
                "product": {"binary_identifier": "retok"},
                "apple": {"team_id_sha256": team_hash},
            }))
            codesign = tools / "codesign"
            codesign.write_text("#!" + sys.executable + "\n" +
                                "import sys\nif sys.argv[1] == '-d':\n    print(" + repr(
                                    "Identifier=retok\nTeamIdentifier=" + team +
                                    "\nAuthority=Developer ID Application: Test Publisher (" + team + ")" +
                                    "\nCodeDirectory v=20500 flags=0x10000(runtime)" +
                                    "\nTimestamp=synthetic timestamp"
                                ) + ", file=sys.stderr)\n")
            codesign.chmod(0o755)
            (tools / "python3").symlink_to(sys.executable)
            shasum = shutil.which("shasum")
            self.assertIsNotNone(shasum, "shasum is required for this regression check")
            (tools / "shasum").symlink_to(shasum)
            artifact_dir = root / "back\\slash-and-new\nline"
            artifact_dir.mkdir()
            artifact = artifact_dir / "retok-macos-x64"
            for stdout, stderr, code, accepted in (
                (b"Retok 0.1.0\n", b"", 0, True),
                (b"Retok 0.1.0\n\n", b"", 0, False),
                (b"Retok 0.1.0\n", b"warning\n", 0, False),
                (b"Retok 0.1.0", b"", 0, False),
                (b"Retok 0.1.0\n", b"", 1, False),
            ):
                with self.subTest(stdout=stdout, stderr=stderr, code=code):
                    artifact.write_text("#!" + sys.executable + "\nimport sys\n"
                                        "assert sys.argv[1:] == ['--version']\n"
                                        f"sys.stdout.buffer.write({stdout!r})\n"
                                        f"sys.stderr.buffer.write({stderr!r})\n"
                                        f"sys.exit({code})\n")
                    artifact.chmod(0o755)
                    result = subprocess.run(
                        ["/bin/bash", str(verifier), str(artifact), "0.1.0"],
                        env={"PATH": str(tools) + os.pathsep + os.defpath, "HOME": str(root)},
                        capture_output=True, timeout=15,
                    )
                    if accepted:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        document = json.loads(result.stdout)
                        self.assertEqual(document["status"], "passed")
                        self.assertEqual(document["artifact_sha256"],
                                         hashlib.sha256(artifact.read_bytes()).hexdigest())
                        self.assertEqual(result.stderr, b"")
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn(b"unexpected version output", result.stderr)
                        self.assertEqual(result.stdout, b"")


if __name__ == "__main__":
    unittest.main()
