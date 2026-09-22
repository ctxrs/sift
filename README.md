<img src="docs/assets/sift-readme-banner.svg" alt="47% of tool output is unhelpful noise. sift uses Jev to remove it safely." width="100%">

Sift uses deterministic local compaction to cut noisy tool output before it
reaches your coding agent. An optional Jev selector can reduce eligible Pi
`grep` results further.

Sift's runner preserves the requested command arguments and executes the command
once. Generic compaction is reversible. For shorter command views, enable
original retention to recover the complete captured output.

## Why Sift

* **Use less context.** Spend tokens on the task instead of repeated paths, logs,
    and formatting.
* **Keep the useful information.** Generic text and JSON compaction can be
    restored. Git and test views retain the details needed to act on failures.
* **Stay close to native speed.** Sift is one native binary with no language
    runtime, daemon, or background service.
* **Run locally by default.** Ordinary compaction makes no network calls and
    Sift has no telemetry.
* **See the result.** `sift gain` reports measured local token savings. Optional
    original retention lets you recover complete captured output with
    `sift recall`.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/sift/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/ctxrs/sift/main/install.ps1 | iex
```

Set up supported coding agents Sift detects, then restart the agent:

```sh
sift init
sift doctor
```

To switch recognized RTK integrations, run `sift init --replace-rtk`. Sift backs
up managed settings and leaves unsupported or customized RTK integrations in
place.

## Try it

Run commands through Sift:

```sh
sift git status
sift cargo test
sift gain
```

After `sift init`, supported agents use Sift automatically. You can also compact
a saved result or stdin directly:

```sh
sift compact build.log
long-command | sift compact
```

Sift only chooses a representation when the complete result uses fewer
`o200k_base` tokens. Short output, live progress, and binary data pass through.
When compaction is not useful or supported, the original output wins.

## Why Sift instead of RTK

Sift favors broad, conservative compaction over a large collection of
command-specific summaries. That gives it consistent behavior on arbitrary tool
output and makes generic compaction reversible. It also has automatic agent
setup, local savings reports, optional original-output recovery, and a
one-command migration for recognized RTK integrations.

In the [v0.4.0 release benchmark](benchmarks/results/2026-09-21-release-v0.4.0/README.md),
Sift was faster than RTK 0.49 on all ten synthetic workloads and stayed within
1.35 ms of running the command directly. Across the seven substantive workloads,
Sift reduced output from 2,662 to 1,518 tokens, or 43%. RTK emitted fewer
tokens on five command-specific cases and tied on two. This benchmark used local
compaction and command views; Jev was off.

## How it works

Sift tries several compact representations for complete output and counts the
full result with its embedded tokenizer. It uses the smallest result only when
it beats the original. Text stays byte-exact after restoration. Supported JSON
keeps values, types, and number spellings.

For recognized Git status and test output, Sift can use a shorter presentation.
Failures, diagnostics, ignored tests, totals, paths, and repository state remain.
You can also ask explicitly for line, JSON-field, summary, error, or test views
when omission is what you want.

An optional semantic selector can reduce large Pi `grep` results further. It is
off by default, limited to explicitly allowed projects, and saves the complete
original for `sift recall`. It uses TypeSafe Jev and therefore sends eligible
passages to an external service. See the reference before enabling it.

## Learn more

* Run `sift --help` or read the [command reference](docs/reference.md)
* [Agent setup and supported integrations](INTEGRATIONS.md)
* [RTK migration and workflow differences](COMPATIBILITY.md)
* [Benchmark method and results](benchmarks/README.md)
* [Releases and manual downloads](https://github.com/ctxrs/sift/releases)

## Build from source

Sift requires Rust 1.88 or newer:

```sh
cargo build --release --locked
```

Sift is licensed under the MIT License.
