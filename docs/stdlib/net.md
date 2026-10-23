# Networking — `std.net` (+ TLS, Unix)

```zz
import std.net
```

| Function | Description |
|---|---|
| `tcp_listen(addr)` | bind `"127.0.0.1:8080"` → `Result<tcp.listener, str>` |
| `tcp_connect(addr, timeout_ms)` | connect with timeout |
| `tcp_accept(listener)` | accept; method `listener.accept()` |
| `tcp_write(stream, data: str)` | UTF-8 text; `stream.write()` |
| `tcp_read(stream, max_bytes)` | text, lossy on non-UTF8; `stream.read()` |
| `tcp_readline(stream)` | `\n` line; `stream.read_line()` |
| `tcp_read_bytes(stream, max_bytes)` | binary-safe `bytes`; `stream.read_bytes()` |
| `tcp_write_bytes(stream, data: bytes)` | binary-safe; `stream.write_bytes()` |
| `tcp_shutdown(stream)` | both directions; `close()` / `shutdown()` |
| `peer_addr` / `local_addr` | `"ip:port"` |
| `set_read_timeout` / `set_write_timeout` | deadlines in ms |

Short `net.*` aliases dispatch on receivers, so method syntax works with or
without the namespace prefix. `tcp_read` is text by construction — use
`read_bytes` for binary protocols.

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

## Notes

- Timeouts are per-call deadlines, not total budgets. Set both read and write
  timeouts for request/response protocols.
- `readline` blocks until `\n` or timeout/close — frame with length prefixes
  or newlines; `read_bytes` for exact binary frames.
- TLS (`std.net` TLS + `std.tls`) and Unix sockets (`std.unix*`) follow the same
  handle + `Result` shape; see [stdlib.md](../stdlib.md) for exact names.
- For HTTP semantics (routing, headers, keep-alive) use [HTTP](http.md), not raw TCP.
