//! Standard library native implementations, consumed by the interpreter.
//!
//! Native functions take `&mut Vec<Value>` (not a slice) because
//! `std.vec.push` must grow the argument vector.

#![allow(clippy::ptr_arg)]

use std::collections::HashMap;
use std::io::{Read, Write};

use zz_runtime::json::{parse_json, to_json_string, JsonValue};
use zz_runtime::value::HttpServer;
use zz_runtime::{EvalError, Interp, NativeEntry, Value};

/// All standard library native functions, keyed by qualified name.
pub fn stdlib_natives() -> HashMap<String, NativeEntry> {
    let mut m = HashMap::new();

    // std.io
    m.insert(
        "std.io.printz".into(),
        NativeEntry {
            arity: 1,
            f: printz,
        },
    );
    m.insert(
        "std.io.println".into(),
        NativeEntry {
            arity: 1,
            f: println,
        },
    );
    m.insert(
        "std.io.read_line".into(),
        NativeEntry {
            arity: 0,
            f: read_line,
        },
    );

    // std.str
    m.insert(
        "std.str.length".into(),
        NativeEntry {
            arity: 1,
            f: str_length,
        },
    );
    m.insert(
        "std.str.split".into(),
        NativeEntry {
            arity: 2,
            f: str_split,
        },
    );
    m.insert(
        "std.str.contains".into(),
        NativeEntry {
            arity: 2,
            f: str_contains,
        },
    );

    // std.vec
    m.insert(
        "std.vec.len".into(),
        NativeEntry {
            arity: 1,
            f: vec_len,
        },
    );
    m.insert(
        "std.vec.push".into(),
        NativeEntry {
            arity: 2,
            f: vec_push,
        },
    );
    m.insert(
        "std.vec.pop".into(),
        NativeEntry {
            arity: 1,
            f: vec_pop,
        },
    );

    // std.json
    m.insert(
        "std.json.parse".into(),
        NativeEntry {
            arity: 1,
            f: json_parse,
        },
    );
    m.insert(
        "std.json.stringify".into(),
        NativeEntry {
            arity: 1,
            f: json_stringify,
        },
    );
    m.insert(
        "std.json.get".into(),
        NativeEntry {
            arity: 2,
            f: json_get,
        },
    );
    m.insert(
        "std.json.as_str".into(),
        NativeEntry {
            arity: 1,
            f: json_as_str,
        },
    );
    m.insert(
        "std.json.as_int".into(),
        NativeEntry {
            arity: 1,
            f: json_as_int,
        },
    );
    m.insert(
        "std.json.as_float".into(),
        NativeEntry {
            arity: 1,
            f: json_as_float,
        },
    );
    m.insert(
        "std.json.as_bool".into(),
        NativeEntry {
            arity: 1,
            f: json_as_bool,
        },
    );

    // std.http
    m.insert(
        "std.http.server".into(),
        NativeEntry {
            arity: 0,
            f: http_server,
        },
    );
    m.insert(
        "std.http.get".into(),
        NativeEntry {
            arity: 3,
            f: http_get,
        },
    );
    m.insert(
        "std.http.post".into(),
        NativeEntry {
            arity: 3,
            f: http_post,
        },
    );
    m.insert(
        "std.http.handle".into(),
        NativeEntry {
            arity: 4,
            f: http_handle,
        },
    );
    m.insert(
        "std.http.listen".into(),
        NativeEntry {
            arity: 2,
            f: http_listen,
        },
    );

    m
}

// --- helpers ---------------------------------------------------------------

fn arg<'a>(args: &'a mut Vec<Value>, i: usize, name: &str) -> Result<&'a Value, EvalError> {
    args.get(i).ok_or_else(|| {
        EvalError::new(
            format!("missing argument `{name}` for native function"),
            zz_runtime::Span::new(0, 0),
        )
    })
}

