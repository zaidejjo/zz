//! Minimal JSON value type, parser, and serializer (Phase 2.5).
//!
//! Hand-rolled to keep the dependency tree small. Supports the full JSON
//! grammar: null, booleans, numbers, strings, arrays, and objects.
//!
//! Objects carry a lazy key→first-position index once they exceed
//! [`JSON_INDEX_THRESHOLD`] entries, making `get`/`has`/`deep_get` O(1)
//! instead of O(n).  `Value::Json` is `Arc`-backed, so cloning a JSON value
//! is an O(1) reference bump — read-only natives borrow instead of
//! deep-cloning the whole document.

use std::collections::HashMap;
use std::fmt;

/// Objects larger than this keep a hash index for key lookups.
const JSON_INDEX_THRESHOLD: usize = 8;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<JsonValue>),
    Obj(JsonObject),
}

impl From<Vec<(String, JsonValue)>> for JsonValue {
    fn from(entries: Vec<(String, JsonValue)>) -> Self {
        JsonValue::Obj(JsonObject::from_entries(entries))
    }
}

impl fmt::Display for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", to_json_string(self))
    }
}

/// An insertion-ordered JSON object with an optional key index.
///
/// The index maps each key to its *first* position, matching the historic
/// linear-scan `find` semantics for duplicate keys.  It is built lazily on
/// first lookup once the object crosses `JSON_INDEX_THRESHOLD` entries, and
/// kept in sync by [`JsonObject::push`] / [`JsonObject::from_entries`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsonObject {
    entries: Vec<(String, JsonValue)>,
    index: Option<HashMap<String, usize>>,
}

impl JsonObject {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn from_entries(entries: Vec<(String, JsonValue)>) -> Self {
        let mut obj = Self {
            entries,
            index: None,
        };
        if obj.entries.len() > JSON_INDEX_THRESHOLD {
            obj.build_index();
        }
        obj
    }

    fn build_index(&mut self) {
        let mut index = HashMap::with_capacity(self.entries.len());
        for (i, (k, _)) in self.entries.iter().enumerate() {
            // First occurrence wins, matching linear-scan `find`.
            index.entry(k.clone()).or_insert(i);
        }
        self.index = Some(index);
    }

