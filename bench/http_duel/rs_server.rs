// bench/http_duel/rs_server.rs — std-only Rust contender (no crates).
// Thread-per-connection, blocking I/O, HTTP/1.1 keep-alive + pipelining,
// prebuilt response bytes, single write_all per request. Mirrors the ZZ
// bench app's hot route; wrk hammers / only.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

static RESP_KA: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nOK";
static RESP_CLOSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nOK";
static RESP_PONG: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\nContent-Type: text/plain; charset=utf-8\r\n\r\npong";

fn find_head_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn token_ci(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| {
        w.iter()
            .zip(needle.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

// Keep-alive default from the HTTP version on the request line;
// an explicit Connection header overrides either way.
fn wants_close(head: &[u8]) -> bool {
    let eol = head
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(head.len());
    let line = &head[..eol];
    let mut keep = line.ends_with(b"HTTP/1.1");
    let mut pos = eol + 2;
    while pos + 1 < head.len() {
        let rel = head[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .unwrap_or(head.len() - pos);
        let eol2 = pos + rel;
        if eol2 == pos {
            break;
        }
        if let Some(ci) = head[pos..eol2].iter().position(|&b| b == b':') {
            let (name, val) = (&head[pos..pos + ci], &head[pos + ci + 1..eol2]);
            if name.eq_ignore_ascii_case(b"connection") {
                let v = trim_left(val);
                if token_ci(v, b"close") {
                    return true;
                }
                if token_ci(v, b"keep-alive") {
                    keep = true;
                }
            }
        }
        pos = eol2 + 2;
    }
    !keep
}

fn trim_left(mut v: &[u8]) -> &[u8] {
    while matches!(v.first(), Some(b' ') | Some(b'\t')) {
        v = &v[1..];
    }
    v
}

fn path_is_ping(head: &[u8]) -> bool {
    let eol = head
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(head.len());
    let line = &head[..eol];
    // "GET /ping HTTP/1.1" — target is the second token.
    let mut parts = line.split(|&b| b == b' ');
    let _ = parts.next();
    matches!(parts.next(), Some(b"/ping"))
}

fn handle(mut s: TcpStream) {
    let _ = s.set_nodelay(true);
    let mut buf = [0u8; 8192];
    let mut len = 0usize;
    loop {
        let hend = loop {
            if let Some(h) = find_head_end(&buf[..len]) {
                break h;
            }
            if len == buf.len() {
                return; // head too large
            }
            match s.read(&mut buf[len..]) {
                Ok(0) => return,
                Ok(n) => len += n,
                Err(_) => return,
            }
        };
        let head = &buf[..hend];
        let close = wants_close(head);
        let resp = if path_is_ping(head) && !close {
            RESP_PONG
        } else if close {
            RESP_CLOSE
        } else {
            RESP_KA
        };
        if s.write_all(resp).is_err() {
            return;
        }
        let left = len - hend;
        buf.copy_within(hend..len, 0);
        len = left;
        if close {
            return;
        }
    }
}

fn main() {
    let listener = TcpListener::bind("0.0.0.0:8080").expect("bind");
    println!("SERVER_READY");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                std::thread::Builder::new()
                    .stack_size(64 * 1024)
                    .spawn(move || handle(s))
                    .ok();
            }
            Err(_) => continue,
        }
    }
}
