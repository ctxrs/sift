//! Complete JSON representations. Numbers remain raw JSON tokens throughout.
//!
//! `json-v1` is JSON_HEADER followed by one JSON value.
//! `json-rows-v1` is ROWS_HEADER followed by an object containing `columns`
//! (unique strings) and `rows` (arrays with exactly that many JSON values).
//! Restoration preserves values and numeric lexemes, not source whitespace.

use crate::Encoding;
use anyhow::{Result, bail, ensure};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use std::collections::{HashMap, HashSet};
use std::fmt;

const JSON_HEADER: &str = "JSON v1 (all values):\n";
const ROWS_HEADER: &str = "JSON rows v1 (each row maps to the columns in order):\n";
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

/// A map without serde_json::Value's number handling or reserved-key behavior.
struct Object<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique keys")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut fields = Vec::new();
                let mut seen = HashSet::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    fields.push((key, value));
                }
                Ok(Object(fields))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Table<'a> {
    columns: Vec<String>,
    #[serde(borrow)]
    rows: Vec<Vec<&'a RawValue>>,
}

fn parse(input: &str) -> Result<&RawValue> {
    ensure!(input.len() <= MAX_JSON_BYTES, "JSON exceeds size limit");
    let value: &RawValue = serde_json::from_str(input)?;
    validate(value, 0)?;
    Ok(value)
}

/// RawValue checks syntax, while this walk rejects duplicate keys at every
/// depth and validates decoded strings (including Unicode surrogate pairs).
fn validate(value: &RawValue, depth: usize) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, "JSON exceeds nesting limit");
    match value.get().as_bytes().first() {
        Some(b'{') => {
            let object: Object<'_> = serde_json::from_str(value.get())?;
            for (_, child) in object.0 {
                validate(child, depth + 1)?;
            }
        }
        Some(b'[') => {
            let children: Vec<&RawValue> = serde_json::from_str(value.get())?;
            for child in children {
                validate(child, depth + 1)?;
            }
        }
        Some(b'"') => {
            let _: String = serde_json::from_str(value.get())?;
        }
        _ => {}
    }
    Ok(())
}

/// Called only on validated JSON. Remove only JSON whitespace outside strings;
/// retain every number lexeme and every escaped or literal string character.
fn minify(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            output.push(ch);
        } else if !matches!(ch, ' ' | '\t' | '\r' | '\n') {
            output.push(ch);
        }
    }
    output
}

fn table(value: &RawValue) -> Option<String> {
    if !value.get().starts_with('[') {
        return None;
    }
    let objects: Vec<&RawValue> = serde_json::from_str(value.get()).ok()?;
    let first: Object<'_> = serde_json::from_str(objects.first()?.get()).ok()?;
    let columns: Vec<String> = first.0.into_iter().map(|(key, _)| key).collect();
    let mut rows = Vec::with_capacity(objects.len());
    for raw in objects {
        let object: Object<'_> = serde_json::from_str(raw.get()).ok()?;
        if object.0.len() != columns.len() {
            return None;
        }
        let fields: HashMap<String, &RawValue> = object.0.into_iter().collect();
        let row: Option<Vec<&RawValue>> = columns
            .iter()
            .map(|column| fields.get(column).copied())
            .collect();
        rows.push(row?);
    }
    let encoded = serde_json::to_string(&Table { columns, rows }).ok()?;
    let payload = minify(&encoded);
    // The table adds two container levels around nested values. Apply the same
    // limits as restore so every emitted candidate remains independently usable.
    parse(&payload).ok()?;
    Some(format!("{ROWS_HEADER}{payload}"))
}

pub(crate) fn candidates(input: &str) -> Vec<(Encoding, String)> {
    let Ok(value) = parse(input) else {
        return Vec::new();
    };
    let mut candidates = vec![(
        Encoding::JsonV1,
        format!("{JSON_HEADER}{}", minify(value.get())),
    )];
    if let Some(encoded) = table(value) {
        candidates.push((Encoding::JsonRowsV1, encoded));
    }
    candidates
}

fn append(output: &mut String, text: &str) -> Result<()> {
    ensure!(
        text.len() <= MAX_JSON_BYTES - output.len(),
        "restored JSON exceeds size limit"
    );
    output.push_str(text);
    Ok(())
}

