# Agent setup and RTK migration

```sh
sift init                         # Set up detected user-level agents
sift init --agent codex           # Select one agent
sift init --agent pi --project    # Current project only
sift init --replace-rtk --dry-run # Preview migration
sift init --replace-rtk
sift doctor
sift init --agent claude --uninstall
```

Setup writes selected integration files, backs up exact original bytes before
replacement, and preserves unrelated settings and hooks. Repeating setup is a
no-op when the installed content is current. Ordinary symlink-managed settings
remain symlinks; setup edits and backs up the resolved target. JSON settings may
start with a UTF-8 BOM. Hermes YAML edits retain unrelated formatting and values. OpenClaw JSON5 changes
can normalize formatting/comments; setup reports this and retains the original backup.
Uninstall removes unchanged Sift-owned entries, preserving customized content.
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
registration, executable availability and Sift settings; it does not verify
host discovery, version, trust, runtime loading or effective permission policy.

## Completion adapters

| Agent | Installed integration | Output scope |
| --- | --- | --- |
| Claude Code | `PostToolUse`, `sift hook claude` | Requires 2.1.121 or newer; completed Bash/PowerShell text fields. Failed commands do not reach this success-only event. |
| Copilot CLI | `postToolUse`, `sift hook copilot` | `modifiedResult` contract; completed Bash/PowerShell text. Distinct from the VS Code route below. |
| Hermes, user scope | `transform_tool_result` plugin | Requires Hermes 2026.9.14 or newer; completed `terminal` output after native result finalization, retaining exit code, error, hints and other metadata. |
| Pi / Oh My Pi | `tool_result` extension | Completed Bash/PowerShell text blocks plus optional successful native `grep` semantic selection; images and metadata retained. |
| OpenCode / current Kilo | `tool.execute.after` plugin | Completed Bash output, plus OpenCode `shell`; title and metadata retained. Legacy Kilo extensions are a separate integration. |

These adapters receive results after execution and replace only eligible text.
They do not rerun commands or change execution permissions. Unsupported shapes,
non-text content, oversized input and compressor failures pass through. They
cannot recover output the host already truncated, alter earlier streamed output,
or cover every background/interactive path. The JSON hook accepts up to 16 MiB
of input and selects at most 8 MiB of text; JavaScript completion plugins bound
selected text to 8 MiB and allow three seconds for compaction.

Pi-only setup installs `extensions/sift/index.js` with a local CommonJS
`package.json`, including when the surrounding project uses ES modules. It
replaces an exactly owned `sift.ts` and removes that old entry so only one
adapter loads. Edited or unowned entries are preserved. The adapter uses one
session-owned compressor for Bash results and successful native Pi `grep`
results; it exits on session shutdown or after 30 seconds idle. PowerShell stays
on the bounded one-shot path. Recognized literal `sift proxy` and
`sift run --raw` commands keep their delivered text unchanged and produce no Pi
compaction usage/original record.
This uses the completion hooks' existing POSIX subset: opaque compound or
backslash commands may not expose raw intent and still receive generic lossless
compaction. The Pi-session adapter explicitly enables delivered-text views for
recognized Git/Cargo commands and literal `node`/`node.exe` invocations of
`node_modules/tape/bin/tape` with nonoption file operands (optional `--`). Cargo may replace passing-status rows in complete
validated libtest blocks with an omitted-line count while retaining all other
delivered bytes, including failures, nonzero-exit and truncation notices. The
field is merged host-delivered text, not proof of complete stdout or successful
execution. A perfectly matching unmarked printed test transcript cannot be
distinguished from genuine harness output. Git uses the existing long-status
presentation only for whole fields ending in LF; every path/status is retained.
Tape recognizes a strict flat TAP13 prefix beginning at byte zero. It may omit
ordinary passing names and row spelling, labeling the count and exact test-number
range. Failures, SKIP/TODO names and reasons, comments, diagnostic owners, plans
and footers remain. A validated terminal Tape footer permits later host notices
or partial tails to remain byte-exact; it does not establish command success.
Incomplete/ambiguous structures and default directive/footer combinations stay
generic. Arbitrary JavaScript, `node --test`, npm wrappers and Tape options are
outside this view. The complete-stream path still requires complete LF framing.
Views must beat generic compaction by exact whole-field token count. Session
responses include `semantic:true` only when a delivered view or Jev passage
selection is used. `encoding` restores a delivered Git/Cargo/Tape presentation,
not omitted passing names or original Git layout. Jev selections instead carry
an explicit incomplete notice and a raw `sift recall` ID. Generic `json-v1`,
session requests without semantic selection, and all semantic failures keep the
ordinary Sift result.
Contextual callbacks that cannot use the session (busy, old binary, or transport failure)
return original text rather than retrying without the command. Missing command
metadata retains the prior generic behavior, including the one-shot busy fallback.
PowerShell and Oh My Pi remain one-shot. When setup selects both hosts at the same
physical plugin file, or a managed template proves prior setup for the other
host, setup uses the shared one-shot `sift.ts` and removes the native Pi bundle
entries. This also applies when OMP setup later finds a native Pi bundle at the
shared root. Uninstalling a shared file affects both.
An old unlabelled plugin at an aliased Pi/OMP target cannot prove its owner:
Pi-only setup leaves it unchanged and explains the manual upgrade. Run setup
when sharing a Pi directory with OMP; another host silently loading that file
cannot be identified at runtime. Restart an already running host after setup. Pi `/reload` alone does not refresh
a native CommonJS module after an adapter upgrade or executable relocation;
restart the Pi process to load the new module.

