//! Completion-only host adapters. These never execute or rewrite a tool call.
//! CLI settings exclusions match exact host tool labels (Bash/PowerShell for
//! Claude, bash/powershell for Copilot), not programs inside shell expressions.
//! Usage events count measured text fields, not tool calls. Fields below 256
//! bytes and unsupported results are omitted, not estimated. Originals are
//! stored only with the state's explicit keep_originals opt-in.

use crate::state::{self, Settings};
use anyhow::Result;
use retok::{CompactResult, Compactor};
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use std::collections::HashSet;
use std::fmt;
use std::io::{Read, Write};
use std::time::Instant;

const MAX_INPUT: usize = 16 * 1024 * 1024;
const MAX_TEXT: usize = 8 * 1024 * 1024;

// Keep values opaque: Value's arbitrary_precision number marker is also a
// legal user object key. Re-encoding through Value can change such objects.
struct Object(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with unique keys")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Object, M::Error> {
                let mut fields = Vec::new();
                let mut keys = HashSet::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate hook field"));
                    }
                    fields.push((key, value));
                }
                Ok(Object(fields))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl Serialize for Object {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Object {
    fn get(&self, key: &str) -> Option<&RawValue> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| &**value)
    }

    fn string(&self, key: &str) -> Option<String> {
        serde_json::from_str(self.get(key)?.get()).ok()
    }

    fn object(&self, key: &str) -> Option<Object> {
        serde_json::from_str(self.get(key)?.get()).ok()
    }

    fn set_text(&mut self, key: &str, text: &str) -> Result<()> {
        if let Some((_, value)) = self.0.iter_mut().find(|(name, _)| name == key) {
            *value = RawValue::from_string(serde_json::to_string(text)?)?;
        }
        Ok(())
    }
}

/// Return a changed host envelope only when a complete selected text wins.
/// Unknown, malformed, oversized, or unprocessable input is a no-op, including
/// tokenizer initialization failure. No host policy or command is interpreted.
// The CLI uses the observer-enabled path; retain this pure API for callers/tests.
#[allow(dead_code)]
pub fn transform(host: &str, input: &str) -> Result<Option<String>> {
    Ok(transform_inner(host, input, &[], &mut |_, _, _, _, _| {}).unwrap_or(None))
}

