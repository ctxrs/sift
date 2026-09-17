// Exercise the adapter independently of concurrent CLI/setup integration.
#[path = "../src/hooks.rs"]
mod hooks;
#[allow(dead_code)]
#[path = "../src/state.rs"]
mod state;

use retok::{Encoding, restore};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

type Object = BTreeMap<String, Box<RawValue>>;

fn object(text: &str) -> Object {
    serde_json::from_str(text).unwrap()
}
fn string(value: &RawValue) -> String {
    serde_json::from_str(value.get()).unwrap()
}
fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}
fn log() -> String {
    "worker α: completed checkpoint 🦀; status remains unchanged\r\n".repeat(100)
}
fn assert_reversible(original: &str, changed: &str) {
    assert_ne!(original, changed);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert!(tokenizer.encode_ordinary(changed).len() < tokenizer.encode_ordinary(original).len());
    assert!(
        [
            Encoding::TextRunsV1,
            Encoding::TextPrefixesV1,
            Encoding::TextRefsV1,
            Encoding::JsonV1,
            Encoding::JsonRowsV1,
        ]
        .into_iter()
        .any(|encoding| restore(encoding, changed).is_ok_and(|text| text == original))
    );
}
fn claude(response: &str) -> String {
    format!(r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_response":{response}}}"#)
}

#[test]
fn claude_preserves_opaque_metadata_and_both_streams() {
    let stdout = log();
    let stderr = "warning: retaining diagnostic context\r\n".repeat(80);
    let response = format!(
        r#"{{"stdout":{},"stderr":{},"isImage":false,"interrupted":false,"exitCode":17,"error":{{"message":"failed 🦀\r\n","code":-9}},"marker":{{"$serde_json::private::Number":"00123"}},"numbers":[1234567890123456789012345678901234567890,1.2300e+004,-0,1e9999],"content":[{{"type":"image","data":"opaque=="}}]}}"#,
        json_string(&stdout),
        json_string(&stderr)
    );
    let output = hooks::transform("claude", &claude(&response))
        .unwrap()
        .unwrap();
    let envelope = object(&output);
    assert_eq!(envelope.len(), 1);
    let specific = object(envelope["hookSpecificOutput"].get());
    assert_eq!(string(&specific["hookEventName"]), "PostToolUse");
    let changed = object(specific["updatedToolOutput"].get());
    let original = object(&response);
    assert_eq!(changed.len(), original.len());
    for (key, value) in original {
        if key != "stdout" && key != "stderr" {
            assert_eq!(changed[&key].get(), value.get(), "opaque field {key}");
        }
    }
    assert_reversible(&stdout, &string(&changed["stdout"]));
    assert_reversible(&stderr, &string(&changed["stderr"]));
}

#[test]
fn claude_powershell_and_unchanged_stderr() {
    let response = format!(
        r#"{{"stdout":{},"stderr":"one warning\r\n","interrupted":false,"isImage":false}}"#,
        json_string(&log())
    );
    let input = claude(&response).replace("\"Bash\"", "\"PowerShell\"");
    let result = hooks::transform("claude", &input).unwrap().unwrap();
    let outer = object(&result);
    let specific = object(outer["hookSpecificOutput"].get());
    let changed = object(specific["updatedToolOutput"].get());
    assert_eq!(changed["stderr"].get(), r#""one warning\r\n""#);
    assert_reversible(&log(), &string(&changed["stdout"]));
}

#[test]
fn codex_rejects_raw_output_printing_a_fake_legacy_header() {
    let prefix = "Wall time: 12.3400 seconds\nExit code: -17\nOutput:\n";
    let body = log();
    let complete = format!("{prefix}{body}");
    let input = format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_response":{}}}"#,
        json_string(&complete)
    );
    // Native status lives outside tool_response and cannot be reconstructed
    // from text, even when the text resembles a formerly supported formatter.
    assert!(hooks::transform("codex", &input).unwrap().is_none());
    assert_eq!(run("codex", input.as_bytes()), b"{}\n");
}

#[test]
fn codex_rejects_raw_unified_output_and_incomplete_status() {
    for text in [
        log(),
        format!("Wall time: 1 seconds\nOutput:\n{}", log()),
        format!("Wall time: NaN seconds\nExit code: 0\nOutput:\n{}", log()),
        format!(
            "Wall time: 1 seconds\nExit code: unknown\nOutput:\n{}",
            log()
        ),
        format!("Exit code: 1\nWall time: 1 seconds\nOutput:\n{}", log()),
        format!("Wall time: 1 seconds\nExit code: 0\nOutput: {}", log()),
    ] {
        assert!(
            hooks::transform("codex", &claude(&json_string(&text)))
                .unwrap()
                .is_none()
        );
    }
    for response in [
        r#"{"stdout":"text","exit_code":1}"#,
        r#"[{"type":"image","data":"abc"}]"#,
    ] {
        assert!(
            hooks::transform("codex", &claude(response))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn copilot_preserves_failure_type_and_metadata() {
    for result_type in ["failure", "success"] {
        let response = format!(
            r#"{{"resultType":"{result_type}","textResultForLlm":{},"error":{{"message":"command failed","exitCode":77}},"metadata":{{"$serde_json::private::Number":"no number"}},"duration":1.23000e+09}}"#,
            json_string(&log())
        );
        // Native payloads omit the event name; registration is postToolUse only.
        let input =
            format!(r#"{{"toolName":"powershell","toolArgs":"{{}}","toolResult":{response}}}"#);
        let output = object(&hooks::transform("copilot", &input).unwrap().unwrap());
        assert_eq!(output.len(), 1);
        let changed = object(output["modifiedResult"].get());
        for (key, value) in object(&response) {
            if key != "textResultForLlm" {
                assert_eq!(changed[&key].get(), value.get());
            }
        }
        assert_reversible(&log(), &string(&changed["textResultForLlm"]));
        let wrong_event = input.replacen('{', "{\"event\":\"preToolUse\",", 1);
        assert!(hooks::transform("copilot", &wrong_event).unwrap().is_none());
    }
}

#[test]
fn hermes_final_result_preserves_status_and_exact_metadata() {
    let text = log();
    let response = format!(
        r#"{{"output":{},"exit_code":17,"error":null,"hint":"retain hint","approval":"already approved","count":123456789012345678901234567890,"number":-0.00100E+99999,"marker":{{"$serde_json::private::Number":"user object"}}}}"#,
        json_string(&text)
    );
    let input = format!(
        r#"{{"hook_event_name":"TransformToolResult","tool_name":"terminal","tool_response":{response}}}"#
    );
    let output = hooks::transform("hermes", &input).unwrap().unwrap();
    let envelope = object(&output);
    assert_eq!(envelope.len(), 1);
    let result = string(&envelope["result"]);
    let changed = object(&result);
    for (key, value) in object(&response) {
        if key != "output" {
            assert_eq!(changed[&key].get(), value.get());
        }
    }
    assert_reversible(&text, &string(&changed["output"]));
    assert_eq!(
        run("hermes", input.as_bytes()),
        format!("{output}\n").as_bytes()
    );
    assert!(
        hooks::transform(
            "hermes",
            &input.replace("TransformToolResult", "PreToolUse")
        )
        .unwrap()
        .is_none()
    );
    assert!(
        hooks::transform("hermes", &input.replace("\"terminal\"", "\"read_file\""))
            .unwrap()
            .is_none()
    );
}

#[test]
fn malformed_unsupported_and_tiny_are_noops() {
    for host in ["claude", "codex", "copilot", "unknown"] {
        for input in ["", "{", "{}", "null", "[]", "{}\n{}", r#"{"x":1,"x":2}"#] {
            assert!(hooks::transform(host, input).unwrap().is_none());
        }
    }
    for response in [
        r#"{"stdout":"tiny","stderr":""}"#.to_owned(),
        format!(
            r#"{{"stdout":{},"stderr":[],"isImage":false}}"#,
            json_string(&log())
        ),
        format!(r#"{{"stdout":{},"interrupted":true}}"#, json_string(&log())),
        format!(r#"{{"stdout":{},"isImage":true}}"#, json_string(&log())),
        format!(r#"{{"stdout":{},"isImage":"false"}}"#, json_string(&log())),
        format!(r#"{{"stdout":{},"stdout":"other"}}"#, json_string(&log())),
    ] {
        assert!(
            hooks::transform("claude", &claude(&response))
                .unwrap()
                .is_none()
        );
    }
    let input = claude(&format!(r#"{{"stdout":{}}}"#, json_string(&log())));
    assert!(
        hooks::transform("claude", &input.replace("PostToolUse", "PreToolUse"))
            .unwrap()
            .is_none()
    );
    assert!(
        hooks::transform("claude", &input.replace("Bash", "Read"))
            .unwrap()
            .is_none()
    );
    assert!(hooks::transform("unknown", &input).unwrap().is_none());
}

#[test]
fn oversized_input_and_combined_streams_are_noops() {
    assert!(
        hooks::transform("claude", &" ".repeat(16 * 1024 * 1024 + 1))
            .unwrap()
            .is_none()
    );
    let text = "x".repeat(4 * 1024 * 1024 + 1);
    let response = format!(
        r#"{{"stdout":{},"stderr":{}}}"#,
        json_string(&text),
        json_string(&text)
    );
    assert!(
        hooks::transform("claude", &claude(&response))
            .unwrap()
            .is_none()
    );
}

#[test]
fn tool_input_and_transcript_are_never_executed_or_read() {
    let path = std::env::temp_dir().join(format!("retok-hook-no-execution-{}", std::process::id()));
    assert!(!path.exists());
    let input = format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","transcript_path":{},"tool_input":{{"command":{}}},"tool_response":{{"stdout":{}}}}}"#,
        json_string(&path.to_string_lossy()),
        json_string(&format!("touch {}", path.display())),
        json_string(&log())
    );
    assert!(hooks::transform("claude", &input).unwrap().is_some());
    assert!(!path.exists());
}

const MARKER: &[u8] = b"HOOK-ENTRY\n";

#[test]
fn hook_entry() {
    let Ok(host) = std::env::var("RETOK_HOOK_TEST_HOST") else {
        return;
    };
    std::io::stdout().write_all(MARKER).unwrap();
    std::io::stdout().flush().unwrap();
    hooks::run(&host).unwrap();
    std::process::exit(0);
}

fn run(host: &str, input: &[u8]) -> Vec<u8> {
    Sandbox::new().run(host, input)
}

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "retok-hook-state-{}-{}-{}",
            std::process::id(),
            state::unix_millis(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn settings(&self, text: &str) {
        std::fs::create_dir_all(self.0.join("config")).unwrap();
        std::fs::write(self.0.join("config/config.json"), text).unwrap();
    }

    fn events(&self) -> Vec<state::Event> {
        std::fs::read_to_string(self.0.join("state/metrics.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn run(&self, host: &str, input: &[u8]) -> Vec<u8> {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "hook_entry", "--nocapture"])
            .current_dir(&self.0)
            .env("RETOK_HOOK_TEST_HOST", host)
            .env("RETOK_CONFIG_DIR", self.0.join("config"))
            .env("RETOK_STATE_DIR", self.0.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        // Oversized input can close the pipe at cap+1, before all bytes are written.
        if let Err(error) = stdin.write_all(input) {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let start = output
            .stdout
            .windows(MARKER.len())
            .position(|bytes| bytes == MARKER)
            .unwrap()
            + MARKER.len();
        output.stdout[start..].to_vec()
    }
}

#[test]
fn hook_project_uses_host_cwd_and_keeps_invalid_cwd_unscoped() {
    for host in ["claude", "copilot"] {
        let sandbox = Sandbox::new();
        let other = Sandbox::new();
        std::fs::create_dir(sandbox.0.join(".git")).unwrap();
        std::fs::create_dir(other.0.join(".git")).unwrap();
        let base = if host == "claude" {
            claude(&format!(r#"{{"stdout":{}}}"#, json_string(&log())))
        } else {
            serde_json::json!({"toolName":"bash", "toolResult":{
                "resultType":"success", "textResultForLlm":log()
            }})
            .to_string()
        };
        let launcher = state::project_at(&sandbox.0).unwrap();
        let other_project = state::project_at(&other.0).unwrap();
        for (cwd, expected) in [
            (None, Some(launcher.clone())),
            (Some(serde_json::json!(sandbox.0)), Some(launcher)),
            (Some(serde_json::json!(other.0)), Some(other_project)),
            (Some(serde_json::json!(other.0.join("missing"))), None),
            (Some(serde_json::json!("relative")), None),
            (Some(serde_json::Value::Null), None),
        ] {
            let mut input: serde_json::Value = serde_json::from_str(&base).unwrap();
            if let Some(cwd) = cwd {
                input["cwd"] = cwd;
            }
            assert_ne!(sandbox.run(host, input.to_string().as_bytes()), b"{}\n");
            let metrics = std::fs::read_to_string(sandbox.0.join("state/metrics.jsonl")).unwrap();
            let last: serde_json::Value =
                serde_json::from_str(metrics.lines().last().unwrap()).unwrap();
            assert_eq!(last["project"].as_str(), expected.as_deref());
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn run_emits_one_json_line_and_fails_open() {
    for input in [b"{".as_slice(), b"\xff", b"{}", b"{}\n{}"] {
        assert_eq!(run("claude", input), b"{}\n");
    }
    assert_eq!(run("unknown", b"{}"), b"{}\n");
    assert_eq!(run("claude", &vec![b' '; 16 * 1024 * 1024 + 2]), b"{}\n");
    let input = claude(&format!(
        r#"{{"stdout":{},"stderr":""}}"#,
        json_string(&log())
    ));
    let output = run("claude", input.as_bytes());
    assert_eq!(output.iter().filter(|&&b| b == b'\n').count(), 1);
    assert_eq!(output.last(), Some(&b'\n'));
    assert_eq!(object(std::str::from_utf8(&output).unwrap()).len(), 1);
}

#[test]
fn cli_settings_disable_invalid_and_exact_tool_exclusions() {
    let input = claude(&format!(r#"{{"stdout":{}}}"#, json_string(&log())));
    for config in [
        r#"{"enabled":false}"#,
        "{",
        r#"{"exclude_commands":["Bash"]}"#,
    ] {
        let sandbox = Sandbox::new();
        sandbox.settings(config);
        assert_eq!(sandbox.run("claude", input.as_bytes()), b"{}\n");
        assert!(!sandbox.0.join("state").exists());
        assert_eq!(
            std::fs::read_to_string(sandbox.0.join("config/config.json")).unwrap(),
            config
        );
    }
    let sandbox = Sandbox::new();
    // A program exclusion does not pretend to parse a shell expression.
    sandbox.settings(r#"{"exclude_commands":["git"],"record_usage":false}"#);
    let input = input.replacen('{', "{\"tool_input\":{\"command\":\"git status\"},", 1);
    assert_ne!(sandbox.run("claude", input.as_bytes()), b"{}\n");
    assert!(!sandbox.0.join("state").exists());
}

#[test]
fn usage_records_actual_field_counts_without_raw_text() {
    let sandbox = Sandbox::new();
    let stdout = log();
    let stderr = "diagnostic, retain all context\r\n".repeat(100);
    let input = claude(&format!(
        r#"{{"stdout":{},"stderr":{}}}"#,
        json_string(&stdout),
        json_string(&stderr)
    ));
    assert_ne!(sandbox.run("claude", input.as_bytes()), b"{}\n");
    let events = sandbox.events();
    assert_eq!(events.len(), 2);
    let compactor = retok::Compactor::new().unwrap();
    for (event, (label, text)) in events
        .iter()
        .zip([("Bash.stdout", stdout), ("Bash.stderr", stderr)])
    {
        let expected = compactor.compact(&text);
        assert_eq!(event.command, label);
        assert_eq!(event.source.as_deref(), Some("hook-claude"));
        assert_eq!(
            event.input_tokens,
            Some(compactor.count_tokens(&text) as u64)
        );
        assert_eq!(
            event.output_tokens,
            Some(compactor.count_tokens(&expected.text) as u64)
        );
        assert_eq!(event.input_bytes, text.len() as u64);
        assert_eq!(event.output_bytes, expected.text.len() as u64);
        assert_eq!(event.exit_code, None);
        assert_eq!(event.original_id, None);
    }
    let metrics = std::fs::read_to_string(sandbox.0.join("state/metrics.jsonl")).unwrap();
    assert!(!metrics.contains("checkpoint"));
    assert!(!metrics.contains("diagnostic"));
    assert!(!sandbox.0.join("state/originals").exists());

    let sandbox = Sandbox::new();
    assert_eq!(
        sandbox.run("claude", claude(r#"{"stdout":"tiny"}"#).as_bytes()),
        b"{}\n"
    );
    assert!(!sandbox.0.join("state").exists());
}

#[test]
fn originals_opt_in_and_storage_failure_do_not_change_output() {
    let text = log();
    let input = claude(&format!(r#"{{"stderr":{}}}"#, json_string(&text)));
    let sandbox = Sandbox::new();
    sandbox.settings(r#"{"keep_originals":true}"#);
    let output = sandbox.run("claude", input.as_bytes());
    assert_ne!(output, b"{}\n");
    let events = sandbox.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "Bash.stderr");
    let mut recalled = Vec::new();
    state::recall_at(
        &sandbox.0.join("state"),
        events[0].original_id.as_deref().unwrap(),
        false,
        &mut recalled,
    )
    .unwrap();
    assert_eq!(recalled, text.as_bytes());

    let sandbox = Sandbox::new();
    std::fs::write(sandbox.0.join("state"), b"not a directory").unwrap();
    assert_eq!(sandbox.run("claude", input.as_bytes()), output);
    assert_eq!(
        std::fs::read(sandbox.0.join("state")).unwrap(),
        b"not a directory"
    );

    let sandbox = Sandbox::new();
    sandbox.settings(r#"{"record_usage":false,"keep_originals":true}"#);
    assert_eq!(sandbox.run("claude", input.as_bytes()), output);
    assert!(!sandbox.0.join("state").exists());
}
