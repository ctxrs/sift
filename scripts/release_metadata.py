"""Offline metadata for default-feature Retok builds; requires Python 3.11+.

Cargo's target-filtered normal-edge closure includes proc-macro support crates.
Dev/build-only edges are excluded. Package hashes identify cached .crate archives,
not compiled object code. The caller supplies the build's source commit and
target; those assertions cannot be recovered from an arbitrary executable.

All license/notice files from each authenticated crate archive are copied in
full. The published tiktoken-rs crate lacks LICENSE, so a reviewed local copy
is required. A reviewed OpenAI/tiktoken MIT license is also required. Never
substitute a license identifier or a generated copyright for either document.
The known winapi-x86_64-pc-windows-gnu 0.4.0 archive omits its licenses;
its supplement comes only from authenticated normal-parent winapi 0.3.9.

The reviewed runtime manifest is UTF-8 JSON, for example (replace placeholders):
{"target":"x86_64-unknown-linux-musl","components":[
  {"name":"rust-std","version":"EXACT_VERSION","source":"https://SOURCE",
   "license":"MIT OR Apache-2.0","sha256":"SOURCE_OR_INPUT_SHA256",
   "notices":[{"label":"Rust licenses","path":"rust-LICENSE.txt"}]},
  {"name":"musl","version":"EXACT_VERSION","source":"https://SOURCE",
   "license":"MIT","sha256":"SOURCE_OR_INPUT_SHA256",
   "notices":[{"label":"musl copyright","path":"musl-COPYRIGHT.txt"}]}
]}
Every target requires rust-std (including bundled Rust runtime support); musl
targets additionally require musl, and windows-gnu requires mingw-w64 and allows
an additional gcc-runtime component. No OS dynamic libraries are included.
Supply exact reviewed source/input hashes, not hashes of unrelated binaries.
Notice paths are absolute or relative to the manifest; labels are public.
Files must be complete reviewed UTF-8 texts, including all attributions.
Canonical manifest content is embedded in the SBOM with paths replaced by file
SHA256 hashes, components sorted by name, and notices sorted by label.
Review establishes license completeness and build provenance;
the tool verifies inventory, hashes, and copying without fetching anything.
"""

import hashlib
import json
import mmap
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib


VERSION = tomllib.loads(
    (Path(__file__).resolve().parent.parent / "Cargo.toml").read_text(encoding="utf-8")
)["package"]["version"]
TRIPLES = {
    "retok-linux-x64": ("x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"),
    "retok-linux-aarch64": ("aarch64-unknown-linux-gnu", "aarch64-unknown-linux-musl"),
    "retok-macos-x64": ("x86_64-apple-darwin",),
    "retok-macos-arm64": ("aarch64-apple-darwin",),
    "retok-windows-x64.exe": ("x86_64-pc-windows-msvc", "x86_64-pc-windows-gnu"),
}
VOCAB_PATH = "assets/o200k_base.tiktoken"
VOCAB_URL = "https://openaipublic.blob.core.windows.net/encodings/o200k_base.tiktoken"
OPENAI_URL = "https://github.com/openai/tiktoken"
SPECIAL_MARKERS = (
    "===== Retok LICENSE =====", "===== OpenAI/tiktoken LICENSE =====",
    "===== o200k_base vocabulary provenance =====",
    "===== regex-syntax Unicode data =====", "===== unicode-ident Unicode data =====",
)
REGEX_LICENSE = "(MIT OR Apache-2.0) AND Unicode-DFS-2016"
WINAPI_REVISION = "796a8e6c2971dc2ff1bcff166e6671284f9b5b6b"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def hashes(value):
    return [{"alg": "SHA-256", "content": value}]


def properties(values):
    return [{"name": "retok:" + key, "value": value}
            for key, value in sorted(values.items())]


def property_map(component):
    entries = component.get("properties", [])
    require(isinstance(entries, list) and all(
        isinstance(p, dict) and isinstance(p.get("name"), str)
        and isinstance(p.get("value"), str) for p in entries), "invalid component properties")
    result = {p["name"]: p["value"] for p in entries}
    require(len(result) == len(entries), "duplicate component properties")
    return result


