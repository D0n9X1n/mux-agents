//! The reply on stdout: one compact ASCII JSON line, or `-Text` lines (MuxAgents.ps1:59-80).

use serde_json::{Map, Value};

/// Compact JSON with non-ASCII as \uXXXX (lowercase, UTF-16 pairs), like ConvertTo-Json -EscapeHandling
/// EscapeNonAscii. serde_json already writes controls as lowercase \u00XX.
pub fn to_stdout(value: &Value) -> String {
    let compact = value.to_string();
    let mut out = String::with_capacity(compact.len());
    // Outside strings the text is ASCII, so every non-ASCII char sits inside a string.
    for ch in compact.chars() {
        if ch.is_ascii() {
            out.push(ch);
            continue;
        }
        let mut units = [0u16; 2];
        for unit in ch.encode_utf16(&mut units) {
            out.push_str(&format!("\\u{unit:04x}"));
        }
    }
    out
}

/// Array values become one line per row, a row's fields joined by two spaces; other values are `key: value`.
pub fn to_text(reply: &Map<String, Value>) -> String {
    let mut lines = Vec::new();
    for (key, value) in reply {
        if key == "ok" || key == "verb" {
            continue;
        }
        match value {
            Value::Array(rows) => lines.extend(rows.iter().map(row)),
            _ => lines.push(format!("{key}: {}", cell(value))),
        }
    }
    lines.join("\n")
}

fn row(value: &Value) -> String {
    match value {
        Value::Object(fields) => fields.values().map(cell).collect::<Vec<_>>().join("  "),
        _ => cell(value),
    }
}

// PowerShell's [string]: True/False, null empty, arrays space-joined. A nested object has no useful
// PowerShell text, so it shows as its JSON.
fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(cell).collect::<Vec<_>>().join(" "),
        Value::Object(_) => to_stdout(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value, json};

    fn map(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn escapes_non_ascii_lowercase_u() {
        assert_eq!(
            to_stdout(&json!("\u{e9}\u{1F600}")),
            "\"\\u00e9\\ud83d\\ude00\""
        );
        assert_eq!(to_stdout(&json!("alpha \u{23F5}")), "\"alpha \\u23f5\"");
        assert_eq!(to_stdout(&json!("a\"b\\c\n\u{1}")), r#""a\"b\\c\n\u0001""#);
    }

    #[test]
    fn keeps_insertion_order() {
        let mut reply = Map::new();
        reply.insert("ok".into(), json!(true));
        reply.insert("verb".into(), json!("t11-show"));
        reply.insert("name".into(), json!("alpha"));
        reply.insert(
            "agents".into(),
            json!([{"name": "one", "pane": "%9"}, {"pane": "%10", "name": "two"}]),
        );
        assert_eq!(
            to_stdout(&Value::Object(reply)),
            r#"{"ok":true,"verb":"t11-show","name":"alpha","agents":[{"name":"one","pane":"%9"},{"pane":"%10","name":"two"}]}"#
        );
    }

    #[test]
    fn text_skips_ok_and_verb() {
        let reply = map(
            json!({"ok": false, "verb": "list", "code": 2, "check": "usage", "message": "usage: x"}),
        );
        assert_eq!(to_text(&reply), "code: 2\ncheck: usage\nmessage: usage: x");
    }

    #[test]
    fn text_arrays_rows_joined_by_two_spaces() {
        let reply = map(json!({
            "ok": true, "verb": "t11-show", "name": "alpha \u{23F5}",
            "agents": [{"name": "one", "pane": "%9"}, {"name": "two", "pane": "%10"}],
            "words": ["plain", 3],
            "none": []
        }));
        assert_eq!(
            to_text(&reply),
            "name: alpha \u{23F5}\none  %9\ntwo  %10\nplain\n3"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn text_bool_is_True_False_null_is_empty() {
        let reply = map(
            json!({"ok": true, "verb": "x", "yes": true, "no": false, "gone": null,
            "rows": [{"a": true, "b": null, "c": false}]}),
        );
        assert_eq!(
            to_text(&reply),
            "yes: True\nno: False\ngone: \nTrue    False"
        );
    }
}
