#!/usr/bin/env python3
"""Retok release signing (Python 3.11+, official signer: Linux x86_64).

  release_signing.py sign UNSIGNED NEW_SIGNED NEW_EVIDENCE --source-commit FULL_SHA \
      --rcodesign RCODESIGN_TAR_GZ --jsign-jar JSIGN_JAR
  release_signing.py record-native EVIDENCE_DIR ARTIFACT RESULT_JSON
  release_signing.py verify ASSET_DIR EVIDENCE_DIR

--rcodesign takes the distribution archive pinned by the contract, not a loose
executable. Both tools are hash/version checked. No automatic tool downloads.
OpenSSL 3, Java, and (by default) an authenticated Infisical CLI are prerequisites.
--secret-source injected reads Apple credentials from the exact contract env
names, and Azure credentials from RETOK_WINDOWS_SIGNING_TENANT_ID_FILE,
RETOK_WINDOWS_SIGNING_CLIENT_ID_FILE, RETOK_WINDOWS_SIGNING_CLIENT_SECRET_FILE.
These must reference private regular files. Never put secrets on the CLI.

Sign accepts exactly the five binaries, before metadata generation. It publishes
a fresh directory with unchanged Linux binaries and pending native evidence in
a separate fresh directory. Transfer each signed artifact and its evidence to
its native OS, run the native checker, and import its sanitized JSON using
record-native. Jsign 7.5 has no verify command. Only then generate metadata.
Native result platform values are exactly "macos" and "windows". The result
must contain exactly schema_version, artifact, artifact_sha256, platform,
status="passed", plus identifier/team_id_sha256/hardened_runtime/secure_timestamp
on macOS or publisher_common_name/publisher_organization/timestamped on Windows.
Boolean checks must be true; strings must match the public contract.

Evidence is a trusted operator record, not an independently signed attestation.
Keep its custody separate from untrusted assets. Offline verification checks
the exact policy and bytes, not OS trust/revocation or build provenance. Source
commit is the caller's assertion, compared with SBOMs when they are present.
"""

import argparse
import base64
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import ssl
import stat
import struct
import subprocess
import tarfile
import tempfile
import urllib.parse
import urllib.request
import uuid
import zipfile


CONTRACT_PATH = Path(__file__).resolve().parent.parent / "contracts/release-signing-v1.json"
SIGNED = ("retok-macos-x64", "retok-macos-arm64", "retok-windows-x64.exe")
LINUX = ("retok-linux-x64", "retok-linux-aarch64")
COMMIT = r"(?:[0-9a-f]{40}|[0-9a-f]{64})"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def regular(path):
    require(stat.S_ISREG(path.lstat().st_mode), "expected a regular, non-symlink file")


def safe_path(path):
    """Reject symlinks even in parent components (including dangling links)."""
    path = Path(os.path.abspath(path))
    require(not any(p.is_symlink() for p in (path, *path.parents)), "symlink path rejected")
    return path


def sha256(path):
    regular(path)
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def read_json(path):
    regular(path)
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result
    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique)
    except (UnicodeError, json.JSONDecodeError):
        raise ValueError("invalid JSON document") from None


def contract():
    value = read_json(CONTRACT_PATH)
    require(value["schema_version"] == 1 and tuple(value["signed_artifacts"]) == SIGNED
            and tuple(value["unsigned_artifacts"]) == LINUX, "unsupported signing contract")
    return value


def policy_hash(policy):
    return hashlib.sha256(canonical(policy).encode()).hexdigest()


def identity(policy, name):
    if name.endswith(".exe"):
        fields = ("authority", "account", "certificate_profile", "code_signing_endpoint",
                  "expected_common_name", "expected_organization", "timestamp_url")
        result = {key: policy["windows"][key] for key in fields}
    else:
        result = {key: policy["apple"][key] for key in ("authority", "publisher", "team_id_sha256")}
    return {**result, "product": policy["product"]}