fn transform_inner(
    host: &str,
    input: &str,
    exclusions: &[String],
    measured: &mut impl FnMut(&str, &str, &str, &CompactResult, u64),
) -> Result<Option<String>> {
    // Codex's current post hook omits native status/metadata and replacement
    // discards that context. Even text resembling legacy framing can be raw
    // command output. No payload-content heuristic can safely identify it.
    if input.len() > MAX_INPUT || !matches!(host, "claude" | "copilot") {
        return Ok(None);
    }
    let root: Object = serde_json::from_str(input)?;
    let tool = root.string(if host == "claude" {
        "tool_name"
    } else {
        "toolName"
    });
    let Some(tool) = tool else { return Ok(None) };
    if exclusions.contains(&tool) {
        return Ok(None);
    }
    let (mut response, fields) = match host {
        "claude" => {
            if root.string("hook_event_name").as_deref() != Some("PostToolUse")
                || !matches!(
                    root.string("tool_name").as_deref(),
                    Some("Bash" | "PowerShell")
                )
            {
                return Ok(None);
            }
            let Some(response) = root.object("tool_response") else {
                return Ok(None);
            };
            for flag in ["isImage", "interrupted"] {
                if let Some(value) = response.get(flag)
                    && serde_json::from_str::<bool>(value.get()).ok() != Some(false)
                {
                    return Ok(None);
                }
            }
            (response, vec!["stdout", "stderr"])
        }
        "copilot" => {
            // Native CLI postToolUse input has no event marker. Its registration
            // must be post-only. Reject conflicting markers when supplied.
            for field in ["hookEventName", "hook_event_name", "event"] {
                if root.get(field).is_some() && root.string(field).as_deref() != Some("postToolUse")
                {
                    return Ok(None);
                }
            }
            if !matches!(
                root.string("toolName").as_deref(),
                Some("bash" | "powershell")
            ) {
                return Ok(None);
            }
            let Some(response) = root.object("toolResult") else {
                return Ok(None);
            };
            if !matches!(
                response.string("resultType").as_deref(),
                Some("success" | "failure")
            ) {
                return Ok(None);
            }
            (response, vec!["textResultForLlm"])
        }
        _ => return Ok(None),
    };

    let mut texts = Vec::new();
    let mut total = 0usize;
    for field in fields {
        if response.get(field).is_none() {
            continue;
        }
        let Some(text) = response.string(field) else {
            return Ok(None);
        };
        total += text.len();
        texts.push((field, text));
    }
    // Bound the combined selected text, not each stream separately. Tiny
    // results do not justify tokenizer startup; leaving them raw is safe.
    if !(256..=MAX_TEXT).contains(&total) {
        return Ok(None);
    }
    let compactor = Compactor::new()?;
    let mut changed = false;
    for (field, text) in texts {
        if text.len() < 256 {
            continue;
        }
        let start = Instant::now();
        let result = compactor.compact(&text);
        let duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        if result.output_tokens < result.input_tokens {
            response.set_text(field, &result.text)?;
            changed = true;
        }
        measured(&tool, field, &text, &result, duration_ms);
    }
    if !changed {
        return Ok(None);
    }

    // RawValue serialization preserves every untouched field's representation.
    let response = serde_json::to_string(&response)?;
    let envelope = match host {
        "claude" => format!(
            "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PostToolUse\",\"updatedToolOutput\":{response}}}}}"
        ),
        "copilot" => format!("{{\"modifiedResult\":{response}}}"),
        _ => unreachable!(),
    };
    Ok(Some(envelope))
}

/// Read at most 16 MiB + one sentinel byte, then write one JSON line. Hook
/// failures must not obstruct the host's original result or fail its tool call.
pub fn run(host: &str) -> Result<()> {
    let mut bytes = Vec::new();
    let mut records = Vec::new();
    let result = std::io::stdin()
        .lock()
        .take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes);
    let output = if result.is_ok() && bytes.len() <= MAX_INPUT {
        let settings = Settings::load().ok().filter(|s| s.enabled);
        settings.and_then(|settings| {
            let input = std::str::from_utf8(&bytes).ok()?;
            let transformed = transform_inner(
                host,
                input,
                &settings.exclude_commands,
                &mut |tool, field, text, result, duration_ms| {
                    if !settings.record_usage {
                        return;
                    }
                    let event = state::Event {
                        unix_millis: state::unix_millis(),
                        command: format!("{tool}.{field}"),
                        input_tokens: Some(result.input_tokens as u64),
                        output_tokens: Some(result.output_tokens as u64),
                        input_bytes: text.len() as u64,
                        output_bytes: result.text.len() as u64,
                        duration_ms,
                        exit_code: None,
                        source: Some(format!("hook-{host}")),
                        original_id: None,
                    };
                    // Each record is one text field, including stderr fields. Its
                    // category identifies it; recall stores its original as stdout.
                    let original = settings.keep_originals.then(|| text.as_bytes().to_vec());
                    records.push((event, original));
                },
            );
            match transformed {
                Ok(output) => output,
                Err(_) => {
                    records.clear();
                    None
                }
            }
        })
    } else {
        None
    };
    // A closed stdout is also harmless: there is no replacement to deliver.
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{}", output.as_deref().unwrap_or("{}")).is_ok() && stdout.flush().is_ok() {
        for (event, original) in records {
            // Optional storage must not alter successful output delivery.
            let _ = state::record(event, original.as_deref().map(|text| (text, &b""[..])));
        }
    }
    Ok(())
}
