use std::io::{Read, Write};

use crate::natives::{arg, expect_str};
use zz_runtime::value::HttpServer;
use zz_runtime::{EvalError, Interp, Value};

pub(crate) fn http_server(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    Ok(Value::HttpServer(HttpServer { routes: Vec::new() }))
}

pub(crate) fn http_get(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    http_route(interp, args, "GET")
}

pub(crate) fn http_post(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
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

pub(crate) fn expect_server(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
) -> Result<HttpServer, EvalError> {
    match arg(args, i, name)? {
        Value::HttpServer(s) => Ok(s.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an http server, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

/// Dispatch a request to the matching route and return the response body.
pub(crate) fn http_handle(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let server = expect_server(args, 0, "std.http.handle")?;
    let method = expect_str(args, 1, "std.http.handle")?;
    let path = expect_str(args, 2, "std.http.handle")?;
    let body = expect_str(args, 3, "std.http.handle")?;
    let body = dispatch(&server, &method, &path, body, interp)?;
    Ok(Value::Str(body))
}

/// Find a route (exact match first, then `*` wildcard) and call its handler.
pub(crate) fn dispatch(
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
pub(crate) fn http_listen(interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
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