def signing_state(name):
    if name.endswith(".exe"):
        return {"signing_command": "succeeded", "pe_certificate_table": "present",
                "digest": "SHA-256", "timestamp_request": "RFC3161",
                "signing": True, "timestamp": True}
    return {"signing_command": "succeeded", "notarization": "Accepted",
            "bytes_unchanged_after_notarization": True, "for_notarization": True}


def native_state(name, digest, policy):
    common = {"schema_version": 1, "artifact": name, "artifact_sha256": digest,
              "platform": "windows" if name.endswith(".exe") else "macos", "status": "passed"}
    if name.endswith(".exe"):
        return {**common, "publisher_common_name": policy["windows"]["expected_common_name"],
                "publisher_organization": policy["windows"]["expected_organization"], "timestamped": True}
    return {**common, "identifier": policy["product"]["binary_identifier"],
            "team_id_sha256": policy["apple"]["team_id_sha256"],
            "hardened_runtime": True, "secure_timestamp": True}


def evidence_document(asset, commit, policy):
    return {"schema_version": 1, "artifact": asset.name, "sha256": sha256(asset),
            "source_commit": commit, "policy_sha256": policy_hash(policy),
            "identity": identity(policy, asset.name), "signing": signing_state(asset.name),
            "native_verification": {"status": "pending"}}


def check_evidence(asset, document, policy, *, native_required):
    require(isinstance(document, dict), "evidence must be an object")
    commit = document.get("source_commit")
    require(isinstance(commit, str) and re.fullmatch(COMMIT, commit), "invalid evidence source commit")
    expected = evidence_document(asset, commit, policy)
    state = document.get("native_verification")
    require(canonical(state) == canonical(native_state(asset.name, expected["sha256"], policy))
            or (not native_required and state == {"status": "pending"}),
            "native verification missing or unsuccessful")
    expected["native_verification"] = state
    require(canonical(document) == canonical(expected), "evidence hash, identity, or policy mismatch")


def verify_release_evidence(asset_dir: Path, evidence_dir: Path) -> None:
    """Require all three native validations; raise ValueError/OSError on failure."""
    asset_dir, evidence_dir = safe_path(asset_dir), safe_path(evidence_dir)
    require(not evidence_dir.is_relative_to(asset_dir)
            and not asset_dir.is_relative_to(evidence_dir), "evidence must be outside assets")
    require(asset_dir.is_dir() and evidence_dir.is_dir(), "missing asset/evidence directory")
    require({p.name for p in evidence_dir.iterdir()} == {n + ".json" for n in SIGNED},
            "evidence inventory mismatch")
    policy, commits = contract(), set()
    for name in SIGNED:
        document = read_json(evidence_dir / (name + ".json"))
        check_evidence(asset_dir / name, document, policy, native_required=True)
        commits.add(document["source_commit"])
    require(len(commits) == 1, "evidence source commits disagree")
    for name in (*SIGNED, *LINUX):
        sidecar = asset_dir / (name + ".cdx.json")
        if sidecar.exists() or sidecar.is_symlink():
            metadata = read_json(sidecar)
            try:
                properties = metadata["metadata"]["component"]["properties"]
                values = [p["value"] for p in properties if p["name"] == "retok:source-commit"]
            except (TypeError, KeyError):
                raise ValueError("invalid SBOM source commit") from None
            require(values == [next(iter(commits))], "SBOM and signing source commit disagree")


def private_write(path, data):
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as stream:
        stream.write(data)
    return path


def write_json(path, value):
    return private_write(path, (canonical(value) + "\n").encode())


def publish_directory(source, destination):
    """Linux atomic rename without replacing a concurrently created destination."""
    libc = ctypes.CDLL(None, use_errno=True)
    rename = libc.renameat2
    rename.argtypes = (ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint)
    rename.restype = ctypes.c_int
    if rename(-100, os.fsencode(source), -100, os.fsencode(destination), 1) != 0:
        raise OSError(ctypes.get_errno(), "cannot publish fresh signing directory")


def child_env(home):
    result = {"PATH": os.defpath, "HOME": str(home), "TMPDIR": str(home),
              "TMP": str(home), "TEMP": str(home), "LANG": "C", "LC_ALL": "C"}
    if os.name == "nt":
        for key in ("SystemRoot", "WINDIR", "SystemDrive"):
            if key in os.environ:
                result[key] = os.environ[key]
    return result


