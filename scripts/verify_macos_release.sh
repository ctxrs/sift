#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'usage: %s ARTIFACT [EXPECTED_VERSION]\n' "$0" >&2
  exit 2
}

die() {
  printf 'macOS signing verification: %s\n' "$*" >&2
  exit 1
}

[[ $# -ge 1 && $# -le 2 ]] || usage
artifact="$1"
expected_version="${2:-}"
[[ -f "${artifact}" && ! -L "${artifact}" ]] || die "artifact must be a regular non-symlink file"
artifact="$(cd "$(dirname "${artifact}")" && pwd)/$(basename "${artifact}")"

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract="${root_dir}/contracts/release-signing-v1.json"
for tool in codesign python3 shasum; do
  command -v "${tool}" >/dev/null 2>&1 || die "missing required tool: ${tool}"
done

policy="$(python3 -I - "${contract}" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
print(value["product"]["binary_identifier"] + "\t" + value["apple"]["team_id_sha256"])
PY
)"
IFS=$'\t' read -r expected_identifier expected_team_hash <<<"${policy}"
[[ -n "${expected_identifier}" && "${expected_team_hash}" =~ ^[0-9a-f]{64}$ ]] || \
  die "invalid signing contract"

before="$(shasum -a 256 "${artifact}" | awk '{print $1}')"
codesign --verify --strict --verbose=4 "${artifact}" >/dev/null 2>&1 || \
  die "strict codesign verification failed"
details="$(codesign -d --verbose=4 "${artifact}" 2>&1)" || die "could not inspect signature"
identifier="$(sed -n 's/^Identifier=//p' <<<"${details}" | head -n 1)"
team_id="$(sed -n 's/^TeamIdentifier=//p' <<<"${details}" | head -n 1)"
[[ "${identifier}" == "${expected_identifier}" ]] || die "unexpected binary identifier"
[[ "${team_id}" =~ ^[A-Z0-9]{10}$ ]] || die "missing or invalid Apple Team ID"
team_hash="$(printf '%s' "${team_id}" | shasum -a 256 | awk '{print $1}')"
[[ "${team_hash}" == "${expected_team_hash}" ]] || die "unexpected Apple publisher"
grep -Eq '^Authority=Developer ID Application: .+ \([A-Z0-9]{10}\)$' <<<"${details}" || \
  die "signature is not a Developer ID Application signature"
grep -Eq '^CodeDirectory .*flags=[^[:space:]]*\([^)]*runtime[^)]*\)' <<<"${details}" || \
  die "signature lacks hardened runtime"
grep -Eq '^Timestamp=.+$' <<<"${details}" || die "signature lacks a secure timestamp"

if [[ -n "${expected_version}" ]]; then
  version_output="$("${artifact}" --version)" || die "signed executable failed --version"
  [[ "${version_output}" == "Retok ${expected_version}" ]] || die "unexpected version output"
fi
after="$(shasum -a 256 "${artifact}" | awk '{print $1}')"
[[ "${after}" == "${before}" ]] || die "artifact changed during verification"

python3 -I - "$(basename "${artifact}")" "${after}" "${team_hash}" <<'PY'
import json, sys
print(json.dumps({
    "schema_version": 1,
    "artifact": sys.argv[1],
    "artifact_sha256": sys.argv[2],
    "platform": "macos",
    "status": "passed",
    "identifier": "retok",
    "team_id_sha256": sys.argv[3],
    "hardened_runtime": True,
    "secure_timestamp": True,
}, sort_keys=True, separators=(",", ":")))
PY
