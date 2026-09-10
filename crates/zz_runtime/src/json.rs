//! Minimal JSON value type, parser, and serializer (Phase 2.5).
//!
//! Hand-rolled to keep the dependency tree small. Supports the full JSON
//! grammar: null, booleans, numbers, strings, arrays, and objects.

use std::fmt;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<JsonValue>),
    Obj(Vec<(String, JsonValue)>),
}

impl fmt::Display for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", to_json_string(self))
    }
}

/// Serialize a JSON value to its canonical compact text form.
pub fn to_json_string(v: &JsonValue) -> String {
    match v {
        JsonValue::Null => "null".into(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Num(n) => format_number(*n),
        JsonValue::Str(s) => format!("\"{}\"", escape_str(s)),
        JsonValue::Arr(items) => {
            let inner: Vec<String> = items.iter().map(to_json_string).collect();
            format!("[{}]", inner.join(","))
        }
        JsonValue::Obj(entries) => {
            let inner: Vec<String> = entries
                .iter()
                .map(|(k, val)| format!("\"{}\":{}", escape_str(k), to_json_string(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// Format a number: integers without decimal point, floats as-is.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

/// Serialize a JSON value to a pretty-printed form with 2-space indentation.
pub fn to_json_string_pretty(v: &JsonValue) -> String {
    let mut out = String::with_capacity(256);
    pretty_print(v, &mut out, 0, 2);
    out
}

fn pretty_print(v: &JsonValue, out: &mut String, indent: usize, step: usize) {
    match v {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(b) => out.push_str(&b.to_string()),
        JsonValue::Num(n) => out.push_str(&format_number(*n)),
        JsonValue::Str(s) => {
            out.push('"');
            out.push_str(&escape_str(s));
            out.push('"');
        }
        JsonValue::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                pad(out, indent + step);
                pretty_print(item, out, indent + step, step);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(out, indent);
            out.push(']');
        }
        JsonValue::Obj(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, val)) in entries.iter().enumerate() {
                pad(out, indent + step);
                out.push('"');
                out.push_str(&escape_str(k));
                out.push_str("\": ");
                pretty_print(val, out, indent + step, step);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(out, indent);
            out.push('}');
        }
    }
}

fn pad(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

/// Count the number of elements in a JSON value (array length or object key count).
/// Returns 0 for scalars.
pub fn json_len(v: &JsonValue) -> usize {
    match v {
        JsonValue::Arr(items) => items.len(),
        JsonValue::Obj(entries) => entries.len(),
        _ => 0,
    }
}

/// Get the type name of a JSON value as a static string.
pub fn json_type_name(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Num(_) => "number",
        JsonValue::Str(_) => "string",
        JsonValue::Arr(_) => "array",
        JsonValue::Obj(_) => "object",
    }
}

/// Get the keys of a JSON object as a vector of strings.
/// Returns empty vector for non-objects.
pub fn json_keys(v: &JsonValue) -> Vec<String> {
    match v {
        JsonValue::Obj(entries) => entries.iter().map(|(k, _)| k.clone()).collect(),
        _ => vec![],
    }
}

/// Check if a JSON object contains a given key.
pub fn json_has(v: &JsonValue, key: &str) -> bool {
    match v {
        JsonValue::Obj(entries) => entries.iter().any(|(k, _)| k == key),
        _ => false,
    }
}

/// Shallow merge two JSON objects. Entries from `b` overwrite `a`.
/// Returns a new object. Non-object inputs produce the second value.
pub fn json_merge(a: &JsonValue, b: &JsonValue) -> JsonValue {
    match (a, b) {
        (JsonValue::Obj(a_entries), JsonValue::Obj(b_entries)) => {
            let mut out = a_entries.clone();
            for (k, v) in b_entries {
                if let Some(existing) = out.iter_mut().find(|(ek, _)| ek == k) {
                    *existing = (k.clone(), v.clone());
                } else {
                    out.push((k.clone(), v.clone()));
                }
            }
            JsonValue::Obj(out)
        }
        (_, b) => b.clone(),
    }
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Parse a JSON document. Returns an error message on malformed input.
pub fn parse_json(src: &str) -> Result<JsonValue, String> {
    let mut p = Parser {
        src,
        pos: 0,
        bytes: src.as_bytes(),
    };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    if p.pos < p.bytes.len() {
        let (line, col) = p.line_col();
        return Err(format!(
            "unexpected trailing characters at line {line}, col {col}"
        ));
    }
    Ok(v)
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    bytes: &'a [u8],
}

impl<'a> Parser<'a> {
    /// Convert byte position to 1-based line and column.
    fn line_col(&self) -> (usize, usize) {
        let mut line = 1;
        let mut col = 1;
        for i in 0..self.pos.min(self.bytes.len()) {
            if self.bytes[i] == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, b: u8, what: &str) -> Result<(), String> {
        if self.eat(b) {
            Ok(())
        } else {
            let (line, col) = self.line_col();
            Err(format!("expected `{what}` at line {line}, col {col}"))
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        match self.peek() {
            Some(b'n') => self.parse_literal("null", JsonValue::Null),
            Some(b't') => self.parse_literal("true", JsonValue::Bool(true)),
            Some(b'f') => self.parse_literal("false", JsonValue::Bool(false)),
            Some(b'"') => self.parse_string().map(JsonValue::Str),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(c) => {
                let (line, col) = self.line_col();
                Err(format!(
                    "unexpected character `{}` at line {line}, col {col}",
                    c as char,
                ))
            }
            None => Err("unexpected end of input".into()),
        }
    }

    fn parse_literal(&mut self, lit: &str, val: JsonValue) -> Result<JsonValue, String> {
        if self.src[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(val)
        } else {
            let (line, col) = self.line_col();
            Err(format!("invalid literal at line {line}, col {col}"))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"', "string")?;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => {
                    let (line, col) = self.line_col();
                    return Err(format!("unterminated string at line {line}, col {col}"));
                }
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'b') => out.push('\u{0008}'),
                        Some(b'f') => out.push('\u{000c}'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => {
                            self.pos += 1;
                            let hex = self.src.get(self.pos..self.pos + 4).ok_or_else(|| {
                                let (line, col) = self.line_col();
                                format!("truncated \\u escape at line {line}, col {col}")
                            })?;
                            let code = u32::from_str_radix(hex, 16).map_err(|_| {
                                let (line, col) = self.line_col();
                                format!("invalid \\u escape at line {line}, col {col}")
                            })?;
                            self.pos += 4;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        Some(c) => {
                            let (line, col) = self.line_col();
                            return Err(format!(
                                "invalid escape `\\{}` at line {line}, col {col}",
                                c as char,
                            ));
                        }
                        None => {
                            let (line, col) = self.line_col();
                            return Err(format!(
                                "unterminated string escape at line {line}, col {col}"
                            ));
                        }
                    }
                    self.pos += 1;
                }
                Some(_) => {
                    let c = self.src[self.pos..].chars().next().unwrap();
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
        self.eat(b'-');
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.eat(b'.') {
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = &self.src[start..self.pos];
        text.parse::<f64>().map(JsonValue::Num).map_err(|_| {
            let (line, col) = self.line_col();
            format!("invalid number `{text}` at line {line}, col {col}")
        })
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.expect(b'[', "array")?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']') {
            return Ok(JsonValue::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            self.expect(b']', "array close")?;
            return Ok(JsonValue::Arr(items));
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.expect(b'{', "object")?;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.eat(b'}') {
            return Ok(JsonValue::Obj(entries));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':', "colon")?;
            self.skip_ws();
            let val = self.parse_value()?;
            entries.push((key, val));
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            self.expect(b'}', "object close")?;
            return Ok(JsonValue::Obj(entries));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_primitives() {
        assert_eq!(parse_json("null").unwrap(), JsonValue::Null);
        assert_eq!(parse_json("true").unwrap(), JsonValue::Bool(true));
        assert_eq!(parse_json("42").unwrap(), JsonValue::Num(42.0));
        assert_eq!(parse_json("-1.5").unwrap(), JsonValue::Num(-1.5));
        assert_eq!(
            parse_json("\"hi\\n\"").unwrap(),
            JsonValue::Str("hi\n".into())
        );
    }

    #[test]
    fn round_trip_nested() {
        let src = r#"{"a": [1, 2.5, "x"], "b": {"c": null}}"#;
        let v = parse_json(src).unwrap();
        assert_eq!(
            v,
            JsonValue::Obj(vec![
                (
                    "a".into(),
                    JsonValue::Arr(vec![
                        JsonValue::Num(1.0),
                        JsonValue::Num(2.5),
                        JsonValue::Str("x".into()),
                    ])
                ),
                (
                    "b".into(),
                    JsonValue::Obj(vec![("c".into(), JsonValue::Null)])
                ),
            ])
        );
        // Serialization round-trips to equivalent JSON.
        let out = to_json_string(&v);
        assert_eq!(parse_json(&out).unwrap(), v);
    }

    #[test]
    fn malformed_errors() {
        assert!(parse_json("{").is_err());
        assert!(parse_json("[1,]").is_err());
        assert!(parse_json("nul").is_err());
        assert!(parse_json("").is_err());
        assert!(parse_json("1 2").is_err());
    }

    #[test]
    fn serializes_cleanly() {
        assert_eq!(to_json_string(&JsonValue::Num(3.0)), "3");
        assert_eq!(to_json_string(&JsonValue::Num(3.5)), "3.5");
        assert_eq!(
            to_json_string(&JsonValue::Obj(vec![(
                "k".into(),
                JsonValue::Str("v".into())
            )])),
            r#"{"k":"v"}"#
        );
    }

    // ── pretty-print ────────────────────────────────────────────────────────

    #[test]
    fn pretty_empty_containers() {
        assert_eq!(to_json_string_pretty(&JsonValue::Arr(vec![])), "[]");
        assert_eq!(to_json_string_pretty(&JsonValue::Obj(vec![])), "{}");
    }

    #[test]
    fn pretty_nested() {
        let v = JsonValue::Obj(vec![
            ("name".into(), JsonValue::Str("test".into())),
            (
                "items".into(),
                JsonValue::Arr(vec![JsonValue::Num(1.0), JsonValue::Num(2.0)]),
            ),
        ]);
        let pretty = to_json_string_pretty(&v);
        assert!(pretty.contains('\n'));
        assert!(pretty.contains("  ")); // 2-space indent
                                        // Round-trips through compact
        assert_eq!(parse_json(&pretty).unwrap(), v);
    }

    // ── json_len ────────────────────────────────────────────────────────────

    #[test]
    fn len_array() {
        let v = JsonValue::Arr(vec![JsonValue::Num(1.0), JsonValue::Num(2.0)]);
        assert_eq!(json_len(&v), 2);
    }

    #[test]
    fn len_object() {
        let v = JsonValue::Obj(vec![
            ("a".into(), JsonValue::Null),
            ("b".into(), JsonValue::Null),
        ]);
        assert_eq!(json_len(&v), 2);
    }

    #[test]
    fn len_scalar() {
        assert_eq!(json_len(&JsonValue::Null), 0);
        assert_eq!(json_len(&JsonValue::Num(42.0)), 0);
        assert_eq!(json_len(&JsonValue::Str("hi".into())), 0);
    }

    // ── json_type_name ──────────────────────────────────────────────────────

    #[test]
    fn type_names() {
        assert_eq!(json_type_name(&JsonValue::Null), "null");
        assert_eq!(json_type_name(&JsonValue::Bool(true)), "bool");
        assert_eq!(json_type_name(&JsonValue::Num(1.0)), "number");
        assert_eq!(json_type_name(&JsonValue::Str("x".into())), "string");
        assert_eq!(json_type_name(&JsonValue::Arr(vec![])), "array");
        assert_eq!(json_type_name(&JsonValue::Obj(vec![])), "object");
    }

    // ── json_keys / json_has ────────────────────────────────────────────────

    #[test]
    fn keys_and_has() {
        let v = JsonValue::Obj(vec![
            ("a".into(), JsonValue::Num(1.0)),
            ("b".into(), JsonValue::Num(2.0)),
        ]);
        let mut keys = json_keys(&v);
        keys.sort();
        assert_eq!(keys, vec!["a".to_string(), "b".to_string()]);
        assert!(json_has(&v, "a"));
        assert!(!json_has(&v, "c"));
        // Non-object
        assert!(json_keys(&JsonValue::Arr(vec![])).is_empty());
        assert!(!json_has(&JsonValue::Null, "x"));
    }

    // ── json_merge ──────────────────────────────────────────────────────────

    #[test]
    fn merge_objects() {
        let a = JsonValue::Obj(vec![
            ("x".into(), JsonValue::Num(1.0)),
            ("y".into(), JsonValue::Num(2.0)),
        ]);
        let b = JsonValue::Obj(vec![
            ("y".into(), JsonValue::Num(99.0)),
            ("z".into(), JsonValue::Num(3.0)),
        ]);
        let merged = json_merge(&a, &b);
        assert!(json_has(&merged, "x"));
        assert!(json_has(&merged, "y"));
        assert!(json_has(&merged, "z"));
        // y should be overwritten
        match &merged {
            JsonValue::Obj(entries) => {
                let y_val = entries.iter().find(|(k, _)| k == "y").unwrap();
                assert_eq!(y_val.1, JsonValue::Num(99.0));
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn merge_non_object() {
        let a = JsonValue::Null;
        let b = JsonValue::Num(42.0);
        assert_eq!(json_merge(&a, &b), JsonValue::Num(42.0));
    }

    // ── error line/col ──────────────────────────────────────────────────────

    #[test]
    fn error_includes_line_col() {
        let err = parse_json("{\n  \"a\": 1,\n  \"b\": }").unwrap_err();
        assert!(err.contains("line 3"), "error should mention line: {err}");
        assert!(err.contains("col"), "error should mention col: {err}");
    }

    // ── escape_str ──────────────────────────────────────────────────────────

    #[test]
    fn escape_special_chars() {
        assert_eq!(escape_str("hello"), "hello");
        assert!(escape_str("\"\\n\t").contains("\\\\"));
    }

    // ── format_number ───────────────────────────────────────────────────────

    #[test]
    fn format_number_integers() {
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(-100.0), "-100");
    }

    #[test]
    fn format_number_floats() {
        assert_eq!(format_number(3.14), "3.14");
        assert_eq!(format_number(-0.5), "-0.5");
    }
}
