# Synthetic command comparison

`compare.py` compares native commands, `retok run -- COMMAND ...`, and native
RTK 0.49.0 subcommands. It needs Python 3.9+, Git, and ordinary POSIX command-line
tools. It uses only Python's standard library, installs nothing, and builds no
binaries. Use a fixed, release-mode Retok candidate after runner changes settle.

The [2026-09-17 release v0.3.0 results](results/2026-09-17-release-v0.3.0/README.md)
include sanitized samples, token and elapsed charts, and execution differences
for the final Linux x64 release. The [earlier v0.2.0 development run](results/2026-09-17-development-v0.2.0/README.md)
is retained unchanged. Neither run establishes an aggregate win over RTK.

```sh
python3 benchmarks/compare.py --self-test
python3 benchmarks/compare.py --retok /path/to/retok --rtk /path/to/rtk \
  --output /path/to/new-local-results --repeats 5 --save-raw
```

On a host with a required build governor, prefix both commands with
`ctx-build-governor exec --`. The caller is responsible for resource admission.
The output directory **must not exist**. Keep generated results outside the
source checkout or in an ignored runtime directory. Results contain local binary
paths, command paths, and execution working directories; review before sharing.
No historical commands, real repositories, user settings, or user stores are used.
HOME, XDG directories, Retok settings/state, temporary files, and Git configuration
are isolated under the output directory. RTK's normal local tracking is included
in its elapsed times, as is Retok's normal tracking. No network access is needed.

## Fixed workloads

| Workload | Native | RTK |
| --- | --- | --- |
| Mixed staged, unstaged, deleted and untracked files | `git status` | `rtk git status` |
| Three modified files and one deleted file | `git diff` | `rtk git diff` |
| Six recent commits | `git log -6` | `rtk git log -6` |
| Seventeen source files | `ls -l src` | `rtk ls -l src` |
| Warning lines in a 40-line log | `grep -n WARN service.log` | `rtk grep -n WARN service.log` |
| Twenty-four JSON records | `cat records.json` | `rtk json records.json` |
| Twelve synthetic tests, one failure and a stderr diagnostic | `fixture-check tests` | `rtk test fixture-check tests` |

The fixtures are intentionally modest. The test workload prints a synthetic test
report; it does not benchmark a compiler or a real test framework. Controls use
`true`, `printf 'ready\n'`, and eight flushed progress messages over about 480 ms.
RTK uses `proxy` for these controls: this is explicitly passthrough, not evidence
that an RTK filter handles arbitrary progress output. The progress fixture is a
Python process, so its native timing includes Python startup.
Fixture timestamps are fixed and Git's index stat cache is refreshed during setup,
before checking read-only command effects; intentional modified/deleted files remain.

## What the measurements mean

* `metadata.json` records binary versions, SHA-256 hashes, harness hash, environment
  platform, fixture hash, and methodology **before scores are produced**. The
  interface is pinned to RTK 0.49.0; another version is rejected. Native utilities
  and the synthetic helper have content hashes too.
* `results.json` retains all 5 or 7 elapsed samples, exit codes, output hashes,
  stream lengths, tokens, audit events, marker checks, and Retok candidate data.
  `results.csv` contains summaries; `elapsed.svg` plots end-to-end median times.
  Separately, `token.svg` plots the already-measured `median_tokens` for the same
  workload/arm rows, with no additional runs or aggregate winner. Its caption
  identifies ordinary `o200k_base` tokens summed across stdout and stderr and
  states that fewer tokens does not establish retention.
  `--save-raw` additionally keeps every timed stdout/stderr sample as local bytes.
* There is one unmeasured command warmup per arm. Timed arm order rotates each
  repetition. End-to-end elapsed time includes process startup, the native
  command, compaction, and local tracking. The no-op control estimates the
  small-command startup/runner cost; it is not subtracted from other workloads.
  Min/max accompany medians. Filesystem caches are warm, not forcibly flushed.
* A separate persistent Retok JSONL protocol warms its tokenizer once. Its
  `input_tokens` counts **each actual emitted stream**, including RTK's output,
  as ordinary `o200k_base`. Stdout and stderr counts are summed; there is no
  cross-stream token boundary. No byte/character estimate or RTK savings counter
  substitutes for token counts. This is the same Retok tokenizer for all arms,
  not an independent tiktoken verification.
* `median_warm_processing_ms` sums the median compact-protocol round trips for
  the original stdout and stderr. This excludes tokenizer startup but includes
  Python JSON serialization and pipe I/O. It is not an extracted runner-internal
  CPU time, and is not comparable to whole RTK command time as a speed ratio.
* PATH shims audit underlying command argv, cwd, and invocation count in separate
  **untimed** runs. Each shim records then execs the real native executable.
  Timings use the actual programs without shims. A matching audit establishes one
  observed native invocation with unchanged argv/cwd. Changed argv, multiple
  calls, and absent child events are explicitly classified. An absent event may
  mean an internal implementation or an absolute-path call; it does not prove
  that no work ran. Exit codes are also checked in every timed run.
* Fixture content hashes, including repository metadata, are checked before and
  after execution. They detect content changes, not access times or identical
  rewrites. This is bounded synthetic execution evidence, not a proof covering
  every subprocess or external side effect.
* Retok compacts each saved native stream through the explicit JSONL protocol,
  then `restore --encoding ENCODING` must reproduce text byte-for-byte, or preserve
  JSON values, types, and numeric lexemes for JSON encodings. JSON whitespace and
  object key order may change; byte equality is separately recorded even for JSON.
  Each timed
  Retok stream must equal that candidate or the exact original. Raw marker-like
  text is never inferred to be encoded. Streams shorter than 256 bytes bypass
  runner compaction; progress may stream raw after the runner's capture window.
  The saved protocol candidate can therefore differ from valid raw runner output.
* Selected literal markers are reported as present or absent in each arm's text.
  Absence means only that exact text is absent from the emitted representation;
  Retok's reversible encoding can also remove a literal from view. These are
  neither semantic-retention percentages nor invented reread/model-call costs.
  RTK's internal JSON reader and rewritten Git calls are useful workflow results
  but must not be described as identical native-command execution.

No aggregate winner, speed claim, billing estimate, or model-task-success claim
is generated. Memory is not measured. Shared-host elapsed times are descriptive
of this run, including scheduling noise. Compare retention, execution compatibility,
output tokens, and elapsed time together before making a practical product claim.
A Retok execution/restore/output mismatch makes the harness fail; RTK differences
remain visible results rather than being silently dropped.