def run(argv, home, *, env=None, timeout=120):
    """No shell, inherited tracing/config variables, or raw child diagnostics."""
    try:
        result = subprocess.run([str(a) for a in argv], env=env or child_env(home),
                                cwd=home, stdin=subprocess.DEVNULL, capture_output=True,
                                timeout=timeout, check=False)
    except (OSError, subprocess.SubprocessError):
        raise ValueError("required signing/validation tool unavailable or timed out") from None
    require(result.returncode == 0, "signing/validation tool failed (diagnostics suppressed)")
    return result.stdout, result.stderr


def tool(name):
    value = shutil.which(name)
    require(value is not None, "missing required tool: " + name)
    return str(Path(value).absolute())


def prepare_tools(root, policy, rcodesign_path, jsign_path):
    require(platform.system() == "Linux" and platform.machine().lower() in ("x86_64", "amd64"),
            "official signing requires Linux x86_64")
    openssl, java = tool("openssl"), tool("java")
    paths = {}
    for key, supplied, pin in (("rcodesign_archive", rcodesign_path, policy["apple"]["rcodesign"]),
                                ("jsign_jar", jsign_path, policy["windows"]["jsign"])):
        source = safe_path(supplied)
        regular(source)
        target = root / key.lower()
        shutil.copyfile(source, target)
        target.chmod(0o600)
        require(sha256(target) == pin["sha256"], "signing tool checksum mismatch")
        paths[key] = target
    try:
        with tarfile.open(paths["rcodesign_archive"], "r:gz") as archive:
            members = [m for m in archive.getmembers() if Path(m.name).name == "rcodesign"]
            require(len(members) == 1 and members[0].isfile(), "invalid rcodesign archive")
            stream = archive.extractfile(members[0])
            require(stream is not None, "missing rcodesign archive member")
            with stream:
                rcodesign = private_write(root / "rcodesign", stream.read())
            rcodesign.chmod(0o700)
    except tarfile.TarError:
        raise ValueError("invalid rcodesign archive") from None
    jar = paths["jsign_jar"]
    require(run([java, "-jar", jar, "--version"], root)[0].strip().lower()
            == ("jsign " + policy["windows"]["jsign"]["version"]).encode(),
            "Jsign version mismatch")
    require(run([rcodesign, "--version"], root)[0].strip()
            in {(name + " " + policy["apple"]["rcodesign"]["version"]).encode()
                for name in ("apple-codesign", "rcodesign")},
            "rcodesign version mismatch")
    return openssl, java, rcodesign, jar


def credentials(root, policy, controls, source):
    require(source in ("infisical", "injected"), "invalid secret source")
    keys = policy["apple"]["credential_keys"] + policy["windows"]["credential_keys"]
    result = {}
    if source == "infisical":
        command, location = tool("infisical"), policy["credential"]
        env = child_env(root)
        # Infisical alone may use its authenticated owner profile or service token.
        env["HOME"] = str(Path.home())
        if controls.get("INFISICAL_TOKEN"):
            env["INFISICAL_TOKEN"] = controls["INFISICAL_TOKEN"]
    for key in keys:
        if source == "injected":
            if key in policy["apple"]["credential_keys"]:
                value = controls.get(key, "")
            else:
                variable = key.replace("AZURE_ARTIFACT_SIGNING_", "RETOK_WINDOWS_SIGNING_") + "_FILE"
                require(controls.get(variable), "missing credential file: " + variable)
                path = safe_path(controls[variable])
                regular(path)
                require(path.stat().st_mode & 0o077 == 0, "credential file must be private")
                value = path.read_text(encoding="utf-8").removesuffix("\n")
        else:
            raw, _ = run([command, "secrets", "get", key, "--plain", "--projectId",
                          location["project_id"], "--env", location["environment"],
                          "--path", location["path"], "--silent"], root, env=env)
            try:
                value = raw.removesuffix(b"\n").decode("utf-8")
            except UnicodeError:
                raise ValueError("malformed signing credential") from None
        require(isinstance(value, str) and value and not any(c in value for c in "\r\n\0"),
                "missing or malformed credential: " + key)
        result[key] = value
    return result


