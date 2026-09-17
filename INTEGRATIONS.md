# Agent setup and RTK migration

```sh
retok init                         # Set up detected user-level agents
retok init --agent codex           # Select one agent
retok init --agent pi --project    # Current project only
retok init --replace-rtk --dry-run # Preview migration
retok init --replace-rtk
retok doctor
retok init --agent claude --uninstall
```

Setup writes selected integration files, backs up exact original bytes before
replacement, and preserves unrelated settings and hooks. Repeating setup is a
no-op when the installed content is current. Ordinary symlink-managed settings
remain symlinks; setup edits and backs up the resolved target. JSON settings may
start with a UTF-8 BOM. Hermes YAML edits retain unrelated formatting and values. OpenClaw JSON5 changes
can normalize formatting/comments; setup reports this and retains the original backup.
Uninstall removes unchanged Retok-owned entries, preserving customized content.
It does not restore an old backup over later edits.

`--replace-rtk` recognizes specific stock RTK hooks, plugins and instruction
blocks. It does not remove a file merely because it mentions RTK. Unknown or
modified integrations need manual migration. Existing automatic RTK coverage is
preserved when the replacement is unsupported; `--instructions-only --agent HOST`
explicitly chooses guidance where that host has a supported instruction target.
RTK's executable, analytics and saved output remain available. Hermes/OpenClaw
stock plugin migration changes activation and retains the RTK plugin files.

Review the printed changes, restart the affected host, and complete its normal
hook/plugin trust flow. **Configured does not mean loaded.** `doctor` checks
registration, executable availability and Retok settings; it does not verify
host discovery, version, trust, runtime loading or effective permission policy.

## Completion adapters

| Agent | Installed integration | Output scope |
| --- | --- | --- |
| Claude Code | `PostToolUse`, `retok hook claude` | Requires 2.1.121 or newer; completed Bash/PowerShell text fields. Failed commands do not reach this success-only event. |
| Copilot CLI | `postToolUse`, `retok hook copilot` | `modifiedResult` contract; completed Bash/PowerShell text. Distinct from the VS Code route below. |
| Hermes, user scope | `transform_tool_result` plugin | Requires Hermes 2026.9.14 or newer; completed `terminal` output after native result finalization, retaining exit code, error, hints and other metadata. |
| Pi / Oh My Pi | `tool_result` extension | Completed Bash/PowerShell text blocks; images and metadata retained. |
| OpenCode / current Kilo | `tool.execute.after` plugin | Completed Bash output, plus OpenCode `shell`; title and metadata retained. Legacy Kilo extensions are a separate integration. |

These adapters receive results after execution and replace only eligible text.
They do not rerun commands or change execution permissions. Unsupported shapes,
non-text content, oversized input and compressor failures pass through. They
cannot recover output the host already truncated, alter earlier streamed output,
or cover every background/interactive path. The JSON hook accepts up to 16 MiB
of input and selects at most 8 MiB of text; JavaScript completion plugins bound
selected text to 8 MiB and allow three seconds for compaction.

Completion adapters remain available on Windows. The POSIX restriction below
applies to pre-execution rewriting, not to every Retok integration.

## Pre-execution adapters

| Agent | Installed integration | Supported tool |
| --- | --- | --- |
| Codex | `PreToolUse` in `hooks.json` | POSIX shell requests: POSIX default or explicit Bash/sh. The canonical Bash payload omits the actual requested shell; see below. |
| Mistral Vibe | `pre_tool` in `hooks.toml` | `bash`; project hooks require an already trusted folder. |
| OpenClaw, user scope | `before_tool_call` plugin | Foreground gateway shell `exec` with Bash, Zsh or Ksh selected; code-mode, node, sandbox and background/PTY calls pass through. Automatic target selection requires sandbox mode off. |

Gemini and VS Code automatic rewrites are withheld: host tests found that
rewriting could bypass whole-request or whole-command approval rules. Cursor and
Droid rewrites await native permission qualification. Setup preserves their RTK
automation; guidance and explicit `retok run` remain available. Copilot CLI
completion is a separate supported route.

These adapters change only the supported command input and retain other tool
arguments. The current rewrite accepts literal plain POSIX commands from a
fixed executable set. For example, with Retok installed at `/abs/retok`:

```sh
# Original
git status --short
# Replacement
command true || git status --short; command '/abs/retok' run --capture -- git status --short
```

The first branch is inert: `command true` succeeds, so that copy of the original
command never executes. Keeping its executable and arguments visible lets a
host parser inspect the original operation. The second invocation executes it
once through Retok. Recognized finite commands use `--capture`; other supported
commands use the runner's normal bounded buffering. Supported literal command
lists retain their separators and receive an inert check for each wrapped command.