def cargo_graph(project, target):
    result = subprocess.run(
        ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
         "--filter-platform", target, "--manifest-path", str(project / "Cargo.toml")],
        check=True, capture_output=True, text=True, timeout=60,
    )
    return json.loads(result.stdout)


def normal_graph(metadata):
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    require(root in packages and packages[root]["name"] == "retok"
            and packages[root]["version"] == VERSION,
            f"expected Retok {VERSION} Cargo root")
    graph = {}
    pending = [root]
    while pending:
        key = pending.pop()
        if key in graph:
            continue
        graph[key] = sorted({d["pkg"] for d in nodes[key]["deps"]
                             if any(k["kind"] is None for k in d["dep_kinds"])})
        pending.extend(graph[key])
    return root, {key: packages[key] for key in graph}, graph


def checked_archive(package, lock):
    """Use Cargo's local registry archive, authenticated against Cargo.lock."""
    key = (package["name"], package["version"], package["source"])
    require(key in lock and (package["source"] or "").startswith("registry+"),
            f"{key[:2]}: expected a locked registry dependency")
    digest = lock[key].get("checksum", "")
    directory = Path(package["manifest_path"]).parent
    # Cargo's registry layout: registry/src/INDEX/NAME-VERSION and cache/INDEX/*.crate.
    archive = directory.parents[1].parent / "cache" / directory.parent.name / (directory.name + ".crate")
    require(re.fullmatch(r"[0-9a-f]{64}", digest) and sha256(archive) == digest,
            f"{package['name']}: cached crate checksum differs from Cargo.lock")
    files = {}
    with tarfile.open(archive, "r:gz") as crate:
        for member in crate.getmembers():
            if not member.isfile():
                continue
            prefix = package["name"] + "-" + package["version"] + "/"
            require(member.name.startswith(prefix), "unexpected crate archive prefix")
            relative = member.name[len(prefix):]
            basename = Path(relative).name.lower()
            if (basename.startswith(("license", "licence", "copying", "notice", "unlicense"))
                    or relative in ("Cargo.toml", ".cargo_vcs_info.json", VOCAB_PATH)
                    or relative == package.get("license_file")):
                files[relative] = crate.extractfile(member).read()
    manifest = tomllib.loads(files["Cargo.toml"].decode("utf-8"))["package"]
    require(all(manifest.get(k) == package.get(k) for k in ("name", "version", "license")),
            f"{package['name']}: registry metadata differs from authenticated archive")
    return digest, files


def license_text(raw, label):
    text = raw.decode("utf-8")
    require(text.strip(), f"{label}: missing license text")
    return text


def mit_text(path, label):
    require(path is not None, f"{label}: supply a reviewed local license file")
    text = license_text(path.read_bytes(), label)
    folded = " ".join(text.lower().split())
    require(all(phrase in folded for phrase in (
        "copyright", "permission is hereby granted, free of charge",
        "the above copyright notice", 'the software is provided "as is"',
        "liability", "dealings in the software",
    )), f"{label}: expected the complete MIT license and attribution")
    return text


def section(marker, text):
    return marker + "\n" + text + ("" if text.endswith("\n") else "\n") + "\n"


def required_runtimes(target):
    names = {"rust-std"}
    if target.endswith("-linux-musl"):
        names.add("musl")
    if target.endswith("-windows-gnu"):
        names.add("mingw-w64")
    return names


