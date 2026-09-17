# RTK workflow coverage

Retok is an independent tool-output compressor. Generic execution covers many
of the same programs as RTK, but does not reproduce RTK's command-specific
parsers, summaries or flags. This page describes the 0.3.0 development source; it
does not claim complete parity with every RTK command variant.

| Workflow | Retok's current choice |
| --- | --- |
| Git, GitHub, builds, tests, linters, package managers, containers, cloud CLIs and search | `retok COMMAND ARG...` or `retok run -- COMMAND ARG...` forwards native argv. Default compaction preserves text bytes or supported JSON values. There is no per-tool semantic summary registry. |
| Long finite commands / terminal invocation | `retok run --capture -- COMMAND ...` waits for complete output within a combined 8 MiB bound, including terminal invocation. Progress is delayed; overflow switches to raw output. Default execution keeps interactive/progress passthrough. |
| Automatic integration | Completion adapters replace eligible final text; pre-execution adapters rewrite supported literal POSIX commands. Unsupported syntax stays unchanged. Gemini/VS Code rewrites are withheld for demonstrated approval-rule regressions; Cursor/Droid await qualification. Host qualification and scopes are listed in [INTEGRATIONS.md](INTEGRATIONS.md). |
| Migrate RTK setup | `retok init --replace-rtk --dry-run`, then `retok init --replace-rtk`. Stock recognition, exact backups and preservation of unrelated settings; unsupported automatic migrations keep RTK active. |
| Plain input / streaming input | `compact`, `pipe` and plain `read` compact one file or stdin. `filter [--capture]` provides bounded stdin buffering and raw fallback. These do not load named RTK filters. |
| Selected file/JSON views | `read --from N --lines N --grep TEXT`; `json --pointer POINTER --field KEY --limit N`. Selection is explicit and can omit information. No implicit depth/string elision. |
| Summary / diagnostic / test views | `summary -- COMMAND`, `err -- COMMAND`, `test -- COMMAND`. Explicit head/tail or keyword/context views with omission labels; no inferred test result or tool-specific parser. |
| Persistent compaction | `compact --protocol=json-v1`; one tokenizer initialization, one response per input line. |
| Raw output | `proxy COMMAND ...` or `run --raw -- COMMAND ...`. |
| Recovery | Explicit-encoding restoration, or opt-in originals through `recall`. Recall accepts unique ID prefixes, line windows and literal grep; retention caps are configurable. No rerun for recovery. |
| Local statistics | `gain` supports project, date, command and source filters, daily/weekly/monthly buckets, history, graph, JSON and CSV. Reports describe retained records; unmeasured output stays unmeasured. |
| Opportunity discovery | `discover FILE...` measures selected saved output. `discover --history PATH --suggest` inspects supported saved Claude/Codex histories and reports observed correction patterns without executing commands or writing rules. |
| Observed usage/cost import | `ccusage --import FILE` reports a selected local export separately from Retok's measured gain. Costs retain their source qualification; no projected savings or package fetching. |
| Setup diagnostics | `doctor` checks configured registration, executable availability and Retok settings. It does not prove the host loaded or trusted an adapter. |
| Distribution | Release binaries and curl/PowerShell installers, plus source builds. Signed/notarized macOS and Authenticode Windows releases, static Linux binaries. |

## Migrating command lines

Changing the executable name alone is not a supported migration. In particular,
RTK's raw `run` and Retok's compacting `run` have different meanings.

| Existing intent | Retok command |
| --- | --- |
| Run with raw output | `retok proxy git diff` |
| Execute a shell string | `retok run -- sh -c 'git log | tail -5'` |
| Inspect test diagnostics | `retok test --context 3 -- cargo test` |
| Invoke the native `test` utility | `retok run -- test -f README.md` |
| Read a selected file window | `retok read FILE --from 20 --lines 40` |
| Select JSON rows/fields | `retok json FILE --pointer /items --field name --limit 10` |
| Run an installed package-local tool | `retok pnpm exec tsc` or `retok npx --no-install tsc` |
| Use a user-chosen filter | `your-command | your-filter | retok compact` |

Pass argv as separate arguments. `proxy 'git diff'` names one executable; it does
not parse a shell string. Retok does not translate RTK's `run -c`, `read --level`,
`--ultra-compact`, named `pipe` filters, formatter detection or tool-specific
flags. Choose native flags, an explicit view, or a filter you already use.
Package-runner behavior remains the responsibility of the runner you select.

## Retained differences and limits

- **Default preservation and explicit selection.** Automatic compaction keeps all
  supplied text bytes or supported JSON values. Opt-in views can omit content;
  their output is not reversible compaction. Diagnostic/test views are heuristics,
  not replacements for RTK's framework parsers, parsed failure totals, cloud
  summaries or source-code views. Savings and convenience vary by workload.
- **Structured formats.** JSON compaction supports complete values and uniform
  top-level object arrays. JSON Pointer/field views add explicit selection, not
  YAML/XML/CSV/JSONL semantic codecs, nested-row factoring, SQL tables or schema
  inference. Other formats can still use reversible text compaction.
- **Execution and policy.** No deny-and-retry integration, blanket permission rule
  or automatic package fetch. Pre-execution rewrites cover a limited literal POSIX
  grammar; they do not establish universal permission equivalence. Codex on Unix
  does not expose the requested shell to the hook, so non-POSIX requests require
  disabling the pre-hook and using manual `retok run`. On Unix,
  signal exits become numeric `128 + signal` rather than re-raising the signal in
  Retok. Separate stdout/stderr are preserved, not their total interleaving.
- **Custom filtering.** No custom TOML filter DSL, filter autoloading or trust
  framework. Explicit native filters compose with `compact`; Retok does not
  silently load project-supplied filters.
- **Reporting.** No projected bills, subscription/quota estimates or claims of
  fewer agent turns. Imported ccusage amounts may be calculated or incompletely
  priced. History suggestions cover known correction patterns; they do not
  learn general rules, join every asynchronous result or prove integration adoption.
  Dedicated recall-efficiency and host-decision audit reports are not implemented.
- **Local state.** No telemetry, summarization model, background daemon or automatic
  history scan. Original retention is opt-in. Retok does not import RTK's analytics
  database or recall store; migration leaves those files and the RTK executable
  available so you can read them with RTK.

Windows executable lookup includes `.exe`, `.com`, `.cmd` and `.bat` through
PATH/PATHEXT; PowerShell scripts need an explicit interpreter. The new batch-shim
and terminal-capture paths still require native Windows qualification. Current
source/fixture coverage must not be read as a release-wide platform pass.

Do not alias `rtk` to `retok`. Keep existing integrations until their replacement
scope and host activation are confirmed.
