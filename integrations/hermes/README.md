# Hermes completion plugin

The native `transform_tool_result` callback compresses completed `terminal`
output through the installed Retok executable. Hermes executes the original
command with its original arguments and approval policy. This plugin registers
no pre-execution hook. The CLI edits only the native result's `output` text;
exit status, error, approval, hints, and other metadata remain intact.

**Supported Hermes floor: v2026.9.14.** This conservative baseline was checked
against the released native hook and dispatcher; it is not a claim about when
the hook first appeared. The manifest declares `requires_hermes: ">=2026.9.14"`.
Hermes must expose and invoke `transform_tool_result` with the final native
result string and accept a returned string replacement. In the qualified
release, this hook runs after `post_tool_call`, before the result enters context.
The earlier `transform_terminal_output` hook precedes truncation, secret
redaction, and status annotations and is unsuitable for this adapter.

The released [manifest implementation](https://github.com/NousResearch/hermes-agent/blob/v2026.9.14/hermes_cli/plugins_manifest.py#L342)
parses `requires_hermes` and compares numeric version components. Its native gate
rejects `2026.9.13` and accepts `2026.9.14`; it ignores prerelease suffixes and
permits unparseable development versions. Custom builds still need the actual
final-result capability. `manifest_version` and `api_version` describe separate
schema/API generations.

Before replacing an active RTK integration, use Hermes v2026.9.14 or newer and
verify that this plugin loads with the final-result hook. Preserve RTK activation
until the replacement meets that prerequisite. Qualification used released
native modules with explicit host-service shims, not a full authenticated Hermes
session, and does not certify every later release or signed distribution.

This directory is an installation template. Replace
`__RETOK_EXECUTABLE_UTF8_HEX__` in `__init__.py` with the hex encoding of the
absolute Retok executable path's UTF-8 bytes. Copy `__init__.py` and `plugin.yaml`
into `$HERMES_HOME/plugins/retok-rewrite/` (`~/.hermes/plugins/retok-rewrite/` by
default). The existing plugin ID is retained for setup compatibility. Add it to
the existing `plugins.enabled` list in `$HERMES_HOME/config.yaml`:

```yaml
plugins:
  enabled:
    - retok-rewrite
```

Preserve existing entries and explicit disables. `plugins.disabled` takes
precedence. Project plugins require Hermes's separate project-plugin opt-in;
this adapter does not enable that setting. To disable this callback, set
`plugins.entries.retok-rewrite.settings.enabled: false` or disable the plugin.

The subprocess receives literal argv `hook hermes` and a UTF-8 JSON request on
stdin: `hook_event_name: "TransformToolResult"`, `tool_name: "terminal"`,
`tool_response` containing the original native JSON object, optional `tool_input`
containing the original arguments, and `cwd` containing the absolute host cwd.
Native result JSON is inserted verbatim, without parsing and reserializing
numbers. The CLI validates it. An exit-0 response with a nonempty string `result`
replaces the final native result string; `{}` means no change.

The adapter uses a two-second subprocess deadline, a 16 MiB request limit, and a
32 MiB stdout limit. Non-string results, results shorter than 256 characters,
invalid UTF-8, missing executables, timeouts, and invalid responses leave the
original result intact. Errors produce no adapter diagnostics. All terminal
backends and Windows use the same completed-text path: Retok runs locally and
never executes or rewrites the terminal command. Native Windows execution still
requires platform qualification.

Run `python3 -B tests/test_hermes.py` from the repository root for adapter
fixtures. Fixtures alone do not establish a full authenticated Hermes session.