def runtime_manifest(manifest, target, local=False):
    require(isinstance(manifest, dict) and set(manifest) == {"target", "components"}
            and manifest["target"] == target, "runtime manifest target/schema mismatch")
    entries = manifest["components"]
    require(isinstance(entries, list) and all(isinstance(e, dict) and isinstance(e.get("name"), str)
            for e in entries), "invalid runtime component inventory")
    names = {e["name"] for e in entries}
    required = required_runtimes(target)
    allowed = required | ({"gcc-runtime"} if target.endswith("-windows-gnu") else set())
    require(required <= names <= allowed and len(entries) == len(names), "runtime component inventory mismatch")
    for entry in entries:
        require(set(entry) == {"name", "version", "source", "license", "sha256", "notices"},
                "runtime component fields must include exact version/source/license/sha256/notices")
        require(all(isinstance(entry[k], str) and entry[k].strip() == entry[k] and entry[k]
                    for k in ("version", "source", "license", "sha256")), "missing runtime component data")
        require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+_-]*", entry["version"])
                and entry["version"].lower() not in ("latest", "stable", "nightly", "unknown"),
                "runtime version must be exact")
        require(re.fullmatch(r"https://[^\s]+", entry["source"]), "runtime source must be a public HTTPS URL")
        require(re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]), "invalid runtime source/input SHA256")
        files = entry["notices"]
        value_key = "path" if local else "sha256"
        require(isinstance(files, list) and files and all(
            isinstance(f, dict) and set(f) == {"label", value_key}
            and isinstance(f["label"], str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 ._()-]*", f["label"])
            and isinstance(f[value_key], str) and f[value_key].strip()
            and (local or re.fullmatch(r"[0-9a-f]{64}", f[value_key]))
            for f in files), "runtime requires public notice labels and local paths or SHA256 hashes")
        require(len({f["label"] for f in files}) == len(files), "duplicate runtime notice label")
    return entries


def runtime_component(entry, target):
    return {"type": "library", "name": entry["name"], "version": entry["version"],
            "bom-ref": f"runtime:{entry['name']}@{entry['version']}:{target}",
            "hashes": hashes(entry["sha256"]), "licenses": [{"expression": entry["license"]}],
            "properties": properties({"source": entry["source"], "target": target,
                                      "checksum-kind": "reviewed runtime source/input SHA-256"})}


def runtime_notice_markers(entry, file):
    label = f"runtime {entry['name']} {entry['version']} {file['label']}"
    return f"===== {label} =====\n", f"\n===== END {label} =====\n"


def load_runtimes(path, target):
    entries = runtime_manifest(json.loads(path.read_bytes()), target, local=True)
    notices = ""
    for entry in sorted(entries, key=lambda e: e["name"]):
        for file in sorted(entry["notices"], key=lambda f: f["label"]):
            data = (path.parent / file.pop("path")).read_bytes()
            file["sha256"] = hashlib.sha256(data).hexdigest()
            text = license_text(data, file["label"])
            start, end = runtime_notice_markers(entry, file)
            require(start not in text and end not in text, "runtime notice contains reserved delimiter")
            notices += start + text + end
    canonical = canonical_runtime_manifest(entries, target)
    return canonical, [runtime_component(e, target) for e in entries], notices


def canonical_runtime_manifest(entries, target):
    components = [dict(e, notices=sorted(e["notices"], key=lambda f: f["label"]))
                  for e in sorted(entries, key=lambda e: e["name"])]
    return json.dumps({"target": target, "components": components}, sort_keys=True,
                      separators=(",", ":"), ensure_ascii=False)


def validate_runtimes(document, notices=None):
    root = document["metadata"]["component"]
    props = property_map(root)
    raw = props.get("retok:runtime-manifest", "")
    require(raw and hashlib.sha256(raw.encode("utf-8")).hexdigest()
            == props.get("retok:runtime-manifest-sha256"), "runtime manifest SHA256 mismatch")
    target = props["retok:target"]
    entries = runtime_manifest(json.loads(raw), target)
    require(raw == canonical_runtime_manifest(entries, target), "runtime manifest is not canonical")
    expected = sorted((runtime_component(e, target) for e in entries), key=lambda c: c["bom-ref"])
    actual = sorted((c for c in document["components"] if c["bom-ref"].startswith("runtime:")),
                    key=lambda c: c["bom-ref"])
    require(actual == expected, "runtime component inventory/data mismatch")
    edges = {e["ref"]: e["dependsOn"] for e in document["dependencies"]}
    for component in expected:
        ref = component["bom-ref"]
        require(ref in edges.get(root["bom-ref"], []) and edges.get(ref) == [],
                "runtime dependency edges mismatch")
    if notices is not None:
        for entry in entries:
            for file in entry["notices"]:
                start, end = runtime_notice_markers(entry, file)
                require(notices.count(start) == 1 and notices.count(end) == 1,
                        "missing/duplicate runtime notice marker")
                text, found, _ = notices.split(start, 1)[1].partition(end)
                require(found and text.strip() and hashlib.sha256(text.encode("utf-8")).hexdigest()
                        == file["sha256"], "runtime notice SHA256 mismatch")
    return {c["bom-ref"] for c in expected}