def decode_secret(value):
    try:
        return base64.b64decode(value, validate=True)
    except ValueError:
        raise ValueError("invalid base64 signing credential") from None


def dn_fields(text):
    result = {}
    for line in text.splitlines():
        match = re.fullmatch(r"\s*(CN|O|OU)\s*=\s*(.*?)\s*", line)
        if match:
            key, value = match.groups()
            require(key not in result, "ambiguous certificate subject")
            result[key] = value.removeprefix('"').removesuffix('"')
    return result


def apple_identity(subject, policy):
    fields = dn_fields(subject)
    team = fields.get("OU", "")
    require(re.fullmatch(r"[A-Z0-9]{10}", team) and
            hashlib.sha256(team.encode()).hexdigest() == policy["apple"]["team_id_sha256"],
            "Apple Team ID does not match publisher policy")
    require(fields.get("CN") == f"Developer ID Application: {policy['apple']['publisher']} ({team})"
            and fields.get("O") == policy["apple"]["publisher"],
            "unexpected Apple Developer ID publisher")
    return team


def prepare_apple(root, policy, secret, openssl):
    p12 = private_write(root / "signer.p12", decode_secret(secret["APPLE_CODESIGN_CERT_P12_B64"]))
    password = private_write(root / "password", secret["APPLE_CODESIGN_CERT_PASSWORD"].encode())
    outputs = []
    for name, flags in (("cert.pem", ["-clcerts", "-nokeys"]),
                        ("key.pem", ["-nocerts", "-nodes"])):
        command = [openssl, "pkcs12", "-in", p12, "-passin", "file:" + str(password), *flags]
        try:
            raw, _ = run(command, root)
        except ValueError:
            raw, _ = run(command + ["-legacy"], root)
        outputs.append(private_write(root / name, raw))
    cert, key = outputs
    require(cert.read_bytes().count(b"-----BEGIN CERTIFICATE-----") == 1,
            "P12 must contain one signing certificate")
    subject, _ = run([openssl, "x509", "-in", cert, "-noout", "-subject",
                      "-nameopt", "sep_multiline,sname,utf8"], root)
    team = apple_identity(subject.decode("utf-8"), policy)
    details = run([openssl, "x509", "-in", cert, "-noout", "-text"], root)[0].decode()
    require("Code Signing" in details and "X509v3 Key Usage: critical" in details
            and "Digital Signature" in details and "1.2.840.113635.100.6.1.13: critical" in details,
            "certificate lacks Developer ID code-signing extensions")
    ca = CONTRACT_PATH.parent / policy["apple"]["ca_file"]
    regular(ca)
    ca = private_write(root / "apple-ca.pem", ca.read_bytes())
    pem = ca.read_text(encoding="ascii")
    require(pem.count("-----BEGIN CERTIFICATE-----") == 1, "invalid Apple CA file")
    der = ssl.PEM_cert_to_DER_cert(pem[pem.index("-----BEGIN CERTIFICATE-----"):])
    require(hashlib.sha256(der).hexdigest() == policy["apple"]["ca_der_sha256"], "Apple CA pin mismatch")
    run([openssl, "verify", "-purpose", "any", "-partial_chain", "-no-CApath", "-no-CAstore",
         "-ignore_critical", "-CAfile", ca, cert], root)
    public_cert = run([openssl, "x509", "-in", cert, "-pubkey", "-noout"], root)[0]
    public_key = run([openssl, "pkey", "-in", key, "-pubout"], root)[0]
    require(public_cert.strip() == public_key.strip(), "P12 certificate and private key disagree")
    p8 = decode_secret(secret["NOTARY_KEY_P8_B64"])
    private = private_write(root / "notary.p8", p8)
    run([openssl, "pkey", "-in", private, "-noout"], root)
    require(re.fullmatch(r"[A-Za-z0-9]{10}", secret["NOTARY_KEY_ID"]), "invalid notary key ID")
    try:
        uuid.UUID(secret["NOTARY_ISSUER"])
    except ValueError:
        raise ValueError("invalid notary identity/key") from None
    return cert, key, (private, secret["NOTARY_ISSUER"], secret["NOTARY_KEY_ID"]), team


