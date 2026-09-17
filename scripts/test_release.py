"""Synthetic packaging checks: python3 -B -m unittest discover -s scripts -v.

Fixtures contain headers only. Mocked version execution tests dispatch/output;
these tests do not establish that production binaries run on their targets.
"""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import release
import release_metadata as metadata


# Deliberately independent of the implementation's asset inventory.
BINARIES = (
    "retok-linux-x64", "retok-linux-aarch64", "retok-macos-x64",
    "retok-macos-arm64", "retok-windows-x64.exe",
)
TEST_VOCAB = b"synthetic vocabulary\n"


def executable(name):
    data = bytearray(256)
    if "linux" in name:
        data[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HHI", data, 16, 3, 183 if "aarch64" in name else 62, 1)
        struct.pack_into("<Q", data, 32, 64)
        struct.pack_into("<HHH", data, 52, 64, 56, 1)
        struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 0, 0, 0, 256, 256, 4096)
    elif "macos" in name:
        cpu = 0x0100000C if "arm64" in name else 0x01000007
        struct.pack_into("<8I", data, 0, 0xFEEDFACF, cpu, 0, 2, 1, 72, 0, 0)
        struct.pack_into("<II", data, 32, 0x19, 72)
    else:
        data = bytearray(512)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 60, 64)
        data[64:68] = b"PE\0\0"
        struct.pack_into("<HHIIIHH", data, 68, 0x8664, 1, 0, 0, 0, 240, 0x22)
        struct.pack_into("<H", data, 88, 0x20B)
    return data + TEST_VOCAB