    /// Borrowed lookup: O(1) when indexed, O(n) otherwise.
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match &self.index {
            Some(idx) => idx
                .get(key)
                .and_then(|&i| self.entries.get(i).map(|(_, v)| v)),
            None => self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn keys(&self) -> Vec<String> {
        self.entries.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn entries(&self) -> &[(String, JsonValue)] {
        &self.entries
    }

    /// Insert preserving first-occurrence index semantics.
    pub fn push(&mut self, k: String, v: JsonValue) {
        let is_new = match &self.index {
            Some(idx) => !idx.contains_key(&k),
            None => true,
        };
        let key = k.clone();
        self.entries.push((k, v));
        if is_new {
            let i = self.entries.len() - 1;
            if let Some(idx) = &mut self.index {
                idx.insert(key, i);
            } else if self.entries.len() > JSON_INDEX_THRESHOLD {
                self.build_index();
            }
        }
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
        JsonValue::Obj(obj) => {
            let inner: Vec<String> = obj
                .entries()
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
        JsonValue::Obj(obj) => {
            if obj.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, val)) in obj.entries().iter().enumerate() {
                pad(out, indent + step);
                out.push('"');
                out.push_str(&escape_str(k));
                out.push_str("\": ");
                pretty_print(val, out, indent + step, step);
                if i + 1 < obj.entries().len() {
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
        JsonValue::Obj(obj) => obj.len(),
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
        JsonValue::Obj(obj) => obj.keys(),
        _ => vec![],
    }
}

/// Check if a JSON object contains a given key.
pub fn json_has(v: &JsonValue, key: &str) -> bool {
    match v {
        JsonValue::Obj(obj) => obj.contains(key),
        _ => false,
    }
}

/// Shallow merge two JSON objects. Entries from `b` overwrite `a`.
/// Returns a new object. Non-object inputs produce the second value.
pub fn json_merge(a: &JsonValue, b: &JsonValue) -> JsonValue {
    match (a, b) {
        (JsonValue::Obj(a_obj), JsonValue::Obj(b_obj)) => {
            let mut out = a_obj.entries().to_vec();
            // Index of `out` keys for O(1) overwrite checks instead of
            // `out.iter_mut().find(...)` inside the `b` loop (O(n²)).
            let mut index: HashMap<String, usize> = out
                .iter()
                .enumerate()
                .map(|(i, (k, _))| (k.clone(), i))
                .collect();
            for (k, v) in b_obj.entries() {
                if let Some(&i) = index.get(k) {
                    out[i] = (k.clone(), v.clone());
                } else {
                    index.insert(k.clone(), out.len());
                    out.push((k.clone(), v.clone()));
                }
            }
            JsonValue::Obj(JsonObject::from_entries(out))
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
            return Ok(JsonValue::Obj(JsonObject::new()));
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
            return Ok(JsonValue::Obj(JsonObject::from_entries(entries)));
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
            JsonValue::from(vec![
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
                    JsonValue::from(vec![("c".into(), JsonValue::Null)])
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
            JsonValue::from(vec![("k".into(), JsonValue::Str("v".into()))]),
            parse_json(r#"{"k":"v"}"#).unwrap()
        );
    }

    // ── pretty-print ────────────────────────────────────────────────────────

    #[test]
    fn pretty_empty_containers() {
        assert_eq!(to_json_string_pretty(&JsonValue::Arr(vec![])), "[]");
        assert_eq!(
            to_json_string_pretty(&JsonValue::Obj(JsonObject::new())),
            "{}"
        );
    }

    #[test]
    fn pretty_nested() {
        let v = JsonValue::from(vec![
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

    // ── json_len ────────────────────────────────────────────────────────

    #[test]
    fn len_array() {
        let v = JsonValue::Arr(vec![JsonValue::Num(1.0), JsonValue::Num(2.0)]);
        assert_eq!(json_len(&v), 2);
    }

    #[test]
    fn len_object() {
        let v = JsonValue::from(vec![
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

    // ── json_type_name ────────────────────────────────────────────────────

    #[test]
    fn type_names() {
        assert_eq!(json_type_name(&JsonValue::Null), "null");
        assert_eq!(json_type_name(&JsonValue::Bool(true)), "bool");
        assert_eq!(json_type_name(&JsonValue::Num(1.0)), "number");
        assert_eq!(json_type_name(&JsonValue::Str("x".into())), "string");
        assert_eq!(json_type_name(&JsonValue::Arr(vec![])), "array");
        assert_eq!(json_type_name(&JsonValue::Obj(JsonObject::new())), "object");
    }

    // ── json_keys / json_has ─────────────────────────────────────────────

    #[test]
    fn keys_and_has() {
        let v = JsonValue::from(vec![
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

    // ── json_merge ───────────────────────────────────────────────────────

    #[test]
    fn merge_objects() {
        let a = JsonValue::from(vec![
            ("x".into(), JsonValue::Num(1.0)),
            ("y".into(), JsonValue::Num(2.0)),
        ]);
        let b = JsonValue::from(vec![
            ("y".into(), JsonValue::Num(99.0)),
            ("z".into(), JsonValue::Num(3.0)),
        ]);
        let merged = json_merge(&a, &b);
        assert!(json_has(&merged, "x"));
        assert!(json_has(&merged, "y"));
        assert!(json_has(&merged, "z"));
        // y should be overwritten
        match &merged {
            JsonValue::Obj(obj) => {
                let y_val = obj.entries().iter().find(|(k, _)| k == "y").unwrap();
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

    // ── object indexing ───────────────────────────────────────────────────

    #[test]
    fn object_index_first_occurrence_wins() {
        // Duplicate keys: the first occurrence must win for get/has.
        let v = JsonValue::from(vec![
            ("k".into(), JsonValue::Num(1.0)),
            ("k".into(), JsonValue::Num(2.0)),
        ]);
        assert!(json_has(&v, "k"));
        match &v {
            JsonValue::Obj(obj) => {
                assert_eq!(obj.get("k").unwrap(), &JsonValue::Num(1.0));
                assert_eq!(obj.len(), 2);
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn object_index_uses_hash_lookup() {
        // Build an object larger than the threshold so an index exists.
        let entries: Vec<(String, JsonValue)> = (0..20)
            .map(|i| (format!("k{i}"), JsonValue::Num(i as f64)))
            .collect();
        let v = JsonValue::from(entries);
        assert!(json_has(&v, "k19"));
        assert!(!json_has(&v, "missing"));
        match &v {
            JsonValue::Obj(obj) => {
                assert_eq!(obj.get("k0").unwrap(), &JsonValue::Num(0.0));
                assert_eq!(obj.get("k19").unwrap(), &JsonValue::Num(19.0));
            }
            _ => panic!("expected object"),
        }
    }

    // ── error line/col ───────────────────────────────────────────────────

    #[test]
    fn error_includes_line_col() {
        let err = parse_json("{\n  \"a\": 1,\n  \"b\": }").unwrap_err();
        assert!(err.contains("line 3"), "error should mention line: {err}");
        assert!(err.contains("col"), "error should mention col: {err}");
    }

    // ── escape_str ───────────────────────────────────────────────────────

    #[test]
    fn escape_special_chars() {
        assert_eq!(escape_str("hello"), "hello");
        assert!(escape_str("\"\\n\t").contains("\\\\"));
    }

    // ── format_number ────────────────────────────────────────────────────

    #[test]
    fn format_number_integers() {
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(-100.0), "-100");
    }

    #[test]
    fn format_number_floats() {
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(-0.5), "-0.5");
    }
}
