"""Offline fixtures only; no builds, downloads, or production license substitutes."""

import copy
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch

import release_metadata as metadata


MIT = '''MIT License

Copyright (c) 2026 Synthetic fixture contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
'''
VOCAB = b"Synthetic o200k fixture, not a production vocabulary.\n"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


class MetadataTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.project = self.root / "project"
        self.project.mkdir()
        (self.project / "Cargo.toml").write_text(
            f'[package]\nname="retok"\nversion="{metadata.VERSION}"\n'
        )
        (self.project / "LICENSE").write_text(MIT)
        self.binary = self.root / "retok-linux-x64"
        self.binary.write_bytes(b"Synthetic binary\0" + VOCAB + b"suffix")
        self.supplement = self.root / "synthetic-license"
        self.supplement.write_text(MIT)
        self.runtime_path = self.root / "runtimes.json"
        self.write_runtime_manifest("x86_64-unknown-linux-gnu", ["rust-std"])
        self.packages = [{"id": "root", "name": "retok", "version": metadata.VERSION}]
        self.lock_entries = []
        self.archives = {}
        self.full_texts = []
        for name in ("tiktoken-rs", "regex-syntax", "unicode-ident", "normal",
                     "dev-only", "build-only", "unused-platform"):
            self.add_crate(name)
        self.nodes = [
            self.node("root", [("tiktoken-rs", None), ("normal", None),
                               ("dev-only", "dev"), ("build-only", "build")]),
            self.node("tiktoken-rs", [("regex-syntax", None), ("unicode-ident", None)]),
            self.node("normal", [("build-only", "build")]),
        ] + [self.node(name, []) for name in ("regex-syntax", "unicode-ident", "dev-only",
                                              "build-only", "unused-platform")]
        self.cargo = {"packages": self.packages, "resolve": {"root": "root", "nodes": self.nodes}}
        self.graph_mock = self.enterContext(patch("release_metadata.cargo_graph", return_value=self.cargo))
        self.write_lock()

    @staticmethod
    def node(name, deps):
        return {"id": name, "deps": [{"pkg": dep, "dep_kinds": [{"kind": kind, "target": None}]}
                                     for dep, kind in deps]}

    def add_crate(self, name, version="1.2.3", declared=None, overrides=None):
        directory = self.root / "registry" / "src" / "example" / (name + "-" + version)
        directory.mkdir(parents=True)
        declared = declared or {"regex-syntax": "MIT OR Apache-2.0",
                    "unicode-ident": "(MIT OR Apache-2.0) AND Unicode-3.0"}.get(name, "MIT")
        manifest = f'[package]\nname="{name}"\nversion="{version}"\nlicense="{declared}"\n'
        (directory / "Cargo.toml").write_text(manifest)
        files = {"Cargo.toml": manifest.encode()}
        if name == "tiktoken-rs":
            files.update({metadata.VOCAB_PATH: VOCAB,
                          ".cargo_vcs_info.json": json.dumps({"git": {"sha1": "a" * 40}}).encode()})
        else:
            files["LICENSE-MIT"] = MIT.encode()
        if name in ("regex-syntax", "unicode-ident"):
            path = "src/unicode_tables/LICENSE-UNICODE" if name == "regex-syntax" else "LICENSE-UNICODE"
            files[path] = (f"Synthetic {name} Unicode data terms and attribution.\nFull fixture text.\n").encode()
            self.full_texts.append(files[path].decode())
        if name == "normal":
            files["NOTICE"] = b"Synthetic attribution that must not be dropped.\n"
            self.full_texts.append(files["NOTICE"].decode())
        for path, raw in (overrides or {}).items():
            if raw is None:
                files.pop(path, None)
            else:
                files[path] = raw
        archive = self.root / "registry" / "cache" / "example" / (name + "-" + version + ".crate")
        archive.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive, "w:gz") as output:
            for path, raw in files.items():
                info = tarfile.TarInfo(name + "-" + version + "/" + path)
                info.size = len(raw)
                output.addfile(info, io.BytesIO(raw))
        self.archives[name] = archive
        self.lock_entries.append(f'[[package]]\nname="{name}"\nversion="{version}"\n'
                                 f'source="{REGISTRY}"\nchecksum="{metadata.sha256(archive)}"\n')
        self.packages.append({"id": name, "name": name, "version": version, "license": declared,
                              "source": REGISTRY, "manifest_path": str(directory / "Cargo.toml")})

    def write_lock(self):
        (self.project / "Cargo.lock").write_text("version=4\n" + "\n".join(self.lock_entries))

    def add_winapi(self, parent=True, parent_edge=None, overrides=None, import_version="0.4.0"):
        self.add_crate("winapi-x86_64-pc-windows-gnu", import_version, "MIT/Apache-2.0",
                       {"LICENSE-MIT": None})
        self.nodes.append(self.node("winapi-x86_64-pc-windows-gnu", []))
        self.nodes[0]["deps"].append(self.node("root", [("winapi-x86_64-pc-windows-gnu", None)])["deps"][0])
        if parent:
            files = {"LICENSE-APACHE": b"Synthetic Apache license and attribution fixture.\n",
                     ".cargo_vcs_info.json": json.dumps({"git": {"sha1": metadata.WINAPI_REVISION}}).encode()}
            files.update(overrides or {})
            self.add_crate("winapi", "0.3.9", "MIT/Apache-2.0", files)
            self.nodes.append(self.node("winapi", [("winapi-x86_64-pc-windows-gnu", parent_edge)]))
            self.nodes[0]["deps"].append(self.node("root", [("winapi", None)])["deps"][0])
        self.write_lock()

    def use_packed_tokenizer(self, overrides=None):
        """A tiny synthetic reviewed representation; never substitute production data."""
        self.packed = b"Synthetic packed prefix\0" + bytes(range(128))
        self.binary.write_bytes(b"Synthetic executable\0" + self.packed + b"trailer")
        gzip_vocab = gzip.compress(VOCAB, mtime=0)
        build_script = b"// Synthetic dictionary generator fixture.\n"
        profile = dict(metadata.PACKED_PROPERTIES, **{
            "source-commit": "c" * 40,
            "source-compressed-sha256": hashlib.sha256(gzip_vocab).hexdigest(),
            "generator-build-script-sha256": hashlib.sha256(build_script).hexdigest(),
            "embedded-sha256": hashlib.sha256(self.packed).hexdigest(),
            "embedded-size": str(len(self.packed)),
        })
        self.enterContext(patch("release_metadata.PACKED_PROPERTIES", profile))
        self.enterContext(patch("release_metadata.PACKED_PREFIX", self.packed[:24]))
        self.enterContext(patch("release_metadata.PACKED_VOCAB_SHA256", hashlib.sha256(VOCAB).hexdigest()))
        self.enterContext(patch("release_metadata.GITHUB_MIT", MIT))
        self.enterContext(patch("release_metadata.LIBYAML_MIT", MIT.replace("contributors", "libyaml fixture contributors")))
        files = {"LICENSE-MIT": None, metadata.PACKED_PATH: gzip_vocab, "build.rs": build_script,
                 ".cargo_vcs_info.json": json.dumps({"git": {"sha1": "c" * 40}}).encode()}
        files.update(overrides or {})
        self.add_crate("bpe-openai", "0.3.1", overrides=files)
        self.add_crate("bpe", "0.2.2", overrides={
            "LICENSE-MIT": None, ".cargo_vcs_info.json": json.dumps({"git": {"sha1": "d" * 40}}).encode()})
        self.add_crate("unsafe-libyaml", "0.2.11", overrides={
            ".cargo_vcs_info.json": json.dumps({"git": {"sha1": "e" * 40}}).encode()})
        for name in ("json5", "serde_yaml_ng", "vocab-builder", "vocab-codec"):
            self.add_crate(name)
        pins = dict(metadata.SUPPLEMENTS)
        for name, version, revision in (("bpe-openai", "0.3.1", "c"), ("bpe", "0.2.2", "d"),
                                         ("unsafe-libyaml", "0.2.11", "e")):
            pins[name, version] = (metadata.sha256(self.archives[name]), revision * 40, "https://example.org/fixture")
        self.enterContext(patch("release_metadata.SUPPLEMENTS", pins))
        self.nodes[0]["deps"] = self.node("root", [
            ("bpe-openai", None), ("normal", None), ("json5", None), ("serde_yaml_ng", None),
            ("tiktoken-rs", "dev"), ("build-only", "build"), ("dev-only", "dev")])["deps"]
        self.nodes.extend([
            self.node("bpe-openai", [("bpe", None), ("regex-syntax", None), ("unicode-ident", None),
                                      ("vocab-builder", "build"), ("tiktoken-rs", "dev")]),
            self.node("bpe", []), self.node("vocab-builder", [("vocab-codec", None)]),
            self.node("vocab-codec", []), self.node("json5", []),
            self.node("serde_yaml_ng", [("unsafe-libyaml", None)]), self.node("unsafe-libyaml", []),
        ])
        self.cargo["retok-normal-graph"] = {
            "root": ["bpe-openai", "normal", "json5", "serde_yaml_ng"],
            "bpe-openai": ["bpe", "regex-syntax", "unicode-ident"],
            "bpe": [], "regex-syntax": [], "unicode-ident": [], "normal": [], "json5": [],
            "serde_yaml_ng": ["unsafe-libyaml"], "unsafe-libyaml": [],
        }
        self.cargo["retok-generator-graph"] = {
            "bpe-openai": ["bpe", "regex-syntax", "unicode-ident", "vocab-builder"],
            "bpe": [], "regex-syntax": [], "unicode-ident": [],
            "vocab-builder": ["vocab-codec"], "vocab-codec": [],
        }
        self.write_lock()

    @staticmethod
    def set_property(component, key, value):
        values = {p["name"].removeprefix("retok:"): p["value"] for p in component["properties"]}
        values[key] = value
        component["properties"] = metadata.properties(values)

    def test_packed_inventory_full_notices_and_exact_binary_binding(self):
        self.use_packed_tokenizer()
        document, notices = metadata.generate(self.project, self.binary, "x86_64-unknown-linux-gnu",
                                             "b" * 40, None, self.supplement, self.runtime_path)
        components = {c["name"]: c for c in document["components"]}
        self.assertEqual(set(components), {"bpe-openai", "bpe", "normal", "regex-syntax", "unicode-ident",
                         "json5", "serde_yaml_ng", "unsafe-libyaml", "vocab-builder", "vocab-codec",
                         "o200k_base", "rust-std"})
        props = metadata.property_map(document["metadata"]["component"])
        self.assertEqual(json.loads(props["retok:vocabulary-build-dependencies"]),
                         ["pkg:cargo/vocab-builder@1.2.3", "pkg:cargo/vocab-codec@1.2.3"])
        self.assertNotIn("vocab-builder", props["retok:normal-dependencies"])
        vp = metadata.property_map(components["o200k_base"])
        self.assertEqual(vp["retok:embedded-sha256"], hashlib.sha256(self.packed).hexdigest())
        self.assertEqual(metadata.component_hash(components["o200k_base"]), hashlib.sha256(VOCAB).hexdigest())
        self.assertNotIn(VOCAB, self.binary.read_bytes())
        self.assertIn("not the raw .tiktoken text", notices)
        self.assertNotIn("assets/o200k_base.tiktoken", notices)
        for name in ("bpe-openai", "bpe", "json5", "serde_yaml_ng", "unsafe-libyaml", "vocab-builder"):
            text = notices.split(f"===== crate {name} {components[name]['version']} =====", 1)[1].split("=====", 1)[0]
            self.assertIn(MIT, text)
        self.assertIn(metadata.LIBYAML_MIT, notices)
        self.assertNotIn(str(self.root), json.dumps(document) + notices)
        self.assertEqual((document, notices), self.generate())
        metadata.validate_project(document, self.project)

    def test_packed_sources_must_be_present_and_match_reviewed_hashes(self):
        self.use_packed_tokenizer()
        _, packages, _, _ = metadata.release_graph(self.cargo)
        lock = {(p["name"], p["version"], p.get("source")): p
                for p in tomllib.loads((self.project / "Cargo.lock").read_text())["package"]}
        _, files = metadata.checked_archive(packages["bpe-openai"], lock)
        for name in (metadata.PACKED_PATH, "build.rs"):
            for raw in (None, b"changed"):
                changed = dict(files)
                if raw is None:
                    changed.pop(name)
                else:
                    changed[name] = raw
                with self.subTest(name=name, raw=raw), self.assertRaisesRegex(ValueError, "missing|SHA-256"):
                    metadata.packed_source(changed)
        with patch("release_metadata.PACKED_VOCAB_SHA256", "0" * 64):
            with self.assertRaisesRegex(ValueError, "source SHA-256"):
                self.generate()
        self.archives["bpe-openai"].unlink()
        with self.assertRaises(FileNotFoundError):
            self.generate()

    def test_packed_missing_or_modified_binary_representation_fails(self):
        self.use_packed_tokenizer()
        for data in (b"", VOCAB, self.packed[:24], self.packed[:-1] + b"changed"):
            self.binary.write_bytes(data)
            with self.subTest(size=len(data)), self.assertRaisesRegex(ValueError, "does not contain|SHA-256"):
                self.generate()

    def test_packed_sidecar_cannot_redefine_reviewed_bytes_or_provenance(self):
        self.use_packed_tokenizer()
        document, notices = self.generate()
        for key, value in (("embedded-sha256", "0" * 64), ("embedded-size", "1"),
                           ("source-compressed-sha256", "0" * 64), ("source-path", metadata.VOCAB_PATH),
                           ("generator-build-script-sha256", "0" * 64), ("source-commit", "f" * 40),
                           ("representation", "raw vocabulary"), ("embedded-offset", "0")):
            changed = copy.deepcopy(document)
            vocab = next(c for c in changed["components"] if c["name"] == "o200k_base")
            self.set_property(vocab, key, value)
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "representation|SHA-256"):
                metadata.validate(changed, notices, self.binary)
        # Rehashing the whole executable cannot hide a changed byte inside the dictionary.
        self.binary.write_bytes(self.binary.read_bytes().replace(self.packed, self.packed[:-1] + b"!"))
        document["metadata"]["component"]["hashes"] = metadata.hashes(metadata.sha256(self.binary))
        with self.assertRaisesRegex(ValueError, "embedded vocabulary SHA-256"):
            metadata.validate(document, notices, self.binary)
        vocab = next(c for c in document["components"] if c["name"] == "o200k_base")
        self.set_property(vocab, "embedded-sha256", hashlib.sha256(self.packed[:-1] + b"!").hexdigest())
        with self.assertRaisesRegex(ValueError, "unreviewed packed"):
            metadata.validate(document, notices, self.binary)

    def test_packed_project_verification_catches_reclassified_build_dependency(self):
        self.use_packed_tokenizer()
        document, notices = self.generate()
        root = document["metadata"]["component"]
        props = metadata.property_map(root)
        build = json.loads(props["retok:vocabulary-build-dependencies"])
        normal = json.loads(props["retok:normal-dependencies"])
        self.set_property(root, "normal-dependencies", json.dumps(sorted(normal + [build.pop()])))
        self.set_property(root, "vocabulary-build-dependencies", json.dumps(build))
        metadata.validate(document, notices, self.binary)
        with self.assertRaisesRegex(ValueError, "normal/build inventory"):
            metadata.validate_project(document, self.project)

    def test_feature_resolved_trees_exclude_metadata_only_dependencies(self):
        self.use_packed_tokenizer()
        # Model metadata's merged feature view: an inactive normal dep and an
        # inactive build transitive. Neither appears in Cargo's resolved trees.
        next(n for n in self.nodes if n["id"] == "bpe")["deps"] = self.node("bpe", [("vocab-codec", None)])["deps"]
        next(n for n in self.nodes if n["id"] == "vocab-builder")["deps"] = self.node("vocab-builder", [("unused-platform", None)])["deps"]
        normal_tree = (f"0retok v{metadata.VERSION} (/synthetic/project)\n"
                       "1bpe-openai v0.3.1\n2bpe v0.2.2\n2regex-syntax v1.2.3\n"
                       "2unicode-ident v1.2.3 (proc-macro)\n1normal v1.2.3\n")
        build_tree = ("0bpe-openai v0.3.1\n1bpe v0.2.2\n1regex-syntax v1.2.3\n"
                      "1unicode-ident v1.2.3\n1vocab-builder v1.2.3\n")
        with patch("release_metadata.subprocess.run") as run:
            run.side_effect = [subprocess.CompletedProcess([], 0, output, "")
                               for output in (json.dumps(self.cargo), normal_tree, build_tree)]
            resolved = ORIGINAL_CARGO_GRAPH(self.project, "aarch64-apple-darwin")
        self.graph_mock.return_value = resolved
        document, notices = self.generate()
        names = {c["name"] for c in document["components"]}
        self.assertIn("vocab-builder", names)
        self.assertNotIn("vocab-codec", names)
        self.assertNotIn("unused-platform", names)
        metadata.validate_project(document, self.project)
        for call, kinds, name in zip(run.call_args_list[1:], ("normal", "normal,build"), ("retok", "bpe-openai")):
            args = call.args[0]
            self.assertEqual(args[:6], ["cargo", "tree", "--offline", "--locked", "--target", "aarch64-apple-darwin"])
            self.assertEqual(args[args.index("--edges") + 1], kinds)
            self.assertEqual(args[args.index("--package") + 1], name)
            self.assertIn("--no-dedupe", args)

    def test_cargo_tree_parser_fails_closed_and_merges_host_target_instances(self):
        for text in ("", "not a tree\n", "1retok v" + metadata.VERSION,
                     f"0retok v{metadata.VERSION}\n2normal v1.2.3\n", "0absent v1.2.3\n"):
            with patch("release_metadata.subprocess.run") as run:
                run.return_value = subprocess.CompletedProcess([], 0, text, "")
                with self.subTest(text=text), self.assertRaisesRegex(ValueError, "Cargo tree"):
                    metadata.cargo_tree_graph(self.project, "target", self.cargo, "normal", "retok")
        text = (f"0retok v{metadata.VERSION}\n1normal v1.2.3\n2regex-syntax v1.2.3\n"
                "1normal v1.2.3\n2unicode-ident v1.2.3\n")
        with patch("release_metadata.subprocess.run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, text, "")
            graph = metadata.cargo_tree_graph(self.project, "target", self.cargo, "normal,build", "retok")
        self.assertEqual(graph["normal"], ["regex-syntax", "unicode-ident"])

    def test_packed_supplements_reject_archive_revision_license_drift(self):
        self.use_packed_tokenizer()
        _, packages, _, _ = metadata.release_graph(self.cargo)
        lock = {(p["name"], p["version"], p.get("source")): p
                for p in tomllib.loads((self.project / "Cargo.lock").read_text())["package"]}
        package = packages["bpe"]
        digest, files = metadata.checked_archive(package, lock)
        for changed_package, changed_digest, changed_files in (
            (dict(package, version="0.2.3"), digest, files),
            (package, "0" * 64, files),
            (dict(package, license="Apache-2.0"), digest, files),
            (package, digest, dict(files, **{".cargo_vcs_info.json": b'{"git":{"sha1":"ffffffffffffffffffffffffffffffffffffffff"}}'})),
        ):
            with self.assertRaisesRegex(ValueError, "unreviewed license supplement"):
                metadata.supplement_texts(changed_package, changed_digest, changed_files)
        with self.archives["vocab-builder"].open("ab") as stream:
            stream.write(b"tampered build archive")
        with self.assertRaisesRegex(ValueError, "cached crate checksum"):
            self.generate()

    def test_full_supplement_required_even_after_overall_notice_rehash(self):
        self.use_packed_tokenizer()
        document, notices = self.generate()
        for name in ("bpe", "bpe-openai", "unsafe-libyaml"):
            component = next(c for c in document["components"] if c["name"] == name)
            marker = f"===== crate {name} {component['version']} ====="
            before, after = notices.split(marker, 1)
            section, separator, rest = after.partition("=====")
            altered = before + marker + section.replace("Permission is hereby granted", "Omitted") + separator + rest
            changed = copy.deepcopy(document)
            self.set_property(changed["metadata"]["component"], "notices-sha256", hashlib.sha256(altered.encode()).hexdigest())
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "complete upstream license"):
                metadata.validate(changed, altered, self.binary)

    def add_yaml_edit(self):
        self.add_crate("yaml-edit", "0.3.1", "Apache-2.0", {
            "LICENSE-MIT": None,
            ".cargo_vcs_info.json": json.dumps({"git": {"sha1": "f" * 40}}).encode(),
        })
        self.nodes.append(self.node("yaml-edit", []))
        self.nodes[0]["deps"].append(self.node("root", [("yaml-edit", None)])["deps"][0])
        if "retok-normal-graph" in self.cargo:
            self.cargo["retok-normal-graph"]["root"].append("yaml-edit")
            self.cargo["retok-normal-graph"]["yaml-edit"] = []
        self.enterContext(patch.dict(metadata.SUPPLEMENTS, {("yaml-edit", "0.3.1"): (
            metadata.sha256(self.archives["yaml-edit"]), "f" * 40, "https://example.org/yaml-edit")}))
        self.write_lock()

    def test_yaml_edit_canonical_apache_terms_and_honest_provenance(self):
        self.use_packed_tokenizer()
        self.add_yaml_edit()
        document, notices = self.generate()
        component = next(c for c in document["components"] if c["name"] == "yaml-edit")
        self.assertEqual(component["licenses"], [{"expression": "Apache-2.0"}])
        self.assertEqual(metadata.component_hash(component), metadata.sha256(self.archives["yaml-edit"]))
        text = notices.split("===== crate yaml-edit 0.3.1 =====", 1)[1].split("=====", 1)[0]
        self.assertIn(metadata.APACHE_2_0, text)
        self.assertIn("declares Apache-2.0 in its authenticated Cargo.toml", text)
        self.assertIn("https://www.apache.org/licenses/LICENSE-2.0.txt", text)
        self.assertIn("cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30", text)
        self.assertIn("/blob/" + "f" * 40 + "/Cargo.toml", text)
        self.assertIn("not an upstream LICENSE file", text)
        self.assertNotIn("/blob/" + "f" * 40 + "/LICENSE", text)
        self.assertNotIn("GitHub upstream supplement", text)
        self.assertNotIn(MIT, text)
        self.assertNotIn(str(self.root), text)
        metadata.validate_project(document, self.project)

    def test_yaml_edit_supplement_rejects_identity_spdx_revision_and_terms_drift(self):
        self.add_yaml_edit()
        package = next(p for p in self.packages if p["name"] == "yaml-edit")
        lock = {(p["name"], p["version"], p.get("source")): p
                for p in tomllib.loads((self.project / "Cargo.lock").read_text())["package"]}
        digest, files = metadata.checked_archive(package, lock)
        for changed_package, changed_digest, changed_files in (
            (dict(package, version="0.3.2"), digest, files),
            (dict(package, name="unreviewed-yaml"), digest, files),
            (package, "0" * 64, files),
            (dict(package, license="MIT"), digest, files),
            (dict(package, license="Apache-2.0 AND MIT"), digest, files),
            (package, digest, dict(files, **{".cargo_vcs_info.json": b'{"git":{"sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}'})),
        ):
            with self.subTest(package=changed_package, digest=changed_digest), self.assertRaisesRegex(
                    ValueError, "unreviewed license supplement"):
                metadata.supplement_texts(changed_package, changed_digest, changed_files)
        for value in ("", metadata.APACHE_2_0.replace("Redistribution.", "Omitted.")):
            with patch("release_metadata.APACHE_2_0", value), self.assertRaisesRegex(ValueError, "canonical Apache license SHA-256"):
                self.generate()
        with patch("release_metadata.APACHE_2_0_SHA256", "0" * 64), self.assertRaisesRegex(
                ValueError, "canonical Apache license SHA-256"):
            self.generate()
        with self.archives["yaml-edit"].open("ab") as stream:
            stream.write(b"tampered fixture archive")
        with self.assertRaisesRegex(ValueError, "cached crate checksum"):
            self.generate()

    def test_yaml_edit_rejects_license_and_provenance_tampering_after_rehash(self):
        self.add_yaml_edit()
        document, notices = self.generate()
        marker = "===== crate yaml-edit 0.3.1 ====="
        before, after = notices.split(marker, 1)
        text, separator, rest = after.partition("=====")
        mutations = (
            text.replace(metadata.APACHE_2_0, ""),
            text.replace("You must give any other recipients", "Removed condition"),
            text.replace(metadata.APACHE_2_0_URL, "https://example.org/unreviewed-license"),
            text.replace(metadata.APACHE_2_0_SHA256, "0" * 64),
            text.replace("f" * 40, "e" * 40),
            text.replace("declares Apache-2.0", "declares MIT"),
            text.replace("not an upstream LICENSE file", "an upstream LICENSE file"),
        )
        for changed_text in mutations:
            changed = copy.deepcopy(document)
            altered = before + marker + changed_text + separator + rest
            self.set_property(changed["metadata"]["component"], "notices-sha256", hashlib.sha256(altered.encode()).hexdigest())
            with self.subTest(text=changed_text[-100:]), self.assertRaisesRegex(ValueError, "canonical Apache license or yaml-edit provenance"):
                metadata.validate(changed, altered, self.binary)
        component = next(c for c in document["components"] if c["name"] == "yaml-edit")
        component["hashes"] = metadata.hashes("0" * 64)
        with self.assertRaisesRegex(ValueError, "unreviewed yaml-edit license source identity"):
            metadata.validate(document, notices, self.binary)

    def test_legacy_sidecar_without_new_build_inventory_remains_valid(self):
        document, notices = self.generate()
        root = document["metadata"]["component"]
        root["properties"] = [p for p in root["properties"] if p["name"] != "retok:vocabulary-build-dependencies"]
        metadata.validate(document, notices, self.binary)
        metadata.validate_project(document, self.project)
        vocab = next(c for c in document["components"] if c["name"] == "o200k_base")
        self.set_property(vocab, "source-commit", "f" * 40)
        with self.assertRaisesRegex(ValueError, "Cargo vocabulary source mismatch"):
            metadata.validate_project(document, self.project)

    def test_winapi_import_license_fallback_and_legacy_spdx(self):
        self.add_winapi()
        document, notices = self.generate()
        for name in ("winapi", "winapi-x86_64-pc-windows-gnu"):
            component = next(c for c in document["components"] if c["name"] == name)
            self.assertEqual(component["licenses"], [{"expression": "MIT OR Apache-2.0"}])
            self.assertEqual(metadata.property_map(component)["retok:cargo-license-expression"], "MIT/Apache-2.0")
            self.assertEqual(component["hashes"][0]["content"], metadata.sha256(self.archives[name]))
        section = notices.split("===== crate winapi-x86_64-pc-windows-gnu 0.4.0 =====", 1)[1].split("=====", 1)[0]
        self.assertIn(MIT, section)
        self.assertIn("Synthetic Apache license and attribution fixture.", section)
        self.assertIn(metadata.sha256(self.archives["winapi"]), section)
        self.assertIn("https://github.com/retep998/winapi-rs/tree/" + metadata.WINAPI_REVISION + "/x86_64", section)
        metadata.validate_project(document, self.project)

    def test_winapi_fallback_requires_normal_parent(self):
        self.add_winapi(parent=False)
        with self.assertRaisesRegex(ValueError, "normal-parent winapi"):
            self.generate()

    def test_winapi_fallback_rejects_build_only_parent_edge(self):
        self.add_winapi(parent_edge="build")
        with self.assertRaisesRegex(ValueError, "normal-parent winapi"):
            self.generate()

    def test_winapi_fallback_reauthenticates_parent_archive(self):
        self.add_winapi()
        _, packages, graph = metadata.normal_graph(self.cargo)
        lock = {(p["name"], p["version"], p.get("source")): p
                for p in tomllib.loads((self.project / "Cargo.lock").read_text())["package"]}
        with self.archives["winapi"].open("ab") as stream:
            stream.write(b"corrupt archive")
        with self.assertRaisesRegex(ValueError, "cached crate checksum"):
            metadata.winapi_import_licenses(packages["winapi-x86_64-pc-windows-gnu"], packages, graph, lock)

    def test_winapi_fallback_rejects_missing_parent_license(self):
        self.add_winapi(overrides={"LICENSE-APACHE": None})
        with self.assertRaisesRegex(ValueError, "missing full license"):
            self.generate()

    def test_winapi_fallback_rejects_empty_parent_license(self):
        self.add_winapi(overrides={"LICENSE-APACHE": b" \n"})
        with self.assertRaisesRegex(ValueError, "missing license text"):
            self.generate()

    def test_winapi_fallback_requires_reviewed_revision(self):
        self.add_winapi(overrides={".cargo_vcs_info.json": b'{"git":{"sha1":"different"}}'})
        with self.assertRaisesRegex(ValueError, "source revision mismatch"):
            self.generate()

    def test_winapi_fallback_does_not_extend_to_other_versions(self):
        self.add_winapi(import_version="0.4.1")
        with self.assertRaisesRegex(ValueError, "no complete license texts"):
            self.generate()

    def write_runtime_manifest(self, target, names):
        manifest = {"target": target, "components": [
            {"name": name, "version": "1.2.3", "source": f"https://example.org/{name}/1.2.3",
             "license": "MIT", "sha256": hashlib.sha256((name + " source fixture").encode()).hexdigest(),
             "notices": [{"label": "License and attribution", "path": str(self.supplement)}]}
            for name in names]}
        self.runtime_path.write_text(json.dumps(manifest))
        return manifest

    def generate(self, target="x86_64-unknown-linux-gnu"):
        return metadata.generate(self.project, self.binary, target, "b" * 40,
                                 self.supplement, self.supplement, self.runtime_path)

    def test_full_metadata_notices_and_determinism(self):
        document, notices = self.generate()
        self.assertEqual((document, notices), self.generate())
        components = {c["name"]: c for c in document["components"]}
        self.assertEqual(set(components), {"normal", "regex-syntax", "tiktoken-rs", "unicode-ident", "o200k_base", "rust-std"})
        for name in ("normal", "regex-syntax", "tiktoken-rs", "unicode-ident"):
            self.assertEqual(components[name]["hashes"][0]["content"], metadata.sha256(self.archives[name]))
            expected = {"regex-syntax": "(MIT OR Apache-2.0) AND Unicode-DFS-2016",
                        "unicode-ident": "(MIT OR Apache-2.0) AND Unicode-3.0"}.get(name, "MIT")
            self.assertEqual(components[name]["licenses"], [{"expression": expected}])
            self.assertIn(f"===== crate {name} 1.2.3 =====", notices)
        self.assertEqual(components["o200k_base"]["hashes"][0]["content"], hashlib.sha256(VOCAB).hexdigest())
        self.assertIn(MIT, notices)
        for text in self.full_texts:
            self.assertIn(text, notices)
        self.assertIn("@spolu", notices)
        self.assertIn("https://github.com/openai/tiktoken", notices)
        self.assertIn("assets/o200k_base.tiktoken", notices)
        encoded = json.dumps(document) + notices
        self.assertNotIn(str(self.root), encoded)
        root = document["metadata"]["component"]
        self.assertEqual(root["hashes"][0]["content"], hashlib.sha256(self.binary.read_bytes()).hexdigest())
        props = metadata.property_map(root)
        self.assertEqual(props["retok:source-commit"], "b" * 40)
        self.assertEqual(props["retok:target"], "x86_64-unknown-linux-gnu")
        edges = {e["ref"]: e["dependsOn"] for e in document["dependencies"]}
        self.assertEqual(edges[root["bom-ref"]], ["pkg:cargo/normal@1.2.3", "pkg:cargo/tiktoken-rs@1.2.3",
                                                  "runtime:rust-std@1.2.3:x86_64-unknown-linux-gnu"])
        metadata.validate_project(document, self.project)

    def test_cache_checksum_failure(self):
        with self.archives["normal"].open("ab") as output:
            output.write(b"tampered")
        with self.assertRaisesRegex(ValueError, "cached crate checksum"):
            self.generate()

    def test_missing_registry_archive_and_license_supplements(self):
        with self.assertRaisesRegex(ValueError, "tiktoken-rs: supply"):
            metadata.generate(self.project, self.binary, "x86_64-unknown-linux-gnu", "b" * 40,
                              None, self.supplement, self.runtime_path)
        with self.assertRaisesRegex(ValueError, "OpenAI/tiktoken: supply"):
            metadata.generate(self.project, self.binary, "x86_64-unknown-linux-gnu", "b" * 40,
                              self.supplement, None, self.runtime_path)
        self.supplement.write_text("MIT")
        with self.assertRaisesRegex(ValueError, "complete MIT"):
            self.generate()
        self.archives["normal"].unlink()
        with self.assertRaises(FileNotFoundError):
            self.generate()

    def test_target_commit_and_vocabulary_must_match(self):
        with self.assertRaisesRegex(ValueError, "target"):
            metadata.generate(self.project, self.binary, "aarch64-apple-darwin", "b" * 40,
                              self.supplement, self.supplement, self.runtime_path)
        with self.assertRaisesRegex(ValueError, "full Git hash"):
            metadata.generate(self.project, self.binary, "x86_64-unknown-linux-gnu", "main",
                              self.supplement, self.supplement, self.runtime_path)
        self.binary.write_bytes(b"wrong vocabulary")
        with self.assertRaisesRegex(ValueError, "does not contain"):
            self.generate()

    def test_root_hash_cannot_be_bypassed_by_updating_checksum_manifest(self):
        document, notices = self.generate()
        self.binary.write_bytes(self.binary.read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "root binary SHA-256"):
            metadata.validate(document, notices, self.binary)

    def test_notice_markers_required_even_if_notice_hash_updated(self):
        document, notices = self.generate()
        for marker in metadata.SPECIAL_MARKERS + ("===== crate normal 1.2.3 =====",):
            with self.subTest(marker=marker):
                altered = notices.replace(marker, "")
                changed = copy.deepcopy(document)
                for prop in changed["metadata"]["component"]["properties"]:
                    if prop["name"] == "retok:notices-sha256":
                        prop["value"] = hashlib.sha256(altered.encode()).hexdigest()
                with self.assertRaisesRegex(ValueError, "notice marker"):
                    metadata.validate(changed, altered, self.binary)

    def test_missing_components_relationships_licenses_and_provenance(self):
        document, notices = self.generate()
        for change, message in (
            (lambda d: d["components"].pop(1), "inventory"),
            (lambda d: d["dependencies"].pop(), "inventory"),
            (lambda d: d["components"][0].pop("licenses"), "license expression"),
            (lambda d: d["components"][0].pop("hashes"), "SHA-256"),
            (lambda d: d["components"][0].pop("externalReferences"), "provenance"),
        ):
            changed = copy.deepcopy(document)
            change(changed)
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                metadata.validate(changed, notices, self.binary)

    def test_project_verification_catches_internally_consistent_omission(self):
        document, notices = self.generate()
        omitted = "pkg:cargo/normal@1.2.3"
        document["components"] = [c for c in document["components"] if c["bom-ref"] != omitted]
        document["dependencies"] = [e for e in document["dependencies"] if e["ref"] != omitted]
        for edge in document["dependencies"]:
            edge["dependsOn"] = [ref for ref in edge["dependsOn"] if ref != omitted]
        for prop in document["metadata"]["component"]["properties"]:
            if prop["name"] == "retok:normal-dependencies":
                prop["value"] = json.dumps([ref for ref in json.loads(prop["value"]) if ref != omitted])
        metadata.validate(document, notices, self.binary)
        with self.assertRaisesRegex(ValueError, "Cargo dependency inventory"):
            metadata.validate_project(document, self.project)

    def test_project_verification_catches_altered_edges_and_lockfile(self):
        document, _ = self.generate()
        root = document["metadata"]["component"]["bom-ref"]
        next(e for e in document["dependencies"] if e["ref"] == root)["dependsOn"].append("pkg:cargo/unicode-ident@1.2.3")
        with self.assertRaisesRegex(ValueError, "relationships"):
            metadata.validate_project(document, self.project)
        with (self.project / "Cargo.lock").open("a") as stream:
            stream.write("\n# changed\n")
        with self.assertRaisesRegex(ValueError, "lockfile hashes"):
            metadata.validate_project(document, self.project)

    def test_cargo_command_is_offline_locked_and_target_filtered(self):
        with patch("release_metadata.subprocess.run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, json.dumps(self.cargo), "")
            result = ORIGINAL_CARGO_GRAPH(self.project, "aarch64-apple-darwin")
        self.assertEqual(result, self.cargo)
        command = run.call_args.args[0]
        self.assertEqual(command[:7], ["cargo", "metadata", "--offline", "--locked", "--format-version", "1", "--filter-platform"])
        self.assertEqual(command[7], "aarch64-apple-darwin")
        self.assertNotIn("build", command)

    def test_runtime_inventory_for_every_target_and_optional_gcc(self):
        cases = [
            ("retok-linux-x64", "x86_64-unknown-linux-gnu", ["rust-std"]),
            ("retok-linux-aarch64", "aarch64-unknown-linux-gnu", ["rust-std"]),
            ("retok-linux-x64", "x86_64-unknown-linux-musl", ["rust-std", "musl"]),
            ("retok-linux-aarch64", "aarch64-unknown-linux-musl", ["rust-std", "musl"]),
            ("retok-macos-x64", "x86_64-apple-darwin", ["rust-std"]),
            ("retok-macos-arm64", "aarch64-apple-darwin", ["rust-std"]),
            ("retok-windows-x64.exe", "x86_64-pc-windows-msvc", ["rust-std"]),
            ("retok-windows-x64.exe", "x86_64-pc-windows-gnu", ["rust-std", "mingw-w64"]),
            ("retok-windows-x64.exe", "x86_64-pc-windows-gnu", ["rust-std", "mingw-w64", "gcc-runtime"]),
        ]
        for name, target, names in cases:
            with self.subTest(target=target, names=names):
                self.binary = self.root / name
                self.binary.write_bytes(b"Synthetic binary\0" + VOCAB)
                manifest = self.write_runtime_manifest(target, names)
                doc, notices = self.generate(target)
                metadata.validate(doc, notices, self.binary)
                metadata.validate_project(doc, self.project)
                actual = {c["name"]: c for c in doc["components"] if c["bom-ref"].startswith("runtime:")}
                self.assertEqual(set(actual), set(names))
                for entry in manifest["components"]:
                    component = actual[entry["name"]]
                    self.assertEqual(component["version"], entry["version"])
                    self.assertEqual(component["hashes"][0]["content"], entry["sha256"])
                    self.assertEqual(component["licenses"], [{"expression": entry["license"]}])
                    self.assertEqual(metadata.property_map(component)["retok:source"], entry["source"])
                    self.assertIn(f"===== runtime {entry['name']} 1.2.3 License and attribution =====\n{MIT}", notices)

    def test_runtime_manifest_requires_exact_inventory_and_data(self):
        original = json.loads(self.runtime_path.read_text())
        mutations = [
            lambda m: m.update(target="aarch64-apple-darwin"),
            lambda m: m.update(components=[]),
            lambda m: m["components"].append(copy.deepcopy(m["components"][0])),
            lambda m: m["components"][0].update(name="OS-dynamic-library"),
            lambda m: m["components"][0].update(version="latest"),
            lambda m: m["components"][0].update(sha256="incorrect"),
            lambda m: m["components"][0].update(notices=[]),
            lambda m: m["components"][0]["notices"][0].update(label=""),
        ] + [lambda m, field=field: m["components"][0].pop(field)
             for field in ("version", "source", "license", "sha256")]
        for change in mutations:
            candidate = copy.deepcopy(original)
            change(candidate)
            self.runtime_path.write_text(json.dumps(candidate))
            with self.subTest(manifest=candidate), self.assertRaises(ValueError):
                self.generate()
        for target, names in (
            ("x86_64-unknown-linux-musl", ["rust-std"]),
            ("x86_64-pc-windows-gnu", ["rust-std", "gcc-runtime"]),
            ("x86_64-apple-darwin", ["rust-std", "musl"]),
            ("x86_64-apple-darwin", ["rust-std", "gcc-runtime"]),
        ):
            self.write_runtime_manifest(target, names)
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, "runtime component inventory"):
                metadata.load_runtimes(self.runtime_path, target)

    def test_canonical_manifest_omits_paths_and_preserves_multiple_complete_notices(self):
        text = "Synthetic additional attribution ©\r\nFull second notice without final newline"
        private_path = self.root / "local-only-location.txt"
        private_path.write_bytes(text.encode("utf-8"))
        manifest = json.loads(self.runtime_path.read_text())
        manifest["components"][0]["notices"].append({"label": "Additional notice", "path": private_path.name})
        self.runtime_path.write_text(json.dumps(manifest))
        doc, notices = self.generate()
        props = metadata.property_map(doc["metadata"]["component"])
        canonical = props["retok:runtime-manifest"]
        self.assertEqual(hashlib.sha256(canonical.encode()).hexdigest(), props["retok:runtime-manifest-sha256"])
        self.assertNotIn("path", canonical)
        self.assertNotIn(str(self.root), json.dumps(doc) + notices)
        self.assertNotIn(private_path.name, json.dumps(doc) + notices)
        self.assertIn(text, notices)
        self.assertEqual(json.loads(canonical)["components"][0]["notices"], [
            {"label": "Additional notice", "sha256": hashlib.sha256(text.encode()).hexdigest()},
            {"label": "License and attribution", "sha256": hashlib.sha256(self.supplement.read_bytes()).hexdigest()},
        ])
        # Reordering input JSON and moving a license file must not change public output.
        manifest["components"][0]["notices"].reverse()
        renamed = self.root / "renamed-private-input.txt"
        renamed.write_bytes(private_path.read_bytes())
        next(f for f in manifest["components"][0]["notices"] if f["label"] == "Additional notice")["path"] = str(renamed)
        self.runtime_path.write_text(json.dumps(manifest, indent=4, sort_keys=True))
        self.assertEqual((doc, notices), self.generate())
        # File contents, rather than local locations, change the canonical binding.
        renamed.write_bytes(b"Changed full synthetic notice")
        changed, _ = self.generate()
        self.assertNotEqual(props["retok:runtime-manifest-sha256"],
                            metadata.property_map(changed["metadata"]["component"])["retok:runtime-manifest-sha256"])

    def test_missing_empty_or_unreadable_runtime_notice_files_fail(self):
        manifest = json.loads(self.runtime_path.read_text())
        path = self.root / "runtime-only-license"
        manifest["components"][0]["notices"][0]["path"] = str(path)
        self.runtime_path.write_text(json.dumps(manifest))
        with self.assertRaises(FileNotFoundError):
            self.generate()
        path.write_bytes(b" \r\n")
        with self.assertRaisesRegex(ValueError, "missing license text"):
            self.generate()
        path.write_bytes(b"\xff")
        with self.assertRaises(UnicodeDecodeError):
            self.generate()

    def test_internal_runtime_bindings_edges_and_full_notices_are_checked(self):
        document, notices = self.generate()
        ref = next(c["bom-ref"] for c in document["components"] if c["name"] == "rust-std")
        for field, value in (("version", "99.0.0"), ("licenses", [{"expression": "Apache-2.0"}]),
                             ("hashes", [{"alg": "SHA-256", "content": "0" * 64}])):
            changed = copy.deepcopy(document)
            next(c for c in changed["components"] if c["bom-ref"] == ref)[field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "runtime component"):
                metadata.validate(changed, notices, self.binary)
        changed = copy.deepcopy(document)
        root = changed["metadata"]["component"]
        for prop in root["properties"]:
            if prop["name"] == "retok:runtime-manifest-sha256":
                prop["value"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "runtime manifest SHA256"):
            metadata.validate(changed, notices, self.binary)
        changed = copy.deepcopy(document)
        next(e for e in changed["dependencies"] if e["ref"] == root["bom-ref"])["dependsOn"].remove(ref)
        with self.assertRaisesRegex(ValueError, "runtime dependency edges"):
            metadata.validate(changed, notices, self.binary)
        # Even a new overall notice hash cannot conceal a removed runtime license paragraph.
        marker = "===== runtime rust-std 1.2.3 License and attribution =====\n"
        prefix, runtime_notice = notices.split(marker)
        altered = prefix + marker + runtime_notice.replace("Permission is hereby granted", "Removed paragraph")
        changed = copy.deepcopy(document)
        for prop in changed["metadata"]["component"]["properties"]:
            if prop["name"] == "retok:notices-sha256":
                prop["value"] = hashlib.sha256(altered.encode()).hexdigest()
        with self.assertRaisesRegex(ValueError, "runtime notice SHA256"):
            metadata.validate(changed, altered, self.binary)

    def test_regex_effective_license_and_cargo_declaration_are_both_checked(self):
        document, notices = self.generate()
        regex = next(c for c in document["components"] if c["name"] == "regex-syntax")
        self.assertEqual(regex["licenses"], [{"expression": "(MIT OR Apache-2.0) AND Unicode-DFS-2016"}])
        self.assertEqual(metadata.property_map(regex)["retok:cargo-license-expression"], "MIT OR Apache-2.0")
        self.assertIn("License expression: MIT OR Apache-2.0", notices)
        self.assertIn("Synthetic regex-syntax Unicode data terms and attribution.", notices)
        regex["licenses"] = [{"expression": "MIT OR Apache-2.0"}]
        with self.assertRaisesRegex(ValueError, "effective license"):
            metadata.validate(document, notices, self.binary)
        with self.assertRaisesRegex(ValueError, "license mismatch"):
            metadata.validate_project(document, self.project)
        regex["licenses"] = [{"expression": "(MIT OR Apache-2.0) AND Unicode-DFS-2016"}]
        for prop in regex["properties"]:
            if prop["name"] == "retok:cargo-license-expression":
                prop["value"] = "MIT"
        with self.assertRaisesRegex(ValueError, "license mismatch"):
            metadata.validate_project(document, self.project)


ORIGINAL_CARGO_GRAPH = metadata.cargo_graph


if __name__ == "__main__":
    unittest.main()
