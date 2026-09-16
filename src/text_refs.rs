//! Lossless text with direct references to earlier literal strings.
//!
//! After HEADER, the JSON array contains strings or unsigned integer indices.
//! A string appends itself; an index appends the exact earlier string at that
//! array position. References to references, self, or future entries are invalid.

use anyhow::{Context, Result, bail, ensure};
use serde::de::{Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

const HEADER: &str = "retok:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n";
const MAX_RESTORED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Serialize)]
#[serde(untagged)]
enum Entry {
    Literal(String),
    Reference(u64),
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor;

        impl Visitor<'_> for EntryVisitor {
            type Value = Entry;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a literal string or unsigned integer reference")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Entry, E> {
                Ok(Entry::Literal(value.to_owned()))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Entry, E> {
                Ok(Entry::Literal(value))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Entry, E> {
                Ok(Entry::Reference(value))
            }
        }

        // Deliberately omit signed, floating-point, map and sequence visitors.
        // In particular, -0, 0.0 and 0e0 are not unsigned integer index tokens.
        deserializer.deserialize_any(EntryVisitor)
    }
}

fn flush_literal(entries: &mut Vec<Entry>, pending: &mut String) {
    if !pending.is_empty() {
        entries.push(Entry::Literal(std::mem::take(pending)));
    }
}

