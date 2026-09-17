"""Completion adapter subprocess fixtures; not a full Hermes session.

Run: python3 -B tests/test_hermes.py
"""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / "integrations/hermes/__init__.py"
NATIVE = (' {"output":' + json.dumps("checkpoint α 🦄\r\n" * 100, ensure_ascii=False)
          + ',"exit_code":17,"error":null,"approval":"approved",'
          '"hint":"failure diagnostic","big":18446744073709551616001,'
          '"decimal":-0.00100E+99999,"nested":{"$serde_json::private::Number":"42"}} ')
REPLACEMENT = NATIVE.replace(json.dumps("checkpoint α 🦄\r\n" * 100, ensure_ascii=False),
                             json.dumps("checkpoint α 🦄", ensure_ascii=False))


class HermesAdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="retok hermes ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.executable = self.root / "retok ' 🦄.py"
        self.receipt = self.root / "argv.json"
        self.request = self.root / "request.bin"
        self.module = types.ModuleType("retok_hermes_fixture")
        source = SOURCE.read_text().replace(
            "__RETOK_EXECUTABLE_UTF8_HEX__", str(self.executable).encode("utf-8").hex())
        exec(compile(source, str(SOURCE), "exec"), self.module.__dict__)
        if os.name == "nt":
            # Windows cannot directly execute a shebang fixture. Only substitute
            # its launcher; exercise the real Windows pipes/timer/process APIs.
            popen = subprocess.Popen
            def launch(argv, **kwargs):
                self.assertEqual(argv, [str(self.executable), "hook", "hermes"])
                if not self.executable.exists():
                    raise FileNotFoundError(str(self.executable))
                return popen([sys.executable, *argv], **kwargs)
            launcher = patch.object(self.module.subprocess, "Popen", launch)
            launcher.start()
            self.addCleanup(launcher.stop)

    def binary(self, body=None, before_read=""):
        if body is None:
            body = f"print(json.dumps({{'result': {REPLACEMENT!r}}}))"
        self.executable.write_text(
            f"#!{sys.executable}\nimport json,sys,time\nfrom pathlib import Path\n"
            + before_read + "\n"
            + f"Path({str(self.receipt)!r}).write_text(json.dumps(sys.argv[1:]))\n"
            + f"Path({str(self.request)!r}).write_bytes(sys.stdin.buffer.read())\n"
            + body + "\n", encoding="utf-8")
        self.executable.chmod(0o700)

    def call(self, result=NATIVE, args=None, name="terminal", **metadata):
        before_args, before_metadata = copy.deepcopy(args), copy.deepcopy(metadata)
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            replacement = self.module._transform_tool_result(name, args, result, **metadata)
        self.assertEqual(args, before_args)
        self.assertEqual(metadata, before_metadata)
        self.assertEqual((out.getvalue(), err.getvalue()), ("", ""))
        return replacement

    def test_success_exact_native_bytes_failed_status_and_unchanged_args(self):
        self.binary()
        marker = self.root / "must not exist"
        args = {"command": f"git status; touch '{marker}'; $(echo nope)",
                "timeout": 12, "workdir": "/remote space", "pty": True,
                "background": True, "extra": {"x": 1}}
        # Backend and PTY/background flags are irrelevant to completed text.
        for backend in ("local", "docker", "ssh", "unknown"):
            with patch.dict(os.environ, {"TERMINAL_ENV": backend}):
                self.assertEqual(self.call(args=args, status="error", error_type="fixture",
                                           session_id="fixture-call"), REPLACEMENT)
        raw = self.request.read_bytes()
        self.assertTrue(raw.endswith(b',"tool_response":' + NATIVE.encode("utf-8") + b'}'))
        request = json.loads(raw, parse_float=str, parse_int=str)
        self.assertEqual(request["hook_event_name"], "TransformToolResult")
        self.assertEqual(request["tool_name"], "terminal")
        self.assertEqual(request["cwd"], os.getcwd())
        self.assertEqual(request["tool_input"]["command"], args["command"])
        self.assertEqual(json.loads(self.receipt.read_text()), ["hook", "hermes"])
        self.assertFalse(marker.exists())
        self.assertEqual(json.loads(REPLACEMENT, parse_float=str)["exit_code"], 17)

    def test_disabled_registration_and_manifest(self):
        hooks = []
        for enabled in (False, True):
            ctx = types.SimpleNamespace(get_config=lambda key, default: enabled,
                                        register_hook=lambda *args: hooks.append(args))
            self.module.register(ctx)
        self.assertEqual(hooks, [("transform_tool_result", self.module._transform_tool_result)])
        manifest = SOURCE.with_name("plugin.yaml").read_text()
        self.assertIn("name: retok-rewrite", manifest)
        self.assertEqual(manifest.count("  - transform_tool_result"), 2)
        self.assertNotIn("pre_tool_call", manifest)
        self.assertNotIn("transform_terminal_output", manifest)

    def test_unsupported_inputs_and_request_bounds_never_spawn(self):
        self.binary()
        for result in (None, {}, [], b"{}", 42, "", "{}", "x" * 300,
                       "[" + " " * 300 + "]", '{"output":"' + "\ud800" * 300 + '"}'):
            with self.subTest(result_type=type(result)):
                self.assertIsNone(self.call(result))
        self.assertIsNone(self.call(name="read_file"))
        self.module.MAX_INPUT_BYTES = 1024
        self.assertIsNone(self.call())
        self.assertIsNone(self.call('{"output":"' + "🦄" * 300 + '"}'))
        self.assertIsNone(self.call('{"output":"' + "x" * 300 + '"}', args={"x": "y" * 1024}))
        self.assertFalse(self.receipt.exists())

    def test_invalid_responses_exit_status_and_stdout_bound_fail_open(self):
        self.module.MAX_OUTPUT_BYTES = 4096
        bodies = ["print('not-json')", "print('null')", "print('[]')", "print('{}')",
                  "print(json.dumps({'result': 42}))", "print(json.dumps({'result': ''}))",
                  "print(json.dumps({'result': ' '}))", "print(json.dumps({'result': '\\ud800'}))",
                  f"print(json.dumps({{'result': {NATIVE!r}}}))",
                  "sys.stdout.buffer.write(b'\\xff')", "print('x' * 4097)"]
        bodies += [f"print(json.dumps({{'result': {REPLACEMENT!r}}})); sys.exit({status})"
                   for status in (1, 2, 3, 7)]
        for body in bodies:
            with self.subTest(body=body):
                self.binary(body)
                self.assertIsNone(self.call(args={"command": "original", "timeout": 9}))
        # A child writing before reading stdin must not deadlock either pipe.
        self.binary(before_read="sys.stdout.buffer.write(b'x' * 100000); sys.stdout.flush()")
        self.assertIsNone(self.call('{"output":"' + "x" * 200000 + '"}'))

    def test_timeout_missing_binary_and_recovery(self):
        self.assertIsNone(self.call())
        self.module.TIMEOUT_SECONDS = 0.2
        self.binary(before_read="time.sleep(30)")
        start = time.monotonic()
        # Exceeds a pipe buffer: the deadline must also bound blocked stdin.
        self.assertIsNone(self.call('{"output":"' + "x" * 200000 + '"}'))
        self.assertLess(time.monotonic() - start, 3)
        self.binary("time.sleep(30)")
        start = time.monotonic()
        self.assertIsNone(self.call())
        self.assertLess(time.monotonic() - start, 3)
        self.module.TIMEOUT_SECONDS = 2
        self.binary()
        self.assertEqual(self.call(), REPLACEMENT)


if __name__ == "__main__":
    unittest.main()
