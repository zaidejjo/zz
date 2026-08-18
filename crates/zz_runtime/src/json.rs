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

/// Serialize a JSON value to its canonical text form.
pub fn to_json_string(v: &JsonValue) -> String {
    match v {
        JsonValue::Null => "null".into(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Num(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
                format!("{n:.0}")
            } else {
                format!("{n}")
            }
        }
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
        return Err(format!("unexpected trailing characters at byte {}", p.pos));
    }
    Ok(v)
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    bytes: &'a [u8],
}

impl<'a> Parser<'a> {
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
            Err(format!("expected `{}` at byte {}", what, self.pos))
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
            Some(c) => Err(format!(
                "unexpected character `{}` at byte {}",
                c as char, self.pos
            )),
            None => Err("unexpected end of input".into()),
        }
    }

    fn parse_literal(&mut self, lit: &str, val: JsonValue) -> Result<JsonValue, String> {
        if self.src[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(val)
        } else {
            Err(format!("invalid literal at byte {}", self.pos))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"', "string")?;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".into()),
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
                            let hex = self
                                .src
                                .get(self.pos..self.pos + 4)
                                .ok_or("truncated \\u escape")?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| format!("invalid \\u escape at byte {}", self.pos))?;
                            self.pos += 4;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        Some(c) => {
                            return Err(format!(
                                "invalid escape `\\{}` at byte {}",
                                c as char, self.pos
                            ))
                        }
                        None => return Err("unterminated string".into()),
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
        text.parse::<f64>()
            .map(JsonValue::Num)
            .map_err(|_| format!("invalid number `{text}`"))
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
}
