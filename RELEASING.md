# Releasing Sift

Sift releases contain five standalone binaries. The release path signs and
notarizes the macOS binaries, Authenticode-signs the Windows binary, verifies
those exact bytes on their native operating systems, then generates SBOMs and
checksums. Signing credentials are never available to Cargo or dependency
tools.

The signing policy and tool pins are in
`contracts/release-signing-v1.json`. The signer accepts an authenticated
Infisical session by default. For isolated owner-operated runs it also accepts
injected Apple values and private Azure credential files; see
`python3 scripts/release_signing.py --help`. Never put credentials in command
arguments, source, logs, or release assets.

## 1. Build unsigned binaries

Build a clean, reviewed commit with `Cargo.lock` locked. Remap absolute source
and dependency-cache paths with Rust's `--remap-path-prefix` for every target;
stripping symbols alone does not remove paths embedded in panic messages. Check
the final artifacts for build-machine paths before publication. The two macOS links
must reserve room for the Developer ID load command:

```sh
RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x1000' \
  cargo zigbuild --release --locked --target x86_64-apple-darwin
RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x1000' \
  cargo zigbuild --release --locked --target aarch64-apple-darwin
```

Build Linux x64 and arm64 as static musl executables with `cargo zigbuild`;
the HTTPS client's C dependency requires Zig for both musl targets. Build
Windows x64 as a GNU PE executable. Place only these files in a fresh input
directory:

```text
sift-linux-x64
sift-linux-aarch64
sift-macos-x64
sift-macos-arm64
sift-windows-x64.exe
```

## 2. Sign

Supply the checksum-pinned rcodesign distribution archive and Jsign jar named
in the policy. The signer rechecks both hashes and versions.

```sh
python3 scripts/release_signing.py sign unsigned signed signing-evidence \
  --source-commit "$(git rev-parse HEAD)" \
  --rcodesign /trusted/apple-codesign-0.29.0-x86_64-unknown-linux-musl.tar.gz \
  --jsign-jar /trusted/jsign-7.5.jar
```

The input remains unchanged. `signed` contains all five final binaries; Linux
bytes are copied exactly. `signing-evidence` contains sanitized pending records
for macOS and Windows. Apple notarization must already be accepted before the
command succeeds.

## 3. Verify exact bytes natively

On macOS, run the checker for both artifacts. Execute `--version` for every
architecture the host can run:

```sh
scripts/verify_macos_release.sh signed/sift-macos-x64 0.4.0 > macos-x64.json
scripts/verify_macos_release.sh signed/sift-macos-arm64 > macos-arm64.json
```

On Windows x64, use Windows PowerShell or PowerShell 7:

```powershell
scripts/verify_windows_release.ps1 `
  -Artifact signed/sift-windows-x64.exe `
  -ExpectedVersion 0.4.0 | Set-Content -NoNewline windows-x64.json
```

Import each result on the release host. Import refuses a different artifact
hash, identity, policy, or result shape.

```sh
python3 scripts/release_signing.py record-native signing-evidence \
  signed/sift-macos-x64 macos-x64.json
python3 scripts/release_signing.py record-native signing-evidence \
  signed/sift-macos-arm64 macos-arm64.json
python3 scripts/release_signing.py record-native signing-evidence \
  signed/sift-windows-x64.exe windows-x64.json
python3 scripts/release_signing.py verify signed signing-evidence
```

## 4. Generate metadata and stage assets

Generate each `.cdx.json` and `.third-party-notices.txt` beside its final signed
binary with `scripts/release.py metadata`. Use reviewed runtime manifests and
license inputs for the exact build target. Metadata binds the final binary hash
and source commit.

Stage only after all three native results have been imported:

```sh
python3 scripts/release.py stage signed release-assets \
  --signing-evidence signing-evidence
python3 scripts/release.py verify release-assets --project .
```

`release-assets` contains the five binaries, ten sidecars, and
`SHA256SUMS`. Signing evidence remains release-control evidence rather than a
public asset. Any binary change after signing invalidates the evidence, SBOM,
and checksums and requires a new release candidate.

## 5. Tag, publish, and update Homebrew

Create the immutable version tag from the reviewed source commit, then publish
exactly the 16 files in `release-assets`. Download every published asset and
compare it byte-for-byte with the staged file before announcing the release.

Only after the tag exists, hash that tag's GitHub source archive and update
`Formula/sift.rb` to the new tag and checksum in a reviewed follow-up change.
The formula must never point at a version whose immutable archive has not yet
been verified. Test `brew install --build-from-source` from the updated formula
before merging it.
