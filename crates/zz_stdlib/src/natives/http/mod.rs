//! `std.http` — HTTP client, web framework, middleware, and developer tools.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::Instant;

use crate::natives::{arg, expect_str};
use zz_runtime::json::{parse_json, to_json_string, JsonValue};
use zz_runtime::value::{HttpServer, Response};
use zz_runtime::{EvalError, Interp, Span, Value};

// ===========================================================================
// Helpers
// ===========================================================================

fn dict_to_headers(v: &Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Value::Dict(entries) = v {
        for (k, val) in entries.iter() {
            if let (Value::Str(key), Value::Str(val)) = (k, val) {
                map.insert((**key).clone(), (**val).clone());
            }
        }
    }
    map
}

fn jsonify_value(v: &Value) -> JsonValue {
    match v {
        Value::Int(i) => JsonValue::Num(*i as f64),
        Value::Float(f) => JsonValue::Num(*f),
        Value::Str(s) => JsonValue::Str((**s).clone()),
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Unit => JsonValue::Null,
        Value::Array(arr) => JsonValue::Arr(arr.iter().map(jsonify_value).collect()),
        Value::Dict(entries) => {
            let obj: Vec<(String, JsonValue)> = entries
                .iter()
                .filter_map(|(k, v)| {
                    if let Value::Str(key) = k {
                        Some(((**key).clone(), jsonify_value(v)))
                    } else {
                        None
                    }
                })
                .collect();
            JsonValue::Obj(obj)
        }
        Value::Json(j) => (**j).clone(),
        _ => JsonValue::Null,
    }
}

fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn parse_query_string(qs: &str) -> Vec<(String, String)> {
    if qs.is_empty() {
        return Vec::new();
    }
    qs.split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?.to_string();
            let val = parts.next().unwrap_or("").to_string();
            Some((key, val))
        })
        .collect()
}

fn extract_headers(text: &str) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if let Some((key, val)) = line.split_once(':') {
            headers.push((key.trim().to_string(), val.trim().to_string()));
        }
    }
    headers
}

fn http_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    }
}

fn format_response_with_headers(
    status: u16,
    extra_headers: &[(String, String)],
    body: &str,
) -> String {
    let reason = http_reason(status);
    let mut hdrs = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close",
        body.len()
    );
    for (k, v) in extra_headers {
        hdrs.push_str(&format!("\r\n{k}: {v}"));
    }
    format!("{hdrs}\r\n\r\n{body}")
}

fn format_response(status: u16, body: &str) -> String {
    format_response_with_headers(
        status,
        &[("Content-Type".into(), "text/plain; charset=utf-8".into())],
        body,
    )
}

/// Build a Value::Dict request object from parsed HTTP request data.
fn build_request_dict(
    method: &str,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    query_pairs: &[(String, String)],
    params: &[(String, String)],
) -> Value {
    let method_val = Value::Str(method.to_string().into());
    let path_val = Value::Str(path.to_string().into());
    let body_val = Value::Str(body.to_string().into());

    let headers_dict: Vec<(Value, Value)> = headers
        .iter()
        .map(|(k, v)| (Value::Str(k.clone().into()), Value::Str(v.clone().into())))
        .collect();

    let query_dict: Vec<(Value, Value)> = query_pairs
        .iter()
        .map(|(k, v)| (Value::Str(k.clone().into()), Value::Str(v.clone().into())))
        .collect();

    let params_dict: Vec<(Value, Value)> = params
        .iter()
        .map(|(k, v)| (Value::Str(k.clone().into()), Value::Str(v.clone().into())))
        .collect();

    Value::Dict(Box::new(vec![
        (Value::Str("method".to_string().into()), method_val),
        (Value::Str("path".to_string().into()), path_val),
        (Value::Str("body".to_string().into()), body_val),
        (
            Value::Str("headers".to_string().into()),
            Value::Dict(Box::new(headers_dict)),
        ),
        (
            Value::Str("query".to_string().into()),
            Value::Dict(Box::new(query_dict)),
        ),
        (
            Value::Str("params".to_string().into()),
            Value::Dict(Box::new(params_dict)),
        ),
    ]))
}

