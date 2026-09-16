use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use retok::{Compactor, Encoding, restore};
use serde_json::{Value, json};

fn compactor() -> &'static Compactor {
    static COMPACTOR: OnceLock<Compactor> = OnceLock::new();
    COMPACTOR.get_or_init(|| Compactor::new().unwrap())
}

fn cli(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_retok"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output().unwrap();
    // Usage errors can close stdin without consuming it.
    let _ = writer.join().unwrap();
    output
}

#[test]
fn exact_counts_never_expand_and_text_restores() {
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    let mut cases = vec![
        String::new(),
        "failure: missing input\n".into(),
        "<|endoftext|> <|fim_prefix|>".into(),
        "event completed: task=7\r\n".repeat(200),
        format!("{}tail", "\"quoted\"\\🦀\n".repeat(100)),
    ];
    // Exercise the tokenizer's long-piece path and Unicode handling without
    // imposing machine-specific timing assertions or changing token boundaries.
    cases.push("a".repeat(200_000));
    cases.push("🦀".repeat(10_000));
    cases.push(format!("{}0{}", "[".repeat(512), "]".repeat(512)));
    for input in cases {
        let result = compactor().compact(&input);
        assert_eq!(result.input_tokens, tokenizer.encode_ordinary(&input).len());
        assert_eq!(
            result.output_tokens,
            tokenizer.encode_ordinary(&result.text).len()
        );
        assert!(result.output_tokens <= result.input_tokens);
        assert_eq!(restore(result.encoding, &result.text).unwrap(), input);
    }
    let result = compactor().compact(&"repeat this complete diagnostic\n".repeat(200));
    assert_eq!(result.encoding, Encoding::TextRunsV1);
}