def binary_shape(path, *, signed=False):
    """Bounded structure checks only; these are not cryptographic verification."""
    regular(path)
    size = path.stat().st_size
    with path.open("rb") as stream:
        header = stream.read(64)
        require(len(header) == 64, "truncated binary")
        if path.name in LINUX:
            machine = 62 if path.name.endswith("x64") else 183
            require(header[:7] == b"\x7fELF\x02\x01\x01" and
                    struct.unpack_from("<H", header, 18)[0] == machine, "invalid Linux binary")
        elif path.name.endswith(".exe"):
            offset = struct.unpack_from("<I", header, 60)[0]
            require(header[:2] == b"MZ" and 64 <= offset <= size - 24, "invalid PE header")
            stream.seek(offset)
            pe = stream.read(24)
            machine, count = struct.unpack_from("<HH", pe, 4)
            optional_size, flags = struct.unpack_from("<HH", pe, 20)
            require(pe[:4] == b"PE\0\0" and machine == 0x8664 and count > 0
                    and flags & 2 and not flags & 0x2000 and optional_size >= 152
                    and offset + 24 + optional_size + count * 40 <= size, "invalid PE64 executable")
            optional = stream.read(optional_size)
            require(optional[:2] == b"\x0b\x02" and struct.unpack_from("<I", optional, 108)[0] >= 5,
                    "missing PE64 data directories")
            address, length = struct.unpack_from("<II", optional, 144)
            if not signed:
                require((address, length) == (0, 0), "input PE is already signed")
            else:
                require(address % 8 == 0 and address >= offset + 24 + optional_size + count * 40
                        and length >= 8 and address + length == size, "missing/invalid PE certificate table")
                stream.seek(address)
                cert_size, revision, kind = struct.unpack("<IHH", stream.read(8))
                require(8 < cert_size <= length and (cert_size + 7) // 8 * 8 == length
                        and revision == 0x200 and kind == 2, "invalid PE WIN_CERTIFICATE")
        else:
            magic, cpu, _, kind, count, length = struct.unpack_from("<6I", header)
            require(magic == 0xFEEDFACF and kind == 2 and count > 0 and
                    cpu == (0x01000007 if path.name.endswith("x64") else 0x0100000C)
                    and count * 8 <= length and 32 + length <= size, "invalid Mach-O executable")
            stream.seek(32)
            end = 32 + length
            for _ in range(count):
                require(stream.tell() + 8 <= end, "truncated Mach-O load commands")
                command, length = struct.unpack("<II", stream.read(8))
                require(length >= 8 and length % 8 == 0 and stream.tell() + length - 8 <= end,
                        "invalid Mach-O load command")
                # Rust arm64 binaries may carry an ad-hoc signature, replaced by rcodesign.
                stream.seek(length - 8, 1)
            require(stream.tell() == end, "invalid Mach-O command table size")


def sign_apple(asset, root, rcodesign, prepared):
    cert, key, notary, team = prepared
    private, issuer, key_id = notary
    api = root / "notary-api.json"
    run([rcodesign, "encode-app-store-connect-api-key", "--output-path", api,
         issuer, key_id, private], root)
    regular(api)
    api.chmod(0o600)
    run([rcodesign, "sign", "--for-notarization", "--binary-identifier", "retok",
         "--pem-file", cert, "--pem-file", key, asset], root, timeout=1200)
    details = b"\n".join(run([rcodesign, "print-signature-info", asset], root)).decode("utf-8")
    require(team in details and "retok" in details, "rcodesign signature identity missing")
    digest = sha256(asset)
    archive = root / (asset.name + ".zip")
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zipped:
        zipped.write(asset, asset.name)
    archive.chmod(0o600)
    stdout, stderr = run([rcodesign, "notary-submit", "--wait", "--max-wait-seconds", "1800",
                          "--api-key-file", api, archive], root, timeout=1860)
    matches = re.findall(rb"created submission ID: ([0-9a-fA-F-]{36})", stdout + b"\n" + stderr)
    require(len(matches) == 1, "notary submission ID missing")
    submission = str(uuid.UUID(matches[0].decode()))
    raw, _ = run([rcodesign, "notary-log", "--api-key-file", api, submission], root)
    try:
        log = json.loads(raw)
        accepted = log["jobId"].lower() == submission and log["status"] == "Accepted"
    except (ValueError, KeyError, TypeError, AttributeError):
        accepted = False
    require(accepted, "notarization not Accepted for this submission/archive")
    require(sha256(asset) == digest, "artifact changed during notarization")


