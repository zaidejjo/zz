# Net — `std.net` (+ TLS, Unix)

TCP networking with binary-safe transfers, real shutdown, TLS (rustls), and Unix-domain sockets. Compiled patterns are opaque handles; all fallible ops return `Result<_, str>`.

```zz
import std.net
```

## What it does

`std.net` connects, listens, accepts, reads/writes text or bytes, sets timeouts, and queries addresses. `tcp_read` is text-oriented (lossy on arbitrary bytes); use `read_bytes` for binary protocols. Short `net.*` aliases dispatch on `tcp.stream` / `tcp.listener` receivers, so method syntax works with or without the namespace prefix.

## Quickstart

```zz
import std.net

func main() -> Result<int, str> {
    listener := net.tcp_listen("127.0.0.1:8080")?
    client := net.tcp_connect("127.0.0.1:8080", 5000)?
    server := listener.accept()?
    client.write("ping\n")?
    line := server.read_line()?
    println("got: {line}")
    client.close()?
    server.close()?
    return .ok(0)
}
```

## Method syntax

Stream and listener handles dispatch method calls:

```zz
import std.net

func main() -> Result<int, str> {
    listener := net.tcp_listen("127.0.0.1:8080")?
    stream := listener.accept()?
    stream.write("pong\n")?
    line := stream.read_line()?
    stream.close()?
    return .ok(0)
}
```

## TLS

```zz
import std.net

func main() -> Result<int, str> {
    tls := net.tls_connect("127.0.0.1:8443", "localhost", 5000)?
    tls.write("GET / HTTP/1.1\r\nHost: example.com\r\n\r\n")?
    resp := tls.read(4096)?
    tls.close()?
    return .ok(0)
}
```

TLS constructors: `tls_connect`, `tls_connect_insecure`, `tls_connect_with_ca`. TLS listener: `tls_listen`, `tls_accept`, `tls_listener_close`.

## Unix sockets

```zz
import std.net

func main() -> Result<int, str> {
    u := net.unix_connect("/tmp/app.sock", 5000)?
    u.write("hello\n")?
    u.close()?
    return .ok(0)
}
```

Unix: `unix_connect`, `unix_listen`, `unix_accept`, `unix_listener_close`, plus `unix.*` method aliases (`write`, `read`, `close`, `shutdown`, timeouts).

## Exhaustive table — TCP

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `net.tcp_listen` | `net.tcp_listen(addr: str) -> Result<tcp.listener, str>` | Bind listener | `net.tcp_listen("127.0.0.1:8080")` |
| `net.tcp_connect` | `net.tcp_connect(addr: str, timeout_ms: int) -> Result<tcp.stream, str>` | Connect with timeout | |
| `net.tcp_accept` | `net.tcp_accept(listener: tcp.listener) -> Result<tcp.stream, str>` | Accept (method: `listener.accept()`) | |
| `net.tcp_write` | `net.tcp_write(stream: tcp.stream, data: str) -> Result<int, str>` | Write UTF-8 text | |
| `net.tcp_read` | `net.tcp_read(stream: tcp.stream, max_bytes: int) -> Result<str, str>` | Read text (lossy on non-UTF8) | |
| `net.tcp_readline` | `net.tcp_readline(stream: tcp.stream) -> Result<str, str>` | Read `\n`-terminated line | |
| `net.tcp_read_bytes` | `net.tcp_read_bytes(stream: tcp.stream, max_bytes: int) -> Result<bytes, str>` | Binary-safe read | |
| `net.tcp_write_bytes` | `net.tcp_write_bytes(stream: tcp.stream, data: bytes) -> Result<int, str>` | Binary-safe write | |
| `net.tcp_shutdown` | `net.tcp_shutdown(stream: tcp.stream) -> Result<unit, str>` | Real shutdown both directions | |
| `net.tcp_read_exact` | `net.tcp_read_exact(stream: tcp.stream, n: int) -> Result<bytes, str>` | Read exactly n bytes | |
| `net.peer_addr` | `net.peer_addr(stream: tcp.stream) -> Result<str, str>` | Remote `"ip:port"` | |
| `net.local_addr` | `net.local_addr(stream: tcp.stream) -> Result<str, str>` | Local `"ip:port"` | |
| `net.set_read_timeout` | `net.set_read_timeout(stream: tcp.stream, ms: int) -> Result<bool, str>` | Read deadline | |
| `net.set_write_timeout` | `net.set_write_timeout(stream: tcp.stream, ms: int) -> Result<bool, str>` | Write deadline | |

## Exhaustive table — TLS

| Function | Signature | Description |
|----------|-----------|-------------|
| `net.tls_connect` | `(addr, server_name, timeout_ms) -> Result<tls, str>` | TLS client connect |
| `net.tls_connect_insecure` | `(addr, server_name, timeout_ms) -> Result<tls, str>` | Skip TLS verify |
| `net.tls_connect_with_ca` | `(addr, server_name, ca_file, timeout_ms) -> Result<tls, str>` | With CA file |
| `net.tls_listen` | `(addr, cert_file, key_file) -> Result<tls.listener, str>` | TLS listener |
| `net.tls_accept` | `(listener: tls.listener) -> Result<tls, str>` | Accept TLS connection |
| `net.tls_listener_close` | `(listener: tls.listener) -> Result<bool, str>` | Close TLS listener |

TLS method aliases (`tls.write`, `tls.read`, `tls.close`, `tls.shutdown`, timeouts, `peer_addr`) dispatch on `tls` stream receivers.

## Exhaustive table — Unix

| Function | Signature | Description |
|----------|-----------|-------------|
| `net.unix_connect` | `(path, timeout_ms) -> Result<unix, str>` | Unix client connect |
| `net.unix_listen` | `(path) -> Result<unix.listener, str>` | Unix listener |
| `net.unix_accept` | `(listener) -> Result<unix, str>` | Accept Unix connection |
| `net.unix_listener_close` | `(listener) -> Result<bool, str>` | Close Unix listener |

Unix method aliases (`unix.write`, `unix.read`, `unix.close`, `unix.shutdown`, timeouts) dispatch on `unix` stream receivers. `unix_listener` aliases: `accept`, `close`, `shutdown`.

## Notes

- Timeouts are per-call deadlines, not total budgets. Set both read and write timeouts for request/response protocols.
- `readline` blocks until `\n` or timeout/close — frame with length prefixes or newlines; `read_bytes` for exact binary frames.
- `stream.close()` / `stream.shutdown()` perform a real both-directions shutdown.
- For HTTP semantics (routing, headers, keep-alive) use [`std.http`](http.md), not raw TCP.

