//! Black-box tests for the AOT HTTP serve path (Step 2: true-AOT dispatch).
//!
//! Builds a ZZ route server to a native binary once, spawns it per test
//! on a free port (via `ZZ_AOT_PORT`), and asserts VM-identical behavior
//! over real sockets: path params, query parsing, header access, echo,
//! custom statuses, no-route 500s, malformed 400s, oversize 413s, a
//! 5 MiB segmented POST, and concurrent connections.
//!
//! Linux-only (the AOT server is fork+epoll) and skipped with
//! `ZZ_SKIP_NATIVE=1`, same as the other native suites.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

const SERVER_ZZ: &str = r#"import std.http
import std.env

func main() {
    port := int(env.var("ZZ_AOT_PORT") ?? "0") ?? 8931
    s := http.server()
    s = http.route_get(s, "/users/:id", |q| "user-{http.param(q, "id") ?? "?"}")
    s = http.route_get(s, "/search", |q| "q:{http.query(q)["q"]}")
    s = http.route_get(s, "/h", |q| "h:{http.header(q, "content-type") ?? "?"}")
    s = http.route_post(s, "/echo", |q| q.body)
    s = http.route_put(s, "/put", |q| http.respond(201, "made", {"X-A": "b"}))
    s = http.pipe(s, |q| .ok(q))
    http.listen(s, port)
}
"#;

/// Skip helper: native backend needs Linux (fork+epoll) and a C toolchain.
fn require_native_serve() -> bool {
    if std::env::consts::OS != "linux" {
        eprintln!("skip: AOT serve tests need Linux (fork+epoll)");
        return false;
    }
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native backend unsupported (ZZ_SKIP_NATIVE=1)");
        return false;
    }
    true
}

fn zz_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let debug = deps.parent().unwrap();
    debug.join("zz")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

/// Build the AOT server binary once per test process.
fn aot_server_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("zz_aot_serve_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let src = dir.join("srv.zz");
        std::fs::write(&src, SERVER_ZZ).expect("write srv.zz");
        let out = Command::new(zz_bin())
            .arg("build")
            .arg(&src)
            .output()
            .expect("spawn zz build");
        assert!(
            out.status.success(),
            "zz build failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // `zz build <dir>/srv.zz` publishes to `<dir>/bin/srv`.
        let bin = dir.join("bin").join("srv");
        assert!(bin.exists(), "built binary missing: {}", bin.display());
        bin
    })
    .clone()
}

struct Server {
    child: Child,
    port: u16,
    bin: PathBuf,
}

fn start_server() -> Server {
    static SERVER_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = aot_server_bin();
    let mut last_err = String::new();
    for _ in 0..5 {
        let port = free_port();
        // Per-test binary copy: the server forks workers that outlive
        // the parent, so Drop kills the whole family via this unique
        // path (a shared binary would nuke other tests' servers).
        // The path must ALSO be unique per attempt, not just per port:
        // free_port() is probe-bind-close, so parallel tests can observe
        // the same port and would otherwise copy over / exec one shared
        // path (spawn fails with ETXTBSY "Text file busy"). pid + counter
        // makes every attempt's copy exclusive.
        let uniq = SERVER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let bin = base.with_extension(format!("t{port}.{}-{uniq}", std::process::id()));
        std::fs::copy(&base, &bin).expect("copy AOT server");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).expect("bin meta").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).expect("bin chmod");
        }
        // Spawn can transiently fail with ETXTBSY ("Text file busy") under
        // parallel load even though this copy's path is exclusive and fully
        // written (observed: complete file, no /proc holders) — back off and
        // retry the exec before falling through to a fresh port + path.
        let mut child = None;
        for _ in 0..50 {
            match Command::new(&bin)
                .env("ZZ_AOT_PORT", port.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => {
                    child = Some(c);
                    break;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                        || e.raw_os_error() == Some(26) =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("spawn AOT server {}: {e}", bin.display()),
            }
        }
        let Some(mut child) = child else {
            last_err = format!("spawn ETXTBSY persisted on {}", bin.display());
            kill_family(&bin);
            let _ = std::fs::remove_file(&bin);
            continue;
        };

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
            Ok(true) => return Server { child, port, bin },
            Ok(false) => unreachable!(),
            Err(_) => {
                last_err = "SERVER_READY timeout".to_string();
                let _ = child.kill();
                let _ = child.wait();
                kill_family(&bin);
            }
        }
    }
    panic!("could not start AOT server: {last_err}");
}

