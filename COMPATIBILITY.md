# RTK workflow coverage

Retok is an independent replacement for the tool-output workflow, not a
command-for-command emulation of RTK. Its differences are intentional where
RTK filters, summarizes, truncates, or rewrites commands.

| Workflow | Retok |
| --- | --- |
| Git, GitHub, builds, tests, linters, package managers, containers, cloud CLIs, search and arbitrary executables | `retok COMMAND ARG...` or `retok run -- COMMAND ARG...`. Native arguments and exit status are preserved; no per-command filter registry is required. |
| Automatic agent integration | Native completion hooks/plugins where the host supports replacing final text. See [the host matrix](INTEGRATIONS.md). Other hosts receive optional instructions. |
| Switch an existing RTK setup | `retok init --replace-rtk`; exact backups, recognized stock entries, unrelated settings retained. Unrecognized custom integrations require manual review. |
| Plain stdin/file filtering | `retok compact`, with `pipe` and `read` aliases. |
| Persistent integration process | `retok compact --protocol=json-v1`; one tokenizer initialization, one response per input line. |
| Raw bypass | `retok proxy COMMAND ...` or `retok run --raw -- COMMAND ...`. |
| Information recovery | Every text representation can be restored with explicit encoding. Optional `keep_originals` storage and `retok recall` recover original captured bytes without rerunning commands. |
| Savings/history/chart | `retok gain`, `--history`, `--daily`, `--graph`, `--json`. Exact measured output tokens; unmeasured streams remain unmeasured. |
| Find opportunities | `retok discover FILE...` replays explicitly selected saved output without executing it. It does not scan all agent histories by default. |
| Configuration | One JSON file: enable/disable, local usage, optional originals, exact exclusions. `retok doctor` reports integration state. |
| Distribution | Signed/notarized macOS, Authenticode Windows, static Linux binaries; curl/PowerShell installers and source builds. |

## Deliberate differences

- **Preserve content.** No failures-only test summaries, source-code elision,
  arbitrary line caps, lossy JSON field selection, or custom lossy filter DSL.
  Repeated text and supported JSON are represented compactly; unsupported text
  remains available in full. Savings can be lower for a particular command.
- **Preserve execution policy and program data.** No automatic `git ...` to
  `retok git ...` rewrite, blanket Retok permission rule, deny-and-retry hook,
  or modification of input to `tail`, a parser, or a file redirect. Native output
  adapters act after execution. Where a host cannot do that reliably, its
  integration is instructions only.
- **Measure output, not hypothetical bills.** No byte/4 token estimates,
  subscription/quota projections, or automatic claims of fewer model turns.
  Replay savings do not establish agent accuracy or billing savings.
- **Keep local state small.** No telemetry, downloaded summarization model,
  background daemon, or automatic prompt-rule learning. Raw-output retention is
  opt-in because reversible representations already retain the information.

RTK-specific options such as `--ultra-compact`, `read --level`, filter names,
`test`/`err` summaries, and custom TOML filters are not translated. Use the native
command's own flags through `run --`. In particular, RTK's raw `run` and Retok's
compacting `run` have different meanings; `proxy` is Retok's raw execution path.

Retok also does not import RTK's analytics database or saved recall entries.
Migration preserves those files and the RTK executable so existing history can
still be read with RTK. Do not alias the name `rtk` to `retok`.