def azure_token(secret):
    try:
        tenant = str(uuid.UUID(secret["AZURE_ARTIFACT_SIGNING_TENANT_ID"]))
        uuid.UUID(secret["AZURE_ARTIFACT_SIGNING_CLIENT_ID"])
    except ValueError:
        raise ValueError("invalid Azure tenant/client ID") from None
    body = urllib.parse.urlencode({"grant_type": "client_credentials",
            "client_id": secret["AZURE_ARTIFACT_SIGNING_CLIENT_ID"],
            "client_secret": secret["AZURE_ARTIFACT_SIGNING_CLIENT_SECRET"],
            "scope": "https://codesigning.azure.net/.default"}).encode()
    request = urllib.request.Request(f"https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token",
                                     data=body, headers={"Content-Type": "application/x-www-form-urlencoded"})
    # No proxy inheritance or redirects carrying the client credential body.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with opener.open(request, timeout=60) as response:
            data = json.load(response)
        token = data["access_token"]
        require(data["token_type"].lower() == "bearer" and 0 < int(data["expires_in"]) <= 7200
                and isinstance(token, str) and token and not any(c.isspace() for c in token),
                "invalid Azure token")
        return token
    except Exception:
        raise ValueError("Azure token request failed (diagnostics suppressed)") from None


def sign_windows(asset, root, java, jar, policy, secret):
    token = private_write(root / "azure-token", azure_token(secret).encode())
    windows = policy["windows"]
    try:
        run([java, "-jar", jar, "sign", "--storetype", "TRUSTEDSIGNING", "--keystore",
             urllib.parse.urlsplit(windows["code_signing_endpoint"]).netloc,
             "--storepass", "file:" + str(token), "--alias",
             windows["account"] + "/" + windows["certificate_profile"], "--alg", "SHA-256",
             "--name", policy["product"]["name"], "--url", policy["product"]["url"],
             "--tsaurl", windows["timestamp_url"], "--tsmode", "RFC3161",
             "--tsretries", "3", "--tsretrywait", "5", asset], root, timeout=1200)
    finally:
        token.unlink()
    binary_shape(asset, signed=True)


