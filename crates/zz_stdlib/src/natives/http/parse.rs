//! Zero-copy HTTP/1.x head parsing (Phase 3.4).
//!
//! `httparse` borrows the already-buffered head bytes: method, path, and
//! header values are views into `head_buf` — no lossy whole-head `String`,
//! no `lines()` split, no per-line lowercase allocs. Only what dispatch
//! needs (method/path/query/owned headers) is materialized, once, and the
//! parser enforces strict framing (garbage → 400) plus a header-count cap
//! (→ 431) the old scan lacked.

/// Max headers per request: generous (cookies/auth), still bounded.
/// Total head bytes stay capped by `MAX_HEAD_BYTES` in the reader.
pub(crate) const MAX_PARSE_HEADERS: usize = 128;

pub(crate) struct ParsedHead<'a> {
    pub method: &'a str,
    pub raw_path: &'a str,
    /// True for `HTTP/1.1` (keep-alive default); 1.0/unknown closes.
    pub version_1_1: bool,
    /// Byte offset where the head ends (body remainder starts).
    pub head_len: usize,
    pub content_length: usize,
    pub expect_continue: bool,
    pub connection: Option<&'a [u8]>,
    pub headers: Vec<(&'a str, &'a [u8])>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadError {
    Malformed,
    TooManyHeaders,
}

pub(crate) fn parse_head(buf: &[u8]) -> Result<ParsedHead<'_>, HeadError> {
    let mut raw = [httparse::EMPTY_HEADER; MAX_PARSE_HEADERS];
    let mut req = httparse::Request::new(&mut raw);
    let head_len = match req.parse(buf) {
        Ok(httparse::Status::Complete(n)) => n,
        // The reader only hands over bytes through `\r\n\r\n`, so a
        // partial parse means framing desync: treat as malformed.
        Ok(httparse::Status::Partial) => return Err(HeadError::Malformed),
        Err(httparse::Error::TooManyHeaders) => return Err(HeadError::TooManyHeaders),
        Err(_) => return Err(HeadError::Malformed),
    };
    let (Some(method), Some(raw_path)) = (req.method, req.path) else {
        return Err(HeadError::Malformed);
    };
    if method.is_empty() || raw_path.is_empty() {
        return Err(HeadError::Malformed);
    }
    let mut headers = Vec::with_capacity(req.headers.len());
    let mut content_length = 0usize;
    let mut expect_continue = false;
    let mut connection = None;
    for h in req.headers.iter() {
        headers.push((h.name, h.value));
        if h.name.eq_ignore_ascii_case("content-length") {
            content_length = std::str::from_utf8(h.value)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
        } else if h.name.eq_ignore_ascii_case("expect") {
            expect_continue = std::str::from_utf8(h.value)
                .is_ok_and(|v| v.trim().eq_ignore_ascii_case("100-continue"));
        } else if h.name.eq_ignore_ascii_case("connection") {
            connection = Some(h.value);
        }
    }
    Ok(ParsedHead {
        method,
        raw_path,
        version_1_1: req.version == Some(1),
        head_len,
        content_length,
        expect_continue,
        connection,
        headers,
    })
}

/// Case-insensitive substring search over raw bytes (header values may
/// not be UTF-8; decoding them just to sniff `close` wastes a copy).
fn contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

/// Fold a `Connection` header value into the keep-alive default:
/// explicit `close` wins, else explicit `keep-alive` wins.
pub(crate) fn fold_connection(keep_alive: bool, connection: Option<&[u8]>) -> bool {
    match connection {
        Some(v) if contains_ignore_case(v, b"close") => false,
        Some(v) if contains_ignore_case(v, b"keep-alive") => true,
        _ => keep_alive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(raw: &[u8]) -> ParsedHead<'_> {
        parse_head(raw).expect("valid head")
    }

    #[test]
    fn borrows_without_copying() {
        let buf = b"GET /users/42?x=1 HTTP/1.1\r\nHost: a\r\nContent-Length: 5\r\n\r\nhello";
        let h = head(buf);
        assert_eq!(h.method, "GET");
        assert_eq!(h.raw_path, "/users/42?x=1");
        assert!(h.version_1_1);
        assert_eq!(h.head_len, buf.len() - 5);
        assert_eq!(h.content_length, 5);
        // Views point into the input buffer, not copies.
        assert!(h.method.as_ptr() >= buf.as_ptr());
    }

    #[test]
    fn version_and_connection_rules() {
        let h10 = head(b"GET / HTTP/1.0\r\n\r\n");
        assert!(!h10.version_1_1);
        assert!(!fold_connection(h10.version_1_1, h10.connection));
        let h11ka = head(b"GET / HTTP/1.1\r\nConnection: keep-alive\r\n\r\n");
        assert!(fold_connection(h11ka.version_1_1, h11ka.connection));
        let h11close = head(b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n");
        assert!(!fold_connection(h11close.version_1_1, h11close.connection));
        let h10ka = head(b"GET / HTTP/1.0\r\nConnection: Keep-Alive\r\n\r\n");
        assert!(fold_connection(h10ka.version_1_1, h10ka.connection));
    }

    #[test]
    fn garbage_is_400() {
        for bad in [
            &b"NOT A REQUEST\r\n\r\n"[..],
            b"GET\r\n\r\n",
            b"GET / HTTP/1.1\r\nNo-Colon-Here\r\n\r\n",
            b"\r\n\r\n",
        ] {
            assert!(
                matches!(parse_head(bad), Err(HeadError::Malformed)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn header_cap_is_431() {
        let mut buf = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..200 {
            buf.extend_from_slice(format!("X-Pad-{i}: v\r\n").as_bytes());
        }
        buf.extend_from_slice(b"\r\n");
        assert!(matches!(parse_head(&buf), Err(HeadError::TooManyHeaders)));
    }

    #[test]
    fn expect_continue_detected() {
        let h = head(b"POST /u HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 3\r\n\r\nabc");
        assert!(h.expect_continue);
        assert_eq!(h.content_length, 3);
    }
}