// ===========================================================================
// HTTP Client (reqwest blocking)
// ===========================================================================

fn build_client() -> Result<reqwest::blocking::Client, EvalError> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| EvalError::new(format!("failed to build HTTP client: {e}"), Span::new(0, 0)))
}

fn build_response_from_reqwest(resp: reqwest::blocking::Response) -> Value {
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp.text().unwrap_or_default();
    Value::Response(Box::new(Response {
        status,
        body,
        headers,
    }))
}

/// `http.get(url: str, headers: {str: str}) -> Result<Response, str>`
pub(crate) fn http_get(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.get")?;
    let headers = args.get(1).cloned().unwrap_or(Value::Dict(Box::default()));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.get(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.send() {
        Ok(resp) => Ok(Value::Result(Box::new(Ok(build_response_from_reqwest(
            resp,
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("HTTP GET failed: {e}").into(),
        ))))),
    }
}

/// `http.post(url: str, body: str, headers: {str: str}) -> Result<Response, str>`
pub(crate) fn http_post(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.post")?;
    let body = expect_str(args, 1, "std.http.post")?;
    let headers = args.get(2).cloned().unwrap_or(Value::Dict(Box::default()));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.post(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.body(body).send() {
        Ok(resp) => Ok(Value::Result(Box::new(Ok(build_response_from_reqwest(
            resp,
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("HTTP POST failed: {e}").into(),
        ))))),
    }
}

/// `http.put(url: str, body: str, headers: {str: str}) -> Result<Response, str>`
pub(crate) fn http_put(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.put")?;
    let body = expect_str(args, 1, "std.http.put")?;
    let headers = args.get(2).cloned().unwrap_or(Value::Dict(Box::default()));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.put(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.body(body).send() {
        Ok(resp) => Ok(Value::Result(Box::new(Ok(build_response_from_reqwest(
            resp,
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("HTTP PUT failed: {e}").into(),
        ))))),
    }
}

/// `http.delete(url: str, headers: {str: str}) -> Result<Response, str>`
pub(crate) fn http_delete(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let url = expect_str(args, 0, "std.http.delete")?;
    let headers = args.get(1).cloned().unwrap_or(Value::Dict(Box::default()));
    let client = build_client()?;
    let hdrs = dict_to_headers(&headers);
    let mut req = client.delete(&url);
    for (k, v) in &hdrs {
        req = req.header(k, v);
    }
    match req.send() {
        Ok(resp) => Ok(Value::Result(Box::new(Ok(build_response_from_reqwest(
            resp,
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("HTTP DELETE failed: {e}").into(),
        ))))),
    }
}

// ===========================================================================
// Response methods (dispatched via method_namespace "http")
// ===========================================================================

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

pub(crate) fn http_response_text(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match arg(args, 0, "std.http.text")? {
        Value::Response(r) => Ok(Value::Str(r.body.clone().into())),
        other => Err(EvalError::new(
            format!("std.http.text: expected an http.response, found `{other}`"),
            span,
        )),
    }
}

pub(crate) fn http_response_json(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match arg(args, 0, "std.http.json")? {
        Value::Response(r) => match parse_json(&r.body) {
            Ok(j) => Ok(Value::Json(Box::new(j))),
            Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
                format!("JSON parse error: {e}").into(),
            ))))),
        },
        other => Err(EvalError::new(
            format!("std.http.json: expected an http.response, found `{other}`"),
            span,
        )),
    }
}

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
                .map(|(k, v)| (Value::Str(k.clone().into()), Value::Str(v.clone().into())))
                .collect();
            Ok(Value::Dict(Box::new(entries)))
        }
        other => Err(EvalError::new(
            format!("std.http.headers: expected an http.response, found `{other}`"),
            span,
        )),
    }
}

