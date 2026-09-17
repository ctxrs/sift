# Agent setup and RTK migration

```sh
retok init                         # Set up detected user-level agents
retok init --agent claude           # Select one agent
retok init --agent pi --project     # Current project only
retok init --replace-rtk            # Migrate recognized RTK integrations
retok init --replace-rtk --dry-run   # Preview without writing
retok doctor
retok init --agent claude --uninstall
```

Setup writes only the selected integration files, keeps exact backups before
replacement, and preserves unrelated settings and hooks. Repeating setup is a
no-op when the installed content is current. Uninstall removes Retok-owned
entries; it does not restore an old backup over settings you changed later.

`--replace-rtk` recognizes stock RTK hooks and supported stock plugin versions.
It does not remove a file merely because its name or contents mention RTK.
Custom or unrecognized integrations are reported for manual migration. RTK's
binary, history and saved output remain available. Review the printed paths and
restart the affected agent after setup.

## Automatic output replacement

| Agent | Installed integration | Scope |
| --- | --- | --- |
| Claude Code | `PostToolUse`, `retok hook claude` | Requires Claude Code 2.1.121 or newer; completed Bash/PowerShell text fields. |
| Copilot CLI | Native `postToolUse`, `retok hook copilot` | Current `modifiedResult` contract; completed Bash/PowerShell text results. This is distinct from VS Code Copilot. |
| Pi / Oh My Pi | `tool_result` extension | Completed Bash/PowerShell text blocks; images and metadata retained. |
| OpenCode | `tool.execute.after` plugin | Completed Bash output; title and metadata retained. |
| Current Kilo | `tool.execute.after` plugin | Current-generation plugin API; not a claim about legacy Kilo extensions. |

The command runs normally under the host's existing permissions. Retok receives
the completed result and replaces only eligible text. It never executes that
command a second time or returns a permission decision. Failure diagnostics are
retained. Claude's success-only `PostToolUse` hook leaves failed commands unchanged; non-text content, unsupported shapes, interruption, oversized inputs,
missing executables and compressor errors pass through. Hook/plugin timeouts
also leave the host's original output available.

Hooks see what the host supplies. They cannot recover output the host already
truncated, change earlier streamed terminal output, or cover every background
and interactive path. The JSON hook accepts up to 16 MiB of input and selects at
most 8 MiB of text. JavaScript plugins also bound selected text to 8 MiB and
allow three seconds for the local compressor. Other tools are left unchanged.

## Instruction-only hosts

Codex, Cursor, Gemini, VS Code Copilot, Droid, Windsurf, Cline, Roo and
Antigravity, Kimi, Hermes, Mistral Vibe and OpenClaw use small host-specific
instructions where a documented location is available. Setup labels these **instructions only**. The agent chooses when to
use `retok run`; this is not an automatic hook. Some hosts expose only project
instructions; setup reports unsupported scopes instead of inventing a path.

This distinction is deliberate. Current Codex output hooks do not expose enough
metadata to safely replace unified-exec results, and nested code-mode calls
retain their original programmatic result. Cursor and several other hosts do
not document neutral shell-output replacement. Gemini's documented replacement
path has denial semantics. Retok does not disguise these limitations by
rewriting commands or asking the agent to retry them. OpenClaw's current live
middleware normalizes even unmatched tool results, potentially truncating text or
removing images before Retok runs; its integration therefore stays instructions
only. Use `--project` from the OpenClaw workspace.

Use an explicit wrapper only for output intended for the model:

```sh
retok run -- git status
retok run -- sh -c 'git log | tail -5'
```

Keep ordinary commands when another program needs their original bytes. Do not
add a broad `retok *` approval rule: it can run arbitrary executables.

## Configuration and qualification

Setup respects supported host home overrides such as `CODEX_HOME` and
`COPILOT_HOME`. The installed executable path is recorded directly, so adding it
to PATH is not required for a native hook/plugin. Instruction-only setup also
records its absolute path for the agent to use. Setup does not alter shell
profiles, permission rules or host trust stores.

Synthetic tests cover protocol shapes, exact metadata preservation, migration,
backups and compressor failure. Native qualification additionally records the
host version and integration path exercised; protocol fixture tests alone are
not proof that every host release or operating system loads the adapter.