pub(crate) fn restore(encoding: Encoding, input: &str) -> Result<String> {
    let header = match encoding {
        Encoding::JsonV1 => JSON_HEADER,
        Encoding::JsonRowsV1 => ROWS_HEADER,
        _ => bail!("not a JSON encoding"),
    };
    let payload = input
        .strip_prefix(header)
        .ok_or_else(|| anyhow::anyhow!("missing or incorrect JSON encoding header"))?;
    let value = parse(payload)?;
    if matches!(encoding, Encoding::JsonV1) {
        return Ok(minify(value.get()));
    }

    ensure!(
        value.get().starts_with('{'),
        "JSON table envelope must be an object"
    );
    let table: Table<'_> = serde_json::from_str(value.get())?;
    let mut seen = HashSet::new();
    ensure!(
        table.columns.iter().all(|key| seen.insert(key)),
        "duplicate table column"
    );
    let keys: Vec<String> = table
        .columns
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<_, _>>()?;
    let mut output = String::new();
    append(&mut output, "[")?;
    for (row_index, row) in table.rows.iter().enumerate() {
        ensure!(
            row.len() == keys.len(),
            "table row width differs from columns"
        );
        if row_index > 0 {
            append(&mut output, ",")?;
        }
        append(&mut output, "{")?;
        for (column_index, (key, value)) in keys.iter().zip(row).enumerate() {
            if column_index > 0 {
                append(&mut output, ",")?;
            }
            append(&mut output, key)?;
            append(&mut output, ":")?;
            append(&mut output, value.get())?;
        }
        append(&mut output, "}")?;
    }
    append(&mut output, "]")?;
    Ok(minify(&output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(input: &str, encoding: Encoding) -> String {
        candidates(input)
            .into_iter()
            .find(|(kind, _)| *kind == encoding)
            .expect("expected encoding candidate")
            .1
    }

    #[test]
    fn minification_preserves_numbers_and_string_lexemes() {
        let input = r#" {
          "big": 1234567890123456789012345678901234567890,
          "decimal": 0.123456789012345678901234567890,
          "exp": 1e+9999, "negative_zero": -0,
          "text": " space  \t\n\"\\ café \uD83D\uDE80 "
        } "#;
        let expected = r#"{"big":1234567890123456789012345678901234567890,"decimal":0.123456789012345678901234567890,"exp":1e+9999,"negative_zero":-0,"text":" space  \t\n\"\\ café \uD83D\uDE80 "}"#;
        let compact = encoded(input, Encoding::JsonV1);
        assert_eq!(compact, format!("{JSON_HEADER}{expected}"));
        assert_eq!(restore(Encoding::JsonV1, &compact).unwrap(), expected);
    }

    #[test]
    fn rows_preserve_all_values_and_map_reordered_keys() {
        let input = r#"[
          {"id":9007199254740993,"item":{"x":[null,true]},"note":"a\"b"},
          {"note":"rare\nvalue","id":9007199254740995,"item":false}
        ]"#;
        let compact = encoded(input, Encoding::JsonRowsV1);
        let expected_table = r#"{"columns":["id","item","note"],"rows":[[9007199254740993,{"x":[null,true]},"a\"b"],[9007199254740995,false,"rare\nvalue"]]}"#;
        assert_eq!(compact, format!("{ROWS_HEADER}{expected_table}"));
        assert_eq!(
            restore(Encoding::JsonRowsV1, &compact).unwrap(),
            r#"[{"id":9007199254740993,"item":{"x":[null,true]},"note":"a\"b"},{"id":9007199254740995,"item":false,"note":"rare\nvalue"}]"#
        );
    }

    #[test]
    fn both_encodings_preserve_numeric_spellings_without_float_conversion() {
        let numbers = [
            "-0",
            "-0.0",
            "-0e+000",
            "-0.000E-99999",
            "18446744073709551616",
            "-18446744073709551617",
            "1e999999999999999999999999999999999999",
            "1e-999999999999999999999999999999999999",
            "1.2300E+0042",
            "0.1234567890123456789012345678901234567890",
        ];
        for number in numbers {
            let plain = encoded(number, Encoding::JsonV1);
            assert_eq!(restore(Encoding::JsonV1, &plain).unwrap(), number);
            let input = format!("[{{\"number\":{number}}},{{\"number\":{number}}}]");
            for (encoding, compact) in candidates(&input) {
                assert_eq!(restore(encoding, &compact).unwrap(), input);
            }
            // Ensure table coverage isn't silently replaced by plain minification.
            let rows = encoded(&input, Encoding::JsonRowsV1);
            assert_eq!(restore(Encoding::JsonRowsV1, &rows).unwrap(), input);
        }
    }

    #[test]
    fn table_columns_escape_keys_and_preserve_null_associations() {
        let input = r#"[{"":null,"\"\\\n":false,"雪":[]},{"雪":{},"":true,"\"\\\n":"null"}]"#;
        let compact = encoded(input, Encoding::JsonRowsV1);
        assert_eq!(
            restore(Encoding::JsonRowsV1, &compact).unwrap(),
            r#"[{"":null,"\"\\\n":false,"雪":[]},{"":true,"\"\\\n":"null","雪":{}}]"#
        );
    }

    #[test]
    fn scalar_empty_and_heterogeneous_json_remain_complete() {
        for input in ["null", "true", "false", "0", "-0.00e+5", "\"\"", "[]", "{}"] {
            let compact = encoded(input, Encoding::JsonV1);
            assert_eq!(restore(Encoding::JsonV1, &compact).unwrap(), input);
        }
        for input in [r#"[{"x":null},{}]"#, r#"[{"x":0},{"y":0}]"#, "[{},null]"] {
            assert_eq!(candidates(input).len(), 1);
            let compact = encoded(input, Encoding::JsonV1);
            assert_eq!(restore(Encoding::JsonV1, &compact).unwrap(), input);
        }
        let compact = encoded("[{},{}]", Encoding::JsonRowsV1);
        assert_eq!(restore(Encoding::JsonRowsV1, &compact).unwrap(), "[{},{}]");
    }

    #[test]
    fn duplicate_keys_are_rejected_at_every_depth() {
        for input in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":1,"\u0061":2}"#,
            r#"[{"outer":{"a":1,"a":2}}]"#,
            r#"{"list":[{"a":1,"a":2}]}"#,
        ] {
            assert!(candidates(input).is_empty(), "{input}");
            assert!(restore(Encoding::JsonV1, &format!("{JSON_HEADER}{input}")).is_err());
        }
    }

    #[test]
    fn reserved_keys_and_format_markers_are_ordinary_data() {
        let input = r#"[{"$serde_json::private::Number":"1e9999","columns":["x"],"rows":"JSON v1 (all values):\n"}]"#;
        for (encoding, compact) in candidates(input) {
            assert_eq!(restore(encoding, &compact).unwrap(), input);
        }
        assert!(candidates(&format!("{JSON_HEADER}{{}}")).is_empty());
        assert!(restore(Encoding::JsonV1, "{}").is_err());
        assert!(restore(Encoding::JsonRowsV1, &format!("{JSON_HEADER}{{}}")).is_err());
        assert!(restore(Encoding::Raw, "{}").is_err());
    }

    #[test]
    fn malformed_json_and_invalid_unicode_fail_closed() {
        for input in [
            "",
            " ",
            "NaN",
            "Infinity",
            "01",
            "1.",
            "1e",
            "{} []",
            "[1,]",
            r#"{"a":}"#,
            r#""\uD800""#,
            r#""\uDC00""#,
            r#"{"\uD800":0}"#,
            "\"literal\nnewline\"",
        ] {
            assert!(candidates(input).is_empty(), "{input:?}");
        }
    }

    #[test]
    fn table_envelopes_require_objects_and_allow_surrounding_whitespace() {
        for payload in [r#"[["a"],[[1]]]"#, "[[],[[]]]"] {
            for framed in [payload.to_owned(), format!(" \t\n{payload}\r\n ")] {
                assert!(restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{framed}")).is_err());
                assert_eq!(
                    restore(Encoding::JsonV1, &format!("{JSON_HEADER}{framed}")).unwrap(),
                    payload
                );
            }
        }
        for payload in [
            r#"{"columns":["a"],"rows":[[1]]}"#,
            " \t\n{\"rows\": [[1]], \"columns\": [\"a\"]}\r\n ",
        ] {
            assert_eq!(
                restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{payload}")).unwrap(),
                r#"[{"a":1}]"#
            );
        }
    }

    #[test]
    fn malformed_tables_are_rejected() {
        for payload in [
            r#"{"columns":["a"],"rows":[[]]}"#,
            r#"{"columns":[],"rows":[[1]]}"#,
            r#"{"columns":["a","\u0061"],"rows":[[1,2]]}"#,
            r#"{"columns":[0],"rows":[[1]]}"#,
            r#"{"columns":[],"rows":{},"extra":0}"#,
            r#"{"columns":[],"rows":[],"extra":0}"#,
            r#"{"columns":[],"columns":[],"rows":[]}"#,
            r#"{"columns":["a"],"rows":[[{"x":1,"x":2}]]}"#,
            r#"{"columns":[]}"#,
        ] {
            assert!(
                restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{payload}")).is_err(),
                "{payload}"
            );
        }
        let empty = format!("{ROWS_HEADER}{{\"columns\":[],\"rows\":[]}}");
        assert_eq!(restore(Encoding::JsonRowsV1, &empty).unwrap(), "[]");
    }

    #[test]
    fn nesting_and_restore_amplification_are_bounded() {
        let nested = format!(
            "{}0{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert!(candidates(&nested).is_empty());
        let column = "k".repeat(4096);
        let payload = format!(
            "{{\"columns\":[\"{column}\"],\"rows\":[{}]}}",
            vec!["[0]"; MAX_JSON_BYTES / column.len() + 1].join(",")
        );
        assert!(payload.len() < MAX_JSON_BYTES);
        assert!(restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{payload}")).is_err());
    }
}