def effective_license(name, cargo_expression):
    if name == "regex-syntax":
        return REGEX_LICENSE
    return "MIT OR Apache-2.0" if cargo_expression == "MIT/Apache-2.0" else cargo_expression


def winapi_import_licenses(package, packages, graph, lock):
    """Offline supplement for the reviewed import archive, never unrelated crates."""
    parents = [p for p in packages.values() if p["name"] == "winapi" and p["version"] == "0.3.9"
               and package["id"] in graph[p["id"]]]
    require(len(parents) == 1, "winapi import licenses require normal-parent winapi 0.3.9")
    parent = parents[0]
    require(parent["source"] == package["source"]
            and parent["license"] == package["license"] == "MIT/Apache-2.0",
            "winapi parent source/license mismatch")
    digest, files = checked_archive(parent, lock)
    require(json.loads(files.get(".cargo_vcs_info.json", b"{}"))
            .get("git", {}).get("sha1") == WINAPI_REVISION, "winapi parent source revision mismatch")
    names = ("LICENSE-MIT", "LICENSE-APACHE")
    require(all(name in files for name in names), "winapi parent missing full license files")
    texts = {name: license_text(files[name], "winapi " + name) for name in names}
    # Reviewed root and x86_64 license files at this revision are byte-identical
    # to the two license files in the Cargo.lock-authenticated winapi archive.
    texts["NOTICE (license provenance)"] = (
        "The winapi-x86_64-pc-windows-gnu 0.4.0 package omits its license files.\n"
        "License texts supplied by Cargo.lock-authenticated winapi 0.3.9.\n"
        f"winapi archive SHA-256: {digest}\n"
        "Reviewed upstream x86_64/LICENSE-MIT and x86_64/LICENSE-APACHE match these texts.\n"
        f"Source: https://github.com/retep998/winapi-rs/tree/{WINAPI_REVISION}/x86_64\n"
    )
    return texts


