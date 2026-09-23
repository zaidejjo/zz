//! Black-box tests for large HTTP request bodies (Registry V2 G1).
//!
//! Spawns a real `zz run` echo server (`route_post /echo → req.body`),
//! POSTs a 5 MiB body in small TCP segments (forcing partial reads), and
//! asserts the echoed bytes are identical. This failed before the
//! `handle_connection_thread` loop-read fix (body truncated at ~8 KiB).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ECHO_ZZ: &str = r#"import std.http

func main() {
    s := http.server()
    s2 := http.route_post(s, "/echo", |req| req.body)
    http.listen(s2, __PORT__)
}
"#;

/// Find a free localhost port (bind :0, read back, release).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

struct Server {
    child: Child,
    port: u16,
    dir: PathBuf,
}

fn start_echo_server() -> Server {
    let dir = std::env::temp_dir().join(format!(
        "zz_http_body_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    // Unique dir per attempt (avoid clashes on retry).
    let dir = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        dir.with_extension(format!("t{}", N.fetch_add(1, Ordering::Relaxed)))
    };
    std::fs::create_dir_all(&dir).expect("tempdir");

    // Try a few ports: the probe frees the port before `zz` binds it.
    let mut last_err = String::new();
    for _ in 0..5 {
        let port = free_port();
        std::fs::write(
            dir.join("echo.zz"),
            ECHO_ZZ.replace("__PORT__", &port.to_string()),
        )
        .expect("write echo.zz");
        let mut child = Command::new(env!("CARGO_BIN_EXE_zz"))
            .arg("run")
            .arg(dir.join("echo.zz"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn zz run echo server");

        // `http.listen` prints SERVER_READY to stderr once bound.
        let stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if line.contains("SERVER_READY") {
                            let _ = tx.send(true);
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(true) => return Server { child, port, dir },
            Ok(false) => unreachable!(),
            Err(_) => {
                last_err = "SERVER_READY timeout".to_string();
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    panic!("could not start echo server: {last_err}");
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// POST `body` in `seg`-byte writes; return (status, response body).
fn post_raw(port: u16, path: &str, body: &[u8], seg: usize) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect echo server");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("read timeout");
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    for chunk in body.chunks(seg.max(1)) {
        stream.write_all(chunk).expect("write body chunk");
    }
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).expect("read response");
    let head_end = find_subslice(&resp, b"\r\n\r\n").expect("response head");
    let head = String::from_utf8_lossy(&resp[..head_end]).to_string();
    let status: u16 = head
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    (status, resp[head_end + 4..].to_vec())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

#[test]
fn post_5mib_body_echoes_intact() {
    let server = start_echo_server();
    // Printable ASCII only: the dev server decodes bodies with
    // `from_utf8_lossy` (text-body semantics, unchanged from V1), so
    // arbitrary bytes would legitimately expand via U+FFFD replacement.
    // Varying content still catches offset/rotation corruption.
    let body: Vec<u8> = (0..5usize * 1024 * 1024)
        .map(|i| ((i * 2654435761 % 90) + 33) as u8)
        .collect();
    // 8 KiB segments: every segment boundary exercises the read loop.
    let (status, echoed) = post_raw(server.port, "/echo", &body, 8192);
    assert_eq!(status, 200, "expected 200 echo");
    assert_eq!(echoed.len(), body.len(), "echo length mismatch");
    assert_eq!(echoed, body, "echo bytes differ (truncation/corruption)");
}

#[test]
fn post_small_body_still_works() {
    let server = start_echo_server();
    let (status, echoed) = post_raw(server.port, "/echo", b"hello", 2);
    assert_eq!(status, 200);
    assert_eq!(echoed, b"hello");
}
