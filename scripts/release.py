#!/usr/bin/env python3
"""Stage and verify already-built Retok 0.1.0 release assets (Python 3.11+ stdlib).

Usage:
  python3 scripts/release.py stage INPUT_DIR NEW_OUTPUT_DIR
  python3 scripts/release.py verify OUTPUT_DIR
  python3 scripts/release.py metadata BINARY --project SOURCE_DIR --target TRIPLE \
      --source-commit FULL_HASH --tiktoken-license LOCAL_LICENSE --openai-license LOCAL_LICENSE \
      --runtime-manifest RUNTIMES.json

INPUT_DIR must contain exactly these binaries:
  retok-linux-x64, retok-linux-aarch64, retok-macos-x64,
  retok-macos-arm64, retok-windows-x64.exe
Each binary requires an
appended .cdx.json and .third-party-notices.txt sidecar (including after .exe).
Generate the sidecars for each final binary using the metadata command. It uses
offline locked Cargo metadata and authenticated local .crate archives, including
complete licenses, Unicode data notices, and the embedded o200k vocabulary.
Supply the actual build target and source commit; default Cargo features are
assumed. Missing tiktoken-rs/OpenAI licenses require reviewed local supplements.
Use verify --project SOURCE_DIR to also compare the inventory and dependency
relationships with the local Cargo graph/lockfile. Plain verify checks the
sidecars' internal inventory, notice bindings, provenance, and binary hashes.

Runtime manifest format (review all versions, sources, hashes, and full texts):
  {"target":"x86_64-apple-darwin","components":[
    {"name":"rust-std","version":"EXACT_VERSION","source":"https://SOURCE",
     "license":"MIT OR Apache-2.0","sha256":"SOURCE_OR_INPUT_SHA256",
     "notices":[{"label":"Rust licenses","path":"rust-LICENSE.txt"}]}]}
Every target requires rust-std, covering its standard library and bundled Rust
runtime support. linux-musl also requires musl; windows-gnu requires mingw-w64
and permits gcc-runtime. macOS requires rust-std only; OS dynamic libraries are
not bundled. Each component uses the same fields and may list multiple notices.
Notice paths may be absolute or relative to the manifest. Complete UTF-8 file
contents are copied; local paths are replaced by public labels and file hashes
in the canonical manifest embedded in the SBOM. Its SHA256 is bound to the root.

Staging preserves asset bytes, normalizes modes to 755/644, and adds sorted,
LF-terminated SHA256SUMS. Identical inputs produce identical asset/checksum
bytes; this does not make the upstream binary builds reproducible.
Only binaries matching the host OS and architecture are executed (--version).
Foreign binaries receive header/architecture checks, not a version attestation.
No source builds, network access, or uploads are performed. Use trusted inputs.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tempfile

import release_metadata


VERSION = "0.1.0"
TARGETS = {
    "retok-linux-x64": ("Linux", "x64"),
    "retok-linux-aarch64": ("Linux", "arm64"),
    "retok-macos-x64": ("Darwin", "x64"),
    "retok-macos-arm64": ("Darwin", "arm64"),
    "retok-windows-x64.exe": ("Windows", "x64"),
}
ASSETS = sorted(name + suffix for name in TARGETS for suffix in (
    "", ".cdx.json", ".third-party-notices.txt"
))


def require(condition, message):
    if not condition:
        raise ValueError(message)


def inventory(directory, checksums):
    expected = set(ASSETS) | ({"SHA256SUMS"} if checksums else set())
    actual = {path.name for path in directory.iterdir()}
    require(actual == expected,
            f"{directory}: inventory mismatch; missing={sorted(expected - actual)}, "
            f"unexpected={sorted(actual - expected)}")
    for name in expected:
        path = directory / name
        require(path.is_file() and not path.is_symlink(),
                f"{path}: expected a regular file, not a symlink")


def binary_header(path, system, arch):
    """Check 64-bit executable headers and header-table bounds, not loadability."""
    size = path.stat().st_size
    with path.open("rb") as stream:
        header = stream.read(64)
        require(len(header) == 64, f"{path.name}: truncated binary header")
        if system == "Linux":
            require(header[:7] == b"\x7fELF\x02\x01\x01",
                    f"{path.name}: expected little-endian ELF64")
            kind, machine, version = struct.unpack_from("<HHI", header, 16)
            offset = struct.unpack_from("<Q", header, 32)[0]
            header_size, entry_size, count = struct.unpack_from("<HHH", header, 52)
            require(kind in (2, 3) and version == 1
                    and machine == {"x64": 62, "arm64": 183}[arch]
                    and header_size == 64 and entry_size == 56 and count > 0
                    and offset >= 64 and offset + count * entry_size <= size,
                    f"{path.name}: invalid ELF executable/architecture/program headers")
        elif system == "Darwin":
            magic, cpu, _, kind, count, commands_size = struct.unpack_from("<6I", header)
            require(magic == 0xFEEDFACF
                    and cpu == {"x64": 0x01000007, "arm64": 0x0100000C}[arch]
                    and kind == 2 and count > 0 and commands_size >= count * 8
                    and 32 + commands_size <= size,
                    f"{path.name}: invalid Mach-O executable/architecture/load commands")
            stream.seek(32)
            end = 32 + commands_size
            for _ in range(count):
                command = stream.read(8)
                require(len(command) == 8, f"{path.name}: truncated Mach-O command")
                length = struct.unpack_from("<I", command, 4)[0]
                require(length >= 8 and length % 8 == 0
                        and stream.tell() - 8 + length <= end,
                        f"{path.name}: invalid Mach-O command size")
                stream.seek(length - 8, 1)
            require(stream.tell() == end, f"{path.name}: invalid Mach-O command table")
        else:
            require(header[:2] == b"MZ", f"{path.name}: expected PE executable")
            offset = struct.unpack_from("<I", header, 60)[0]
            require(64 <= offset <= size - 24, f"{path.name}: invalid PE offset")
            stream.seek(offset)
            pe = stream.read(24)
            machine, count = struct.unpack_from("<HH", pe, 4)
            optional_size, flags = struct.unpack_from("<HH", pe, 20)
            require(pe[:4] == b"PE\0\0" and machine == 0x8664 and count > 0
                    and flags & 2 and not flags & 0x2000 and optional_size >= 112
                    and offset + 24 + optional_size + 40 * count <= size
                    and stream.read(2) == b"\x0b\x02",
                    f"{path.name}: invalid PE64 executable/architecture/section headers")


def metadata(directory, name):
    path = directory / (name + ".cdx.json")
    data = json.loads(path.read_text(encoding="utf-8"))
    require(isinstance(data, dict) and data.get("bomFormat") == "CycloneDX"
            and isinstance(data.get("specVersion"), str) and data["specVersion"]
            and type(data.get("version")) is int and data["version"] > 0,
            f"{path.name}: expected a versioned CycloneDX document")
    info = data.get("metadata")
    component = info.get("component") if isinstance(info, dict) else None
    require(isinstance(component, dict)
            and isinstance(component.get("name"), str)
            and component.get("name", "").lower() == "retok"
            and component.get("version") == VERSION,
            f"{path.name}: metadata.component must identify Retok {VERSION}")
    components = data.get("components")
    require(isinstance(components, list) and components and all(
        isinstance(item, dict) and all(
            isinstance(item.get(key), str) and item[key].strip()
            for key in ("name", "version")
        ) for item in components
    ), f"{path.name}: expected dependency components with names and versions")
    notices = directory / (name + ".third-party-notices.txt")
    notice_text = notices.read_bytes().decode("utf-8")
    require(notice_text.strip(),
            f"{notices.name}: empty notices")
    release_metadata.validate(data, notice_text, directory / name)
    return data


def checksums(directory):
    lines = []
    for name in ASSETS:
        digest = hashlib.sha256()
        with (directory / name).open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        lines.append(f"{digest.hexdigest()}  {name}\n")
    return "".join(lines).encode("ascii")


def validate(directory, with_checksums, project=None):
    inventory(directory, with_checksums)
    if with_checksums:
        require((directory / "SHA256SUMS").read_bytes() == checksums(directory),
                "SHA256SUMS: hashes, inventory, order, or formatting do not match")
    machine = platform.machine().lower()
    host = (platform.system(), {"x86_64": "x64", "amd64": "x64",
                               "aarch64": "arm64", "arm64": "arm64"}.get(machine))
    # Validate every structure before executing any supplied binary.
    for name, (system, arch) in TARGETS.items():
        binary_header(directory / name, system, arch)
        document = metadata(directory, name)
        if project is not None:
            release_metadata.validate_project(document, project)
    for name, target in TARGETS.items():
        path = directory / name
        if target == host:
            require(os.access(path, os.X_OK), f"{name}: native binary is not executable")
            result = subprocess.run([str(path.resolve()), "--version"],
                                    stdin=subprocess.DEVNULL, capture_output=True,
                                    timeout=10, check=False)
            require(result.returncode == 0
                    and result.stdout in (b"Retok 0.1.0\n", b"Retok 0.1.0\r\n")
                    and not result.stderr,
                    f"{name}: --version must succeed and report only Retok {VERSION}")
            print(f"{name}: version verified ({VERSION})")
        else:
            print(f"{name}: structure verified; version not run (foreign target)")


def stage(source, destination):
    inventory(source, checksums=False)
    require(not destination.exists() and not destination.is_symlink(),
            f"{destination}: output must not already exist")
    # Publish only a complete validated directory; leave failed staging private.
    with tempfile.TemporaryDirectory(prefix=".retok-stage-", dir=destination.parent) as temp:
        staged = Path(temp) / "assets"
        staged.mkdir()
        for name in ASSETS:
            shutil.copyfile(source / name, staged / name)
            (staged / name).chmod(0o755 if name in TARGETS else 0o644)
        validate(staged, with_checksums=False)
        (staged / "SHA256SUMS").write_bytes(checksums(staged))
        (staged / "SHA256SUMS").chmod(0o644)
        staged.rename(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    staging = commands.add_parser("stage", help="stage 15 inputs into a new directory")
    staging.add_argument("source", type=Path)
    staging.add_argument("destination", type=Path)
    verification = commands.add_parser("verify", help="verify 15 assets and SHA256SUMS")
    verification.add_argument("directory", type=Path)
    verification.add_argument("--project", type=Path,
                              help="also check the offline Cargo dependency graph and lockfile")
    generation = commands.add_parser("metadata", help="generate offline sidecars for one final binary")
    generation.add_argument("binary", type=Path)
    generation.add_argument("--project", type=Path, required=True)
    generation.add_argument("--target", required=True)
    generation.add_argument("--source-commit", required=True)
    generation.add_argument("--tiktoken-license", type=Path)
    generation.add_argument("--openai-license", type=Path, required=True)
    generation.add_argument("--runtime-manifest", type=Path, required=True,
                            help="reviewed runtime components and local notice files (see top-level --help)")
    args = parser.parse_args()
    try:
        if args.command == "stage":
            stage(args.source, args.destination)
        elif args.command == "metadata":
            require(args.binary.name in TARGETS, "unknown binary asset name")
            binary_header(args.binary, *TARGETS[args.binary.name])
            document, notices = release_metadata.generate(
                args.project, args.binary, args.target, args.source_commit,
                args.tiktoken_license, args.openai_license, args.runtime_manifest,
            )
            outputs = [args.binary.with_name(args.binary.name + suffix)
                       for suffix in (".cdx.json", ".third-party-notices.txt")]
            require(not any(p.exists() or p.is_symlink() for p in outputs),
                    "metadata sidecars already exist; use a fresh input directory")
            outputs[0].write_bytes((json.dumps(document, indent=2, sort_keys=True) + "\n").encode("utf-8"))
            outputs[1].write_bytes(notices.encode("utf-8"))
        else:
            validate(args.directory, with_checksums=True, project=args.project)
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"release: {error}\n")
    print("Metadata generated." if args.command == "metadata" else "Release assets verified.")


if __name__ == "__main__":
    main()
