#!/usr/bin/env python3
"""Synthetic execution benchmark. Python standard library; see README.md."""
import argparse
import csv
from dataclasses import dataclass
import hashlib
import html
import json
import os
from pathlib import Path
import platform
import select
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import unittest


def digest(data):
    return hashlib.sha256(data).hexdigest()


def save_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def execute(argv, cwd, env, data=None):
    start = time.perf_counter_ns()
    result = subprocess.run(argv, cwd=cwd, env=env, input=data,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
    return result, (time.perf_counter_ns() - start) / 1e6


def isolated_env(root):
    env = {"PATH": os.defpath, "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
           "TZ": "UTC", "TERM": "dumb", "NO_COLOR": "1", "GIT_PAGER": "cat",
           "PAGER": "cat", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
           "GIT_OPTIONAL_LOCKS": "0", "GIT_TERMINAL_PROMPT": "0",
           "GIT_AUTHOR_NAME": "Synthetic Author", "GIT_AUTHOR_EMAIL": "author@example.invalid",
           "GIT_COMMITTER_NAME": "Synthetic Author", "GIT_COMMITTER_EMAIL": "author@example.invalid",
           "GIT_AUTHOR_DATE": "2025-01-01T12:00:00Z", "GIT_COMMITTER_DATE": "2025-01-01T12:00:00Z"}
    for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME",
                "RETOK_CONFIG_DIR", "RETOK_STATE_DIR", "TMPDIR"):
        directory = root / key.lower()
        directory.mkdir(parents=True)
        env[key] = str(directory)
    return env


def fixture(root, env):
    repo = root / "repo"
    repo.mkdir()
    def git(*args):
        result, _ = execute(["git", *args], repo, env)
        if result.returncode:
            raise RuntimeError(result.stderr.decode())
    git("init", "-q", "--initial-branch=main", "--template=")
    git("config", "core.hooksPath", os.devnull)
    (repo / "src").mkdir()
    for i in range(18):
        (repo / "src" / f"module_{i:02}.txt").write_text(
            f"Module {i}\n" + "\n".join(f"setting_{j}=value_{i * 12 + j}" for j in range(12)) + "\n")
    (repo / "records.json").write_text(json.dumps([
        {"id": i, "name": f"item-{i:02}", "enabled": i % 3 != 0,
         "region": ["east", "west", "north"][i % 3], "count": i * 7}
        for i in range(24)], indent=2) + "\n")
    (repo / "service.log").write_text("".join(
        f"2025-01-01T12:00:{i:02}Z {'WARN' if i % 7 == 0 else 'INFO'} request={i} status={503 if i % 7 == 0 else 200}\n"
        for i in range(40)))
    git("add", ".")
    git("-c", "commit.gpgsign=false", "commit", "-qm", "Create synthetic application")
    for i in range(6):
        (repo / "CHANGELOG.txt").write_text(f"Revision {i}: adjust validation for item {i}\n")
        git("add", "CHANGELOG.txt")
        git("-c", "commit.gpgsign=false", "commit", "-qm", f"Adjust validation for item {i}")
    for i in (1, 4, 7, 10):
        path = repo / "src" / f"module_{i:02}.txt"
        path.write_text(path.read_text().replace("setting_3=", "revised_setting_3="))
    git("add", "src/module_01.txt")
    (repo / "src/module_17.txt").unlink()
    (repo / "notes.txt").write_text("Investigate the validation warning.\n")
    programs = root / "programs"
    programs.mkdir()
    helper = programs / "fixture-check"
    helper.write_text(f"#!{sys.executable}\n" + '''import sys, time
if sys.argv[1] == "progress":
    for i in range(8):
        print(f"processed batch {i + 1}/8", flush=True)
        time.sleep(0.06)
else:
    for i in range(12):
        print(f"test validation_{i:02} ... " + ("FAILED" if i == 7 else "ok"))
    print("FAIL validation_07: expected status 200, received 503", file=sys.stderr)
    print("tests: 11 passed; 1 failed")
    sys.exit(1)
''')
    helper.chmod(0o700)
    env["PATH"] = str(programs) + os.pathsep + env["PATH"]
    # ls output should not depend on when the benchmark happened to start.
    for path in repo.rglob("*"):
        if ".git" not in path.relative_to(repo).parts:
            os.utime(path, (1735732800, 1735732800))
    # Settle the stat cache after normalizing timestamps, before measuring effects.
    # Exit 1 is expected for the deliberately modified/deleted fixture files.
    refreshed, _ = execute(["git", "update-index", "--refresh"], repo, env)
    if refreshed.returncode not in (0, 1):
        raise RuntimeError("fixture index refresh failed")
    return repo


