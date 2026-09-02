//! `std.http` — HTTP client and web framework.

use std::collections::HashMap;
use std::io::{Read, Write};

use crate::natives::{arg, expect_int, expect_str};
use zz_runtime::json::{parse_json, to_json_string, JsonValue};
use zz_runtime::value::{HttpServer, Response};
use zz_runtime::{EvalError, Interp, Span, Value};

// ---------------------------------------------------------------------------
// HTTP Client (reqwest blocking)
// ---------------------------------------------------------------------------

fn build_client() -> Result<reqwest::blocking::Client, EvalError> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| EvalError::new(format!("failed to build HTTP client: {e}"), Span::new(0, 0)))
}

fn dict_to_headers(v: &Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Value::Dict(entries) = v {
        for (k, val) in entries.iter() {
            if let (Value::Str(key), Value::Str(val)) = (k, val) {
                map.insert(key.clone(), val.clone());
            }
        }
    }
    map
}

fn build_response(resp: reqwest::blocking::Response) -> Value {
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp.text().unwrap_or_default();
    Value::Response(Response {
        status,
        body,
        headers,
    })
}

/// `http.get(url: str, headers: {str: str}) -> Result<http.response, str>`
pub(crate) fn http_get(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.get")?;
    let headers = args.get(1).cloned().unwrap_or(Value::Dict(vec![]));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.get(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.send() {
        Ok(resp) => Ok(Value::Result(Ok(Box::new(build_response(resp))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "HTTP GET failed: {e}"
        )))))),
    }
}

/// `http.post(url: str, body: str, headers: {str: str}) -> Result<http.response, str>`
pub(crate) fn http_post(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.post")?;
    let body = expect_str(args, 1, "std.http.post")?;
    let headers = args.get(2).cloned().unwrap_or(Value::Dict(vec![]));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.post(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.body(body).send() {
        Ok(resp) => Ok(Value::Result(Ok(Box::new(build_response(resp))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "HTTP POST failed: {e}"
        )))))),
    }
}

/// `http.put(url: str, body: str, headers: {str: str}) -> Result<http.response, str>`
pub(crate) fn http_put(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.put")?;
    let body = expect_str(args, 1, "std.http.put")?;
    let headers = args.get(2).cloned().unwrap_or(Value::Dict(vec![]));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.put(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.body(body).send() {
        Ok(resp) => Ok(Value::Result(Ok(Box::new(build_response(resp))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "HTTP PUT failed: {e}"
        )))))),
    }
}

/// `http.delete(url: str, headers: {str: str}) -> Result<http.response, str>`
pub(crate) fn http_delete(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.delete")?;
    let headers = args.get(1).cloned().unwrap_or(Value::Dict(vec![]));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.delete(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.send() {
        Ok(resp) => Ok(Value::Result(Ok(Box::new(build_response(resp))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "HTTP DELETE failed: {e}"
        )))))),
    }
}

// ---------------------------------------------------------------------------
// Response methods (dispatched via method_namespace "http")
// ---------------------------------------------------------------------------

/// `res.status() -> int`
pub(crate) fn http_response_status(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match arg(args, 0, "std.http.status")? {
        Value::Response(r) => Ok(Value::Int(r.status as i64)),
        other => Err(EvalError::new(
            format!("std.http.status: expected an http.response, found `{other}`"),
            span,
        )),
    }
}

/// `res.text() -> str`
pub(crate) fn http_response_text(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match arg(args, 0, "std.http.text")? {
        Value::Response(r) => Ok(Value::Str(r.body.clone())),
        other => Err(EvalError::new(
            format!("std.http.text: expected an http.response, found `{other}`"),
            span,
        )),
    }
}

/// `res.json() -> json`
pub(crate) fn http_response_json(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match arg(args, 0, "std.http.json")? {
        Value::Response(r) => match parse_json(&r.body) {
            Ok(j) => Ok(Value::Json(j)),
            Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
                "JSON parse error: {e}"
            )))))),
        },
        other => Err(EvalError::new(
            format!("std.http.json: expected an http.response, found `{other}`"),
            span,
        )),
    }
}

/// `res.headers() -> {str: str}`
pub(crate) fn http_response_headers(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match arg(args, 0, "std.http.headers")? {
        Value::Response(r) => {
            let entries: Vec<(Value, Value)> = r
                .headers
                .iter()
                .map(|(k, v)| (Value::Str(k.clone()), Value::Str(v.clone())))
                .collect();
            Ok(Value::Dict(entries))
        }
        other => Err(EvalError::new(
            format!("std.http.headers: expected an http.response, found `{other}`"),
            span,
        )),
    }
}