def generate(project, binary, target, commit, tiktoken_license, openai_license, runtime_path):
    require(target in TRIPLES.get(binary.name, ()), "target does not match binary asset name")
    require(re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit), "source commit must be a full Git hash")
    runtime_raw, runtimes, runtime_notices = load_runtimes(runtime_path, target)
    root, packages, graph = normal_graph(cargo_graph(project, target))
    lock = {(p["name"], p["version"], p.get("source")): p
            for p in tomllib.loads((project / "Cargo.lock").read_text())["package"]}
    root_ref = f"retok@{VERSION}:{target}"
    refs = {key: f"pkg:cargo/{p['name']}@{p['version']}" for key, p in packages.items()}
    refs[root] = root_ref
    require(len(set(refs.values())) == len(refs), "duplicate package identities across registries")
    notices = section(SPECIAL_MARKERS[0], license_text((project / "LICENSE").read_bytes(), "Retok"))
    components = []
    vocab = None
    for key in sorted(packages, key=lambda key: refs[key]):
        if key == root:
            continue
        package = packages[key]
        digest, files = checked_archive(package, lock)
        expression = package.get("license")
        require(isinstance(expression, str) and expression.strip(),
                f"{package['name']}: missing SPDX license expression")
        texts = {path: license_text(raw, path) for path, raw in files.items()
                 if path not in ("Cargo.toml", ".cargo_vcs_info.json", VOCAB_PATH)}
        if package["name"] == "tiktoken-rs" and not texts:
            texts["LICENSE (upstream supplement)"] = mit_text(tiktoken_license, "tiktoken-rs")
        if (package["name"], package["version"]) == ("winapi-x86_64-pc-windows-gnu", "0.4.0") and not texts:
            texts = winapi_import_licenses(package, packages, graph, lock)
        require(texts, f"{package['name']}: no complete license texts in cached crate")
        marker = f"===== crate {package['name']} {package['version']} ====="
        notices += section(marker, "License expression: " + expression)
        for path, text in sorted(texts.items()):
            notices += section("--- " + path + " ---", text)
        for name, unicode_marker in (("regex-syntax", SPECIAL_MARKERS[3]),
                                     ("unicode-ident", SPECIAL_MARKERS[4])):
            if package["name"] == name:
                unicode_texts = [text for path, text in texts.items() if "unicode" in path.lower()]
                require(unicode_texts, f"{name}: missing Unicode data license")
                notices += section(unicode_marker, "\n".join(unicode_texts))
        components.append({
            "type": "library", "bom-ref": refs[key], "purl": refs[key],
            "name": package["name"], "version": package["version"],
            "hashes": hashes(digest), "licenses": [{"expression": effective_license(package["name"], expression)}],
            "properties": properties({"checksum-kind": "Cargo.lock package archive SHA-256",
                                      "source": package["source"], "cargo-license-expression": expression}),
        })
        if package["name"] == "tiktoken-rs":
            notices += "tiktoken-rs acknowledges @spolu for the original code and .tiktoken files.\n\n"
            require(VOCAB_PATH in files, "tiktoken-rs: missing o200k vocabulary")
            raw = files[VOCAB_PATH]
            require(raw, "empty o200k vocabulary")
            with binary.open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as mapped:
                offset = mapped.find(raw)
                require(offset >= 0, "binary does not contain the cached o200k vocabulary")
            vocab_hash = hashlib.sha256(raw).hexdigest()
            revision = json.loads(files[".cargo_vcs_info.json"])["git"]["sha1"]
            require(re.fullmatch(r"[0-9a-f]{40}", revision), "invalid tiktoken-rs source revision")
            vocab_ref = "o200k_base:sha256:" + vocab_hash
            vocab = {
                "type": "data", "bom-ref": vocab_ref, "name": "o200k_base",
                "version": vocab_hash, "hashes": hashes(vocab_hash),
                "licenses": [{"expression": "MIT"}],
                "externalReferences": [{"type": "distribution", "url": VOCAB_URL},
                                       {"type": "vcs", "url": OPENAI_URL}],
                "properties": properties({"embedded-in": root_ref, "source-crate": refs[key],
                                          "source-path": VOCAB_PATH, "source-commit": revision,
                                          "embedded-offset": str(offset), "embedded-size": str(len(raw))}),
            }
            notices += section(SPECIAL_MARKERS[2],
                               f"OpenAI/tiktoken: {OPENAI_URL}\nVocabulary: {VOCAB_URL}\n"
                               f"Bundled by {refs[key]} at {revision}, {VOCAB_PATH}\nSHA-256: {vocab_hash}")
    require(vocab is not None, "normal dependency graph does not contain tiktoken-rs")
    notices += section(SPECIAL_MARKERS[1], mit_text(openai_license, "OpenAI/tiktoken"))
    notices += runtime_notices
    components.append(vocab)
    components.extend(runtimes)
    edges = [{"ref": refs[key], "dependsOn": sorted(refs[d] for d in deps)}
             for key, deps in graph.items()]
    next(e for e in edges if e["ref"] == property_map(vocab)["retok:source-crate"])["dependsOn"].append(vocab["bom-ref"])
    next(e for e in edges if e["ref"] == root_ref)["dependsOn"].extend(c["bom-ref"] for c in runtimes)
    edges.extend({"ref": c["bom-ref"], "dependsOn": []} for c in runtimes)
    for edge in edges:
        edge["dependsOn"].sort()
    edges.append({"ref": vocab["bom-ref"], "dependsOn": []})
    document = {
        "bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
        "metadata": {"component": {
            "type": "application", "bom-ref": root_ref, "name": "retok", "version": VERSION,
            "hashes": hashes(sha256(binary)), "licenses": [{"expression": "MIT"}],
            "properties": properties({
                "target": target, "source-commit": commit,
                "cargo-lock-sha256": sha256(project / "Cargo.lock"),
                "cargo-manifest-sha256": sha256(project / "Cargo.toml"),
                "runtime-manifest": runtime_raw,
                "runtime-manifest-sha256": hashlib.sha256(runtime_raw.encode("utf-8")).hexdigest(),
                "normal-dependencies": json.dumps(sorted(refs[k] for k in graph if k != root)),
                "notices-sha256": hashlib.sha256(notices.encode("utf-8")).hexdigest(),
            }),
        }},
        "components": sorted(components, key=lambda c: c["bom-ref"]),
        "dependencies": sorted(edges, key=lambda d: d["ref"]),
    }
    validate(document, notices, binary)
    return document, notices