fn expect_str(args: &mut Vec<Value>, i: usize, name: &str) -> Result<String, EvalError> {
    match arg(args, i, name)? {
        Value::Str(s) => Ok(s.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a string, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn expect_array(args: &mut Vec<Value>, i: usize, name: &str) -> Result<Vec<Value>, EvalError> {
    match arg(args, i, name)? {
        Value::Array(vs) => Ok(vs.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an array, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

// --- std.io -----------------------------------------------------------------

fn printz(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let v = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for std.io.printz",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    print!("{v}");
    Ok(Value::Unit)
}

fn println(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let v = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for std.io.println",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    println!("{v}");
    Ok(Value::Unit)
}

fn read_line(_interp: &mut Interp, _args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| {
        EvalError::new(
            format!("failed to read line: {e}"),
            zz_runtime::Span::new(0, 0),
        )
    })?;
    // Strip the trailing newline (and CR for Windows line endings).
    Ok(Value::Str(line.trim_end_matches(['\n', '\r']).to_string()))
}

// --- std.str ----------------------------------------------------------------

fn str_length(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.length")?;
    Ok(Value::Int(s.chars().count() as i64))
}

fn str_split(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.split")?;
    let sep = expect_str(args, 1, "std.str.split")?;
    let parts: Vec<Value> = s.split(&sep).map(|p| Value::Str(p.to_string())).collect();
    Ok(Value::Array(parts))
}

fn str_contains(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.contains")?;
    let sub = expect_str(args, 1, "std.str.contains")?;
    Ok(Value::Bool(s.contains(&sub)))
}

// --- std.vec ----------------------------------------------------------------

fn vec_len(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "std.vec.len")?;
    Ok(Value::Int(vs.len() as i64))
}

fn vec_push(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "std.vec.push")?;
    let x = args.get(1).cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument `x` for std.vec.push",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    vs.push(x);
    Ok(Value::Array(vs))
}

fn vec_pop(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "std.vec.pop")?;
    if vs.is_empty() {
        return Err(EvalError::new(
            "std.vec.pop: cannot pop from an empty array",
            zz_runtime::Span::new(0, 0),
        ));
    }
    vs.pop();
    Ok(Value::Array(vs))
}

// --- std.json ---------------------------------------------------------------

fn json_parse(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.json.parse")?;
    match parse_json(&s) {
        Ok(j) => Ok(Value::Json(j)),
        Err(msg) => Err(EvalError::new(
            format!("std.json.parse: {msg}"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn json_stringify(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let v = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for std.json.stringify",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    let j = value_to_json(&v)?;
    Ok(Value::Str(to_json_string(&j)))
}

fn json_get(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let j = expect_json(args, 0, "std.json.get")?;
    let key = expect_str(args, 1, "std.json.get")?;
    match j {
        JsonValue::Obj(entries) => entries
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| Value::Json(v))
            .ok_or_else(|| {
                EvalError::new(
                    format!("std.json.get: no key `{key}`"),
                    zz_runtime::Span::new(0, 0),
                )
            }),
        other => Err(EvalError::new(
            format!("std.json.get: expected an object, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn json_as_str(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_str")? {
        JsonValue::Str(s) => Ok(Value::Str(s)),
        other => Err(EvalError::new(
            format!("std.json.as_str: expected a string, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn json_as_int(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_int")? {
        JsonValue::Num(n) if n.fract() == 0.0 && n.is_finite() => Ok(Value::Int(n as i64)),
        other => Err(EvalError::new(
            format!("std.json.as_int: expected an integer, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn json_as_float(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_float")? {
        JsonValue::Num(n) => Ok(Value::Float(n)),
        other => Err(EvalError::new(
            format!("std.json.as_float: expected a number, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn json_as_bool(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_bool")? {
        JsonValue::Bool(b) => Ok(Value::Bool(b)),
        other => Err(EvalError::new(
            format!("std.json.as_bool: expected a boolean, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn expect_json(args: &mut Vec<Value>, i: usize, name: &str) -> Result<JsonValue, EvalError> {
    match arg(args, i, name)? {
        Value::Json(j) => Ok(j.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a JSON value, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

/// Convert a runtime value to a JSON value.
fn value_to_json(v: &Value) -> Result<JsonValue, EvalError> {
    let err = |what: &str| {
        EvalError::new(
            format!("std.json.stringify: cannot serialize {what}"),
            zz_runtime::Span::new(0, 0),
        )
    };
    match v {
        Value::Int(i) => Ok(JsonValue::Num(*i as f64)),
        Value::Float(f) => Ok(JsonValue::Num(*f)),
        Value::Str(s) => Ok(JsonValue::Str(s.clone())),
        Value::Bool(b) => Ok(JsonValue::Bool(*b)),
        Value::Unit => Ok(JsonValue::Null),
        Value::Option(Some(inner)) => value_to_json(inner),
        Value::Option(None) => Ok(JsonValue::Null),
        Value::Result(Ok(inner)) => value_to_json(inner),
        Value::Result(Err(_)) => Ok(JsonValue::Null),
        Value::Array(vs) => {
            let mut items = Vec::with_capacity(vs.len());
            for x in vs {
                items.push(value_to_json(x)?);
            }
            Ok(JsonValue::Arr(items))
        }
        Value::Dict(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, val) in entries {
                let key = match k {
                    Value::Str(s) => s.clone(),
                    other => return Err(err(&format!("a non-string key (`{other}`)"))),
                };
                out.push((key, value_to_json(val)?));
            }
            Ok(JsonValue::Obj(out))
        }
        Value::Func(_) | Value::Native(_) => Err(err("a function")),
        Value::Json(j) => Ok(j.clone()),
        Value::HttpServer(_) => Err(err("an http server")),
    }
}

// --- std.http ---------------------------------------------------------------

fn http_server(_interp: &mut Interp, _args: &mut Vec<Value>) -> Result<Value, EvalError> {
    Ok(Value::HttpServer(HttpServer { routes: Vec::new() }))
}

fn http_get(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    http_route(interp, args, "GET")
}

fn http_post(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    http_route(interp, args, "POST")
}

fn http_route(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    method: &str,
) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http")?;
    let path = expect_str(args, 1, "std.http")?;
    let handler = args.get(2).cloned().ok_or_else(|| {
        EvalError::new(
            "missing handler for std.http route",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    if !matches!(handler, Value::Func(_)) {
        return Err(EvalError::new(
            "std.http: route handler must be a function",
            zz_runtime::Span::new(0, 0),
        ));
    }
    let _ = interp; // handler is stored, not called, at registration time
    let mut server = server;
    server.routes.push((method.to_string(), path, handler));
    Ok(Value::HttpServer(server))
}

fn expect_server(args: &mut Vec<Value>, i: usize, name: &str) -> Result<HttpServer, EvalError> {
    match arg(args, i, name)? {
        Value::HttpServer(s) => Ok(s.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an http server, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

/// Dispatch a request to the matching route and return the response body.
fn http_handle(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http.handle")?;
    let method = expect_str(args, 1, "std.http.handle")?;
    let path = expect_str(args, 2, "std.http.handle")?;
    let body = expect_str(args, 3, "std.http.handle")?;
    let body = dispatch(&server, &method, &path, body, interp)?;
    Ok(Value::Str(body))
}

/// Find a route (exact match first, then `*` wildcard) and call its handler.
fn dispatch(
    server: &HttpServer,
    method: &str,
    path: &str,
    body: String,
    interp: &mut Interp,
) -> Result<String, EvalError> {
    let route = server
        .routes
        .iter()
        .find(|(m, p, _)| m == method && p == path)
        .or_else(|| {
            server
                .routes
                .iter()
                .find(|(m, p, _)| m == method && p == "*")
        })
        .ok_or_else(|| {
            EvalError::new(
                format!("std.http: no route for {method} {path}"),
                zz_runtime::Span::new(0, 0),
            )
        })?;
    let handler = route.2.clone();
    let arg = if method == "POST" {
        Value::Str(body)
    } else {
        Value::Str(path.to_string())
    };
    match interp.call(handler, vec![arg], zz_runtime::Span::new(0, 0))? {
        Value::Str(s) => Ok(s),
        other => Err(EvalError::new(
            format!("std.http: handler returned `{other}`, expected a string"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

/// Blocking HTTP server loop. Handles one request at a time.
fn http_listen(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http.listen")?;
    let port = match args.get(1) {
        Some(Value::Int(p)) => *p,
        other => {
            return Err(EvalError::new(
                format!("std.http.listen: expected an int port, found `{other:?}`"),
                zz_runtime::Span::new(0, 0),
            ))
        }
    };
    let listener = std::net::TcpListener::bind(("0.0.0.0", port as u16)).map_err(|e| {
        EvalError::new(
            format!("std.http.listen: cannot bind port {port}: {e}"),
            zz_runtime::Span::new(0, 0),
        )
    })?;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let response = handle_connection(&server, &mut stream, interp);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
    Ok(Value::Unit)
}

/// Read one HTTP request from the stream and produce a full response.
fn handle_connection(
    server: &HttpServer,
    stream: &mut std::net::TcpStream,
    interp: &mut Interp,
) -> String {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return http_response(500, "internal error"),
    };
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    let mut lines = text.lines();
    let Some(request_line) = lines.next() else {
        return http_response(400, "bad request");
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return http_response(400, "bad request");
    };
    let mut body = String::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        if let Some(idx) = text.find("\r\n\r\n") {
            let start = idx + 4;
            if start < n {
                body = text[start..].chars().take(content_length).collect();
            }
        }
    }
    match dispatch(server, method, path, body, interp) {
        Ok(body) => http_response(200, &body),
        Err(e) => http_response(500, &e.message),
    }
}

fn http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        500 => "Internal Server Error",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Vec<Value>) -> Result<Value, EvalError> {
        let mut interp = Interp::new();
        let mut args = args;
        let entry = stdlib_natives()[name];
        (entry.f)(&mut interp, &mut args)
    }

    #[test]
    fn str_length_counts_chars() {
        assert_eq!(
            call("std.str.length", vec![Value::Str("héllo".into())]).unwrap(),
            Value::Int(5)
        );
    }

    #[test]
    fn str_split_splits() {
        assert_eq!(
            call(
                "std.str.split",
                vec![Value::Str("a,b,c".into()), Value::Str(",".into())]
            )
            .unwrap(),
            Value::Array(vec![
                Value::Str("a".into()),
                Value::Str("b".into()),
                Value::Str("c".into()),
            ])
        );
    }

    #[test]
    fn str_contains_finds_substring() {
        assert_eq!(
            call(
                "std.str.contains",
                vec![Value::Str("hello".into()), Value::Str("ell".into())]
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call(
                "std.str.contains",
                vec![Value::Str("hello".into()), Value::Str("xyz".into())]
            )
            .unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn vec_len_counts() {
        assert_eq!(
            call(
                "std.vec.len",
                vec![Value::Array(vec![Value::Int(1), Value::Int(2)])]
            )
            .unwrap(),
            Value::Int(2)
        );
    }

    #[test]
    fn vec_push_appends() {
        assert_eq!(
            call(
                "std.vec.push",
                vec![Value::Array(vec![Value::Int(1)]), Value::Int(2),]
            )
            .unwrap(),
            Value::Array(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn vec_pop_removes_last() {
        assert_eq!(
            call(
                "std.vec.pop",
                vec![Value::Array(vec![Value::Int(1), Value::Int(2)])]
            )
            .unwrap(),
            Value::Array(vec![Value::Int(1)])
        );
    }

    #[test]
    fn vec_pop_empty_errors() {
        let err = call("std.vec.pop", vec![Value::Array(vec![])]).unwrap_err();
        assert!(err.message.contains("empty array"), "{}", err.message);
    }

    #[test]
    fn wrong_type_errors() {
        let err = call("std.str.length", vec![Value::Int(5)]).unwrap_err();
        assert!(err.message.contains("expects a string"), "{}", err.message);
    }

    #[test]
    fn read_line_from_dev_null_is_empty() {
        // In the test harness stdin is /dev/null, so read_line yields "".
        assert_eq!(
            call("std.io.read_line", vec![]).unwrap(),
            Value::Str(String::new())
        );
    }
}