// ---------------------------------------------------------------------------
// Server — Route registration (per-route model)
// ---------------------------------------------------------------------------

pub(crate) fn http_server(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::HttpServer(HttpServer { routes: Vec::new() }))
}

/// `http.route_get(server, path, handler) -> http.server`
pub(crate) fn http_route_get(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    http_route(interp, args, "GET", span)
}

/// `http.route_post(server, path, handler) -> http.server`
pub(crate) fn http_route_post(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    http_route(interp, args, "POST", span)
}

/// `http.route_put(server, path, handler) -> http.server`
pub(crate) fn http_route_put(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    http_route(interp, args, "PUT", span)
}

/// `http.route_delete(server, path, handler) -> http.server`
pub(crate) fn http_route_delete(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    http_route(interp, args, "DELETE", span)
}

fn http_route(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    method: &str,
    span: Span,
) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http")?;
    let path = expect_str(args, 1, "std.http")?;
    let handler = args
        .get(2)
        .cloned()
        .ok_or_else(|| EvalError::new("missing handler for std.http route", span))?;
    if !matches!(handler, Value::Func(_)) {
        return Err(EvalError::new(
            "std.http: route handler must be a function",
            span,
        ));
    }
    let _ = interp;
    let mut server = server;
    server.routes.push((method.to_string(), path, handler));
    Ok(Value::HttpServer(server))
}

fn expect_server(args: &mut Vec<Value>, i: usize, name: &str) -> Result<HttpServer, EvalError> {
    match arg(args, i, name)? {
        Value::HttpServer(s) => Ok(s.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an http server, found `{other}`"),
            Span::new(0, 0),
        )),
    }
}

// ---------------------------------------------------------------------------
// Server — Request helpers (for single-handler model)
// ---------------------------------------------------------------------------

/// `http.request_method(req: dict) -> str`
pub(crate) fn http_request_method(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, 0, "std.http.request_method")? {
        Value::Dict(entries) => entries.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.http.request_method: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    for (k, v) in &req {
        if let (Value::Str(key), val) = (k, v) {
            if key == "method" {
                return Ok(val.clone());
            }
        }
    }
    Err(EvalError::new(
        "std.http.request_method: missing 'method' key",
        span,
    ))
}

/// `http.request_path(req: dict) -> str`
pub(crate) fn http_request_path(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, 0, "std.http.request_path")? {
        Value::Dict(entries) => entries.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.http.request_path: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    for (k, v) in &req {
        if let (Value::Str(key), val) = (k, v) {
            if key == "path" {
                return Ok(val.clone());
            }
        }
    }
    Err(EvalError::new(
        "std.http.request_path: missing 'path' key",
        span,
    ))
}

/// `http.request_body(req: dict) -> str`
pub(crate) fn http_request_body(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, 0, "std.http.request_body")? {
        Value::Dict(entries) => entries.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.http.request_body: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    for (k, v) in &req {
        if let (Value::Str(key), val) = (k, v) {
            if key == "body" {
                return Ok(val.clone());
            }
        }
    }
    Ok(Value::Str(String::new()))
}

/// `http.request_headers(req: dict) -> {str: str}`
pub(crate) fn http_request_headers(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, 0, "std.http.request_headers")? {
        Value::Dict(entries) => entries.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.http.request_headers: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    for (k, v) in &req {
        if let (Value::Str(key), val) = (k, v) {
            if key == "headers" {
                return Ok(val.clone());
            }
        }
    }
    Ok(Value::Dict(vec![]))
}

/// `http.request_query(req: dict) -> str`
pub(crate) fn http_request_query(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, 0, "std.http.request_query")? {
        Value::Dict(entries) => entries.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.http.request_query: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    for (k, v) in &req {
        if let (Value::Str(key), val) = (k, v) {
            if key == "query" {
                return Ok(val.clone());
            }
        }
    }
    Ok(Value::Str(String::new()))
}

// ---------------------------------------------------------------------------
// Server — Response builders
// ---------------------------------------------------------------------------