// ===========================================================================
// Server — Route registration
// ===========================================================================

pub(crate) fn http_server(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::HttpServer(Box::new(HttpServer {
        routes: Vec::new(),
        middlewares: Vec::new(),
        log_enabled: false,
        static_dir: None,
    })))
}

pub(crate) fn http_route_get(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    http_route(interp, args, "GET", span)
}

pub(crate) fn http_route_post(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    http_route(interp, args, "POST", span)
}

pub(crate) fn http_route_put(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    http_route(interp, args, "PUT", span)
}

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
    Ok(Value::HttpServer(Box::new(server)))
}

fn expect_server(args: &mut Vec<Value>, i: usize, name: &str) -> Result<HttpServer, EvalError> {
    match arg(args, i, name)? {
        Value::HttpServer(s) => Ok((**s).clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an http server, found `{other}`"),
            Span::new(0, 0),
        )),
    }
}

// ===========================================================================
// Feature 6: Structured HTTP Logging
// ===========================================================================

/// `http.log(server, enabled: bool) -> Server`
pub(crate) fn http_log(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut server = expect_server(args, 0, "std.http.log")?;
    let enabled = match args.get(1) {
        Some(Value::Bool(b)) => *b,
        other => {
            return Err(EvalError::new(
                format!("std.http.log: expected a bool, found `{other:?}`"),
                span,
            ))
        }
    };
    server.log_enabled = enabled;
    Ok(Value::HttpServer(Box::new(server)))
}

// ===========================================================================
// Feature 5: Middleware Pipeline
// ===========================================================================

/// `http.pipe(server, middleware_fn) -> Server`
pub(crate) fn http_pipe(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut server = expect_server(args, 0, "std.http.pipe")?;
    let middleware = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("std.http.pipe: missing middleware function", span))?;
    if !matches!(middleware, Value::Func(_)) {
        return Err(EvalError::new(
            "std.http.pipe: middleware must be a function",
            span,
        ));
    }
    server.middlewares.push(middleware);
    Ok(Value::HttpServer(Box::new(server)))
}

// ===========================================================================
// Feature 4: Static File Serving
// ===========================================================================

/// `http.serve_dir(server, dir_path: str) -> Server`
pub(crate) fn http_serve_dir(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let mut server = expect_server(args, 0, "std.http.serve_dir")?;
    let dir = expect_str(args, 1, "std.http.serve_dir")?;
    server.static_dir = Some(dir);
    Ok(Value::HttpServer(Box::new(server)))
}

// ===========================================================================
// Feature 7: Built-in HTTP Testing Kit
// ===========================================================================

/// `http.test(server, method: str, path: str, body: str) -> Response`
pub(crate) fn http_test(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http.test")?;
    let method = expect_str(args, 1, "std.http.test")?;
    let path_with_query = expect_str(args, 2, "std.http.test")?;
    let body = expect_str(args, 3, "std.http.test")?;

    // Split path and query
    let (path, query_pairs) = if let Some((p, q)) = path_with_query.split_once('?') {
        (p.to_string(), parse_query_string(q))
    } else {
        (path_with_query.clone(), Vec::new())
    };

    let headers = Vec::new();
    let params = Vec::new();

    let result = dispatch_with_request(
        &server,
        &method,
        &path,
        &body,
        &headers,
        &query_pairs,
        &params,
        interp,
        span,
    );

    match result {
        Ok((status, resp_body, resp_headers)) => Ok(Value::Response(Box::new(Response {
            status,
            body: resp_body,
            headers: resp_headers,
        }))),
        Err(e) => Ok(Value::Response(Box::new(Response {
            status: 500,
            body: e.message,
            headers: Vec::new(),
        }))),
    }
}

// ===========================================================================
// Feature 3: Query & Form Data Parsing
// ===========================================================================

