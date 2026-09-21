use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sift::{CompactResult, Compactor, Encoding};

mod command_view;
mod discover;
mod filter;
mod hooks;
mod pre_hooks;
mod rewrite;
mod runner;
mod setup;
mod state;
mod usage;
mod views;

const HELP: &str = "Sift — token-counted tool output compaction

Usage:
  sift init [--agent HOST] [--replace-rtk] [--project] [--dry-run]
  sift init --uninstall [--agent HOST]
  sift doctor [--agent HOST]
  sift hook HOST
  sift rewrite [--json] [--shell posix|powershell] -- 'COMMAND'
  sift run [--raw|--capture] -- COMMAND [ARG...]
  sift COMMAND [ARG...]
  sift proxy COMMAND [ARG...]
  sift gain [--json] [--history] [--daily] [--graph]
  sift ccusage --import FILE [--json|--csv]
  sift config [--create]
  sift recall --list | ID [--stderr]
  sift discover [--json] [FILE ...]
  sift discover --history PATH [--suggest] [--json]
  sift read [FILE|-] [--from N] [--lines N] [--grep TEXT]
  sift json [FILE|-] [--pointer POINTER] [--field KEY] [--limit N]
  sift summary|err|test [OPTIONS] -- COMMAND [ARG...]
  sift filter [--capture]
  sift compact [FILE|-]
  sift compact --protocol=json-v1
  sift restore --encoding ENCODING [FILE|-]

Encodings: raw, json-v1, json-rows-v1, json-min-v1, json-columns-v1,
           text-runs-v1, text-prefixes-v1, text-refs-v1, text-lines-v1,
           text-symbols-v1

Read stdin when FILE is omitted or '-'. Use '--' before a filename starting '-'.
Plain compact writes only the selected representation, without adding a newline.
JSONL protocol returns encoding and exact ordinary o200k_base token counts.
Restore requires an explicit encoding; raw mode also preserves non-UTF-8 bytes.
Run executes argv directly, preserving stdin, streams and exit status.
Run may abbreviate ordinary Git status and passing Cargo test rows; --raw preserves output.
Interactive and long-running output passes through; proxy always passes through.
";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u64,
    text: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default = "default_complete")]
    complete: bool,
    #[serde(default)]
    tokenizer: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    id: u64,
    request: Request,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    delivered_view: bool,
}

#[derive(Serialize)]
struct SessionResponse {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic: Option<bool>,
    #[serde(flatten)]
    response: Response,
}

fn default_complete() -> bool {
    true
}

#[derive(Serialize)]
struct Response {
    version: u8,
    #[serde(flatten)]
    result: CompactResult,
}

