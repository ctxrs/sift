"""Compress completed Hermes terminal results; the CLI owns output decisions."""

import json
import os
import subprocess
import threading

RETOK_EXECUTABLE = bytes.fromhex('__RETOK_EXECUTABLE_UTF8_HEX__').decode('utf-8')
TIMEOUT_SECONDS = 2
MAX_INPUT_BYTES = 16 * 1024 * 1024
MAX_OUTPUT_BYTES = 32 * 1024 * 1024
MIN_RESULT_CHARS = 256


def register(ctx):
    if ctx.get_config("enabled", True) is not False:
        ctx.register_hook("transform_tool_result", _transform_tool_result)


def _transform_tool_result(tool_name=None, args=None, result=None, **_metadata):
    try:
        if tool_name != "terminal" or not isinstance(result, str):
            return
        if not MIN_RESULT_CHARS <= len(result) <= MAX_INPUT_BYTES:
            return
        if not result.lstrip().startswith("{") or not result.rstrip().endswith("}"):
            return
        metadata = {"hook_event_name": "TransformToolResult", "tool_name": "terminal"}
        if isinstance(args, dict):
            metadata["tool_input"] = args
        # This is the host's cwd, never a remote/container terminal workdir.
        metadata["cwd"] = os.getcwd()
        # Preserve the native JSON verbatim, including large number lexemes.
        # The CLI validates the complete envelope and owns native-result edits.
        request = (json.dumps(metadata, ensure_ascii=False, allow_nan=False)[:-1]
                   + ',"tool_response":' + result + '}').encode("utf-8")
        if len(request) > MAX_INPUT_BYTES:
            return
        output = _complete(request)
        if output is None:
            return
        response = json.loads(output.decode("utf-8"))
        replacement = response.get("result") if isinstance(response, dict) else None
        if isinstance(replacement, str) and replacement.strip() and replacement != result:
            replacement.encode("utf-8")
            return replacement
    except Exception:
        # Fail open without diagnostics containing tool output or host metadata.
        return


def _complete(request):
    failed = threading.Event()
    with subprocess.Popen(
        [RETOK_EXECUTABLE, "hook", "hermes"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        shell=False,
    ) as child:
        def expire():
            failed.set()
            try:
                child.kill()
            except OSError:
                pass

        def write_input():
            try:
                with child.stdin:
                    child.stdin.write(request)
            except OSError:
                failed.set()

        timer = threading.Timer(TIMEOUT_SECONDS, expire)
        timer.daemon = True
        writer = threading.Thread(target=write_input, daemon=True)
        timer.start()
        writer.start()
        try:
            output = child.stdout.read(MAX_OUTPUT_BYTES + 1)
            if len(output) > MAX_OUTPUT_BYTES:
                expire()
                return
            status = child.wait()
        finally:
            if child.poll() is None:
                child.kill()
            child.wait()
            writer.join()
            timer.cancel()
            timer.join()
        if status == 0 and not failed.is_set():
            return output