/// `http.query(req: dict) -> {str: str}`
pub(crate) fn http_query(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    extract_dict_field(args, 0, "query", "std.http.query", span)
}

/// `http.header(req: dict, name: str) -> Result<str, str>`
pub(crate) fn http_header(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, 0, "std.http.header")? {
        Value::Dict(entries) => (**entries).clone(),
        other => {
            return Err(EvalError::new(
                format!("std.http.header: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    let name = expect_str(args, 1, "std.http.header")?;
    for (k, v) in &req {
        if let (Value::Str(key), val) = (k, v) {
            if (**key).to_lowercase() == name.to_lowercase() {
                return Ok(Value::Result(Box::new(Ok(val.clone()))));
            }
        }
    }
    Ok(Value::Result(Box::new(Err(Value::Str(
        format!("header `{name}` not found").into(),
    )))))
}

/// `http.body_json(req: dict) -> json`
pub(crate) fn http_body_json(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let body = extract_dict_field_str(args, 0, "body", "std.http.body_json", span)?;
    match parse_json(&body) {
        Ok(j) => Ok(Value::Json(Box::new(j))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("JSON parse error: {e}").into(),
        ))))),
    }
}

/// `http.body_form(req: dict) -> {str: str}`
pub(crate) fn http_body_form(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let body = extract_dict_field_str(args, 0, "body", "std.http.body_form", span)?;
    let pairs = parse_query_string(&body);
    let dict: Vec<(Value, Value)> = pairs
        .iter()
        .map(|(k, v)| (Value::Str(k.clone().into()), Value::Str(v.clone().into())))
        .collect();
    Ok(Value::Dict(Box::new(dict)))
}

// ===========================================================================
// Feature 1: Dynamic Routing — Path Parameters
// ===========================================================================