fn protocol(
    input: impl BufRead,
    mut output: impl Write,
    source: Option<&str>,
    tool: Option<&str>,
    session: bool,
) -> Result<bool> {
    let compactor = Compactor::new().context("cannot initialize tokenizer")?;
    let settings = source
        .filter(|_| !session)
        .map(|_| state::Settings::load())
        .transpose()?;
    if session {
        output.write_all(b"{\"version\":1,\"session\":1}\n")?;
        output.flush()?;
    }
    let mut input = input;
    let mut line = Vec::new();
    let mut failed = false;
    let mut last_id = 0;
    loop {
        line.clear();
        let read = if session {
            // JSON escaping can expand an 8 MiB text by six; bound framing too.
            let read = input
                .by_ref()
                .take(48 * 1024 * 1024 + 1025)
                .read_until(b'\n', &mut line)?;
            ensure!(
                line.len() <= 48 * 1024 * 1024 + 1024,
                "session request too large"
            );
            read
        } else {
            input.read_until(b'\n', &mut line)?
        };
        if read == 0 {
            break;
        }
        let mut id = None;
        let mut semantic = false;
        let result = (|| -> Result<(Response, Option<(state::Event, String)>)> {
            let invalid = |error: serde_json::Error| {
                anyhow::anyhow!(
                    "invalid JSON request at line {}, column {}",
                    error.line(),
                    error.column()
                )
            };
            let mut raw = false;
            let mut semantic_argv = None;
            let request: Request = if session {
                let envelope: SessionRequest = serde_json::from_slice(&line).map_err(invalid)?;
                ensure!(
                    envelope.id > last_id && envelope.id <= 9_007_199_254_740_991,
                    "session ID must increase within the safe integer range"
                );
                last_id = envelope.id;
                id = Some(envelope.id);
                ensure!(
                    envelope.request.text.len() + envelope.command.as_ref().map_or(0, String::len)
                        <= 8 * 1024 * 1024,
                    "session command and text too large"
                );
                let argv = envelope.command.as_deref().and_then(hooks::literal_argv);
                raw = argv.as_deref().is_some_and(hooks::explicit_raw);
                if envelope.delivered_view {
                    semantic_argv = argv;
                }
                envelope.request
            } else {
                serde_json::from_slice(&line).map_err(invalid)?
            };
            let fresh_settings = if session {
                source.map(|_| state::Settings::load()).transpose()?
            } else {
                None
            };
            let settings = if session { &fresh_settings } else { &settings };
            ensure!(
                request.version == 1,
                "unsupported protocol version; expected 1"
            );
            ensure!(
                request
                    .tokenizer
                    .as_deref()
                    .is_none_or(|name| name == "o200k_base"),
                "unsupported tokenizer; expected o200k_base"
            );
            // Error/completion flags never authorize omissions or establish an
            // exit status. Only the explicit Pi session contract permits a view.
            let _ = (request.is_error, request.complete);
            let started = std::time::Instant::now();
            let result = if !raw
                && settings
                    .as_ref()
                    .is_none_or(|s| s.enabled && tool.is_none_or(|name| !s.excludes(name)))
            {
                let mut original = compactor.compact(&request.text);
                if let Some(proposal) = semantic_argv
                    .as_deref()
                    .and_then(|argv| command_view::delivered_candidate(argv, &request.text))
                {
                    let selected = compactor.compact(&proposal);
                    if selected.output_tokens < original.output_tokens {
                        original = CompactResult {
                            input_tokens: original.input_tokens,
                            ..selected
                        };
                        semantic = true;
                    }
                }
                original
            } else {
                let tokens = compactor.count_tokens(&request.text);
                CompactResult {
                    text: request.text.clone(),
                    encoding: Encoding::Raw,
                    input_tokens: tokens,
                    output_tokens: tokens,
                }
            };
            let record = source.filter(|_| !raw).map(|source| {
                let event = state::Event {
                    unix_millis: state::unix_millis(),
                    command: tool.unwrap_or("tool-output").into(),
                    input_tokens: Some(result.input_tokens as u64),
                    output_tokens: Some(result.output_tokens as u64),
                    input_bytes: request.text.len() as u64,
                    output_bytes: result.text.len() as u64,
                    duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    exit_code: None,
                    source: Some(source.into()),
                    original_id: None,
                };
                (event, request.text)
            });
            Ok((Response { version: 1, result }, record))
        })();
        let record = match result {
            Ok((response, record)) => {
                if let Some(id) = id {
                    serde_json::to_writer(
                        &mut output,
                        &SessionResponse {
                            id,
                            semantic: semantic.then_some(true),
                            response,
                        },
                    )?;
                } else {
                    serde_json::to_writer(&mut output, &response)?;
                }
                record
            }
            Err(error) => {
                failed = true;
                serde_json::to_writer(
                    &mut output,
                    &serde_json::json!({"version": 1, "error": error.to_string()}),
                )?;
                None
            }
        };
        output.write_all(b"\n")?;
        output.flush()?;
        if let Some((event, original)) = record {
            // Count only successfully delivered responses. Optional storage
            // cannot replace output or turn successful delivery into failure.
            let _ = state::record(event, Some((original.as_bytes(), &[])));
        }
        if session {
            // Completion acknowledges the recording attempt, never storage or
            // provider delivery. Errors retire the session before another request.
            ensure!(!failed, "session request failed");
            serde_json::to_writer(
                &mut output,
                &serde_json::json!({"version":1,"id":id,"done":true}),
            )?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(failed)
}

fn parse_encoding(value: &str) -> Result<Encoding> {
    match value {
        "raw" => Ok(Encoding::Raw),
        "json-v1" => Ok(Encoding::JsonV1),
        "json-rows-v1" => Ok(Encoding::JsonRowsV1),
        "json-min-v1" => Ok(Encoding::JsonMinV1),
        "json-columns-v1" => Ok(Encoding::JsonColumnsV1),
        "text-runs-v1" => Ok(Encoding::TextRunsV1),
        "text-prefixes-v1" => Ok(Encoding::TextPrefixesV1),
        "text-refs-v1" => Ok(Encoding::TextRefsV1),
        "text-lines-v1" => Ok(Encoding::TextLinesV1),
        "text-symbols-v1" => Ok(Encoding::TextSymbolsV1),
        _ => bail!("unsupported encoding; use 'sift --help' for supported encodings"),
    }
}

fn execute(args: &[OsString], raw: bool, capture: bool, view: Option<&views::View>) -> Result<i32> {
    let settings = match state::Settings::load() {
        Ok(settings) => settings,
        Err(error) => {
            let _ = writeln!(
                io::stderr(),
                "sift: {error:#}; passing command output through"
            );
            state::Settings {
                enabled: false,
                record_usage: false,
                ..Default::default()
            }
        }
    };
    let excluded = args
        .first()
        .is_some_and(|arg| settings.excludes(&arg.to_string_lossy()));
    let command = args
        .first()
        .map(|arg| {
            std::path::Path::new(arg)
                .file_name()
                .unwrap_or(arg)
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "external".into());
    let command = if command
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
        && command.len() <= 128
    {
        command
    } else {
        "external".into()
    };
    let mut semantic_compactor = None;
    runner::run_presented(
        args,
        runner::Options {
            raw: raw || !settings.enabled || excluded,
            capture,
        },
        |bytes, stderr| {
            if let Some(view) = view {
                return views::render(view, bytes)
                    .ok()
                    .map(runner::Presentation::Bytes);
            }
            let text = std::str::from_utf8(bytes).ok()?;
            let proposal = command_view::candidate(args, text, stderr)?;
            let compactor = semantic_compactor
                .get_or_insert_with(Compactor::new)
                .as_ref()
                .ok()?;
            let original = compactor.compact(text);
            let selected = compactor.compact(&proposal);
            if selected.output_tokens >= original.output_tokens {
                return Some(runner::Presentation::Compacted(original));
            }
            Some(runner::Presentation::Semantic {
                bytes: selected.text.into_bytes(),
                tokens: (original.input_tokens, selected.output_tokens),
            })
        },
        |observation| {
            let (
                Some(stdout_bytes),
                Some(stderr_bytes),
                Some(stdout_emitted),
                Some(stderr_emitted),
            ) = (
                observation.stdout.read_bytes,
                observation.stderr.read_bytes,
                observation.stdout.emitted_bytes,
                observation.stderr.emitted_bytes,
            )
            else {
                return;
            };
            let semantic = observation.stdout.presented_tokens.is_some()
                || observation.stderr.presented_tokens.is_some();
            let counts = |stream: &runner::StreamObservation<'_>| {
                stream
                    .presented_tokens
                    .map(|(input, output)| (input as u64, output as u64))
                    .or_else(|| {
                        stream
                            .compacted
                            .map(|r| (r.input_tokens as u64, r.output_tokens as u64))
                    })
                    .or_else(|| (stream.read_bytes == Some(0)).then_some((0, 0)))
                    .or_else(|| {
                        // A semantic proposal already initialized the shared
                        // tokenizer. Count the other complete raw stream too,
                        // without charging startup to ordinary tiny passthrough.
                        if !semantic || view.is_some() {
                            return None;
                        }
                        let text = std::str::from_utf8(stream.original?).ok()?;
                        let tokens = Compactor::new().ok()?.count_tokens(text) as u64;
                        Some((tokens, tokens))
                    })
            };
            let tokens = counts(&observation.stdout)
                .zip(counts(&observation.stderr))
                .map(|(out, err)| (out.0 + err.0, out.1 + err.1));
            let event = state::Event {
                unix_millis: state::unix_millis(),
                command,
                input_tokens: tokens.map(|t| t.0),
                output_tokens: tokens.map(|t| t.1),
                input_bytes: stdout_bytes + stderr_bytes,
                output_bytes: stdout_emitted + stderr_emitted,
                duration_ms: observation.duration.as_millis().min(u64::MAX as u128) as u64,
                exit_code: Some(observation.status),
                source: Some(
                    if view.is_some() {
                        "view"
                    } else if semantic {
                        "run-view"
                    } else {
                        "run"
                    }
                    .into(),
                ),
                original_id: None,
            };
            let originals = observation.stdout.original.zip(observation.stderr.original);
            // Usage storage is optional and cannot turn a successful command into a failure.
            let _ = state::record(event, originals);
        },
    )
}

fn run() -> Result<i32> {
    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>().into_iter();
    let Some(command) = args.next() else {
        bail!("missing command; use 'sift --help'");
    };
    if command == "--help" || command == "-h" {
        io::stdout().write_all(HELP.as_bytes())?;
        return Ok(0);
    }
    if command == "--version" {
        writeln!(io::stdout(), "Sift {}", env!("CARGO_PKG_VERSION"))?;
        return Ok(0);
    }
    if command == "init" || command == "doctor" {
        let remaining: Vec<_> = args.collect();
        if command == "init" {
            setup::run(&remaining)?;
        } else {
            setup::doctor(&remaining)?;
        }
        return Ok(0);
    }
    if command == "hook" {
        let host = args
            .next()
            .and_then(|s| s.into_string().ok())
            .context("hook requires a supported agent name")?;
        ensure!(
            [
                "claude", "copilot", "hermes", "codex", "cursor", "gemini", "vscode", "droid",
                "vibe"
            ]
            .contains(&host.as_str())
                && args.next().is_none(),
            "hook requires a supported agent name"
        );
        if !matches!(host.as_str(), "claude" | "copilot" | "hermes") {
            return pre_hooks::run(&host);
        }
        hooks::run(&host)?;
        return Ok(0);
    }
    if command == "rewrite" {
        return rewrite::run(&args.collect::<Vec<_>>());
    }
    if command == "filter" {
        let remaining: Vec<_> = args.collect();
        ensure!(
            remaining.is_empty() || remaining == [OsString::from("--capture")],
            "filter accepts only --capture"
        );
        filter::run(!remaining.is_empty())?;
        return Ok(0);
    }
    if command == "gain" || command == "config" || command == "recall" {
        let remaining: Vec<_> = args.collect();
        match command.to_str().unwrap() {
            "gain" => state::gain(&remaining)?,
            "config" => state::config(&remaining)?,
            _ => state::recall(&remaining)?,
        }
        return Ok(0);
    }
    if command == "discover" {
        discover::run(&args.collect::<Vec<_>>())?;
        return Ok(0);
    }
    if command == "ccusage" {
        usage::run(&args.collect::<Vec<_>>())?;
        return Ok(0);
    }
    if matches!(
        command.to_str(),
        Some("read" | "json" | "summary" | "err" | "test")
    ) {
        match views::parse(command.to_str().unwrap(), &args.collect::<Vec<_>>())? {
            views::Action::Help => {
                io::stdout().write_all(views::HELP.as_bytes())?;
                return Ok(0);
            }
            views::Action::Input { path, view }
                if command == "read" && !view.is_read_selection() =>
            {
                args = path
                    .map(|path| vec![OsString::from("--"), path])
                    .unwrap_or_default()
                    .into_iter();
            }
            views::Action::Input { path, view } => {
                views::run_input(path.as_deref(), &view)?;
                return Ok(0);
            }
            views::Action::Command { argv, view } => {
                return execute(&argv, false, true, Some(&view));
            }
        }
    }
    let command = if command == "pipe" || command == "read" {
        OsString::from("compact")
    } else {
        command
    };
    if command != "compact" && command != "restore" {
        if command == "run" || command == "proxy" {
            let mut remaining: Vec<_> = args.collect();
            let mut raw = command == "proxy";
            let mut capture = false;
            while remaining
                .first()
                .is_some_and(|arg| arg == "--raw" || arg == "--capture")
            {
                let flag = remaining.remove(0);
                raw |= flag == "--raw";
                capture |= flag == "--capture";
            }
            if remaining
                .first()
                .is_some_and(|arg| arg == "--help" || arg == "-h")
            {
                io::stdout().write_all(HELP.as_bytes())?;
                return Ok(0);
            }
            if remaining.first().is_some_and(|arg| arg == "--") {
                remaining.remove(0);
            }
            return execute(&remaining, raw, capture, None);
        }
        ensure!(
            !command.to_string_lossy().starts_with('-'),
            "unknown option; use 'sift --help'"
        );
        return execute(
            &std::iter::once(command).chain(args).collect::<Vec<_>>(),
            false,
            false,
            None,
        );
    }
    let mut file: Option<OsString> = None;
    let mut encoding = None;
    let mut jsonl = false;
    let mut session = false;
    let mut record_source = None;
    let mut record_tool = None;
    let mut positional = false;
    while let Some(arg) = args.next() {
        let value = arg.to_str();
        if !positional && value == Some("--") {
            positional = true;
        } else if !positional && matches!(value, Some("--help" | "-h")) {
            io::stdout().write_all(HELP.as_bytes())?;
            return Ok(0);
        } else if !positional
            && value.is_some_and(|v| v == "--record-source" || v.starts_with("--record-source="))
        {
            ensure!(
                command == "compact" && record_source.is_none(),
                "--record-source is allowed once for compact only"
            );
            let source = option_value(value.unwrap(), &mut args)?;
            ensure!(
                ["pi", "omp", "opencode", "kilo"].contains(&source.as_str()),
                "unsupported integration source"
            );
            record_source = Some(source);
        } else if !positional
            && value.is_some_and(|v| v == "--record-tool" || v.starts_with("--record-tool="))
        {
            ensure!(
                command == "compact" && record_tool.is_none(),
                "--record-tool is allowed once for compact only"
            );
            let tool = option_value(value.unwrap(), &mut args)?;
            ensure!(
                ["bash", "powershell", "exec"].contains(&tool.as_str()),
                "unsupported integration tool"
            );
            record_tool = Some(tool);
        } else if !positional
            && value.is_some_and(|v| v == "--protocol" || v.starts_with("--protocol="))
        {
            ensure!(
                command == "compact" && !jsonl,
                "--protocol is allowed once for compact only"
            );
            let option = option_value(value.unwrap(), &mut args)?;
            ensure!(
                option == "json-v1" || option == "session-v1",
                "unsupported protocol; expected json-v1 or session-v1"
            );
            session = option == "session-v1";
            jsonl = true;
        } else if !positional
            && value.is_some_and(|v| v == "--encoding" || v.starts_with("--encoding="))
        {
            ensure!(
                command == "restore" && encoding.is_none(),
                "--encoding is allowed once for restore only"
            );
            encoding = Some(parse_encoding(&option_value(value.unwrap(), &mut args)?)?);
        } else {
            ensure!(
                positional || !value.is_some_and(|v| v.starts_with('-') && v != "-"),
                "unknown option; use 'sift --help'"
            );
            ensure!(file.is_none(), "expected at most one input file");
            file = Some(arg);
        }
    }
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    ensure!(
        record_tool.is_none() || record_source.is_some(),
        "--record-tool requires --record-source"
    );
    if jsonl {
        ensure!(file.is_none(), "JSONL protocol reads stdin; omit FILE");
        ensure!(
            !session
                || (record_source.as_deref() == Some("pi")
                    && record_tool.as_deref() == Some("bash")),
            "session-v1 requires Pi Bash recording context"
        );
        return protocol(
            io::stdin().lock(),
            output,
            record_source.as_deref(),
            record_tool.as_deref(),
            session,
        )
        .map(i32::from);
    }
    ensure!(
        record_source.is_none(),
        "--record-source requires --protocol=json-v1"
    );
    if command == "restore" {
        ensure!(encoding.is_some(), "restore requires --encoding");
    }
    let mut input: Box<dyn Read> = match file {
        Some(path) if path != "-" => Box::new(BufReader::new(
            File::open(path).context("cannot open input file")?,
        )),
        _ => Box::new(io::stdin().lock()),
    };
    if encoding == Some(Encoding::Raw) {
        io::copy(&mut input, &mut output)?;
    } else {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).context("cannot read input")?;
        match (encoding, std::str::from_utf8(&bytes)) {
            (None, Err(_)) => output.write_all(&bytes)?,
            (None, Ok(text)) => {
                output.write_all(Compactor::new()?.compact(text).text.as_bytes())?
            }
            (Some(encoding), Ok(text)) => {
                output.write_all(sift::restore(encoding, text)?.as_bytes())?
            }
            (Some(_), Err(_)) => {
                bail!("encoded input must be UTF-8; raw mode accepts arbitrary bytes")
            }
        }
    }
    output.flush()?;
    Ok(0)
}

fn option_value(arg: &str, args: &mut impl Iterator<Item = OsString>) -> Result<String> {
    if let Some((_, value)) = arg.split_once('=') {
        return Ok(value.to_owned());
    }
    args.next()
        .context("missing option value")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("option value must be UTF-8"))
}

fn main() {
    let status = match run() {
        Ok(status) => status,
        Err(error) if is_broken_pipe(&error) => 0,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "sift: {error:#}");
            1
        }
    };
    std::process::exit(status);
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            || cause
                .downcast_ref::<serde_json::Error>()
                .is_some_and(|error| error.io_error_kind() == Some(io::ErrorKind::BrokenPipe))
    })
}