/// `http.response_json(data: json, status: int) -> http.response`
pub(crate) fn http_response_json_builder(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let data = match arg(args, 0, "std.http.response_json")? {
        Value::Json(j) => to_json_string(j),
        other => to_json_string(&jsonify_value(other)),
    };
    let status = expect_int(args, 1, "std.http.response_json")? as u16;
    Ok(Value::Response(Response {
        status,
        body: data,
        headers: vec![("Content-Type".into(), "application/json".into())],
    }))
}

/// `http.response_text(data: str, status: int) -> http.response`
pub(crate) fn http_response_text_builder(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "std.http.response_text")?;
    let status = expect_int(args, 1, "std.http.response_text")? as u16;
    Ok(Value::Response(Response {
        status,
        body: data,
        headers: vec![("Content-Type".into(), "text/plain".into())],
    }))
}

/// `http.response_html(data: str, status: int) -> http.response`
pub(crate) fn http_response_html_builder(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let data = expect_str(args, 0, "std.http.response_html")?;
    let status = expect_int(args, 1, "std.http.response_html")? as u16;
    Ok(Value::Response(Response {
        status,
        body: data,
        headers: vec![("Content-Type".into(), "text/html".into())],
    }))
}

fn jsonify_value(v: &Value) -> JsonValue {
    match v {
        Value::Int(i) => JsonValue::Num(*i as f64),
        Value::Float(f) => JsonValue::Num(*f),
        Value::Str(s) => JsonValue::Str(s.clone()),
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Unit => JsonValue::Null,
        Value::Array(arr) => JsonValue::Arr(arr.iter().map(jsonify_value).collect()),
        Value::Dict(entries) => {
            let obj: Vec<(String, JsonValue)> = entries
                .iter()
                .filter_map(|(k, v)| {
                    if let Value::Str(key) = k {
                        Some((key.clone(), jsonify_value(v)))
                    } else {
                        None
                    }
                })
                .collect();
            JsonValue::Obj(obj)
        }
        Value::Json(j) => j.clone(),
        _ => JsonValue::Null,
    }
}

// ---------------------------------------------------------------------------
// Server — Dispatch & Listen
// ---------------------------------------------------------------------------

/// Dispatch a request to the matching route and return the response body.
pub(crate) fn http_handle(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http.handle")?;
    let method = expect_str(args, 1, "std.http.handle")?;
    let path = expect_str(args, 2, "std.http.handle")?;
    let body = expect_str(args, 3, "std.http.handle")?;
    match dispatch(&server, &method, &path, body, interp, span) {
        Ok(body) => Ok(Value::Result(Ok(Box::new(Value::Str(body))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.message))))),
    }
}

/// Find a route (exact match first, then `*` wildcard) and call its handler.
fn dispatch(
    server: &HttpServer,
    method: &str,
    path: &str,
    body: String,
    interp: &mut Interp,
    span: Span,
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
        .ok_or_else(|| EvalError::new(format!("std.http: no route for {method} {path}"), span))?;
    let handler = route.2.clone();
    let arg = if method == "POST" || method == "PUT" || method == "DELETE" {
        Value::Str(body)
    } else {
        Value::Str(path.to_string())
    };
    match interp.call(handler, vec![arg], span)? {
        Value::Str(s) => Ok(s),
        Value::Response(r) => Ok(r.body),
        other => Err(EvalError::new(
            format!("std.http: handler returned `{other}`, expected a string or response"),
            span,
        )),
    }
}

/// Blocking HTTP server loop. Handles one request at a time.
pub(crate) fn http_listen(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http.listen")?;
    let port = match args.get(1) {
        Some(Value::Int(p)) => *p,
        other => {
            return Err(EvalError::new(
                format!("std.http.listen: expected an int port, found `{other:?}`"),
                span,
            ))
        }
    };
    let listener = std::net::TcpListener::bind(("0.0.0.0", port as u16)).map_err(|e| {
        EvalError::new(
            format!("std.http.listen: cannot bind port {port}: {e}"),
            span,
        )
    })?;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let response = handle_connection(&server, &mut stream, interp, span);
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
    span: Span,
) -> String {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return format_response(500, "internal error"),
    };
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    let mut lines = text.lines();
    let Some(request_line) = lines.next() else {
        return format_response(400, "bad request");
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return format_response(400, "bad request");
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
    match dispatch(server, method, path, body, interp, span) {
        Ok(body) => format_response(200, &body),
        Err(e) => format_response(500, &e.message),
    }
}

fn format_response(status: u16, body: &str) -> String {
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