pub(crate) fn candidate(input: &str) -> Option<String> {
    if input.len() > MAX_RESTORED_BYTES {
        return None;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for line in input.split_inclusive('\n') {
        *counts.entry(line).or_default() += 1;
    }
    let mut anchors: HashMap<&str, u64> = HashMap::new();
    let mut entries = Vec::new();
    let mut pending = String::new();
    let mut used_reference = false;
    for line in input.split_inclusive('\n') {
        if let Some(&index) = anchors.get(line) {
            flush_literal(&mut entries, &mut pending);
            entries.push(Entry::Reference(index));
            used_reference = true;
            continue;
        }

        let count = counts[line];
        if count > 1 {
            let index = u64::try_from(entries.len() + usize::from(!pending.is_empty())).ok()?;
            let escaped_bytes = serde_json::to_string(line).ok()?.len() - 2;
            // Conservative byte prefilter: isolating the first literal costs
            // at most six framing bytes; each later reference can add four.
            // The caller still compares complete candidates by actual tokens.
            let saving = escaped_bytes
                .saturating_sub(index.to_string().len() + 4)
                .saturating_mul(count - 1);
            if saving > 6 {
                flush_literal(&mut entries, &mut pending);
                entries.push(Entry::Literal(line.to_owned()));
                anchors.insert(line, index);
                continue;
            }
        }
        // Only unreferenced text is coalesced. Never extend an anchor literal.
        pending.push_str(line);
    }
    if !used_reference {
        return None;
    }
    flush_literal(&mut entries, &mut pending);
    Some(format!("{HEADER}{}", serde_json::to_string(&entries).ok()?))
}

fn literal_at(entries: &[Entry], position: usize) -> Result<&str> {
    match &entries[position] {
        Entry::Literal(text) => Ok(text),
        Entry::Reference(index) => {
            let index = usize::try_from(*index).context("text reference index is too large")?;
            ensure!(
                index < position,
                "text reference must point to an earlier string"
            );
            match &entries[index] {
                Entry::Literal(text) => Ok(text),
                Entry::Reference(_) => bail!("text reference cannot point to another reference"),
            }
        }
    }
}

pub(crate) fn restore(input: &str) -> Result<String> {
    let body = input
        .strip_prefix(HEADER)
        .context("invalid text-refs-v1 header")?;
    let entries: Vec<Entry> = serde_json::from_str(body).context("invalid text references")?;
    let mut total = 0usize;
    for position in 0..entries.len() {
        total = total
            .checked_add(literal_at(&entries, position)?.len())
            .context("text reference size overflow")?;
        ensure!(
            total <= MAX_RESTORED_BYTES,
            "text references exceed the 64 MiB restoration limit"
        );
    }
    let mut output = String::new();
    output
        .try_reserve_exact(total)
        .context("cannot allocate restored text")?;
    for position in 0..entries.len() {
        output.push_str(literal_at(&entries, position)?);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_independently_written_sequences() {
        for (body, expected) in [
            (
                r#"["left\n","middle\r\n",0,"tail"]"#,
                "left\nmiddle\r\nleft\ntail",
            ),
            (r#"["",0,"A",2]"#, "AA"),
            (r#"["0",0]"#, "00"),
            (
                r#"["\"\\\u0000雪\r\n",0,"last"]"#,
                "\"\\\0雪\r\n\"\\\0雪\r\nlast",
            ),
            ("[]", ""),
            (" \n[\"a\", \"b\", 0]\t", "aba"),
        ] {
            assert_eq!(restore(&format!("{HEADER}{body}")).unwrap(), expected);
        }
    }

    #[test]
    fn anchors_remain_stable_when_unique_lines_are_coalesced() {
        let a = "a sufficiently long repeated line A\n";
        let b = "another sufficiently long repeated line B\n";
        let input = format!("intro a\nintro b\n{a}unique 1\nunique 2\n{b}{a}gap\n{b}tail");
        let expected_body = serde_json::json!([
            "intro a\nintro b\n",
            a,
            "unique 1\nunique 2\n",
            b,
            1,
            "gap\n",
            3,
            "tail"
        ])
        .to_string();
        let encoded = candidate(&input).unwrap();
        assert_eq!(encoded, format!("{HEADER}{expected_body}"));
        assert_eq!(restore(&encoded).unwrap(), input);
    }

    #[test]
    fn preserves_escapes_line_endings_markers_and_final_bytes() {
        for line in [
            "a repeated diagnostic with spaces  \n",
            "a repeated diagnostic with CRLF\r\n",
            "a repeated diagnostic with \"quotes\", \\ and \0\n",
            "a repeated diagnostic with 雪 and 🦀\n",
            HEADER,
        ] {
            for tail in ["", "unterminated", "\r", "\n"] {
                let input = format!("{line}unique\n{line}{tail}");
                assert_eq!(restore(&candidate(&input).unwrap()).unwrap(), input);
            }
        }
    }

    #[test]
    fn skips_inputs_without_worthwhile_exact_repeats() {
        for input in [
            "",
            "unique",
            "a\nb\na\n",
            "\n\n\n\n",
            "a long line\na long line",
            "a long line\r\na long line\n",
        ] {
            assert!(candidate(input).is_none(), "{input:?}");
        }
        let input = "x".repeat(MAX_RESTORED_BYTES + 1);
        assert!(candidate(&input).is_none());
    }

    #[test]
    fn rejects_invalid_reference_targets_and_numeric_spellings() {
        for body in [
            "[0]",
            r#"["x",1]"#,
            r#"["x",2]"#,
            r#"["x",0,1]"#,
            r#"["x",-1]"#,
            r#"["x",-0]"#,
            r#"["x",0.0]"#,
            r#"["x",0e0]"#,
            r#"["x",18446744073709551615]"#,
            r#"["x",18446744073709551616]"#,
        ] {
            assert!(restore(&format!("{HEADER}{body}")).is_err(), "{body}");
        }
    }

    #[test]
    fn rejects_wrong_shapes_invalid_strings_and_framing() {
        for body in [
            "{}",
            "null",
            "\"literal\"",
            "[true]",
            "[null]",
            "[[]]",
            "[{}]",
            r#"[{"$serde_json::private::Number":"0"}]"#,
            r#"["\uD800"]"#,
            r#"["\uDC00"]"#,
            "[\"x\",]",
            "[] trailing",
        ] {
            assert!(restore(&format!("{HEADER}{body}")).is_err(), "{body}");
        }
        assert!(restore("[\"x\",0]").is_err());
        assert!(restore(&format!(" {HEADER}[]")).is_err());
    }

    #[test]
    fn checks_aggregate_expansion_before_allocating_output() {
        let literal = "x".repeat(16 * 1024);
        let repeats = MAX_RESTORED_BYTES / literal.len();
        let body = format!(
            "[{}{}]",
            serde_json::to_string(&literal).unwrap(),
            ",0".repeat(repeats - 1)
        );
        let restored = restore(&format!("{HEADER}{body}")).unwrap();
        assert_eq!(restored.len(), MAX_RESTORED_BYTES);
        assert!(restored.bytes().all(|byte| byte == b'x'));
        drop(restored);
        let excessive = format!("{},\"x\"]", &body[..body.len() - 1]);
        assert!(restore(&format!("{HEADER}{excessive}")).is_err());
    }
}