#[test]
fn json_values_numbers_and_rows_survive_selection() {
    let input = format!("[{}]", (0..100).map(|i| format!(r#"{{ "record_identifier": {i}, "exact_decimal": 123456789012345678901234567890.123456789, "status_description": "ready" }}"#)).collect::<Vec<_>>().join(",\n"));
    let result = compactor().compact(&input);
    assert!(matches!(
        result.encoding,
        Encoding::JsonV1 | Encoding::JsonRowsV1
    ));
    let restored = restore(result.encoding, &result.text).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&restored).unwrap(),
        serde_json::from_str::<Value>(&input).unwrap()
    );
    assert!(result.output_tokens < result.input_tokens);
}

#[test]
fn selector_chooses_the_cheapest_complete_candidate() {
    let row = r#"{ "long_field_name": 7, "status": "ready" }"#;
    let input = format!("[\n{}{row}\n]\n", format!("{row},\n").repeat(99));
    // Independently spell out all three representations, including their headers.
    let minified = format!(
        "JSON v1 (all values):\n[{}]",
        vec![r#"{"long_field_name":7,"status":"ready"}"#; 100].join(",")
    );
    let table = format!(
        "JSON rows v1 (each row maps to the columns in order):\n{{\"columns\":[\"long_field_name\",\"status\"],\"rows\":[{}]}}",
        vec![r#"[7,"ready"]"#; 100].join(",")
    );
    let runs = format!(
        "retok:text-runs-v1 counts repeat exact JSON strings; concatenate\n{}",
        json!([
            [1, "[\n"],
            [99, format!("{row},\n")],
            [1, format!("{row}\n]\n")]
        ])
    );
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    let candidates = [
        (Encoding::Raw, &input),
        (Encoding::JsonV1, &minified),
        (Encoding::JsonRowsV1, &table),
        (Encoding::TextRunsV1, &runs),
    ];
    let (encoding, text) = candidates
        .into_iter()
        .min_by_key(|(_, text)| tokenizer.encode_ordinary(text).len())
        .unwrap();
    let result = compactor().compact(&input);
    assert_eq!(result.encoding, encoding);
    assert_eq!(&result.text, text);
}

#[test]
fn plain_bytes_and_explicit_raw_restore() {
    for input in [
        &b"\xff\0\r\n\x80"[..],
        b"failure: bad input\r\n",
        b"retok:text-runs-v1 counts repeat exact JSON strings; concatenate\n[[2,\"x\"]]",
    ] {
        for args in [&["compact"][..], &["restore", "--encoding=raw"][..]] {
            let output = cli(args, input);
            assert!(output.status.success(), "{:?}", output.stderr);
            assert_eq!(output.stdout, input);
        }
    }
    let output = cli(&["restore", "--encoding=text-runs-v1"], b"not encoded");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn prefix_protocol_and_cli_restore_match_independent_expansion() {
    let input: String = (0..100)
        .map(|i| format!("packages/synthetic/components/Widget{i}.rs\r\n"))
        .collect();
    let request = json!({"version": 1, "text": input});
    let response = cli(
        &["compact", "--protocol=json-v1"],
        format!("{request}\n").as_bytes(),
    );
    assert!(response.status.success());
    let result: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(result["encoding"], "text-prefixes-v1");
    let header = "retok:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate\n";
    let suffixes: Vec<String> = (0..100).map(|i| format!("{i}.rs\r\n")).collect();
    let expected = format!(
        "{header}{}",
        json!([["packages/synthetic/components/Widget", suffixes]])
    );
    assert_eq!(result["text"], expected);
    let expanded: String = suffixes
        .iter()
        .map(|suffix| format!("packages/synthetic/components/Widget{suffix}"))
        .collect();
    assert_eq!(expanded, input);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        result["input_tokens"],
        tokenizer.encode_ordinary(&input).len()
    );
    assert_eq!(
        result["output_tokens"],
        tokenizer.encode_ordinary(&expected).len()
    );
    assert!(tokenizer.encode_ordinary(&expected).len() < tokenizer.encode_ordinary(&input).len());
    let restored = cli(
        &["restore", "--encoding", "text-prefixes-v1"],
        expected.as_bytes(),
    );
    assert!(restored.status.success());
    assert_eq!(restored.stdout, input.as_bytes());
}

#[test]
fn protocol_recovers_from_request_errors_and_preserves_flags() {
    let text = "failed: missing artifact\r\n".repeat(100);
    let request =
        json!({"version":1,"text":text,"is_error":true,"complete":false,"tokenizer":"o200k_base"});
    let input = format!(
        "not json\n{}\n{}\n{}\n{}\n",
        json!({"version":2,"text":"x"}),
        json!({"version":1,"text":"x","tokenizer":"unknown"}),
        request,
        json!({"version":1,"text":"last"})
    );
    let output = cli(&["compact", "--protocol=json-v1"], input.as_bytes());
    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 5);
    for response in &responses[..3] {
        assert!(response["error"].is_string());
        assert!(response.get("text").is_none());
    }
    let result = &responses[3];
    let encoding = serde_json::from_value(result["encoding"].clone()).unwrap();
    assert_eq!(
        restore(encoding, result["text"].as_str().unwrap()).unwrap(),
        text
    );
    assert_eq!(responses[4]["text"], "last");
}

#[test]
fn reference_protocol_and_cli_restore_match_independent_expansion() {
    let repeated = "warning: packages/synthetic/components/navigation/LongComponentName.rs: required configuration value unavailable; supply the complete configuration before retrying the operation\r\n";
    let between = "another event\n";
    let last = "separate event\n";
    let input = format!("{repeated}{between}{repeated}{last}{repeated}");
    let request = json!({"version": 1, "text": input});
    let response = cli(
        &["compact", "--protocol=json-v1"],
        format!("{request}\n").as_bytes(),
    );
    assert!(response.status.success());
    let result: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(result["encoding"], "text-refs-v1");
    let expected = format!(
        "retok:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n{}",
        json!([repeated, between, 0, last, 0])
    );
    assert_eq!(result["text"], expected);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        result["input_tokens"],
        tokenizer.encode_ordinary(&input).len()
    );
    assert_eq!(
        result["output_tokens"],
        tokenizer.encode_ordinary(&expected).len()
    );
    let restored = cli(&["restore", "--encoding=text-refs-v1"], expected.as_bytes());
    assert!(restored.status.success());
    assert_eq!(restored.stdout, input.as_bytes());
}

#[test]
fn protocol_flushes_before_stdin_closes() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;
    let mut child = Command::new(env!("CARGO_BIN_EXE_retok"))
        .args(["compact", "--protocol=json-v1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let _ = tx.send(line);
    });
    stdin
        .write_all(b"{\"version\":1,\"text\":\"ready\"}\n")
        .unwrap();
    let response = rx.recv_timeout(Duration::from_secs(30));
    if response.is_err() {
        let _ = child.kill();
    }
    drop(stdin);
    let status = child.wait().unwrap();
    reader.join().unwrap();
    assert!(status.success());
    assert_eq!(
        serde_json::from_str::<Value>(&response.unwrap()).unwrap()["text"],
        "ready"
    );
}

#[test]
fn closed_output_is_quiet_and_successful() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_retok"))
        .args(["restore", "--encoding", "raw"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let _ = child.stdin.take().unwrap().write_all(b"a complete message");
    assert!(child.wait().unwrap().success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.is_empty());
}