def component_hash(component):
    value = component.get("hashes")
    require(isinstance(value, list) and len(value) == 1 and isinstance(value[0], dict)
            and value[0].get("alg") == "SHA-256"
            and isinstance(value[0].get("content"), str)
            and re.fullmatch(r"[0-9a-f]{64}", value[0]["content"]), "missing/invalid SHA-256")
    return value[0]["content"]


def validate(document, notices, binary):
    """Validate internal inventory/bindings; this is not a signed build attestation."""
    root = document["metadata"]["component"]
    props = property_map(root)
    require(props.get("retok:target") in TRIPLES.get(binary.name, ()), "missing/mismatched root target")
    require(re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", props.get("retok:source-commit", "")),
            "missing/invalid root source commit")
    require(component_hash(root) == sha256(binary), "root binary SHA-256 mismatch")
    require(props.get("retok:notices-sha256") == hashlib.sha256(notices.encode("utf-8")).hexdigest(),
            "notices SHA-256 mismatch")
    for key in ("cargo-lock-sha256", "cargo-manifest-sha256"):
        require(re.fullmatch(r"[0-9a-f]{64}", props.get("retok:" + key, "")), "missing " + key)
    components = document["components"]
    require(all(isinstance(c.get("bom-ref"), str) and c["bom-ref"] for c in [root] + components),
            "missing component reference")
    refs = {c["bom-ref"] for c in [root] + components}
    require(len(refs) == len(components) + 1, "duplicate component reference")
    runtimes = [c for c in components if c["bom-ref"].startswith("runtime:")]
    libraries = [c for c in components if c.get("type") == "library" and c not in runtimes]
    vocabularies = [c for c in components if c.get("type") == "data" and c.get("name") == "o200k_base"]
    require(len(vocabularies) == 1 and len(libraries) + len(runtimes) + 1 == len(components),
            "missing vocabulary/component inventory")
    inventory = json.loads(props.get("retok:normal-dependencies", "null"))
    require(inventory == sorted(c["bom-ref"] for c in libraries), "normal dependency inventory mismatch")
    require({"tiktoken-rs", "regex-syntax", "unicode-ident"} <= {c["name"] for c in libraries},
            "missing required normal dependency")
    for component in components:
        component_hash(component)
        licenses = component.get("licenses")
        require(isinstance(licenses, list) and len(licenses) == 1 and isinstance(licenses[0], dict)
                and isinstance(licenses[0].get("expression"), str) and licenses[0]["expression"].strip(),
                "missing component license expression")
    for library in libraries:
        declared = property_map(library).get("retok:cargo-license-expression")
        require(declared and library["licenses"] == [{"expression": effective_license(library["name"], declared)}],
                "Cargo/effective license expression mismatch")
        require(library.get("purl") == f"pkg:cargo/{library['name']}@{library['version']}"
                == library["bom-ref"], "mismatched package identity")
        require(f"===== crate {library['name']} {library['version']} =====" in notices,
                "missing dependency notice marker")
    for marker in SPECIAL_MARKERS:
        require(marker in notices, "missing required notice marker: " + marker)
    edges = document.get("dependencies")
    require(isinstance(edges, list) and all(isinstance(e, dict) and isinstance(e.get("ref"), str)
            and isinstance(e.get("dependsOn"), list) and all(isinstance(d, str) for d in e["dependsOn"])
            for e in edges), "missing dependency relationships")
    graph = {e["ref"]: e["dependsOn"] for e in edges}
    require(len(graph) == len(edges) and set(graph) == refs
            and all(set(deps) <= refs and len(set(deps)) == len(deps) for deps in graph.values()),
            "dependency relationship inventory mismatch")
    validate_runtimes(document, notices)
    reachable, pending = set(), [root["bom-ref"]]
    while pending:
        ref = pending.pop()
        if ref not in reachable:
            reachable.add(ref)
            pending.extend(graph[ref])
    require(reachable == refs, "unreachable dependency component")
    vocab = vocabularies[0]
    vp = property_map(vocab)
    require(vp.get("retok:embedded-in") == root["bom-ref"]
            and vp.get("retok:source-path") == VOCAB_PATH
            and re.fullmatch(r"[0-9a-f]{40}", vp.get("retok:source-commit", ""))
            and vp.get("retok:source-crate") in {c["bom-ref"] for c in libraries if c["name"] == "tiktoken-rs"}
            and vocab["bom-ref"] in graph[vp["retok:source-crate"]]
            and vocab.get("externalReferences") == [{"type": "distribution", "url": VOCAB_URL},
                                                    {"type": "vcs", "url": OPENAI_URL}],
            "missing vocabulary provenance")
    digest = component_hash(vocab)
    require(vocab["version"] == digest and vocab["bom-ref"] == "o200k_base:sha256:" + digest,
            "vocabulary hash identity mismatch")
    offset = int(vp.get("retok:embedded-offset", "-1"))
    size = int(vp.get("retok:embedded-size", "0"))
    require(offset >= 0 and size > 0 and offset + size <= binary.stat().st_size,
            "invalid embedded vocabulary location")
    with binary.open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as mapped:
        require(hashlib.sha256(mapped[offset:offset + size]).hexdigest() == digest,
                "embedded vocabulary SHA-256 mismatch")


