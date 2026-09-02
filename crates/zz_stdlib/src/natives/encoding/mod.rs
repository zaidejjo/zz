use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn encoding_base64_encode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "encoding.base64_encode")?;
    use base64::Engine;
    Ok(Value::Str(
        base64::engine::general_purpose::STANDARD.encode(data.as_bytes()),
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
        Ok(bytes) => Ok(Value::Result(Ok(Box::new(Value::Str(
            String::from_utf8_lossy(&bytes).to_string(),
        ))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "base64 decode error: {e}"
        )))))),
    }
}

pub(crate) fn encoding_hex_encode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "encoding.hex_encode")?;
    let hex: String = data.bytes().map(|b| format!("{b:02x}")).collect();
    Ok(Value::Str(hex))
}

pub(crate) fn encoding_hex_decode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let encoded = expect_str(args, 0, "encoding.hex_decode")?;
    if encoded.len() % 2 != 0 {
        return Ok(Value::Result(Err(Box::new(Value::Str(
            "odd-length hex string".to_string(),
        )))));
    }
    match (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
    {
        Ok(bytes) => Ok(Value::Result(Ok(Box::new(Value::Str(
            String::from_utf8_lossy(&bytes).to_string(),
        ))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "hex decode error: {e}"
        )))))),
    }
}

pub(crate) fn encoding_url_encode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "encoding.url_encode")?;
    Ok(Value::Str(urlencoding::encode(&data).into_owned()))
}

pub(crate) fn encoding_url_decode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let encoded = expect_str(args, 0, "encoding.url_decode")?;
    match urlencoding::decode(&encoded) {
        Ok(s) => Ok(Value::Result(Ok(Box::new(Value::Str(s.into_owned()))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "URL decode error: {e}"
        )))))),
    }
}