# Fixed before measuring: modest source tree, records, logs, one failing test report.
WORKLOADS = [
    ("git-status", ["git", "status"], ["git", "status"], ["module_01.txt", "module_17.txt", "notes.txt"]),
    ("git-diff", ["git", "diff"], ["git", "diff"], ["revised_setting_3", "module_17.txt"]),
    ("git-log", ["git", "log", "-6"], ["git", "log", "-6"], ["Synthetic Author", "Adjust validation for item 5"]),
    ("listing", ["ls", "-l", "src"], ["ls", "-l", "src"], ["module_00.txt", "module_16.txt"]),
    ("search", ["grep", "-n", "WARN", "service.log"], ["grep", "-n", "WARN", "service.log"], ["status=503", "request=35"]),
    ("json", ["cat", "records.json"], ["json", "records.json"], ["item-00", "item-23", "north"]),
    ("tests-diagnostics", ["fixture-check", "tests"], ["test", "fixture-check", "tests"], ["validation_07", "received 503", "11 passed"]),
    ("control-noop", ["true"], ["proxy", "true"], []),
    ("control-short", ["printf", "ready\\n"], ["proxy", "printf", "ready\\n"], ["ready"]),
    ("control-progress", ["fixture-check", "progress"], ["proxy", "fixture-check", "progress"], ["processed batch 8/8"]),
]


def tree_hash(repo):
    return digest(json.dumps([(str(p.relative_to(repo)), digest(p.read_bytes()))
                              for p in sorted(repo.rglob("*")) if p.is_file()]).encode())


def audit_shims(root, env):
    """Observe PATH-launched children separately; never include shim cost in timings."""
    directory = root / "audit-bin"
    directory.mkdir()
    commands = {w[1][0] for w in WORKLOADS}
    for name in commands:
        real = shutil.which(name, path=env["PATH"])
        if not real:
            raise RuntimeError(f"missing native command: {name}")
        shim = directory / name
        shim.write_text(f"#!{sys.executable}\n" +
                        "import json, os, sys\n" +
                        "with open(os.environ['BENCH_AUDIT'], 'a') as f:\n" +
                        f"    f.write(json.dumps({{'program': {name!r}, 'args': sys.argv[1:], 'cwd': os.getcwd()}}) + '\\n')\n" +
                        f"os.execv({real!r}, [{real!r}, *sys.argv[1:]])\n")
        shim.chmod(0o700)
    return directory


def audit(argv, native, repo, env, shims, log):
    log.write_text("")
    before = tree_hash(repo)
    result, _ = execute(argv, repo, {**env, "PATH": str(shims) + os.pathsep + env["PATH"],
                                   "BENCH_AUDIT": str(log)})
    events = [json.loads(line) for line in log.read_text().splitlines()]
    exact = events == [{"program": native[0], "args": native[1:], "cwd": str(repo)}]
    unchanged = before == tree_hash(repo)
    # Absence of a PATH event is not proof of no execution (internal/absolute calls).
    return {"path_child_events": events, "exactly_one_native_argv_cwd_observed": exact,
            "fixture_content_unchanged": unchanged, "exit_code": result.returncode}, result


class Protocol:
    def __init__(self, binary, cwd, env):
        self.process = subprocess.Popen([binary, "compact", "--protocol=json-v1"], cwd=cwd,
                                        env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL)
        self.ask("")  # warm tokenizer before any measured protocol processing

    def ask(self, text):
        self.process.stdin.write((json.dumps({"version": 1, "text": text,
                                             "tokenizer": "o200k_base"}) + "\n").encode())
        self.process.stdin.flush()
        if not select.select([self.process.stdout], [], [], 30)[0]:
            raise RuntimeError("tokenizer protocol timed out")
        response = json.loads(self.process.stdout.readline())
        if "error" in response:
            raise RuntimeError(response["error"])
        return response

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process.stdout.close()