Variable expansions, assignments, pipelines, redirects, control-flow constructs,
quoted executable names and unknown command forms are left unchanged. A request
identified as PowerShell also passes through, including `rewrite --shell powershell`.
The rewrite does not evaluate scripts or introduce a nested shell. Preview it
without executing anything:

```sh
retok rewrite --json --shell posix -- 'git status --short'
```

JSON reports `changed` and the resulting command (the original when unchanged).
Exit 0 means rewritten; exit 1 means unchanged. Plain mode prints only a rewrite.
For an explicit complex command, use `retok run -- sh -c 'git log | tail -5'`;
place compaction after the whole pipeline so downstream programs read native bytes.

**Permission behavior needs qualification in each host.** Keeping approval fields
or the original executable visible is not a blanket guarantee of equivalent
policy. Opaque wrappers, nested shells and quoted command forms can prevent host
parsers from recognizing the original operation. Do not add a broad `retok *`
approval rule: Retok can execute arbitrary programs. Setup does not grant command permissions,
change sandbox settings or trust hooks on the user's behalf. In Codex, approve
the installed hook through the host's native trust flow before expecting it to run.

The Linux x64 0.3.0 release passed 41 checks against a specific installed Linux
Codex build reporting version 0.153.1: automatic compaction and restoration,
execution once, forbidden/prompt rules, concurrent-hook denial, Bash/dash status and stream handling, generated
user/project setup, and native hook trust. Tests included spaces/apostrophes in
Retok's path, quoted arguments, continuations and supported command lists. They
used a synthetic local provider, not real model tasks. This is evidence for that
host build and supported POSIX forms, not universal policy equivalence or
qualification of every stock Codex release, macOS, Windows or PowerShell.

Vibe’s tested denylist remains effective, but a previously allowed command may
now require confirmation for the wrapper. Setup does not add `command *` or
other broad approval rules.

Hermes uses a completed-result hook instead of command rewriting; its earlier
pre-execution prototype was discarded after native deny-rule tests. OpenClaw’s
pre-execution route passed 18 final-artifact checks through its pinned
native dispatcher, policy, approval and final-spawn paths. Those checks used
synthetic approval transport/storage and bounded child supervision; they are not
full gateway UI, plugin discovery, durable-storage or model-review qualification.
An existing exact-command approval can require another prompt, or be denied when
asking is disabled. Setup does not add grants to hide that difference. Full native
sessions remain distinct from source and native-module fixtures. Setup activates
only the Retok plugin and preserves explicit disables/denies. OpenClaw with an
existing restrictive plugin allowlist may require explicit `--agent openclaw` to
add that one plugin; this is separate from command approval. See the
[Hermes](integrations/hermes/README.md) and
[OpenClaw](integrations/openclaw/README.md) adapter details for target restrictions.
Project setup for these two hosts installs instructions, not a project plugin or
its separate host opt-in.

**Codex on Unix cannot detect every unsupported shell request.** Its canonical
Bash hook payload contains the command but omits the actual shell selection.
An explicit `shell=pwsh` request can therefore look identical to a POSIX request.
The pre-hook supports POSIX shell requests only; before requesting a non-POSIX
shell, disable the Retok pre-hook in the host and use manual `retok run` instead.
For example, explicitly invoke an installed PowerShell with
`retok run -- pwsh -NoProfile -Command 'Get-Location'`. The adapter does not infer
the missing shell or duplicate the host's runtime policy.

On native Windows these pre-execution rewrites pass through. Migration preserves
working RTK automation instead of silently replacing it with an inactive adapter.
Shared Copilot migration preserves RTK activation when removing it would also
remove VS Code coverage. Fresh Copilot CLI setup installs its completion route.

## Instruction scopes and configuration

Roo, Kimi, Windsurf, Antigravity and project Cline use instructions. Setup reports
unsupported scopes rather than inventing a location. Explicit
`--instructions-only --agent HOST` is also available where a guidance target
exists. Instructions ask the agent to choose `retok run`; they are not automatic
compaction. Keep ordinary commands when another program needs their original bytes.

Setup records the absolute Retok executable path, so native adapters do not need
it on PATH. Supported relocated homes include `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`COPILOT_HOME`, `PI_CODING_AGENT_DIR` (Pi/OMP), `FACTORY_HOME_OVERRIDE` (with
`.factory` appended), `VIBE_HOME`, `KIMI_CODE_HOME`, `HERMES_HOME`,
`OPENCLAW_STATE_DIR` and `OPENCLAW_CONFIG_PATH`. Project setup stays in its selected
project scope. No shell profiles or host trust stores are edited.

Protocol, setup and migration fixtures check declared shapes and failure cases.
They are distinct from running the installed adapter through an actual host's
loader, trust and permission checks. A native pass applies only to the recorded
host version, platform and integration path.
