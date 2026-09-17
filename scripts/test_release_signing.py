import hashlib
import io
import json
import os
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
from unittest.mock import patch

try:
    import release_signing as signing
except ModuleNotFoundError:
    from scripts import release_signing as signing


SIGNED = signing.SIGNED
LINUX = signing.LINUX
COMMIT = "a" * 40
OTHER_COMMIT = "b" * 64


def linux_binary(name):
    data = bytearray(64)
    data[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<H", data, 18, 183 if name.endswith("aarch64") else 62)
    return bytes(data)


def mac_binary(name):
    data = bytearray(64)
    struct.pack_into("<6I", data, 0, 0xFEEDFACF,
                     0x0100000C if name.endswith("arm64") else 0x01000007,
                     0, 2, 1, 8)
    struct.pack_into("<II", data, 32, 0, 8)
    return bytes(data)


def pe_binary(signed=False):
    address, length = (376, 16) if signed else (0, 0)
    data = bytearray(392 if signed else 368)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 68, 0x8664, 1, 0, 0, 0, 240, 0x22)
    struct.pack_into("<H", data, 88, 0x20B)
    struct.pack_into("<I", data, 196, 5)
    struct.pack_into("<II", data, 232, address, length)
    if signed:
        struct.pack_into("<IHH", data, address, length, 0x200, 2)
    return bytes(data)


def write_json(path, value):
    path.write_text(json.dumps(value), encoding="utf-8")


def expected_native(name, digest, policy):
    common = {"schema_version": 1, "artifact": name, "artifact_sha256": digest,
              "platform": "windows" if name.endswith(".exe") else "macos", "status": "passed"}
    if name.endswith(".exe"):
        return {**common, "publisher_common_name": policy["windows"]["expected_common_name"],
                "publisher_organization": policy["windows"]["expected_organization"], "timestamped": True}
    return {**common, "identifier": policy["product"]["binary_identifier"],
            "team_id_sha256": policy["apple"]["team_id_sha256"],
            "hardened_runtime": True, "secure_timestamp": True}


