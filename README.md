# Retok

Retok reduces tool output before an agent reads it. It runs locally, keeps every
text byte or JSON value, and selects a compact representation only when its full
framing uses fewer ordinary `o200k_base` tokens. It is an independent project
inspired by RTK.

Use native output hooks to keep command execution and permissions with your
agent, or run any executable through `retok run`. There are no model calls,
telemetry, background services, or shell-profile changes. Supported JSON keeps
values, types, numeric lexemes, rows, and key associations; whitespace and object
key order can change.

```sh
retok init --replace-rtk     # Back up and migrate recognized RTK integrations
retok git status           # Or use an explicit command wrapper
retok gain                 # Local measured output savings
```

See [agent integrations and migration](INTEGRATIONS.md) for automatic coverage
and [RTK workflow coverage](COMPATIBILITY.md) for deliberate differences.

The [command benchmark and charts](benchmarks/results/2026-09-17-development-v0.2.0/README.md)
compare output tokens, retained information and elapsed time. In that synthetic
development run, Retok took about 100–112 ms on small workloads versus RTK's
2.7–14.5 ms. Tokenizer startup is a cost; Retok does not claim to be faster.

## Install

Install the latest [GitHub release](https://github.com/ctxrs/retok/releases).
Linux and macOS (x64 or ARM64; requires `curl` and `sha256sum` or `shasum`):

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/retok/main/install.sh | sh
```

Install and switch recognized RTK integrations in one command:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/retok/main/install.sh | sh -s -- --replace-rtk
```

Use `--init` instead to set up detected agents without removing RTK. Existing
Retok installations can use `retok init --replace-rtk` directly.

Windows x64, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/ctxrs/retok/main/install.ps1 | iex
```

To install and switch recognized RTK integrations on Windows:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/ctxrs/retok/main/install.ps1))) -ReplaceRtk
```

Supported release targets are Linux x64 and aarch64, macOS 13 or newer on x64
and arm64, and Windows x64. Windows ARM64 and 32-bit systems are not supported.
The Linux assets are statically linked musl executables and do not require
glibc. Retok does not claim compatibility with a specific older Linux
kernel.

Both installers download the binary and its `.third-party-notices.txt` sidecar,
verify both against the release's `SHA256SUMS`, and stage both in the installation
directory before replacing files. Notices are installed beside the binary as
`retok.third-party-notices.txt` on Unix or `retok.exe.third-party-notices.txt` on
Windows. The default directory is `~/.local/bin` on Unix and
`%LOCALAPPDATA%\Programs\Retok` on Windows. Add that directory to your PATH,
then run `retok --help`. No administrator access is needed. Installation alone
leaves agent settings untouched; `--init` or `--replace-rtk` explicitly runs setup.
Neither installer edits PATH or shell profiles.

These commands execute the installer from `ctxrs/retok` on GitHub over HTTPS.
The installers download release files and `SHA256SUMS` from that same repository
over HTTPS. This trusts GitHub, HTTPS, and the repository's maintainers: the
checksums detect mismatched or corrupted downloads, but are not an independent
signature and cannot protect against a compromised release and checksum file.
Starting with v0.1.1, macOS binaries carry a notarized Apple Developer ID
signature and the Windows binary carries a timestamped Authenticode signature.
The v0.1.0 binaries remain unsigned. The installers do not disable Gatekeeper,
SmartScreen, or other operating-system protections.

To select v0.2.0 or a different directory, set the environment variables for the
installer (either variable can be used on its own):

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/retok/main/install.sh | RETOK_VERSION=v0.2.0 RETOK_INSTALL_DIR="$HOME/.local/bin" sh
```

```powershell
$env:RETOK_VERSION = 'v0.2.0'
$env:RETOK_INSTALL_DIR = "$env:LOCALAPPDATA\Programs\Retok"
irm https://raw.githubusercontent.com/ctxrs/retok/main/install.ps1 | iex
```

Installer checks use synthetic releases and make no network requests:
`python3 tests/test_install.py` on Unix and
`powershell -NoProfile -File tests/install.Tests.ps1` on Windows
(`pwsh` also works).

## Build and use

Requires Rust 1.88 or newer. Dependencies and the tokenizer are downloaded at
build time; the tokenizer's vocabulary is embedded in the binary.

```sh
cargo build --release --locked
./target/release/retok compact output.txt
printf 'short diagnostic\n' | ./target/release/retok compact
./target/release/retok --help
```

`compact [FILE|-]` reads a complete file or stdin and writes only the selected
model-facing text. It adds no trailing newline. Invalid UTF-8 passes through
byte for byte. A short result normally stays unchanged because the entire
representation, including its explanatory header, must beat the original token
count. Ties keep the original. Special-token-looking strings are counted as
ordinary text, not special tokens.

## Run commands

```sh
retok run -- git status --short
retok cargo test
retok proxy git diff                 # Explicit raw output
retok run -- sh -c 'git log | tail -5' # Compact after the entire pipeline
```

`run` passes argv directly to the executable, inheriting stdin, environment and
working directory. It preserves stdout and stderr separately and returns the
child's exit status. It never retries a command to recover output. Shorthand
`retok COMMAND ...` has the same behavior; use `run --` for names that collide
with Retok's own commands.

TTY output passes through. For noninteractive output, Retok buffers up to 8 MiB
and waits up to 250 ms after the first output while the command is still running.
When either threshold is reached it streams the original bytes for the rest of
that command. Short, complete output is compacted; very small results and invalid
UTF-8 stay raw. Tokenizer work after completion adds processing time. This keeps
prompts and ongoing progress visible without truncating output.

Use Retok at the end of a programmatic pipeline. `retok git log | tail -5` would
make `tail` read the compact representation. To retain native pipeline semantics,
wrap the whole pipeline in an explicit shell as above, or let a supported native
output hook compact the final agent-visible result. Retok does not automatically
rewrite shell commands or install permission allow rules.

`retok pipe` and `retok read` are aliases for `compact`, with the same single-file
arguments. They do not implement RTK's lossy filtering flags.

## Local usage and original output

```sh
retok gain --daily --graph
retok gain --json --history
retok config --create
retok recall --list
retok recall ID                      # Original stdout, when saved
retok recall ID --stderr
retok discover --json output.txt     # Potential savings; never executes input
```

Automatic integrations and captured `run` output record local counts, timing,
status, and a short tool/executable label. They do not record command arguments.
`gain` reports measured ordinary `o200k_base` tokens; unmeasured streams are
excluded. These are output-token measurements, not claims about model billing,
input-cache costs, task success, or total conversation usage. Plain `compact`
and `discover` do not change the usage history.

Configuration is `config.json` under `RETOK_CONFIG_DIR`, otherwise
`$XDG_CONFIG_HOME/retok` or `~/.config/retok` on Unix (including macOS), and
`%APPDATA%\Retok` on Windows. `retok config` prints it; `--create` writes defaults
without overwriting an existing file:

```json
{"enabled":true,"record_usage":true,"keep_originals":false,"exclude_commands":[]}
```

Disable `record_usage` to stop recording. `enabled:false` disables automatic
compaction and command-wrapper compaction; explicit `compact` still works.
`exclude_commands` contains exact executable basenames for `run`, or exact tool
labels such as `Bash` for Claude or `bash` for plugins. No shell command is parsed to infer an
executable inside a script.

Original output is saved only when `keep_originals:true`, for complete bounded
captures; it may contain sensitive data. `recall` reads the saved bytes without
rerunning a command. Streaming or inherited output is never accumulated for
recall. Original storage is limited to 100 entries, 100 MiB, and 30 days, pruned
when another original is saved. Metrics rotate at 10 MiB with one backup.
`gain --reset` clears metrics and leaves saved originals.

State uses `RETOK_STATE_DIR`, otherwise `$XDG_STATE_HOME/retok` or
`~/.local/state/retok` on Unix, and `%LOCALAPPDATA%\Retok` on Windows. Unix state
files/directories have private permissions. Local storage failures do not replace
command output or change a child's exit status. A busy usage lock skips the
record after a brief bounded wait; usage history is best effort.

## JSONL integration

Use a persistent process to load the tokenizer once:

```sh
./target/release/retok compact --protocol=json-v1
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
./target/release/retok restore --encoding text-runs-v1 compacted.txt
./target/release/retok restore --encoding raw original.bin
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
| `text-runs-v1` | `retok:text-runs-v1 counts repeat exact JSON strings; concatenate\n` followed by a JSON array of `[count,string]` pairs. Concatenate each decoded string exactly `count` times. |
| `text-prefixes-v1` | `retok:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate\n` followed by a JSON array of literal strings or `[prefix,[suffixes]]` pairs. Copy literals; for each suffix copy its prefix then the suffix. |
| `text-refs-v1` | `retok:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n` followed by a JSON array of literal strings or backward references to earlier string entries. |

Here `\n` in a header means a literal LF byte. For example, this text-run payload
restores `ready` twice, each followed by CRLF, then `done` with no final newline:

```text
retok:text-runs-v1 counts repeat exact JSON strings; concatenate
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
retok:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate
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
retok:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N
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

JSON table conversion applies to uniform arrays of objects. Duplicate keys,
including escaped spellings of the same key, are unsupported and are not
normalized. JSON compaction/restoration is bounded to 16 MiB and nesting depth
64. Unsupported or malformed JSON remains eligible for reversible text encoding
or raw output. All columns, rows, and values must be present; invalid framing
fails restoration. Numeric values never pass through floating-point conversion.

## Limits and checks

Compaction buffers the complete input, and JSONL buffers one request at a time.
It is intended for tool results that fit in memory. Exact tokenization can use
substantial time and memory for large inputs, especially long unbroken strings;
there is no approximate token-count shortcut or streaming chunk boundary that
could change the count. Codec limits disable candidates; they never silently
truncate input. Raw restoration streams bytes.

Token reduction is an offline metric for this tokenizer. It does not establish
provider billing savings, fewer model turns, or unchanged agent success rates.
Automatic adapters are available for the hosts listed in [INTEGRATIONS.md](INTEGRATIONS.md).
Other hosts use explicit commands or instructions. Adapters retain original output
when compaction fails.

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

The code is MIT licensed; dependencies retain their own licenses.
