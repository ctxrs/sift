# Retok

Retok compacts tool output locally, choosing a reversible representation only
when it uses fewer ordinary `o200k_base` tokens than the original. It is an
independent project inspired by RTK's tool-output workflow. It is not affiliated
with RTK and does not claim RTK command parity.

Retok preserves repeated text byte for byte. For supported JSON it preserves
every value, type, numeric lexeme, row, and key association; JSON whitespace may
change. Other content passes through. No telemetry, model calls, shell execution,
background services, or automatic agent/settings changes are involved.

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

For shell pipelines, use your shell's native exit-status handling. In Bash:

```bash
set -o pipefail
your-command 2>&1 | ./target/release/retok compact
```

`pipefail` makes a failed upstream command produce a failed pipeline; it returns
the rightmost failing command's status. If you need the original command's exact
status regardless of Retok's status, capture Bash's `PIPESTATUS` array immediately
after the pipeline and use element zero. Retok itself does not run commands or
change their effects. A closed downstream pipe is treated as normal completion.

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
Retok supplies representations, not an automatic host integration or universal
agent hook. Adapters must retain original output when compaction fails.

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

The code is MIT licensed; dependencies retain their own licenses.
