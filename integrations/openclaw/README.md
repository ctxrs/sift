# OpenClaw pre-tool plugin

This plugin registers `before_tool_call` for the shell `exec` tool. It returns
updated parameters with only `command` replaced; OpenClaw owns execution,
approvals, and result delivery. Code-mode `exec` calls are excluded.

This directory is an installation template. Replace the unquoted
`__SIFT_EXECUTABLE_JSON__` in `index.mjs` with a JSON string encoding the absolute
Sift executable path. Install `index.mjs`, `openclaw.plugin.json`, and
`package.json` together in `$OPENCLAW_STATE_DIR/extensions/sift-rewrite/`
(`~/.openclaw/extensions/sift-rewrite/` by default). The package manifest points
the native loader at `./index.mjs`.

The plugin entry in `openclaw.json` is:

```json
{
  "plugins": {
    "entries": {
      "sift-rewrite": { "enabled": true, "config": { "enabled": true } }
    }
  }
}
```

Merge this entry with existing configuration. `OPENCLAW_CONFIG_PATH` may override
the default `$OPENCLAW_STATE_DIR/openclaw.json`. A nonempty `plugins.allow` list
must already include `sift-rewrite` or be explicitly updated by the user;
`plugins.deny` and explicit disables take precedence. The adapter never changes
configuration, plugin trust, execution allowlists, or approval decisions. It
reads its options from `api.pluginConfig`, not the root `api.config`.

The subprocess receives literal argv:
`rewrite --json --shell posix -- COMMAND`. Only exit 0 with valid JSON containing
`changed: true` and a nonempty `command` applies a rewrite. Exit 1 and all errors
leave the tool call unchanged. The deadline is two seconds, the input limit is
32 KiB, and stdout/stderr are bounded at 128 KiB each. Nothing is logged.

Supported target: local gateway execution with Bash, Zsh, or Ksh selected in
`SHELL`. Other shells, node/sandbox targets, sandboxed automatic selection,
background/PTY calls, and native Windows pass through unchanged. No failed executable/shell lookup is
cached; later calls can recover after configuration changes.

OpenClaw evaluates shell allowlists after parameter rewrites. Keeping approval
fields intact is necessary but does not by itself preserve original command
policy: Sift must keep the original operation visible to the host's parser.
Coverage here is public-source inspection and subprocess fixtures; full native
loader and approval qualification remain required.

Run fixtures with `node tests/openclaw.mjs` from the repository root.