def classify(audit_result, native_exit):
    if not audit_result["fixture_content_unchanged"]:
        return "incompatible: fixture changed"
    if audit_result["exit_code"] != native_exit:
        return "incompatible: exit differs"
    if not audit_result["exactly_one_native_argv_cwd_observed"]:
        return "different implementation/argv or unverified child execution"
    return "same native argv/cwd once; exit and fixture contents preserved"


@dataclass(frozen=True)
class Number:
    lexeme: str


def json_values(data):
    """Compare JSON values/types and numeric lexemes, allowing object key order."""
    return json.loads(data, parse_int=Number, parse_float=Number,
                      object_pairs_hook=lambda pairs: ("object", sorted(pairs, key=lambda p: p[0])))


def chart(path, rows, metric, caption):
    maximum = max(r[metric] for r in rows) or 1
    caption = html.escape(caption)
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="{65 + 23 * len(rows)}" role="img" aria-label="{caption}">',
             '<rect width="100%" height="100%" fill="white"/>',
             f'<text x="10" y="22" font-family="sans-serif">{caption}</text>']
    for i, row in enumerate(rows):
        y = 45 + i * 23
        color = {"native": "#555", "retok": "#1565c0", "rtk": "#bd5d00"}[row["arm"]]
        label = html.escape(row["workload"] + " / " + row["arm"])
        parts.extend([f'<text x="10" y="{y + 12}" font-size="12" font-family="sans-serif">{label}</text>',
                      f'<rect x="240" y="{y}" height="15" width="{600 * row[metric] / maximum:.2f}" fill="{color}"/>',
                      f'<text x="{245 + 600 * row[metric] / maximum:.2f}" y="{y + 12}" font-size="12">{row[metric]:.2f}</text>'])
    path.write_text("\n".join(parts) + "\n</svg>\n")