def sign_directory(source: Path, destination: Path, evidence_dir: Path, source_commit: str,
                   rcodesign: Path, jsign_jar: Path, secret_source: str = "infisical") -> None:
    """Publish final bytes plus pending evidence. Inputs must be quiescent and trusted."""
    require(isinstance(source_commit, str) and re.fullmatch(COMMIT, source_commit), "invalid source commit")
    source, destination, evidence_dir = map(safe_path, (source, destination, evidence_dir))
    paths = (source, destination, evidence_dir)
    require(all(not a.is_relative_to(b) for a in paths for b in paths if a != b)
            and len(set(paths)) == 3, "input, output, and evidence directories must be disjoint")
    require(source.is_dir() and {p.name for p in source.iterdir()} == set(SIGNED + LINUX),
            "sign inputs must be exactly the five binaries; generate metadata after signing")
    require(not destination.exists() and not evidence_dir.exists(), "output/evidence must be fresh")
    for name in SIGNED + LINUX:
        binary_shape(source / name)
    policy = contract()
    old_umask = os.umask(0o077)
    try:
        with tempfile.TemporaryDirectory(prefix=".retok-sign-", dir=destination.parent) as temp, \
                tempfile.TemporaryDirectory(prefix=".retok-evidence-", dir=evidence_dir.parent) as evtemp:
            root = Path(temp)
            assets, evidence = root / "assets", Path(evtemp) / "evidence"
            assets.mkdir(mode=0o700)
            evidence.mkdir(mode=0o700)
            for name in SIGNED + LINUX:
                shutil.copyfile(source / name, assets / name)
                (assets / name).chmod(0o600)
                binary_shape(assets / name)
            openssl, java, signer, jar = prepare_tools(root, policy, rcodesign, jsign_jar)
            secret = credentials(root, policy, os.environ, secret_source)
            try:
                prepared = prepare_apple(root, policy, secret, openssl)
                for name in SIGNED:
                    asset = assets / name
                    if name.endswith(".exe"):
                        sign_windows(asset, root, java, jar, policy, secret)
                    else:
                        sign_apple(asset, root, signer, prepared)
                    write_json(evidence / (name + ".json"), evidence_document(asset, source_commit, policy))
            finally:
                secret.clear()
            for name in SIGNED + LINUX:
                (assets / name).chmod(0o755)
            for name in LINUX:
                require(sha256(assets / name) == sha256(source / name), "Linux input changed")
            # Separate directories cannot share one rename. Publish evidence first;
            # the asset rename is the commit point, with rollback on rename failure.
            require(not destination.exists() and not evidence_dir.exists(), "output appeared during signing")
            publish_directory(evidence, evidence_dir)
            try:
                publish_directory(assets, destination)
            except BaseException:
                shutil.rmtree(evidence_dir)
                raise
    finally:
        os.umask(old_umask)


def record_native(evidence_dir: Path, artifact: Path, result_json: Path) -> None:
    """Import a trusted native checker result; this does not run native tools."""
    artifact, evidence_dir, result_json = map(safe_path, (artifact, evidence_dir, result_json))
    require(artifact.name in SIGNED, "unknown signed artifact")
    require(not evidence_dir.is_relative_to(artifact.parent)
            and not artifact.parent.is_relative_to(evidence_dir), "evidence must be outside assets")
    policy = contract()
    evidence_path = evidence_dir / (artifact.name + ".json")
    document = read_json(evidence_path)
    check_evidence(artifact, document, policy, native_required=False)
    result = read_json(result_json)
    expected = native_state(artifact.name, document["sha256"], policy)
    require(canonical(result) == canonical(expected), "native result schema, hash, or policy mismatch")
    require(sha256(artifact) == document["sha256"], "artifact changed during native import")
    require(canonical(read_json(evidence_path)) == canonical(document), "evidence changed during native import")
    document["native_verification"] = result
    with tempfile.TemporaryDirectory(prefix=".retok-native-", dir=evidence_dir.parent) as update:
        write_json(Path(update) / "evidence.json", document).replace(evidence_path)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    sign = commands.add_parser("sign")
    for name in ("source", "destination", "evidence"):
        sign.add_argument(name, type=Path)
    sign.add_argument("--source-commit", required=True)
    sign.add_argument("--rcodesign", type=Path, required=True, help="checksum-pinned distribution tar.gz")
    sign.add_argument("--jsign-jar", type=Path, required=True)
    sign.add_argument("--secret-source", choices=("infisical", "injected"), default="infisical")
    native = commands.add_parser("record-native")
    native.add_argument("evidence", type=Path)
    native.add_argument("artifact", type=Path)
    native.add_argument("result_json", type=Path)
    verify = commands.add_parser("verify")
    verify.add_argument("assets", type=Path)
    verify.add_argument("evidence", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "sign":
            sign_directory(args.source, args.destination, args.evidence, args.source_commit,
                           args.rcodesign, args.jsign_jar, args.secret_source)
        elif args.command == "record-native":
            record_native(args.evidence, args.artifact, args.result_json)
        else:
            verify_release_evidence(args.assets, args.evidence)
    except (ValueError, OSError, KeyError, TypeError, AttributeError, struct.error) as error:
        parser.exit(1, f"release signing: {error}\n")


if __name__ == "__main__":
    main()