def synthetic_metadata(binary):
    notices = "Synthetic notices for packaging tests only.\n"
    notices += "\n".join(metadata.SPECIAL_MARKERS) + "\n"
    libs = []
    for name in ("tiktoken-rs", "regex-syntax", "unicode-ident"):
        ref = f"pkg:cargo/{name}@1.2.3"
        declared = {"regex-syntax": "MIT OR Apache-2.0",
                    "unicode-ident": "(MIT OR Apache-2.0) AND Unicode-3.0"}.get(name, "MIT")
        libs.append({"type": "library", "name": name, "version": "1.2.3",
                     "bom-ref": ref, "purl": ref, "hashes": metadata.hashes("1" * 64),
                     "licenses": [{"expression": metadata.effective_license(name, declared)}],
                     "properties": metadata.properties({"cargo-license-expression": declared})})
        notices += f"===== crate {name} 1.2.3 =====\nSynthetic license text\n"
    vocab_hash = hashlib.sha256(TEST_VOCAB).hexdigest()
    vocab_ref = "o200k_base:sha256:" + vocab_hash
    vocab = {"type": "data", "name": "o200k_base", "version": vocab_hash,
             "bom-ref": vocab_ref, "hashes": metadata.hashes(vocab_hash),
             "licenses": [{"expression": "MIT"}],
             "externalReferences": [{"type": "distribution", "url": metadata.VOCAB_URL},
                                    {"type": "vcs", "url": metadata.OPENAI_URL}],
             "properties": metadata.properties({"embedded-in": "root", "source-path": metadata.VOCAB_PATH,
                                                "source-commit": "3" * 40,
                                                "embedded-offset": str(binary.stat().st_size - len(TEST_VOCAB)),
                                                "embedded-size": str(len(TEST_VOCAB)),
                                                "source-crate": libs[0]["bom-ref"]})}
    target = metadata.TRIPLES[binary.name][0]
    runtime_text = "Synthetic runtime license for tests only.\n"
    runtime_entry = {"name": "rust-std", "version": "1.90.0", "source": "https://example.org/rust-1.90.0",
                     "license": "MIT", "sha256": "7" * 64,
                     "notices": [{"label": "Rust license", "sha256": hashlib.sha256(runtime_text.encode()).hexdigest()}]}
    runtime = metadata.runtime_component(runtime_entry, target)
    start, end = metadata.runtime_notice_markers(runtime_entry, runtime_entry["notices"][0])
    notices += start + runtime_text + end
    runtime_manifest = metadata.canonical_runtime_manifest([runtime_entry], target)
    root = {"type": "application", "name": "retok", "version": release.VERSION, "bom-ref": "root",
            "hashes": metadata.hashes(metadata.sha256(binary)),
            "properties": metadata.properties({
                "target": metadata.TRIPLES[binary.name][0], "source-commit": "4" * 40,
                "cargo-lock-sha256": "5" * 64, "cargo-manifest-sha256": "6" * 64,
                "runtime-manifest": runtime_manifest,
                "runtime-manifest-sha256": hashlib.sha256(runtime_manifest.encode()).hexdigest(),
                "notices-sha256": hashlib.sha256(notices.encode()).hexdigest(),
                "normal-dependencies": json.dumps(sorted(c["bom-ref"] for c in libs)),
            })}
    return {"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
            "metadata": {"component": root}, "components": libs + [vocab, runtime],
            "dependencies": [{"ref": "root", "dependsOn": [c["bom-ref"] for c in libs] + [runtime["bom-ref"]]},
                             {"ref": runtime["bom-ref"], "dependsOn": []},
                             {"ref": vocab_ref, "dependsOn": []}]
                            + [{"ref": c["bom-ref"], "dependsOn": [vocab_ref] if i == 0 else []}
                               for i, c in enumerate(libs)]}, notices


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "inputs"
        self.source.mkdir()
        self.output = self.root / "release"
        self.evidence = self.root / "signing-evidence"
        self.evidence.mkdir()
        self.names = []
        for name in BINARIES:
            (self.source / name).write_bytes(executable(name))
            bom, notices = synthetic_metadata(self.source / name)
            (self.source / (name + ".cdx.json")).write_text(json.dumps(bom), encoding="utf-8")
            (self.source / (name + ".third-party-notices.txt")).write_bytes(notices.encode("utf-8"))
            self.names.extend((name, name + ".cdx.json", name + ".third-party-notices.txt"))
        self.enterContext(contextlib.redirect_stdout(io.StringIO()))
        self.system = self.enterContext(patch("release.platform.system", return_value="Unknown"))
        self.machine = self.enterContext(patch("release.platform.machine", return_value="unknown"))
        self.run = self.enterContext(patch("release.subprocess.run"))
        self.run.return_value = subprocess.CompletedProcess(
            [], 0, f"Retok {release.VERSION}\n".encode(), b""
        )
        self.verify_signing = self.enterContext(
            patch("release.release_signing.verify_release_evidence")
        )

    def test_stage_reproducible_bytes_inventory_modes_and_verify(self):
        release.stage(self.source, self.output, self.evidence)
        second = self.root / "second"
        release.stage(self.source, second, self.evidence)
        self.assertEqual({p.name for p in self.output.iterdir()}, set(self.names) | {"SHA256SUMS"})
        expected = "".join(
            f"{hashlib.sha256((self.source / name).read_bytes()).hexdigest()}  {name}\n"
            for name in sorted(self.names)
        ).encode("ascii")
        self.assertEqual((self.output / "SHA256SUMS").read_bytes(), expected)
        for name in self.names + ["SHA256SUMS"]:
            self.assertEqual((self.output / name).read_bytes(), (second / name).read_bytes())
            if name != "SHA256SUMS":
                self.assertEqual((self.source / name).read_bytes(), (self.output / name).read_bytes())
            if os.name != "nt":
                self.assertEqual(stat.S_IMODE((self.output / name).stat().st_mode),
                                 0o755 if name in BINARIES else 0o644)
        release.validate(self.output, with_checksums=True)
        self.run.assert_not_called()

    def test_each_native_target_runs_only_its_binary(self):
        for system, machine, name in (
            ("Linux", "x86_64", "retok-linux-x64"),
            ("Linux", "aarch64", "retok-linux-aarch64"),
            ("Darwin", "x86_64", "retok-macos-x64"),
            ("Darwin", "arm64", "retok-macos-arm64"),
            ("Windows", "AMD64", "retok-windows-x64.exe"),
        ):
            with self.subTest(name=name):
                self.system.return_value = system
                self.machine.return_value = machine
                self.run.reset_mock()
                release.stage(self.source, self.root / name, self.evidence)
                self.run.assert_called_once()
                args = self.run.call_args.args[0]
                self.assertEqual(Path(args[0]).name, name)
                self.assertEqual(args[1:], ["--version"])

    def test_version_failures_do_not_publish_output(self):
        self.system.return_value = "Linux"
        self.machine.return_value = "x86_64"
        for code, stdout, stderr in (
            (0, b"Retok 9.9.9\n", b""),
            (1, f"Retok {release.VERSION}\n".encode(), b""),
            (0, f"retok {release.VERSION}\n".encode(), b""),
            (0, f"Retok {release.VERSION}\nextra\n".encode(), b""),
            (0, f"Retok {release.VERSION}\n".encode(), b"warning"),
        ):
            with self.subTest(code=code, stdout=stdout, stderr=stderr):
                self.run.return_value = subprocess.CompletedProcess([], code, stdout, stderr)
                with self.assertRaisesRegex(ValueError, "--version"):
                    release.stage(self.source, self.output, self.evidence)
                self.assertFalse(self.output.exists())
                self.assertEqual(list(self.root.glob(".retok-stage-*")), [])
        for failure in (OSError("cannot execute"), subprocess.TimeoutExpired("retok", 10)):
            self.run.side_effect = failure
            with self.assertRaises(type(failure)):
                release.stage(self.source, self.output, self.evidence)
            self.assertFalse(self.output.exists())

    def test_crlf_version_output_is_accepted(self):
        self.system.return_value = "Windows"
        self.machine.return_value = "AMD64"
        self.run.return_value.stdout = f"Retok {release.VERSION}\r\n".encode()
        release.stage(self.source, self.output, self.evidence)

    def test_exact_inventory_rejects_missing_and_extra_files(self):
        extra = self.source / "unexpected"
        extra.write_text("extra")
        with self.assertRaisesRegex(ValueError, "unexpected"):
            release.stage(self.source, self.output, self.evidence)
        extra.unlink()
        (self.source / self.names[-1]).unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            release.stage(self.source, self.output, self.evidence)

    def test_symlinks_and_directories_rejected(self):
        path = self.source / BINARIES[0]
        path.unlink()
        path.mkdir()
        with self.assertRaisesRegex(ValueError, "regular file"):
            release.stage(self.source, self.output, self.evidence)
        path.rmdir()
        path.symlink_to(self.source / BINARIES[1])
        with self.assertRaisesRegex(ValueError, "symlink"):
            release.stage(self.source, self.output, self.evidence)

    def test_existing_destination_is_preserved(self):
        self.output.mkdir()
        marker = self.output / "keep"
        marker.write_text("keep")
        with self.assertRaisesRegex(ValueError, "already exist"):
            release.stage(self.source, self.output, self.evidence)
        self.assertEqual(marker.read_text(), "keep")

    def test_signing_evidence_is_required_before_staging(self):
        self.verify_signing.side_effect = ValueError("invalid signing evidence")
        with self.assertRaisesRegex(ValueError, "signing evidence"):
            release.stage(self.source, self.output, self.evidence)
        self.assertFalse(self.output.exists())

    def test_copied_bytes_are_rechecked_against_signing_evidence(self):
        signed_name = "retok-macos-x64"
        calls = 0

        def verify(directory, _evidence):
            nonlocal calls
            calls += 1
            if calls == 1:
                binary = self.source / signed_name
                raw = binary.read_bytes()
                binary.write_bytes(raw[:-len(TEST_VOCAB)] + b"changed after first check" + TEST_VOCAB)
                document, notices = synthetic_metadata(binary)
                (self.source / (signed_name + ".cdx.json")).write_text(
                    json.dumps(document), encoding="utf-8"
                )
                (self.source / (signed_name + ".third-party-notices.txt")).write_text(
                    notices, encoding="utf-8"
                )
                return
            self.assertNotEqual(directory, self.source)
            raise ValueError("copied bytes do not match signing evidence")

        self.verify_signing.side_effect = verify
        with self.assertRaisesRegex(ValueError, "copied bytes"):
            release.stage(self.source, self.output, self.evidence)
        self.assertEqual(calls, 2)
        self.assertFalse(self.output.exists())

    def test_tampering_and_noncanonical_checksums_rejected(self):
        release.stage(self.source, self.output, self.evidence)
        sums = self.output / "SHA256SUMS"
        original = sums.read_bytes()
        for altered in (original.replace(b"\n", b"\r\n"),
                        b"\n".join(reversed(original.splitlines())) + b"\n",
                        original + original.splitlines()[0] + b"\n"):
            sums.write_bytes(altered)
            with self.assertRaisesRegex(ValueError, "SHA256SUMS"):
                release.validate(self.output, with_checksums=True)
        sums.write_bytes(original)
        asset = self.output / BINARIES[0]
        asset.write_bytes(asset.read_bytes() + b"tampered")
        with self.assertRaisesRegex(ValueError, "SHA256SUMS"):
            release.validate(self.output, with_checksums=True)
        self.run.assert_not_called()

    def test_wrong_architecture_truncation_and_scripts_rejected(self):
        for name in BINARIES:
            path = self.source / name
            original = path.read_bytes()
            wrong_arch = bytearray(original)
            arch_offset = 18 if "linux" in name else 4 if "macos" in name else 68
            wrong_arch[arch_offset:arch_offset + 2] = b"\0\0"
            for invalid in (original[:20], original[:90], b"#!/bin/sh\n" + b"x" * 256, wrong_arch):
                with self.subTest(name=name, size=len(invalid)):
                    path.write_bytes(invalid)
                    with self.assertRaises(ValueError):
                        release.stage(self.source, self.output, self.evidence)
                    self.assertFalse(self.output.exists())
            path.write_bytes(original)

    def test_metadata_and_notices_must_be_meaningful(self):
        path = self.source / (BINARIES[0] + ".cdx.json")
        original = path.read_bytes()
        for invalid in (b"not json", b"[]", b"{}",
                        original.replace(release.VERSION.encode(), b"9.9.9"),
                        original.replace(b'"name": "retok"', b'"name": 123'),
                        original.replace(b'"version": "1.2.3"', b'"version": ""')):
            path.write_bytes(invalid)
            with self.assertRaises(ValueError):
                release.stage(self.source, self.output, self.evidence)
        path.write_bytes(original)
        notices = self.source / (BINARIES[0] + ".third-party-notices.txt")
        notices.write_text(" \n")
        with self.assertRaisesRegex(ValueError, "empty notices"):
            release.stage(self.source, self.output, self.evidence)

    @unittest.skipIf(os.name == "nt", "POSIX execute permissions")
    def test_native_execution_cannot_be_skipped_by_removing_permissions(self):
        release.stage(self.source, self.output, self.evidence)
        self.system.return_value = "Linux"
        self.machine.return_value = "x86_64"
        (self.output / BINARIES[0]).chmod(0o644)
        with self.assertRaisesRegex(ValueError, "not executable"):
            release.validate(self.output, with_checksums=True)
        self.run.assert_not_called()

    def test_cli_failure_is_concise_and_nonzero(self):
        with patch.object(sys, "argv", ["release.py", "verify", str(self.source)]):
            with contextlib.redirect_stderr(io.StringIO()) as stderr:
                with self.assertRaises(SystemExit) as error:
                    release.main()
        self.assertEqual(error.exception.code, 1)
        self.assertIn("SHA256SUMS", stderr.getvalue())
        self.assertNotIn("Traceback", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
