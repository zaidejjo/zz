use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

// Lowercase hex lookup table: index = nibble (0..16), value = ASCII char.
const HEX_LOWER: [u8; 16] = *b"0123456789abcdef";

// Hex-value lookup table: index = byte, value = nibble (0..15) or -1 if not
// a hex digit. Built once at compile time.
const HEX_VAL: [i8; 256] = {
    let mut t = [-1i8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        t[i] = match b {
            b'0'..=b'9' => (b - b'0') as i8,
            b'a'..=b'f' => (b - b'a' + 10) as i8,
            b'A'..=b'F' => (b - b'A' + 10) as i8,
            _ => -1,
        };
        i += 1;
    }
    t
};

/// Fast hex encode: one pass, lookup table, pre-sized output. ~10x faster
/// than `format!("{b:02x}")` per byte.
fn hex_encode_fast(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    unsafe {
        let v = out.as_mut_vec();
        for &b in data {
            v.push(HEX_LOWER[(b >> 4) as usize]);
            v.push(HEX_LOWER[(b & 0x0f) as usize]);
        }
    }
    out
}

/// Fast hex decode: one pass, lookup table, pre-sized output. Returns an
/// error on odd length or non-hex input. ~5x faster than `from_str_radix`
/// per pair.
fn hex_decode_fast(encoded: &str) -> Result<Vec<u8>, String> {
    let bytes = encoded.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = HEX_VAL[bytes[i] as usize];
        let lo = HEX_VAL[bytes[i + 1] as usize];
        if hi < 0 || lo < 0 {
            return Err("hex decode error: invalid digit found in string".to_string());
        }
        out.push((hi as u8) << 4 | (lo as u8));
        i += 2;
    }
    Ok(out)
}

pub(crate) fn encoding_base64_encode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "encoding.base64_encode")?;
    use base64::Engine;
    Ok(Value::Str(
        base64::engine::general_purpose::STANDARD
            .encode(data.as_bytes())
            .into(),
    ))
}

pub(crate) fn encoding_base64_decode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let encoded = expect_str(args, 0, "encoding.base64_decode")?;
    use base64::Engine;
    match base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes()) {
        Ok(bytes) => Ok(Value::Result(Box::new(Ok(Value::Str(
            String::from_utf8_lossy(&bytes).to_string().into(),
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("base64 decode error: {e}").into(),
        ))))),
    }
}

/// `encoding.base64_decode_bytes(str) -> Result<bytes, str>`
///
/// Like `base64_decode` but preserves raw bytes (binary tarballs cannot
/// round-trip through UTF-8 strings).
pub(crate) fn encoding_base64_decode_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let encoded = expect_str(args, 0, "encoding.base64_decode_bytes")?;
    use base64::Engine;
    match base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes()) {
        Ok(bytes) => Ok(Value::Result(Box::new(Ok(Value::Bytes(Box::new(
            zz_runtime::BytesData::from_vec(bytes),
        )))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("base64 decode error: {e}").into(),
        ))))),
    }
}

pub(crate) fn encoding_hex_encode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "encoding.hex_encode")?;
    Ok(Value::Str(hex_encode_fast(data.as_bytes()).into()))
}

pub(crate) fn encoding_hex_decode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let encoded = expect_str(args, 0, "encoding.hex_decode")?;
    match hex_decode_fast(&encoded) {
        Ok(bytes) => Ok(Value::Result(Box::new(Ok(Value::Str(
            String::from_utf8_lossy(&bytes).to_string().into(),
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("hex decode error: {e}").into(),
        ))))),
    }
}

pub(crate) fn encoding_url_encode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "encoding.url_encode")?;
    Ok(Value::Str(urlencoding::encode(&data).into_owned().into()))
}

pub(crate) fn encoding_url_decode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let encoded = expect_str(args, 0, "encoding.url_decode")?;
    match urlencoding::decode(&encoded) {
        Ok(s) => Ok(Value::Result(Box::new(Ok(Value::Str(
            s.into_owned().into(),
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("URL decode error: {e}").into(),
        ))))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_runtime::Span;

    fn str_val(s: &str) -> Value {
        Value::Str(s.to_string().into())
    }

    #[test]
    fn decode_bytes_preserves_binary() {
        let mut interp = Interp::new();
        // 0xff is invalid UTF-8: the Str variant would mangle these bytes.
        let mut args = vec![str_val("/w8AAD4Y")]; // ff 0f 00 00 3e 18
        let out = encoding_base64_decode_bytes(&mut interp, &mut args, Span::new(0, 0)).unwrap();
        match out {
            Value::Result(r) => match &*r {
                Ok(Value::Bytes(b)) => {
                    assert_eq!(b.as_slice(), &[0xff, 0x0f, 0x00, 0x00, 0x3e, 0x18]);
                }
                other => panic!("expected Ok(Bytes), got {other:?}"),
            },
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[test]
    fn decode_bytes_rejects_garbage() {
        let mut interp = Interp::new();
        let mut args = vec![str_val("!!!not-base64!!!")];
        let out = encoding_base64_decode_bytes(&mut interp, &mut args, Span::new(0, 0)).unwrap();
        match out {
            Value::Result(r) => assert!(r.is_err()),
            other => panic!("expected Result, got {other:?}"),
        }
    }
}