def benchmark(args):
    retok, rtk = [str(Path(p).resolve(strict=True)) for p in (args.retok, args.rtk)]
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = isolated_env(output / "isolated")
    repo = fixture(output, env)
    binaries = {}
    for name, binary in (("retok", retok), ("rtk", rtk), ("git", shutil.which("git", path=env["PATH"])),
                         ("python", sys.executable)):
        result, _ = execute([binary, "--version"], repo, env)
        if result.returncode:
            raise RuntimeError(f"{name} version probe failed")
        binaries[name] = {"local_path": binary, "sha256": digest(Path(binary).read_bytes()),
                          "version": result.stdout.decode().strip()}
    if binaries["rtk"]["version"] != "rtk 0.49.0":
        raise RuntimeError("This interface matrix is pinned to rtk 0.49.0")
    for name in ("ls", "grep", "cat", "true", "printf", "fixture-check"):
        path = Path(shutil.which(name, path=env["PATH"]))
        binaries[name] = {"local_path": str(path), "sha256": digest(path.read_bytes())}
    metadata = {"schema": 1, "binaries": binaries, "harness_sha256": digest(Path(__file__).read_bytes()),
                "platform": platform.platform(), "repeats": args.repeats,
                "tokenizer": "ordinary o200k_base, same warmed Retok protocol input_tokens for ALL arms",
                "fixture_sha256": tree_hash(repo), "raw_samples_saved": args.save_raw,
                "timing": "one unmeasured warmup per arm; rotating order; subprocess wall time; no audit shims",
                "protocol_timing": "warmed compact request round trip, excludes startup, includes JSON/pipe overhead",
                "memory": "not measured", "rows": []}
    save_json(output / "metadata.json", {k: v for k, v in metadata.items() if k != "rows"})
    shims = audit_shims(output, env)
    protocol = Protocol(retok, repo, env)
    rows = metadata["rows"]
    try:
        for workload, native, rtk_args, markers in WORKLOADS:
            commands = {"native": native, "retok": [retok, "run", "--", *native], "rtk": [rtk, *rtk_args]}
            native_audit, original = audit(native, native, repo, env, shims, output / "audit.jsonl")
            if not native_audit["exactly_one_native_argv_cwd_observed"] or not native_audit["fixture_content_unchanged"]:
                raise RuntimeError("native audit failed")
            candidates = []
            for raw in (original.stdout, original.stderr):
                candidate = protocol.ask(raw.decode("utf-8"))
                restored, _ = execute([retok, "restore", "--encoding", candidate["encoding"]], repo, env,
                                      candidate["text"].encode())
                restore_exact = restored.stdout == raw
                is_json = candidate["encoding"] in ("json-v1", "json-rows-v1")
                contract_verified = (json_values(restored.stdout) == json_values(raw)) if is_json else restore_exact
                if restored.returncode or not contract_verified:
                    raise RuntimeError(f"explicit encoding restore failed: {workload}")
                times = []
                for _ in range(args.repeats):
                    start = time.perf_counter_ns()
                    protocol.ask(raw.decode("utf-8"))
                    times.append((time.perf_counter_ns() - start) / 1e6)
                candidates.append({**candidate, "restore_exact": restore_exact,
                                   "restore_contract_verified": contract_verified,
                                   "restore_contract": "JSON values/types/numeric lexemes" if is_json else "exact bytes",
                                   "processing_ms": times})
            checks = {}
            for arm, argv in commands.items():
                observed, _ = audit(argv, native, repo, env, shims, output / "audit.jsonl")
                checks[arm] = observed
                execute(argv, repo, env)  # unmeasured warmup
            samples = {arm: [] for arm in commands}
            for repeat in range(args.repeats):
                order = list(commands)
                order = order[repeat % 3:] + order[:repeat % 3]
                for arm in order:
                    before = tree_hash(repo)
                    result, elapsed = execute(commands[arm], repo, env)
                    if tree_hash(repo) != before:
                        raise RuntimeError(f"timed fixture mutation: {workload}/{arm}")
                    streams = []
                    for stream, data, raw, candidate in zip(("stdout", "stderr"),
                            (result.stdout, result.stderr), (original.stdout, original.stderr), candidates):
                        text = data.decode("utf-8")
                        selection = "raw" if data == raw else ("candidate" if data == candidate["text"].encode() else "other")
                        streams.append({"stream": stream, "bytes": len(data), "sha256": digest(data),
                                        "tokens": protocol.ask(text)["input_tokens"],
                                        "matches_saved_original_or_candidate": selection})
                        if args.save_raw:
                            (output / f"{workload}-{arm}-{repeat}-{stream}.bin").write_bytes(data)
                    text = (result.stdout + result.stderr).decode("utf-8")
                    samples[arm].append({"elapsed_ms": elapsed, "exit_code": result.returncode,
                                         "streams": streams,
                                         "markers_present": [m for m in markers if m in text],
                                         "markers_absent": [m for m in markers if m not in text]})
            for arm in commands:
                arm_samples = samples[arm]
                elapsed = [s["elapsed_ms"] for s in arm_samples]
                tokens = [sum(s["tokens"] for s in sample["streams"]) for sample in arm_samples]
                status = classify(checks[arm], original.returncode)
                if any(s["exit_code"] != original.returncode for s in arm_samples):
                    status = "incompatible: timed exit differs"
                exact = all(s["matches_saved_original_or_candidate"] != "other"
                            for sample in arm_samples for s in sample["streams"])
                if arm == "retok" and not exact:
                    status = "incompatible: emitted output differs from original and explicit candidate"
                row = {"workload": workload, "arm": arm, "argv": commands[arm], "execution": status,
                       "audit": checks[arm], "median_elapsed_ms": statistics.median(elapsed),
                       "min_elapsed_ms": min(elapsed), "max_elapsed_ms": max(elapsed),
                       "median_tokens": statistics.median(tokens), "tokens_stable": len(set(tokens)) == 1,
                       "retok_output_verified": exact if arm == "retok" else None,
                       "samples": arm_samples}
                if arm == "retok":
                    row["saved_original_protocol"] = candidates
                    row["median_warm_processing_ms"] = sum(statistics.median(c["processing_ms"]) for c in candidates)
                rows.append(row)
            save_json(output / "results.json", metadata)
            print(workload, "complete", flush=True)
    finally:
        protocol.close()
    for name, binary in binaries.items():
        if digest(Path(binary["local_path"]).read_bytes()) != binary["sha256"]:
            raise RuntimeError(f"binary changed during benchmark: {name}")
    metadata["binary_hashes_unchanged_after_run"] = True
    save_json(output / "results.json", metadata)
    fields = ["workload", "arm", "execution", "median_elapsed_ms", "min_elapsed_ms", "max_elapsed_ms",
              "median_tokens", "tokens_stable", "retok_output_verified", "median_warm_processing_ms"]
    with (output / "results.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)
    chart(output / "elapsed.svg", rows, "median_elapsed_ms",
          "End-to-end milliseconds (includes command and startup; lower is faster)")
    chart(output / "token.svg", rows, "median_tokens",
          "Median ordinary o200k_base tokens (stdout + stderr; fewer tokens does not establish retention)")
    return 1 if any(r["arm"] in ("native", "retok") and not r["execution"].startswith("same native") for r in rows) else 0


class HarnessChecks(unittest.TestCase):
    def test_json_restoration_contract(self):
        self.assertEqual(json_values('{"b": [true, null], "a": 1.00}'),
                         json_values('{"a":1.00,"b":[true,null]}'))
        self.assertNotEqual(json_values('{"a":1.00}'), json_values('{"a":1.0}'))
        self.assertNotEqual(json_values('{"a":true}'), json_values('{"a":1}'))

    def test_isolation_fixture_audit_and_classification(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            env = isolated_env(root / "isolated")
            self.assertNotIn("SSH_AUTH_SOCK", env)
            repo = fixture(root, env)
            shims = audit_shims(root, env)
            checked, result = audit(["git", "status"], ["git", "status"], repo, env, shims, root / "audit")
            self.assertTrue(classify(checked, 0).startswith("same native"))
            self.assertIn(b"notes.txt", result.stdout)
            diff_audit, _ = audit(["git", "diff"], ["git", "diff"], repo, env, shims, root / "audit")
            self.assertTrue(diff_audit["fixture_content_unchanged"])
            self.assertTrue(classify(checked, 1).startswith("incompatible"))
            checked["exactly_one_native_argv_cwd_observed"] = False
            self.assertIn("unverified", classify(checked, 0))
            result, _ = execute(["fixture-check", "tests"], repo, env)
            self.assertEqual(result.returncode, 1)
            self.assertIn(b"received 503", result.stderr)
            with self.assertRaises(FileExistsError):
                benchmark(argparse.Namespace(retok=__file__, rtk=__file__, output=str(root)))

    def test_duplicate_execution_and_mutation_are_not_compatible(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            env = isolated_env(root / "isolated")
            repo = fixture(root, env)
            shims = audit_shims(root, env)
            checked, _ = audit(["sh", "-c", "git status; git status"], ["git", "status"],
                               repo, env, shims, root / "audit")
            self.assertEqual(len(checked["path_child_events"]), 2)
            self.assertFalse(checked["exactly_one_native_argv_cwd_observed"])
            checked, _ = audit(["sh", "-c", "printf changed > notes.txt"], ["printf", "changed"],
                               repo, env, shims, root / "audit")
            self.assertEqual(classify(checked, 0), "incompatible: fixture changed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--retok", help="final candidate binary path")
    parser.add_argument("--rtk", help="pinned native RTK 0.49.0 binary path")
    parser.add_argument("--output", help="new local artifact directory; must not exist")
    parser.add_argument("--repeats", type=int, choices=(5, 7), default=5)
    parser.add_argument("--save-raw", action="store_true")
    parser.add_argument("--self-test", action="store_true", help="test harness only, no Retok/RTK benchmark")
    args = parser.parse_args()
    if args.self_test:
        return not unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(HarnessChecks)).wasSuccessful()
    if not all((args.retok, args.rtk, args.output)):
        parser.error("--retok, --rtk and --output are required")
    return benchmark(args)


if __name__ == "__main__":
    sys.exit(main())
