# Sift reference

Detailed installation, command, configuration, integration protocol, and format documentation. For the quick start, see the [main README](../README.md).

## Install

Install the latest [GitHub release](https://github.com/ctxrs/sift/releases).
Linux and macOS (x64 or ARM64; requires `curl` and `sha256sum` or `shasum`):

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/sift/main/install.sh | sh
```

Install and switch recognized RTK integrations in one command:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/sift/main/install.sh | sh -s -- --replace-rtk
```

Use `--init` instead to set up detected agents without removing RTK. Existing
Sift installations can use `sift init --replace-rtk` directly.

Windows x64, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/ctxrs/sift/main/install.ps1 | iex
```

To install and switch recognized RTK integrations on Windows:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/ctxrs/sift/main/install.ps1))) -ReplaceRtk
```

Supported release targets are Linux x64 and aarch64, macOS 13 or newer on x64
and arm64, and Windows x64. Windows ARM64 and 32-bit systems are not supported.
The Linux assets are statically linked musl executables and do not require
glibc. Sift does not claim compatibility with a specific older Linux
kernel.

Both installers download the binary and its `.third-party-notices.txt` sidecar,
verify both against the release's `SHA256SUMS`, and stage both in the installation
directory before replacing files. Notices are installed beside the binary as
`sift.third-party-notices.txt` on Unix or `sift.exe.third-party-notices.txt` on
Windows. The default directory is `~/.local/bin` on Unix and
`%LOCALAPPDATA%\Programs\Sift` on Windows. Add that directory to your PATH,
then run `sift --help`. No administrator access is needed. Installation alone
leaves agent settings untouched; `--init` or `--replace-rtk` explicitly runs setup.
Neither installer edits PATH or shell profiles.

These commands execute the installer from `ctxrs/sift` on GitHub over HTTPS.
The installers download release files and `SHA256SUMS` from that same repository
over HTTPS. This trusts GitHub, HTTPS, and the repository's maintainers: the
checksums detect mismatched or corrupted downloads, but are not an independent
signature and cannot protect against a compromised release and checksum file.
Starting with v0.1.1, macOS binaries carry a notarized Apple Developer ID
signature and the Windows binary carries a timestamped Authenticode signature.
The v0.1.0 binaries remain unsigned. The installers do not disable Gatekeeper,
SmartScreen, or other operating-system protections.

To pin this release or choose a different directory, set
the installer environment variables (either variable can be used on its own):

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/sift/main/install.sh | SIFT_VERSION=v0.4.0 SIFT_INSTALL_DIR="$HOME/.local/bin" sh
```

```powershell
$env:SIFT_VERSION = 'v0.4.0'
$env:SIFT_INSTALL_DIR = "$env:LOCALAPPDATA\Programs\Sift"
irm https://raw.githubusercontent.com/ctxrs/sift/main/install.ps1 | iex
```

Installer checks use synthetic releases and make no network requests:
`python3 tests/test_install.py` on Unix and
`powershell -NoProfile -File tests/install.Tests.ps1` on Windows
(`pwsh` also works).

For Homebrew, add this repository as an explicit tap and build from the pinned
release source (requires a Rust build toolchain):

```sh
brew tap ctxrs/sift https://github.com/ctxrs/sift
brew install ctxrs/sift/sift
```

### Update and uninstall

Rerun the installer to update the binary and notices. After moving or updating
Sift, rerun `sift init` to refresh supported managed adapters; custom edits remain
preserved. Homebrew installations use `brew upgrade ctxrs/sift/sift`.

Before removing Sift, uninstall its managed integration for each configured agent
and project, for example `sift init --agent codex --uninstall` (add `--project`
inside a configured project). This preserves unrelated settings and customized
entries; review any manual-removal notice. It does not reactivate old RTK hooks
or delete saved Sift usage/originals.

Then remove the executable and notices from the directory you installed into.
For the default locations:

```sh
rm -- "$HOME/.local/bin/sift" "$HOME/.local/bin/sift.third-party-notices.txt"
```

```powershell
Remove-Item -LiteralPath "$env:LOCALAPPDATA\Programs\Sift\sift.exe", "$env:LOCALAPPDATA\Programs\Sift\sift.exe.third-party-notices.txt"
```

For Homebrew, use `brew uninstall ctxrs/sift/sift`. Configuration and retained
output remain in the [documented local directories](#local-usage-and-original-output)
unless you explicitly remove them.

## Build and use

Requires Rust 1.88 or newer. Cargo downloads dependencies at build time. The
count tables and precompiled tokenizer pattern are embedded in the binary;
the build validates the count tables and pre-tokenizer; the runtime borrows those
immutable tables without repeating validation or downloading a vocabulary. The adapted
public algorithms and licenses are recorded in [THIRD_PARTY_TOKENIZER.md](../THIRD_PARTY_TOKENIZER.md).
This reference describes the current source for v0.4.0.
Check your installed version's `sift --help` and subcommand help for available
features.

```sh
cargo build --release --locked
./target/release/sift compact output.txt
printf 'short diagnostic\n' | ./target/release/sift compact
./target/release/sift --help
```

`compact [FILE|-]` reads a complete file or stdin and writes only the selected
model-facing text. It adds no trailing newline. Invalid UTF-8 passes through
byte for byte. A short result normally stays unchanged because the entire
representation, including its explanatory header, must beat the original token
count. Ties keep the original. Special-token-looking strings are counted as
ordinary text, not special tokens.

## Run commands

```sh
sift run -- git status --short
sift run --capture -- cargo test     # Wait for a finite command to finish
sift proxy git diff                 # Explicit raw output
sift run -- sh -c 'git log | tail -5' # Compact after the entire pipeline
```

`run` passes argv directly to the executable, inheriting stdin, environment and
working directory. It preserves stdout and stderr separately and returns the
child's exit status. It never retries a command to recover output. Shorthand
`sift COMMAND ...` has the same behavior; use `run --` for names that collide
with Sift's own commands.

By default, terminal input or output makes the runner pass through. Otherwise,
Sift buffers up to 8 MiB and waits up to 250 ms after the first output while the command is still running.
When either threshold is reached it streams the original bytes for the rest of
that command. Short, complete output is compacted; very small results and invalid
UTF-8 stay raw. Tokenizer work after completion adds processing time. This keeps
prompts and ongoing progress visible without truncating output.

For ordinary `git status`, Sift can use short stage/type columns while retaining
every displayed path, branch and advisory. Conflicts and unfamiliar sections keep
their original formatting. For Cargo's ordinary test harness, Sift validates
the reported test rows against the final counts before replacing passing rows
with an explicit omission count. Failed and ignored test names, diagnostics and
the final result remain. These presentations compete with reversible compaction
of the original; the complete output must use fewer tokens to be selected.
Precise output modes such as Git porcelain and custom Cargo harness formats do
not receive these presentations. Use `proxy` or `--raw` for native bytes from
the runner, or enable original retention to recall complete captures later.
Completion hooks also honor literal `sift proxy` and `sift run --raw` commands
in Claude Bash, Copilot Bash, and explicitly local POSIX Hermes sessions.
Other shells, complex shell expressions, or missing command metadata can still
receive generic completion compaction; disable that hook when raw output is
required there. An agent's own output limits still apply.

Claude's Bash completion hook can use these same presentations when it receives
a literal command and explicit completion metadata. Pipelines and variable
expansions keep generic compaction. Claude's `PostToolUse` event covers successful
commands; this does not add compaction for failed commands.

`--capture` waits for child exit and both output streams to close, without the
250 ms deadline. It can capture a finite command even from a terminal, while
stdin stays inherited. Progress and prompts are delayed; it does not allocate a
PTY. Reaching the combined 8 MiB limit flushes the buffered bytes and switches
both streams to raw forwarding. `--raw` takes precedence over capture.

Use Sift at the end of a programmatic pipeline. `sift git log | tail -5` would
make `tail` read the compact representation. To retain native pipeline semantics,
wrap the whole pipeline in an explicit shell as above, or let a supported native
output hook compact the final agent-visible result. Sift does not automatically
rewrite pipelines. Its pre-execution adapters only rewrite the supported command
forms described in [INTEGRATIONS.md](../INTEGRATIONS.md); setup installs no blanket
permission rule.

`sift pipe` and plain `sift read` use `compact`'s lossless single-file behavior.
For streaming stdin, `sift filter` uses bounded buffering and otherwise passes
bytes through; `filter --capture` waits for EOF within the same size bound.
Neither command loads named RTK filters.

## Explicit views

These commands can omit information. Sift never selects these line/field/excerpt
views automatically; the two command presentations above have separate rules.

```sh
sift read build.log --from 20 --lines 40 --grep 'error'
sift json response.json --pointer /items --field name --field status --limit 10
sift summary --lines 20 -- cargo build
sift err --context 3 -- cargo check
sift test --context 3 -- cargo test
```

`read` starts at a one-based source line, matches literal case-sensitive text,
and limits the matching lines. Without selection options it remains a lossless
compact alias. `json` validates the full document, selects a JSON Pointer, limits
the selected array, then keeps the requested fields. Missing fields, duplicate
keys and invalid JSON fail; numbers retain their original spelling. File/stdin
views accept up to 16 MiB; JSON nesting is limited to 64 levels.

`summary` keeps the first and last N lines of each stream (20 each by default).
`err` and `test` select diagnostic keywords with context (two lines by default).
They label omissions and use substring matching: a line such as `0 failures` can
match. They do not parse test totals or infer success. No matches or non-text
output stays raw. These command views use bounded complete capture, delay
progress and preserve the child's status; capture overflow passes through raw.
Use `sift run -- test -f FILE` to invoke the native `test` utility.

## Optional semantic selection

Semantic selection is off by default. Its first supported route is a successful
native Pi `grep` result containing one text block. Pi supplies the current task
and divides the fresh result into exact, ordered passages. Deterministic rules
keep task anchors and truncation notices; the pinned `jev-1.13.0` model judges
the remaining passages for relevance and counterevidence. Jev selects passages;
it does not summarize, rewrite facts, choose tools, or authorize commands.

Enable it for one canonical checkout only after reviewing the data flow:

```sh
export TYPESAFE_API_KEY='...'
sift semantic shadow --project .  # Send eligible inputs; keep ordinary Sift output
sift semantic enable --project .  # Permit recoverable passage omission
sift semantic status
sift semantic disable             # Disable semantic calls globally
```

Key presence alone never enables the feature. `shadow` and `enable` add the
canonical checkout to the local allowlist; the selected mode applies to all
previously allowlisted checkouts. An eligible request sends the current task,
passage IDs, and passage text over HTTPS to TypeSafe. Sift sends no prior
conversation, system prompt, tool schema, project path, command arguments, or
saved history. The API key is read from the environment and is never written to
Sift configuration or receipts. TypeSafe is an external processor; its account
terms and retention policy still apply, and Sift does not itself establish a
zero-data-retention agreement.

The local adapter must identify the grep search path, and Sift resolves it
through the filesystem before any request. Missing paths, failed resolution,
`..` escapes, and symlinks whose targets leave the allowlisted checkout stay on
ordinary local compaction. The resolved path is used only for this gate and is
not included in the TypeSafe request.

Every timeout, missing key, invalid response, ineligible result, or local storage
failure returns the ordinary local Sift result. In select mode, Sift first saves
the complete raw grep text in its private originals store. It emits an explicit
`INCOMPLETE` notice, omitted passage IDs, and `sift recall ID` only when the
semantic frame saves at least 300 bytes and also beats ordinary Sift by exact
`o200k_base` token count. Those originals use the configured entry, byte, and
age limits and can later expire. `shadow` never omits passages. Sanitized semantic
receipts record status, counts, latency, model usage, and the ordinary/semantic
token comparison without task or passage text; semantic savings are kept out of
ordinary `sift gain` totals.

The frozen experiment behind this scope retained every labeled relevant and
critical fact across 24 repeated holdout selections while reducing rendered
evidence from 6,330 to 1,401 tokens (77.9%); downstream answers scored 24/24 in
both raw and selected arms. Cache-weighted main-model input fell 30.1%, but total
tokens across Jev and the main model increased. This is a context-capacity and
main-model-cost feature, not a claim of fewer aggregate provider tokens or proven
latency improvement.

## Local usage and original output

```sh
sift gain --daily --graph
sift gain --project --weekly
sift gain --project --history --csv
sift gain --since 2026-09-01 --until 2026-10-01 --command git --json
sift config --create
sift recall --list
sift recall ID                      # Original stdout, when saved
sift recall PREFIX --stderr --from 20 --lines 40 --grep error
sift discover --json output.txt     # Potential savings; never executes input
sift discover --history ./saved-sessions --suggest --json
sift ccusage --import usage.json --csv
```

Automatic integrations and captured `run` output record local counts, timing,
status, a short tool/executable label and a local project path when available.
They do not record command arguments. The project is the canonical checkout root,
or the process's working directory outside Git; linked worktrees remain separate.
Completion hooks use the host's reported working directory when supplied;
an unavailable reported directory leaves the event unscoped. Without that field,
they use the hook process's directory, which can differ from the command's.

`gain` reports retained measured ordinary `o200k_base` tokens; unmeasured streams
are excluded from token totals. CLI command presentations are marked `run-view`;
Claude hook records keep their `hook-claude` source label. Both can include
savings from omitted passing test lines.
Explicit command views are marked `view` and have no measured token counts.
Reports cover all projects by
default; `--project [PATH]` selects one checkout (the current checkout if omitted).
Old records without project identity appear only in all-project reports. Daily,
weekly and monthly buckets use UTC, with Monday starting the week. `--since`
is inclusive and `--until` exclusive; each accepts a UTC date or Unix milliseconds.
`--command` and `--source` filter exact labels. CSV can export totals, periods or
history; export CSV history and period summaries separately. JSON includes
available measurement and parse diagnostics. These output-token measurements do
not establish model billing, input-cache costs, task success or total conversation
usage. Plain `compact` and `discover` do not change the usage history.

Configuration is `config.json` under `SIFT_CONFIG_DIR`, otherwise
`$XDG_CONFIG_HOME/sift` or `~/.config/sift` on Unix (including macOS), and
`%APPDATA%\Sift` on Windows. `sift config` prints it; `--create` writes defaults
without overwriting an existing file:

```json
{
  "enabled": true,
  "record_usage": true,
  "keep_originals": false,
  "exclude_commands": [],
  "originals_max_entries": 100,
  "originals_max_bytes": 104857600,
  "originals_max_days": 30,
  "semantic_selection": {
    "mode": "off",
    "allowed_projects": []
  }
}
```

Disable `record_usage` to stop recording. `enabled:false` disables automatic
compaction and command-wrapper compaction; explicit `compact` still works.
`exclude_commands` contains exact executable basenames for `run`, or exact tool
labels such as `Bash` for Claude or `bash` for completion plugins. Pre-execution
adapters also check executable names in their supported literal command forms;
they do not evaluate arbitrary scripts to discover exclusions.

Ordinary original output is saved only when `keep_originals:true`, for complete
bounded captures; it may contain sensitive data. Semantic selection is the one
exception: select mode must save its complete grep input before omitting passages,
even when ordinary retention is off. `recall` reads saved bytes without rerunning
a command. Streaming or inherited output is never accumulated for recall.
Original retention defaults to 100 entries, 100 MiB total and 30 days;
set positive `originals_max_entries`, `originals_max_bytes` and
`originals_max_days` to change these caps. Pruning happens when another original
is saved, not immediately when settings change. Oversized originals are skipped.
`recall` accepts an exact ID or a unique prefix. Without selectors it writes the
entire saved stream as raw bytes; navigation defaults to at most 200 matching
lines and permits up to 10,000. Metrics rotate at 10 MiB with one backup.
`gain --reset` clears metrics and leaves saved originals.

State uses `SIFT_STATE_DIR`, otherwise `$XDG_STATE_HOME/sift` or
`~/.local/state/sift` on Unix, and `%LOCALAPPDATA%\Sift` on Windows. Unix state
files/directories have private permissions. Local storage failures do not replace
command output or change a child's exit status. A busy usage lock skips the
record after a brief bounded wait; usage history is best effort.

### Selected history and imported usage

`discover --history PATH` reads only the selected file or directory. It supports
Claude and Codex saved-session shapes and reports potential savings from captured
text, along with missing, unsupported or limited measurements. Scans are bounded
and skip symlinks. `--since YYYY-MM-DD` filters by UTC command date;
`--project PATH` matches the recorded working directory exactly (unlike `gain`,
it does not resolve a checkout root). `--suggest` reports a small set of repeated
command-correction patterns. It never executes historical commands, writes agent rules or changes Sift usage records.
History reports omit paths, arguments and transcript text. Recognizing a Sift
call in a transcript does not prove it ran or saved tokens.

`ccusage --import FILE` reads an existing local daily/session JSON export,
including project-grouped daily exports. It does not run or install ccusage,
fetch prices, scan histories or persist the import. Missing counters remain
unavailable; totals are shown only when supplied, and inconsistent totals fail.
Reported USD may be calculated or incompletely priced by ccusage. It is neither
an invoice nor money saved by Sift, and remains separate from `gain`. Imports
are limited to a 32 MiB regular file; combined multi-section reports are unsupported.

## JSONL integration

Use a persistent process to load the tokenizer once:

```sh
./target/release/sift compact --protocol=json-v1
```

Send one JSON object per input line on stdin:

```json
{"version":1,"text":"short diagnostic\n","is_error":false,"complete":true,"tokenizer":"o200k_base"}
```

`version` and `text` are required. `is_error`, `complete`, and `tokenizer` are
optional, defaulting to `false`, `true`, and `o200k_base`. Both flags describe the
source; errors and incomplete results retain all supplied information. Unknown
fields, malformed requests, unsupported versions, and unsupported tokenizers
are errors. The only supported tokenizer is `o200k_base`.

Each successful line produces an immediately flushed response:

```text
{"version":1,"text":"...","encoding":"raw","input_tokens":N,"output_tokens":N}
```

`N` above denotes an integer token count. The `text` field alone is intended for
the model. Counts include every character of that field, including framing;
the response envelope is integration metadata and is not counted. Preserve
`encoding` separately if you need restoration. Plain mode does not emit this
metadata, so integrations that need reliable restoration should use JSONL.

A rejected request produces `{"version":1,"error":"..."}`. Processing continues
with the next line, and the process exits with status 1 at EOF if any request
failed. Consumers must reject error responses and retain the original tool
result if compaction fails. Never display an error envelope as the tool result
or mistake it for a successful empty response. Input/output failures also exit
nonzero, except for an ordinary closed downstream pipe.

## Explicit restoration and formats

```sh
./target/release/sift restore --encoding text-runs-v1 compacted.txt
./target/release/sift restore --encoding raw original.bin
```

Restoration requires an explicit encoding, so text that happens to resemble a
header cannot trigger decoding in raw mode. `raw` copies arbitrary bytes; the
other encodings require UTF-8. The library exposes `Compactor::new()`,
`Compactor::compact(&str)`, `CompactResult`, `Encoding`, and
`restore(Encoding, &str)` for the same workflow.

| Encoding | Representation and restoration |
| --- | --- |
| `raw` | Original content, unchanged. |
| `json-v1` | `JSON v1 (all values):\n` followed by a minified JSON value. Restore the value. |
| `json-rows-v1` | `JSON rows v1 (each row maps to the columns in order):\n` followed by `{"columns":[...],"rows":[...]}`. Each row's values map to its corresponding unique column names. |
| `json-min-v1` | One minified JSON value, without a header. Restore the value. |
| `json-columns-v1` | `JSON columns v1: arrays are columns; scalars repeat for all rows\n` followed by `{"rows":N,"columns":{...}}`. Each column is an N-element array of scalar values, or one scalar repeated for every row. |
| `text-runs-v1` | `sift:text-runs-v1 counts repeat exact JSON strings; concatenate\n` followed by a JSON array of `[count,string]` pairs. Concatenate each decoded string exactly `count` times. |
| `text-prefixes-v1` | `sift:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate\n` followed by a JSON array of literal strings or `[prefix,[suffixes]]` pairs. Copy literals; for each suffix copy its prefix then the suffix. |
| `text-refs-v1` | `sift:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n` followed by a JSON array of literal strings or backward references to earlier string entries. |
| `text-lines-v1` | `sift:lines-v1 [N,prefix] then N lines; prepend prefix\n` followed by blocks of one `[N,prefix]` JSON line and N literal suffix lines. Prepend that block's prefix to each suffix. |
| `text-symbols-v1` | `sift:symbols-v1 substitute each character using this JSON dictionary:\n` followed by one JSON dictionary line, then the literal body. Replace each original body character with its dictionary value when present. |

Here `\n` in a header means a literal LF byte. For example, this text-run payload
restores `ready` twice, each followed by CRLF, then `done` with no final newline:

```text
sift:text-runs-v1 counts repeat exact JSON strings; concatenate
[[2,"ready\r\n"],[1,"done"]]
```

Text encoding groups consecutive identical LF-delimited lines, retaining the LF
and any preceding CR. Adjacent unrepeated lines share one count-1 literal string
to avoid adding framing around every line. JSON string escaping safely represents quotes, markers,
Unicode, and control characters. Restoration accepts positive integer counts
and nonempty strings; an empty run array restores an empty string. Checked size
arithmetic rejects expansion beyond 64 MiB before allocating the restored text.
Inputs larger than this are ineligible for text-run encoding, rather than clipped.

Prefix encoding factors common prefixes across consecutive lines. Literal spans
remain in their original position; no separate dictionary or reordered lines are
needed. For example:

```text
sift:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate
["Checking files\n",["src/components/",["Button.rs\r\n","Dialog.rs\r\n"]],"done"]
```

This restores the heading, two complete paths with CRLF endings, then `done`
without a trailing newline. Prefixes end only at UTF-8 character boundaries.
The encoder greedily groups consecutive lines sharing the first pair's longest
common prefix, coalesces adjacent literals, and discards groups that add byte
overhead. The whole candidate, including its header, must still win by exact
token count. This does not promise the globally optimal prefix grouping.
Restoration accepts empty prefixes, suffixes, literal strings, and suffix arrays;
each expands according to the same rule. Checked total expansion is limited to
64 MiB, and larger source text is ineligible for prefix encoding.

Reference encoding keeps the first occurrence of a repeated line as a literal
string, then uses its zero-based array index for later occurrences. Literals and
references remain in output order, for example:

```text
sift:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N
["repeated diagnostic\r\n","other event\n",0,"done"]
```

This restores the diagnostic, the other event, the diagnostic again, and `done`
with no final newline. References must be unsigned integer tokens naming an
earlier string entry. Forward references, references to references, negative
numbers (including `-0`), fractions, and exponents are invalid. Empty strings and
arrays are allowed. Restored text is bounded to 64 MiB. The complete candidate
competes with raw and the other encodings by exact token count.

Additional reference candidates share common prefixes across nonadjacent lines.
A prefix is simply an earlier literal fragment in the same format:
`["src/components/","A.rs\n",0,"B.rs\r\n"]` restores two complete paths.
The encoder preserves worthwhile whole-line references, considers up to 32
prefixes ranked by potential byte savings, and emits at most a prefix and suffix
per segment. Actual complete token counts decide whether to use the result;
every existing candidate remains available, and ties retain the earlier choice.

Two segmentations compete independently: literal LF bytes and the two literal
characters backslash and `n`. The second can compact repeated paths or logs
inside serialized strings. It does not decode JSON or interpret source-code
escapes: every separator and byte stays in the fragment stream. Both use the
same `text-refs-v1` restoration rule and 64 MiB source/restoration bound.

Literal-line frames avoid JSON escaping around every suffix. Counts are positive
unsigned integers and prefixes contain no LF; suffixes retain their LF and any
preceding CR. Only the final suffix may lack LF. The input and checked restored
size are bounded to 64 MiB.

Symbol frames use unique single-character dictionary keys and string values.
Substitution is not recursive: a character inside an inserted value is never
replaced again. For example, `{"§":"src/"}` followed by `§a.rs\n§b.rs` restores
two paths. The encoder chooses up to 32 symbols absent from the original and
shares existing reference plans; complete token counts decide whether this
framing helps. The decoder accepts empty dictionaries/values and rejects
duplicate keys, malformed values, or expansion beyond 64 MiB before allocating
the restored body.

Symbol frames also try literal comma-separated fragments for inputs of at most
64 KiB and 4,096 fragments. Commas inside quotes are ordinary bytes; this does
not parse CSV or JSON. The complete frame must beat every existing candidate's
token count, and an equal count keeps the earlier representation.

JSON table conversion applies to uniform arrays of objects. Duplicate keys,
including escaped spellings of the same key, are unsupported and are not
normalized. JSON compaction/restoration is bounded to 16 MiB and nesting depth
64. Unsupported or malformed JSON remains eligible for reversible text encoding
or raw output. All columns, rows, and values must be present; invalid framing
fails restoration. Numeric values never pass through floating-point conversion.
Scalar-column tables exclude nested object/array cells; positional JSON rows
remain available for those values. Empty scalar-column tables use `rows:0` and
an empty `columns` object. No row, field, or value is discarded by either codec.

## Limits and checks

Compaction buffers the complete input, and JSONL buffers one request at a time.
It is intended for tool results that fit in memory. Exact tokenization can use
substantial time and memory for large inputs, especially long unbroken strings;
there is no approximate token-count shortcut or streaming chunk boundary that
could change the count. Codec limits disable candidates; they never silently
truncate input. Raw restoration streams bytes.

Rust library builds use rkyv's little-endian, aligned, 32-bit-pointer archive
format. A downstream crate cannot enable a conflicting rkyv format in the same
dependency graph; Cargo rejects those incompatible feature combinations.
The build host and target must have matching endianness so the generated
pre-tokenizer can be validated before embedding. The supported release targets
use little-endian hosts and targets.

Token reduction is an offline metric for this tokenizer. It does not establish
provider billing savings, fewer model turns, or unchanged agent success rates.
Automatic adapters are available for the hosts listed in [INTEGRATIONS.md](../INTEGRATIONS.md).
Other hosts use explicit commands or instructions. Adapters retain original output
when compaction fails.

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
node tests/pi_session.mjs
```

The code is MIT licensed; dependencies retain their own licenses.