Pi semantic selection is separately opt-in and project allowlisted. Only a
successful native `grep` result with exactly one text block is eligible. Pi sends
the current task and exact result passages to the local Sift child; Sift makes
the TypeSafe request only when the current canonical checkout is allowlisted and
the grep path resolves inside that checkout and `TYPESAFE_API_KEY` is set.
Missing, unresolved, or symlink-escaped paths stay local. `read`, Bash,
PowerShell, images, errors, ambiguous shapes, and results outside the passage bounds never enter this semantic route.
See [Optional semantic selection](docs/reference.md#optional-semantic-selection) for the
external data flow, shadow mode, recovery, limits, and evidence.

For successful Claude Bash calls with a literal command and explicit completion
metadata, the hook also considers the same Git-status and Cargo-test
presentations as `sift run`. They must beat generic compaction by exact token
count. The command, permissions, exit metadata and other response fields remain
unchanged. Shell expressions and other completion hosts keep generic compaction.

Completion adapters remain available on Windows. The POSIX restriction below
applies to pre-execution rewriting, not to every Sift integration.

## Pre-execution adapters

| Agent | Installed integration | Supported tool |
| --- | --- | --- |
| Codex | `PreToolUse` in `hooks.json` | POSIX shell requests: POSIX default or explicit Bash/sh. The canonical Bash payload omits the actual requested shell; see below. |
| Mistral Vibe | `pre_tool` in `hooks.toml` | `bash`; project hooks require an already trusted folder. |
| OpenClaw, user scope | `before_tool_call` plugin | Foreground gateway shell `exec` with Bash, Zsh or Ksh selected; code-mode, node, sandbox and background/PTY calls pass through. Automatic target selection requires sandbox mode off. |

Gemini and VS Code automatic rewrites are withheld: host tests found that
rewriting could bypass whole-request or whole-command approval rules. Cursor and
Droid rewrites await native permission qualification. Setup preserves their RTK
automation; guidance and explicit `sift run` remain available. Copilot CLI
completion is a separate supported route.

These adapters change only the supported command input and retain other tool
arguments. The current rewrite accepts literal plain POSIX commands from a
fixed executable set. For example, with Sift installed at `/abs/sift`:

```sh
# Original
git status --short
# Replacement
command true || git status --short; command '/abs/sift' run --capture -- git status --short
```

The first branch is inert: `command true` succeeds, so that copy of the original
command never executes. Keeping its executable and arguments visible lets a
host parser inspect the original operation. The second invocation executes it
once through Sift. Recognized finite commands use `--capture`; other supported
commands use the runner's normal bounded buffering. Supported literal command
lists retain their separators and receive an inert check for each wrapped command.

Variable expansions, assignments, pipelines, redirects, control-flow constructs,
quoted executable names and unknown command forms are left unchanged. A request
identified as PowerShell also passes through, including `rewrite --shell powershell`.
The rewrite does not evaluate scripts or introduce a nested shell. Preview it
without executing anything:

```sh
sift rewrite --json --shell posix -- 'git status --short'
```

JSON reports `changed` and the resulting command (the original when unchanged).
Exit 0 means rewritten; exit 1 means unchanged. Plain mode prints only a rewrite.
For an explicit complex command, use `sift run -- sh -c 'git log | tail -5'`;
place compaction after the whole pipeline so downstream programs read native bytes.

**Permission behavior needs qualification in each host.** Keeping approval fields
or the original executable visible is not a blanket guarantee of equivalent
policy. Opaque wrappers, nested shells and quoted command forms can prevent host
parsers from recognizing the original operation. Do not add a broad `sift *`
approval rule: Sift can execute arbitrary programs. Setup does not grant command permissions,
change sandbox settings or trust hooks on the user's behalf. In Codex, approve
the installed hook through the host's native trust flow before expecting it to run.

The Linux x64 0.3.0 release passed 41 checks against a specific installed Linux
Codex build reporting version 0.153.1: automatic compaction and restoration,
execution once, forbidden/prompt rules, concurrent-hook denial, Bash/dash status and stream handling, generated
user/project setup, and native hook trust. Tests included spaces/apostrophes in
Sift's path, quoted arguments, continuations and supported command lists. They
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
only the Sift plugin and preserves explicit disables/denies. OpenClaw with an
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
shell, disable the Sift pre-hook in the host and use manual `sift run` instead.
For example, explicitly invoke an installed PowerShell with
`sift run -- pwsh -NoProfile -Command 'Get-Location'`. The adapter does not infer
the missing shell or duplicate the host's runtime policy.

On native Windows these pre-execution rewrites pass through. Migration preserves
working RTK automation instead of silently replacing it with an inactive adapter.
Shared Copilot migration preserves RTK activation when removing it would also
remove VS Code coverage. Fresh Copilot CLI setup installs its completion route.

## Instruction scopes and configuration

Roo, Kimi, Windsurf, Antigravity and project Cline use instructions. Setup reports
unsupported scopes rather than inventing a location. Explicit
`--instructions-only --agent HOST` is also available where a guidance target
exists. Instructions ask the agent to choose `sift run`; they are not automatic
compaction. Keep ordinary commands when another program needs their original bytes.

Setup records the absolute Sift executable path, so native adapters do not need
it on PATH. Supported relocated homes include `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`COPILOT_HOME`, `PI_CODING_AGENT_DIR` (Pi/OMP), `FACTORY_HOME_OVERRIDE` (with
`.factory` appended), `VIBE_HOME`, `KIMI_CODE_HOME`, `HERMES_HOME`,
`OPENCLAW_STATE_DIR` and `OPENCLAW_CONFIG_PATH`. Project setup stays in its selected
project scope. No shell profiles or host trust stores are edited.

Protocol, setup and migration fixtures check declared shapes and failure cases.
They are distinct from running the installed adapter through an actual host's
loader, trust and permission checks. A native pass applies only to the recorded
host version, platform and integration path.