/// `http.param(req: dict, name: str) -> Result<str, str>`
pub(crate) fn http_param(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, 0, "std.http.param")? {
        Value::Dict(entries) => (**entries).clone(),
        other => {
            return Err(EvalError::new(
                format!("std.http.param: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    let name = expect_str(args, 1, "std.http.param")?;
    for (k, v) in &req {
        if let (Value::Str(key), Value::Dict(params)) = (k, v) {
            if &**key == "params" {
                for (pk, pv) in &**params {
                    if let (Value::Str(pname), Value::Str(pval)) = (pk, pv) {
                        if **pname == name.as_str() {
                            return Ok(Value::Result(Box::new(Ok(Value::Str(pval.clone())))));
                        }
                    }
                }
            }
        }
    }
    Ok(Value::Result(Box::new(Err(Value::Str(
        format!("param `{name}` not found").into(),
    )))))
}

// ===========================================================================
// Request dict helpers (used by query, header, param, body_json, body_form)
// ===========================================================================

fn extract_dict_field(
    args: &mut Vec<Value>,
    i: usize,
    field: &str,
    func_name: &str,
    span: Span,
) -> Result<Value, EvalError> {
    let req = match arg(args, i, func_name)? {
        Value::Dict(entries) => (**entries).clone(),
        other => {
            return Err(EvalError::new(
                format!("{func_name}: expected a dict, found `{other}`"),
                span,
            ))
        }
    };
    for (k, v) in &req {
        if let Value::Str(key) = k {
            if **key == field {
                return Ok(v.clone());
            }
        }
    }
    Ok(Value::Dict(Box::default()))
}

fn extract_dict_field_str(
    args: &mut Vec<Value>,
    i: usize,
    field: &str,
    func_name: &str,
    span: Span,
) -> Result<String, EvalError> {
    let val = extract_dict_field(args, i, field, func_name, span)?;
    match val {
        Value::Str(s) => Ok((*s).clone()),
        _ => Ok("".to_string()),
    }
}

// ===========================================================================
// Route pattern matching (Feature 1: Dynamic Routing)
// ===========================================================================

/// Match a route pattern like "/users/:id" against an actual path.
/// Returns Some(params) if matched, None otherwise.
fn match_route_pattern(pattern: &str, actual: &str) -> Option<Vec<(String, String)>> {
    let pattern_segments: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let actual_segments: Vec<&str> = actual.trim_matches('/').split('/').collect();

    if pattern_segments.len() != actual_segments.len() {
        return None;
    }

    let mut params = Vec::new();
    for (pat, act) in pattern_segments.iter().zip(actual_segments.iter()) {
        if let Some(param_name) = pat.strip_prefix(':') {
            params.push((param_name.to_string(), act.to_string()));
        } else if *pat != *act && *pat != "*" {
            return None;
        }
    }
    Some(params)
}

// ===========================================================================
// Dispatch — Core request handling with middleware, params, auto-JSON
// ===========================================================================

/// Dispatch result: (status, body, headers)
type DispatchResult = Result<(u16, String, Vec<(String, String)>), EvalError>;

#[allow(clippy::too_many_arguments)]
fn dispatch_with_request(
    server: &HttpServer,
    method: &str,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    query_pairs: &[(String, String)],
    params: &[(String, String)],
    interp: &mut Interp,
    span: Span,
) -> DispatchResult {
    // Build the request dict
    let req_dict = build_request_dict(method, path, body, headers, query_pairs, params);

    // Run middleware chain
    let mut current_req = req_dict.clone();
    for mw in &server.middlewares {
        match interp.call(mw.clone(), vec![current_req.clone()], span)? {
            Value::Result(r) => match &*r {
                Ok(val) => {
                    // Middleware passed — it may have modified the request dict
                    if let Value::Dict(_) = val {
                        current_req = (*val).clone();
                    }
                }
                Err(err_val) => {
                    // Middleware rejected — err_val should be a Response
                    match err_val {
                        Value::Response(r) => {
                            return Ok((r.status, r.body.clone(), r.headers.clone()));
                        }
                        other => {
                            return Ok((401, format!("{other}"), vec![]));
                        }
                    }
                }
            },
            other => {
                return Err(EvalError::new(
                    format!("std.http: middleware returned `{other}`, expected .ok or .err"),
                    span,
                ));
            }
        }
    }

    // Find matching route (exact match first, then pattern match, then wildcard)
    let mut matched_params: Vec<(String, String)> = Vec::new();
    let handler = server
        .routes
        .iter()
        .find(|(m, p, _)| {
            if m != method {
                return false;
            }
            if p == path {
                return true;
            }
            // Try pattern matching
            if let Some(mut pm) = match_route_pattern(p, path) {
                matched_params.append(&mut pm);
                return true;
            }
            false
        })
        .or_else(|| {
            server
                .routes
                .iter()
                .find(|(m, p, _)| m == method && p == "*")
        })
        .map(|r| r.2.clone());

    let handler = match handler {
        Some(h) => h,
        None => {
            // No route matched — try static file serving
            if let Some(ref dir) = server.static_dir {
                return serve_static_file(dir, path, span);
            }
            return Err(EvalError::new(
                format!("std.http: no route for {method} {path}"),
                span,
            ));
        }
    };

    // Build enriched request dict with params
    let enriched_req = if matched_params.is_empty() {
        current_req
    } else {
        let params_dict: Vec<(Value, Value)> = matched_params
            .iter()
            .map(|(k, v)| (Value::Str(k.clone().into()), Value::Str(v.clone().into())))
            .collect();
        // Replace the "params" key in the request dict
        if let Value::Dict(mut entries) = current_req {
            entries.retain(|(k, _)| k != &Value::Str("params".to_string().into()));
            entries.push((
                Value::Str("params".to_string().into()),
                Value::Dict(Box::new(params_dict)),
            ));
            Value::Dict(entries)
        } else {
            current_req
        }
    };

    // Call handler
    let return_val = interp.call(handler, vec![enriched_req], span)?;
    match return_val {
        // String response → 200 text/plain
        Value::Str(s) => Ok((
            200,
            (*s).clone(),
            vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        )),
        // Response object → use its status/headers/body
        Value::Response(r) => Ok((r.status, r.body, r.headers)),
        // Dict/Array → auto-serialize to JSON
        ref val @ (Value::Dict(_) | Value::Array(_)) => {
            let json_val = jsonify_value(val);
            let body = to_json_string(&json_val);
            Ok((
                200,
                body,
                vec![(
                    "Content-Type".into(),
                    "application/json; charset=utf-8".into(),
                )],
            ))
        }
        // Other values → convert to string
        other => {
            // Other values → convert to string
            Ok((
                200,
                format!("{other}"),
                vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
            ))
        }
    }
}

/// Serve a static file from the given directory.
fn serve_static_file(dir: &str, path: &str, _span: Span) -> DispatchResult {
    let clean_path = if path.starts_with('/') {
        path.strip_prefix('/').unwrap_or(path)
    } else {
        path
    };

    // Prevent directory traversal
    if clean_path.contains("..") {
        return Ok((403, "Forbidden".into(), vec![]));
    }

    let file_path = if clean_path.is_empty() {
        format!("{dir}/index.html")
    } else {
        format!("{dir}/{clean_path}")
    };

    match std::fs::read_to_string(&file_path) {
        Ok(contents) => {
            let mime = guess_mime(&file_path);
            Ok((200, contents, vec![("Content-Type".into(), mime.into())]))
        }
        Err(_) => Ok((404, "Not Found".into(), vec![])),
    }
}

/// Public dispatch for http.handle (used by E2E tests).
pub(crate) fn dispatch(
    server: &HttpServer,
    method: &str,
    path: &str,
    body: String,
    interp: &mut Interp,
    span: Span,
) -> Result<String, EvalError> {
    let result = dispatch_with_request(server, method, path, &body, &[], &[], &[], interp, span)?;
    Ok(result.1)
}

// ===========================================================================
// Server — Handle & Listen
// ===========================================================================

/// `http.handle(server, method, path, body) -> Result<str, str>`
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
        Ok(body) => Ok(Value::Result(Box::new(Ok(Value::Str(body.into()))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(e.message.into()))))),
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
    if server.log_enabled {
        eprintln!("[INFO] Server listening on 0.0.0.0:{port}");
    }
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
    let start = Instant::now();

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
    let (Some(method), Some(raw_path)) = (parts.next(), parts.next()) else {
        return format_response(400, "bad request");
    };

    // Split path and query string
    let (path, query_pairs) = if let Some((p, q)) = raw_path.split_once('?') {
        (p.to_string(), parse_query_string(q))
    } else {
        (raw_path.to_string(), Vec::new())
    };

    // Extract headers
    let req_headers = extract_headers(&text);

    // Extract body
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

    // Dispatch with full request context
    let result = dispatch_with_request(
        server,
        method,
        &path,
        &body,
        &req_headers,
        &query_pairs,
        &[],
        interp,
        span,
    );

    let (status, resp_body, resp_headers) = match result {
        Ok(r) => r,
        Err(e) => (500, e.message, vec![]),
    };

    let elapsed = start.elapsed();

    // Feature 6: Structured logging
    if server.log_enabled {
        let color_start = match status {
            200..=299 => "\x1b[32m", // green
            300..=399 => "\x1b[33m", // yellow
            400..=499 => "\x1b[31m", // red
            500..=599 => "\x1b[35m", // magenta
            _ => "\x1b[0m",
        };
        let reset = "\x1b[0m";
        let reason = http_reason(status);
        eprintln!(
            "{color_start}[{status}] {method} {path} {reason}{reset} {:.2}ms",
            elapsed.as_secs_f64() * 1000.0
        );
    }

    format_response_with_headers(status, &resp_headers, &resp_body)
}