def validate_project(document, project):
    """Compare an SBOM with authoritative local inputs, without generating notices."""
    component = document["metadata"]["component"]
    props = property_map(component)
    require(props["retok:cargo-lock-sha256"] == sha256(project / "Cargo.lock")
            and props["retok:cargo-manifest-sha256"] == sha256(project / "Cargo.toml"),
            "Cargo source/lockfile hashes differ from metadata")
    root, packages, graph = normal_graph(cargo_graph(project, props["retok:target"]))
    refs = {key: f"pkg:cargo/{p['name']}@{p['version']}" for key, p in packages.items()}
    refs[root] = component["bom-ref"]
    runtime_refs = validate_runtimes(document)
    libraries = {c["bom-ref"]: c for c in document["components"]
                 if c["type"] == "library" and c["bom-ref"] not in runtime_refs}
    require(set(libraries) == {refs[k] for k in graph if k != root}, "Cargo dependency inventory mismatch")
    lock = {(p["name"], p["version"], p.get("source")): p
            for p in tomllib.loads((project / "Cargo.lock").read_text())["package"]}
    for key, package in packages.items():
        if key == root:
            continue
        library = libraries[refs[key]]
        entry = lock[(package["name"], package["version"], package["source"])]
        require(component_hash(library) == entry["checksum"]
                and property_map(library).get("retok:cargo-license-expression") == package["license"]
                and library["licenses"] == [{"expression": effective_license(package["name"], package["license"])}],
                "Cargo package checksum/license mismatch")
    actual = {e["ref"]: set(e["dependsOn"]) for e in document["dependencies"]}
    vocab = next(c for c in document["components"] if c["type"] == "data")
    for key, deps in graph.items():
        expected = {refs[d] for d in deps}
        if key == root:
            expected.update(runtime_refs)
        if packages[key]["name"] == "tiktoken-rs":
            expected.add(vocab["bom-ref"])
        require(actual[refs[key]] == expected, "Cargo dependency relationships mismatch")
