use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};

use anyhow::{Context, Result, bail, ensure};
use retok::{CompactResult, Compactor, Encoding};
use serde::{Deserialize, Serialize};

const HELP: &str = "Retok — lossless, token-counted tool output compaction

Usage:
  retok compact [FILE|-]
  retok compact --protocol=json-v1
  retok restore --encoding ENCODING [FILE|-]

Encodings: raw, json-v1, json-rows-v1, text-runs-v1, text-prefixes-v1, text-refs-v1

Read stdin when FILE is omitted or '-'. Use '--' before a filename starting '-'.
Plain compact writes only the selected representation, without adding a newline.
JSONL protocol returns encoding and exact ordinary o200k_base token counts.
Restore requires an explicit encoding; raw mode also preserves non-UTF-8 bytes.
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

fn default_complete() -> bool {
    true
}

#[derive(Serialize)]
struct Response {
    version: u8,
    #[serde(flatten)]
    result: CompactResult,
}

fn protocol(input: impl BufRead, mut output: impl Write) -> Result<bool> {
    let compactor = Compactor::new().context("cannot initialize tokenizer")?;
    let mut input = input;
    let mut line = Vec::new();
    let mut failed = false;
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let result = (|| -> Result<Response> {
            let request: Request = serde_json::from_slice(&line).map_err(|error| {
                anyhow::anyhow!(
                    "invalid JSON request at line {}, column {}",
                    error.line(),
                    error.column()
                )
            })?;
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
            // Error/incomplete payloads get the same lossless selection. These
            // flags describe the source, never permission to drop information.
            let _ = (request.is_error, request.complete);
            Ok(Response {
                version: 1,
                result: compactor.compact(&request.text),
            })
        })();
        match result {
            Ok(response) => serde_json::to_writer(&mut output, &response)?,
            Err(error) => {
                failed = true;
                serde_json::to_writer(
                    &mut output,
                    &serde_json::json!({"version": 1, "error": error.to_string()}),
                )?;
            }
        }
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(failed)
}

fn parse_encoding(value: &str) -> Result<Encoding> {
    match value {
        "raw" => Ok(Encoding::Raw),
        "json-v1" => Ok(Encoding::JsonV1),
        "json-rows-v1" => Ok(Encoding::JsonRowsV1),
        "text-runs-v1" => Ok(Encoding::TextRunsV1),
        "text-prefixes-v1" => Ok(Encoding::TextPrefixesV1),
        "text-refs-v1" => Ok(Encoding::TextRefsV1),
        _ => bail!("unsupported encoding; use 'retok --help' for supported encodings"),
    }
}

fn run() -> Result<bool> {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        bail!("missing command; use 'retok --help'");
    };
    if command == "--help" || command == "-h" {
        io::stdout().write_all(HELP.as_bytes())?;
        return Ok(false);
    }
    if command == "--version" {
        writeln!(io::stdout(), "Retok {}", env!("CARGO_PKG_VERSION"))?;
        return Ok(false);
    }
    ensure!(
        command == "compact" || command == "restore",
        "unknown command; use 'retok --help'"
    );
    let mut file: Option<OsString> = None;
    let mut encoding = None;
    let mut jsonl = false;
    let mut positional = false;
    while let Some(arg) = args.next() {
        let value = arg.to_str();
        if !positional && value == Some("--") {
            positional = true;
        } else if !positional && matches!(value, Some("--help" | "-h")) {
            io::stdout().write_all(HELP.as_bytes())?;
            return Ok(false);
        } else if !positional
            && value.is_some_and(|v| v == "--protocol" || v.starts_with("--protocol="))
        {
            ensure!(
                command == "compact" && !jsonl,
                "--protocol is allowed once for compact only"
            );
            let option = option_value(value.unwrap(), &mut args)?;
            ensure!(
                option == "json-v1",
                "unsupported protocol; expected json-v1"
            );
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
                "unknown option; use 'retok --help'"
            );
            ensure!(file.is_none(), "expected at most one input file");
            file = Some(arg);
        }
    }
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    if jsonl {
        ensure!(file.is_none(), "JSONL protocol reads stdin; omit FILE");
        return protocol(io::stdin().lock(), output);
    }
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
                output.write_all(retok::restore(encoding, text)?.as_bytes())?
            }
            (Some(_), Err(_)) => {
                bail!("encoded input must be UTF-8; raw mode accepts arbitrary bytes")
            }
        }
    }
    output.flush()?;
    Ok(false)
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

fn main() -> std::process::ExitCode {
    match run() {
        Ok(false) => std::process::ExitCode::SUCCESS,
        Ok(true) => std::process::ExitCode::FAILURE,
        Err(error) if is_broken_pipe(&error) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "retok: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
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
