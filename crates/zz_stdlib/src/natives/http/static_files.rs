//! Hardened static file serving for `std.http` (Phase 2.1).
//!
//! - Traversal-proof: percent-decode → lexical normalize → `canonicalize`
//!   containment (catches `..`, `%2e%2e`, and symlink escapes).
//! - Binary-safe: bodies are raw bytes, never `String`.
//! - Cache-aware: `ETag` + `If-None-Match` → 304.
//! - Streaming clients: single `Range: bytes=a-b` → 206, bad range → 416.
//! - No directory listings: directories need `index.html` or 404.

use super::DispatchResult;

fn guess_mime(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("mjs") | Some("js") => "application/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("wasm") => "application/wasm",
        Some("xml") => "application/xml",
        Some("csv") => "text/csv; charset=utf-8",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("otf") => "font/otf",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// Strong-enough ETag for static assets: length + mtime mix.
/// mtime granularity is coarse on some filesystems; length disambiguates.
fn etag_for(len: u64, mtime: std::time::SystemTime) -> String {
    let nanos = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("\"{len:x}-{nanos:x}\"")
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// `If-None-Match` hits when `*` or our ETag is listed (weak prefix tolerated).
fn none_match(headers: &[(String, String)], etag: &str) -> bool {
    let Some(inm) = header(headers, "if-none-match") else {
        return false;
    };
    inm.split(',').any(|tag| {
        let tag = tag.trim().strip_prefix("W/").unwrap_or(tag.trim());
        tag == "*" || tag == etag
    })
}

/// Parse a single `Range: bytes=a-b | a- | -n` header.
/// Returns `(start, end_inclusive)` clamped to `len`, or `None` when
/// unsatisfiable (caller replies 416) or not a single byte-range.
fn parse_range(headers: &[(String, String)], len: u64) -> Option<Option<(u64, u64)>> {
    let Some(raw) = header(headers, "range") else {
        return Some(None);
    };
    let spec = raw.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None; // multi-range: 416 (documented P2 choice, no multipart)
    }
    let (start_s, end_s) = spec.split_once('-')?;
    if start_s.is_empty() {
        // suffix: last N bytes
        let n: u64 = end_s.trim().parse().ok()?;
        if n == 0 || len == 0 {
            return None;
        }
        let n = n.min(len);
        return Some(Some((len - n, len - 1)));
    }
    let start: u64 = start_s.trim().parse().ok()?;
    if start >= len {
        return None;
    }
    let end = if end_s.trim().is_empty() {
        len - 1
    } else {
        end_s.trim().parse::<u64>().ok()?.min(len - 1)
    };
    if end < start {
        return None;
    }
    Some(Some((start, end)))
}

fn not_found() -> DispatchResult {
    Ok((
        404,
        b"Not Found".to_vec(),
        vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
    ))
}

/// Serve `path` from `dir`. Never touches the interpreter: pure I/O.
pub(crate) fn serve_static_file(
    dir: &str,
    path: &str,
    headers: &[(String, String)],
) -> DispatchResult {
    // 1. Strip leading `/`, percent-decode (`%2e%2e` must not survive to join).
    let rel = path.strip_prefix('/').unwrap_or(path);
    if rel.contains('\0') {
        return Ok((
            400,
            b"Bad Request".to_vec(),
            vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        ));
    }
    let decoded = urlencoding::decode(rel)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| rel.to_string());
    // 2. Strict: any `..` segment is an attack signal → 403, even when
    //    it would clamp inside the root (predictable, scanner-friendly).
    if decoded.split('/').any(|s| s == "..") {
        return Ok((
            403,
            b"Forbidden".to_vec(),
            vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        ));
    }
    // 3. Lexical normalize (defense in depth) + canonicalize containment
    //    (catches symlink escapes that lexing cannot see).
    let norm = crate::natives::fs::path::normalize(&decoded, crate::natives::fs::path::Style::Unix);
    let Ok(root) = std::fs::canonicalize(dir) else {
        return not_found();
    };
    let joined = root.join(norm.trim_start_matches('/'));
    let Ok(mut canon) = std::fs::canonicalize(&joined) else {
        return not_found();
    };
    if !canon.starts_with(&root) {
        return Ok((
            403,
            b"Forbidden".to_vec(),
            vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        ));
    }
    // 4. Directories need index.html (never list).
    if canon.is_dir() {
        canon.push("index.html");
        if !canon.is_file() {
            return not_found();
        }
    }
    let Ok(meta) = std::fs::metadata(&canon) else {
        return not_found();
    };
    if !meta.is_file() {
        return not_found();
    }
    let len = meta.len();
    let etag = etag_for(len, meta.modified().unwrap_or(std::time::UNIX_EPOCH));
    let mime = guess_mime(&canon);

    let mut out_headers = vec![
        ("Content-Type".into(), mime.to_string()),
        ("ETag".into(), etag.clone()),
        ("Accept-Ranges".into(), "bytes".to_string()),
    ];

    // 5. Conditional: fresh cache → 304, empty body.
    if none_match(headers, &etag) {
        return Ok((304, Vec::new(), out_headers));
    }

    // Length may have raced between metadata and read; the ETag stays as
    // computed (a revalidation simply misses next time — safe direction)
    // and a short range read below degrades to 404.
    let actual = len;

    // 6. Read raw bytes (binary-safe — never through `String`).
    // Range hits seek instead of loading the whole file: video clients
    // requesting a few KB must not page in gigabytes.
    let data = match parse_range(headers, actual) {
        // 7a. Unsatisfiable/multi → 416 (no body read at all).
        None => {
            return Ok((
                416,
                b"Range Not Satisfiable".to_vec(),
                vec![
                    ("Content-Type".into(), "text/plain; charset=utf-8".into()),
                    ("Content-Range".into(), format!("bytes */{actual}")),
                ],
            ));
        }
        // 7b. Single range → 206: bounded seek + take.
        Some(Some((start, end))) => {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = match std::fs::File::open(&canon) {
                Ok(f) => f,
                Err(_) => return not_found(),
            };
            if f.seek(SeekFrom::Start(start)).is_err() {
                return not_found();
            }
            let mut body = Vec::with_capacity((end - start + 1).min(1 << 20) as usize);
            if f.take(end - start + 1).read_to_end(&mut body).is_err()
                || body.len() as u64 != end - start + 1
            {
                // Raced shrink (or I/O error): don't emit a lying Content-Range.
                return not_found();
            }
            out_headers.push((
                "Content-Range".into(),
                format!("bytes {start}-{end}/{actual}"),
            ));
            return Ok((206, body, out_headers));
        }
        // 7c. No range → 200: full read.
        Some(None) => match std::fs::read(&canon) {
            Ok(d) => d,
            Err(_) => return not_found(),
        },
    };
    Ok((200, data, out_headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zz_static_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn serves_text_with_etag() {
        let dir = tmpdir("text");
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let (st, body, h) = serve_static_file(dir.to_str().unwrap(), "/a.txt", &[]).unwrap();
        assert_eq!(st, 200);
        assert_eq!(body, b"hello");
        assert!(h.iter().any(|(k, _)| k == "ETag"));
        assert!(h
            .iter()
            .any(|(k, v)| k == "Content-Type" && v.contains("text/plain")));
    }

    #[test]
    fn serves_binary_identical() {
        let dir = tmpdir("bin");
        let raw: Vec<u8> = (0..256).map(|i| i as u8).collect::<Vec<_>>().repeat(4);
        std::fs::write(dir.join("b.bin"), &raw).unwrap();
        let (st, body, _) = serve_static_file(dir.to_str().unwrap(), "/b.bin", &[]).unwrap();
        assert_eq!(st, 200);
        assert_eq!(body, raw);
    }

    #[test]
    fn traversal_blocked() {
        let dir = tmpdir("trav");
        std::fs::write(dir.join("ok.txt"), b"ok").unwrap();
        // Any `..` segment — plain or percent-encoded — is forbidden.
        for evil in [
            "/../ok.txt",
            "/%2e%2e/ok.txt",
            "/%2E%2E/ok.txt",
            "/a/../../ok.txt",
        ] {
            let (st, _, _) = serve_static_file(dir.to_str().unwrap(), evil, &[]).unwrap();
            assert_eq!(st, 403, "{evil}");
        }
        // Symlink escapes are contained.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", dir.join("evil")).unwrap();
            let (st, _, _) =
                serve_static_file(dir.to_str().unwrap(), "/evil/hostname", &[]).unwrap();
            assert_eq!(st, 403);
        }
    }

    #[test]
    fn conditional_returns_304() {
        let dir = tmpdir("cond");
        std::fs::write(dir.join("c.css"), b"body{}").unwrap();
        let (_, _, h) = serve_static_file(dir.to_str().unwrap(), "/c.css", &[]).unwrap();
        let etag = h.iter().find(|(k, _)| k == "ETag").unwrap().1.clone();
        let (st, body, _) = serve_static_file(
            dir.to_str().unwrap(),
            "/c.css",
            &hdrs(&[("If-None-Match", &etag)]),
        )
        .unwrap();
        assert_eq!(st, 304);
        assert!(body.is_empty());
    }

    #[test]
    fn range_returns_206_and_416() {
        let dir = tmpdir("range");
        std::fs::write(dir.join("v.mp4"), b"0123456789").unwrap();
        let (st, body, h) = serve_static_file(
            dir.to_str().unwrap(),
            "/v.mp4",
            &hdrs(&[("Range", "bytes=2-5")]),
        )
        .unwrap();
        assert_eq!(st, 206);
        assert_eq!(body, b"2345");
        assert!(h
            .iter()
            .any(|(k, v)| k == "Content-Range" && v == "bytes 2-5/10"));
        let (st416, _, _) = serve_static_file(
            dir.to_str().unwrap(),
            "/v.mp4",
            &hdrs(&[("Range", "bytes=99-100")]),
        )
        .unwrap();
        assert_eq!(st416, 416);
    }

    #[test]
    fn missing_is_404_and_no_listing() {
        let dir = tmpdir("miss");
        let (st, _, _) = serve_static_file(dir.to_str().unwrap(), "/nope.txt", &[]).unwrap();
        assert_eq!(st, 404);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let (st2, _, _) = serve_static_file(dir.to_str().unwrap(), "/sub", &[]).unwrap();
        assert_eq!(st2, 404);
    }

    #[test]
    fn index_html_served_for_dir() {
        let dir = tmpdir("idx");
        std::fs::write(dir.join("index.html"), b"<h1>hi</h1>").unwrap();
        let (st, body, h) = serve_static_file(dir.to_str().unwrap(), "/", &[]).unwrap();
        assert_eq!(st, 200);
        assert_eq!(body, b"<h1>hi</h1>");
        assert!(h
            .iter()
            .any(|(k, v)| k == "Content-Type" && v.contains("text/html")));
    }
}