/// Kill leftover fork workers by exact binary path (unique per test).
/// Best-effort: only matches this test's server family.
fn kill_family(bin: &std::path::Path) {
    let _ = Command::new("pkill")
        .arg("-f")
        .arg(format!("^{}$", bin.display()))
        .output();
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        kill_family(&self.bin);
        let _ = std::fs::remove_file(&self.bin);
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

/// Send a raw request, segmented body writes; return (status, headers, body).
fn raw_request(
    head: &str,
    body: &[u8],
    seg: usize,
    port: u16,
) -> (u16, HashMap<String, String>, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect AOT server");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("read timeout");
    stream.write_all(head.as_bytes()).expect("write head");
    for chunk in body.chunks(seg.max(1)) {
        stream.write_all(chunk).expect("write body chunk");
    }
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).expect("read response");
    let head_end = find_subslice(&resp, b"\r\n\r\n").expect("response head");
    let head_text = String::from_utf8_lossy(&resp[..head_end]).to_string();
    let mut lines = head_text.lines();
    let status: u16 = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    (status, headers, resp[head_end + 4..].to_vec())
}

fn get(port: u16, path: &str, extra: &[(&str, &str)]) -> (u16, HashMap<String, String>, Vec<u8>) {
    let mut head = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    raw_request(&head, &[], 1, port)
}

#[test]
fn aot_serve_param_and_query() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    let (status, _, body) = get(server.port, "/users/42", &[]);
    assert_eq!(status, 200);
    assert_eq!(body, b"user-42");
    let (status, _, body) = get(server.port, "/search?q=hi", &[]);
    assert_eq!(status, 200);
    assert_eq!(body, b"q:hi");
}

#[test]
fn aot_serve_header_echo() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    let (status, _, body) = get(server.port, "/h", &[("Content-Type", "text/x")]);
    assert_eq!(status, 200);
    assert_eq!(body, b"h:text/x");
    let (status, _, body) = get(server.port, "/h", &[]);
    assert_eq!(status, 200);
    assert_eq!(body, b"h:?");
}

#[test]
fn aot_serve_echo_small_segmented() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    let head =
        "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\nConnection: close\r\n\r\n";
    let (status, _, body) = raw_request(head, b"hello", 2, server.port);
    assert_eq!(status, 200);
    assert_eq!(body, b"hello");
}

#[test]
fn aot_serve_put_custom_status() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    let head =
        "PUT /put HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (status, headers, body) = raw_request(head, &[], 1, server.port);
    assert_eq!(status, 201);
    assert_eq!(body, b"made");
    assert_eq!(headers.get("X-A").map(String::as_str), Some("b"));
}

#[test]
fn aot_serve_no_route_is_500() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    let (status, _, body) = get(server.port, "/nope", &[]);
    assert_eq!(status, 500);
    assert!(
        String::from_utf8_lossy(&body).contains("no route"),
        "unexpected 500 body: {}",
        String::from_utf8_lossy(&body)
    );
}

#[test]
fn aot_serve_malformed_is_400() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    let (status, _, _) = raw_request("GARBAGE\r\n\r\n", &[], 1, server.port);
    assert_eq!(status, 400);
}

#[test]
fn aot_serve_oversize_is_413() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    // 60 MiB declared, nothing sent: must be rejected before buffering.
    let head = "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 62914560\r\nConnection: close\r\n\r\n";
    let (status, _, _) = raw_request(head, &[], 1, server.port);
    assert_eq!(status, 413);
}

#[test]
fn aot_serve_post_5mib_segmented() {
    if !require_native_serve() {
        return;
    }
    let server = start_server();
    // Printable ASCII only (same caveat as the VM G1 test: ZZ strings
    // are text; arbitrary bytes would legitimately expand via lossy
    // decoding on the VM side).
    let body: Vec<u8> = (0..5usize * 1024 * 1024)
        .map(|i| ((i * 2654435761 % 90) + 33) as u8)
        .collect();
    let head = format!(
        "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // 8 KiB segments: every boundary exercises the body read loop.
    let (status, _, echoed) = raw_request(&head, &body, 8192, server.port);
    assert_eq!(status, 200, "expected 200 echo");
    assert_eq!(echoed.len(), body.len(), "echo length mismatch");
    assert_eq!(echoed, body, "echo bytes differ (truncation/corruption)");
}

#[test]
fn aot_serve_concurrent() {
    if !require_native_serve() {
        return;
    }
    use std::sync::{Arc, Barrier};
    let server = start_server();
    let port = server.port;
    let n = 20;
    let barrier = Arc::new(Barrier::new(n));
    let mut handles = Vec::new();
    for i in 0..n {
        let b = barrier.clone();
        handles.push(std::thread::spawn(move || {
            b.wait();
            let (status, _, body) = get(port, &format!("/users/{i}"), &[]);
            (status, body)
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        let (status, body) = h.join().expect("worker panicked");
        assert_eq!(status, 200);
        assert_eq!(body, format!("user-{i}").into_bytes());
    }
    // Keep the server alive until all workers finish.
    drop(server);
}