def expected_evidence(asset, commit, policy, native=True):
    name = asset.name
    if name.endswith(".exe"):
        fields = ("authority", "account", "certificate_profile", "code_signing_endpoint",
                  "expected_common_name", "expected_organization", "timestamp_url")
        identity = {key: policy["windows"][key] for key in fields}
        signing_state = {"signing_command": "succeeded", "pe_certificate_table": "present",
                         "digest": "SHA-256", "timestamp_request": "RFC3161",
                         "signing": True, "timestamp": True}
    else:
        identity = {key: policy["apple"][key] for key in ("authority", "publisher", "team_id_sha256")}
        signing_state = {"signing_command": "succeeded", "notarization": "Accepted",
                         "bytes_unchanged_after_notarization": True, "for_notarization": True}
    identity["product"] = policy["product"]
    digest = hashlib.sha256(asset.read_bytes()).hexdigest()
    document = {"schema_version": 1, "artifact": name, "sha256": digest,
                "source_commit": commit,
                "policy_sha256": hashlib.sha256(
                    json.dumps(policy, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
                ).hexdigest(),
                "identity": identity, "signing": signing_state,
                "native_verification": {"status": "pending"}}
    if native:
        document["native_verification"] = expected_native(name, digest, policy)
    return document


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.assets = root / "assets"
        self.evidence = root / "evidence"
        self.assets.mkdir()
        self.evidence.mkdir()
        self.policy = signing.contract()
        for name in SIGNED:
            (self.assets / name).write_bytes((name + " fixture\n").encode())

    def write_evidence(self, commit=COMMIT, native=True):
        for name in SIGNED:
            asset = self.assets / name
            write_json(self.evidence / (name + ".json"), expected_evidence(asset, commit, self.policy, native))

    def read_evidence(self, name):
        return json.loads((self.evidence / (name + ".json")).read_text(encoding="utf-8"))

    def test_verify_accepts_complete_exact_evidence_and_matching_hashes_commit(self):
        self.write_evidence()

        signing.verify_release_evidence(self.assets, self.evidence)

    def test_verify_rejects_missing_and_extra_evidence(self):
        self.write_evidence()
        (self.evidence / (SIGNED[0] + ".json")).unlink()
        with self.assertRaisesRegex(ValueError, "inventory"):
            signing.verify_release_evidence(self.assets, self.evidence)

        self.write_evidence()
        (self.evidence / "unexpected.json").write_text("{}")
        with self.assertRaisesRegex(ValueError, "inventory"):
            signing.verify_release_evidence(self.assets, self.evidence)

    def test_verify_rejects_tampered_artifact(self):
        self.write_evidence()
        (self.assets / SIGNED[0]).write_bytes(b"tampered")

        with self.assertRaises(ValueError):
            signing.verify_release_evidence(self.assets, self.evidence)

    def test_verify_rejects_wrong_publisher_team_and_policy(self):
        for label, change in (
            ("publisher", lambda doc: doc["identity"].update(publisher="wrong")),
            ("team", lambda doc: doc["identity"].update(team_id_sha256="0" * 64)),
            ("policy", lambda doc: doc.update(policy_sha256="0" * 64)),
        ):
            with self.subTest(label=label):
                self.write_evidence()
                document = self.read_evidence(SIGNED[0])
                change(document)
                write_json(self.evidence / (SIGNED[0] + ".json"), document)
                with self.assertRaises(ValueError):
                    signing.verify_release_evidence(self.assets, self.evidence)

    def test_verify_rejects_pending_native_result(self):
        self.write_evidence()
        document = self.read_evidence(SIGNED[0])
        document["native_verification"] = {"status": "pending"}
        write_json(self.evidence / (SIGNED[0] + ".json"), document)

        with self.assertRaisesRegex(ValueError, "native"):
            signing.verify_release_evidence(self.assets, self.evidence)

    def test_verify_rejects_differing_source_commits(self):
        self.write_evidence()
        document = self.read_evidence(SIGNED[1])
        document["source_commit"] = OTHER_COMMIT
        write_json(self.evidence / (SIGNED[1] + ".json"), document)

        with self.assertRaisesRegex(ValueError, "source commits"):
            signing.verify_release_evidence(self.assets, self.evidence)

    def test_verify_binds_present_sbom_source_commit(self):
        self.write_evidence()
        sidecar = {"metadata": {"component": {"properties": [
            {"name": "retok:source-commit", "value": COMMIT}
        ]}}}
        write_json(self.assets / (LINUX[0] + ".cdx.json"), sidecar)
        signing.verify_release_evidence(self.assets, self.evidence)

        sidecar["metadata"]["component"]["properties"][0]["value"] = OTHER_COMMIT
        write_json(self.assets / (LINUX[0] + ".cdx.json"), sidecar)
        with self.assertRaisesRegex(ValueError, "SBOM"):
            signing.verify_release_evidence(self.assets, self.evidence)


class NativeRecordTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.assets = root / "assets"
        self.evidence = root / "evidence"
        self.assets.mkdir()
        self.evidence.mkdir()
        self.policy = signing.contract()

    def prepare(self, name):
        artifact = self.assets / name
        artifact.write_bytes((name + " fixture\n").encode())
        document = expected_evidence(artifact, COMMIT, self.policy, native=False)
        write_json(self.evidence / (name + ".json"), document)
        result = expected_native(name, document["sha256"], self.policy)
        result_path = self.assets / (name + ".result.json")
        write_json(result_path, result)
        return artifact, result_path, result

    def test_record_native_accepts_exact_macos_and_windows_schemas(self):
        for name in (SIGNED[0], SIGNED[-1]):
            with self.subTest(name=name):
                artifact, result_path, expected = self.prepare(name)
                signing.record_native(self.evidence, artifact, result_path)
                document = json.loads((self.evidence / (name + ".json")).read_text())
                self.assertEqual(document["native_verification"], expected)
                expected_keys = ({"schema_version", "artifact", "artifact_sha256", "platform", "status",
                                  "publisher_common_name", "publisher_organization", "timestamped"}
                                 if name.endswith(".exe") else
                                 {"schema_version", "artifact", "artifact_sha256", "platform", "status",
                                  "identifier", "team_id_sha256", "hardened_runtime", "secure_timestamp"})
                self.assertEqual(set(expected), expected_keys)

    def test_record_native_rejects_wrong_hash_fields_status_and_identity(self):
        name = SIGNED[0]
        mutations = (
            ("hash", lambda result: result.update(artifact_sha256="0" * 64)),
            ("missing field", lambda result: result.pop("secure_timestamp")),
            ("extra field", lambda result: result.update(extra=True)),
            ("status", lambda result: result.update(status="pending")),
            ("identity", lambda result: result.update(identifier="wrong")),
        )
        for label, change in mutations:
            with self.subTest(label=label):
                artifact, result_path, result = self.prepare(name)
                change(result)
                write_json(result_path, result)
                with self.assertRaisesRegex(ValueError, "schema|hash|policy"):
                    signing.record_native(self.evidence, artifact, result_path)


class InputInventoryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.source = root / "source"
        self.destination = root / "signed"
        self.evidence = root / "evidence"
        self.source.mkdir()
        self.write_inputs()

    def write_inputs(self, signed_pe=False):
        for name in SIGNED:
            data = pe_binary(signed=True) if signed_pe and name.endswith(".exe") else (
                pe_binary() if name.endswith(".exe") else mac_binary(name)
            )
            (self.source / name).write_bytes(data)
        for name in LINUX:
            (self.source / name).write_bytes(linux_binary(name))

    def call_sign(self):
        signing.sign_directory(self.source, self.destination, self.evidence, COMMIT,
                               self.source / "rcodesign.tar.gz", self.source / "jsign.jar")

    def test_sign_input_inventory_rejects_missing_and_extra_files(self):
        (self.source / SIGNED[0]).unlink()
        with self.assertRaisesRegex(ValueError, "exactly the five"):
            self.call_sign()

        self.write_inputs()
        (self.source / "unexpected").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "exactly the five"):
            self.call_sign()

    def test_sign_input_inventory_rejects_symlink_and_non_regular_file(self):
        path = self.source / SIGNED[0]
        path.unlink()
        path.symlink_to(self.source / SIGNED[1])
        with self.assertRaisesRegex(ValueError, "regular"):
            self.call_sign()

        path.unlink()
        path.mkdir()
        with self.assertRaisesRegex(ValueError, "regular"):
            self.call_sign()

    def test_sign_input_inventory_rejects_already_signed_pe(self):
        self.write_inputs(signed_pe=True)

        with self.assertRaisesRegex(ValueError, "already signed"):
            self.call_sign()


class ToolAndCredentialTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.policy = signing.contract()

    def make_archive(self, path):
        with tarfile.open(path, "w:gz") as archive:
            data = b"synthetic rcodesign"
            member = tarfile.TarInfo("rcodesign")
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))

    def test_prepare_tools_rejects_bad_checksum(self):
        archive = self.root / "rcodesign.tar.gz"
        jar = self.root / "jsign.jar"
        self.make_archive(archive)
        jar.write_bytes(b"synthetic jsign")

        with patch.object(signing.platform, "system", return_value="Linux"), \
             patch.object(signing.platform, "machine", return_value="x86_64"), \
             patch.object(signing, "tool", side_effect=("/usr/bin/openssl", "/usr/bin/java")):
            with self.assertRaisesRegex(ValueError, "checksum"):
                signing.prepare_tools(self.root, self.policy, archive, jar)

    def test_prepare_tools_rejects_bad_version_after_pinned_fixture_checks(self):
        archive = self.root / "rcodesign.tar.gz"
        jar = self.root / "jsign.jar"
        self.make_archive(archive)
        jar.write_bytes(b"synthetic jsign")
        pins = {
            "rcodesign_archive": self.policy["apple"]["rcodesign"]["sha256"],
            "jsign_jar": self.policy["windows"]["jsign"]["sha256"],
        }

        def fake_sha256(path):
            return pins[path.name]

        def fake_run(argv, home, **kwargs):
            return (b"jsign 7.4\n" if argv[0] == "/usr/bin/java" else b"rcodesign 0.29.0\n", b"")

        with patch.object(signing.platform, "system", return_value="Linux"), \
             patch.object(signing.platform, "machine", return_value="x86_64"), \
             patch.object(signing, "tool", side_effect=("/usr/bin/openssl", "/usr/bin/java")), \
             patch.object(signing, "sha256", side_effect=fake_sha256), \
             patch.object(signing, "run", side_effect=fake_run):
            with self.assertRaisesRegex(ValueError, "Jsign version"):
                signing.prepare_tools(self.root, self.policy, archive, jar)

    def valid_controls(self):
        controls = {key: "apple-secret" for key in self.policy["apple"]["credential_keys"]}
        for key, value in zip(self.policy["windows"]["credential_keys"], (
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
            "azure-secret",
        )):
            variable = key.replace("AZURE_ARTIFACT_SIGNING_", "RETOK_WINDOWS_SIGNING_") + "_FILE"
            path = self.root / variable
            path.write_text(value + "\n", encoding="utf-8")
            path.chmod(0o600)
            controls[variable] = str(path)
        return controls

    def test_injected_credentials_accept_private_files_and_strip_one_newline(self):
        controls = self.valid_controls()

        result = signing.credentials(self.root, self.policy, controls, "injected")

        self.assertEqual(result["APPLE_CODESIGN_CERT_PASSWORD"], "apple-secret")
        self.assertEqual(result["AZURE_ARTIFACT_SIGNING_TENANT_ID"],
                         "11111111-1111-1111-1111-111111111111")

    def test_secret_source_and_injected_privacy_validation_reject_bad_inputs(self):
        with self.assertRaisesRegex(ValueError, "invalid secret source"):
            signing.credentials(self.root, self.policy, {}, "network")

        controls = self.valid_controls()
        path = Path(controls["RETOK_WINDOWS_SIGNING_CLIENT_SECRET_FILE"])
        path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "private"):
            signing.credentials(self.root, self.policy, controls, "injected")

        controls = self.valid_controls()
        controls["APPLE_CODESIGN_CERT_PASSWORD"] = "contains\nnewline"
        with self.assertRaisesRegex(ValueError, "malformed credential"):
            signing.credentials(self.root, self.policy, controls, "injected")


class OrchestrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.source = root / "source"
        self.destination = root / "signed"
        self.evidence = root / "evidence"
        self.source.mkdir()
        self.original_linux = {}
        for name in SIGNED:
            (self.source / name).write_bytes(mac_binary(name) if not name.endswith(".exe") else pe_binary())
        for name in LINUX:
            data = linux_binary(name) + name.encode()
            self.original_linux[name] = data
            (self.source / name).write_bytes(data)

    def test_signing_precedes_evidence_publication_and_linux_bytes_are_unchanged(self):
        events = []
        published_linux = {}
        secret = {"private": "credential"}

        def signed_apple(asset, *args):
            events.append(("sign", asset.name))

        def signed_windows(asset, *args):
            events.append(("sign", asset.name))

        def published(source, destination):
            events.append(("publish", destination.name))
            if destination == self.destination:
                published_linux.update({name: (source / name).read_bytes() for name in LINUX})

        with patch.object(signing, "prepare_tools", return_value=(Path("openssl"), Path("java"),
                                                                    Path("rcodesign"), Path("jsign.jar"))), \
             patch.object(signing, "credentials", return_value=secret), \
             patch.object(signing, "prepare_apple", return_value=(None, None, None, "team")), \
             patch.object(signing, "sign_apple", side_effect=signed_apple), \
             patch.object(signing, "sign_windows", side_effect=signed_windows), \
             patch.object(signing, "publish_directory", side_effect=published):
            signing.sign_directory(self.source, self.destination, self.evidence, COMMIT,
                                   self.source / "rcodesign.tar.gz", self.source / "jsign.jar",
                                   secret_source="injected")

        self.assertEqual(events, [("sign", SIGNED[0]), ("sign", SIGNED[1]),
                                   ("sign", SIGNED[2]), ("publish", "evidence"),
                                   ("publish", "signed")])
        self.assertEqual(published_linux, self.original_linux)
        self.assertEqual(secret, {})


if __name__ == "__main__":
    unittest.main()
