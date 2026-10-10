# Standard Library

Exhaustive reference of all built-in functions and modules.

## Built-in Functions

Available without imports:

| Function | Signature | Description |
|----------|-----------|-------------|
| `print` | `print(v: T) -> unit` | Print value without newline |
| `println` | `println(v: T) -> unit` | Print value with newline |
| `input` | `input(prompt: str) -> str` | Read line from stdin (optional prompt) |
| `typeof` | `typeof(v: T) -> str` | Runtime type name as a string (e.g. `"int"`, `"str"`, `"bytes"`) |
| `str` | `str(v: T) -> str` | Convert to string (keeps `.ok`/`.some` wrappers — see Console I/O) |
| `int` | `int(v: T) -> Option<int>` | Parse/convert to int; `.none` on failure (reason discarded — use `??` fallback) |
| `float` | `float(v: T) -> Option<float>` | Parse/convert to float; `.none` on failure (reason discarded — use `??` fallback) |
| `len` | `len(v: T) -> int` | Length of array, tuple, bytes, string, dict, or range |
| `range` | `range(stop: int)` / `range(start: int, stop: int)` / `range(start: int, stop: int, step: int)` | Create integer range (`range(5)` = `0..5`, `range(1, 5)` = `1..5`) |
| `map` | `map(arr: [T] \| T.., f: func(T) -> U) -> [U]` | Apply function to each element (also `std.vec` for arrays) |
| `filter` | `filter(arr: [T] \| T.., f: func(T) -> bool) -> [T]` | Keep elements where predicate is true (also `std.vec` for arrays) |
| `enumerate` | `enumerate(arr: [T] \| T..)` | Index + value pairs (also `vec.enumerate` / `std.vec.enumerate`) |
| `zip` | `zip(a: [T] \| T.., b: [U] \| U..)` | Pair elements from two iterables |

> Namespace rule: globals are convenience aliases. Canonical homes are
> `std.vec.*` for array pipelines (`vec.*` short form works after
> `import std.vec`), `str.*` for `ord`/`chr` (bare `ord`/`chr` are
> aliases). Prefer the canonical home in new code; the bare forms stay
> until 0.3. `int`/`float` return `Option` — the parse-failure reason is
> discarded, so fall back explicitly at the call site:
> `int(s) ?? 0`. `typeof` returns a type *name string*, not a type value —
> compare with string equality only.

## Module Index

| Module | Purpose |
|--------|---------|
| `std.str` | String manipulation |
| `std.vec` | Array operations |
| `std.json` | JSON parsing/serialization |
| `std.http` | HTTP server + client |
| `std.net` | TCP networking |
| `std.fs` | Filesystem operations |
| `std.env` | Environment variables, CLI args |
| `std.math` | Math functions |
| `std.time` | Time, sleep, calendar dates |
| `std.term` | Raw mode, single-key reads, terminal size |
| `std.map` | Dict helpers (safe lookup, merge) |
| `std.set` | Set helpers over arrays |
| `std.dec` | Exact decimal math on strings |
| `std.bytes` | Linear string/byte builders |
| `std.csv` | CSV parsing/serialization |
| `std.colors` | ANSI styling: palette, styles, truecolor |

---

## Console I/O (built-ins, no import)

```zz
println("hello")
print("Enter name: ")
name := input("Enter name: ")
println("Hello, {name}")
```

`print`/`println` unwrap one layer for clean output: `.ok(v)` prints `v`,
`.some(v)` prints `v`, bare `.none` prints `none`. Printing a bare
`.err` aborts with a readable hinted error (handle it with `match`
instead), and printing a bare function value is a compile error
(`cannot print function 'env.os': did you mean 'env.os()'?`).
Interpolation (`"{v}"`) and `str(v)` keep the full wrappers.

---

## `std.str` -- String Operations

```zz
import std.str
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `str.length` | `str.length(s: str) -> int` | String length |
| `str.split` | `str.split(s: str, sep: str) -> [str]` | Split by separator |
| `str.contains` | `str.contains(s: str, sub: str) -> bool` | Check substring |
| `str.count` | `str.count(s: str, sub: str) -> int` | Non-overlapping occurrences (empty `sub` counts chars+1) |
| `str.trim` | `str.trim(s: str) -> str` | Trim whitespace |
| `str.to_upper` | `str.to_upper(s: str) -> str` | Uppercase |
| `str.to_lower` | `str.to_lower(s: str) -> str` | Lowercase |
| `str.replace` | `str.replace(s: str, old: str, new: str) -> str` | Replace substring |
| `str.starts_with` | `str.starts_with(s: str, prefix: str) -> bool` | Check prefix |
| `str.ends_with` | `str.ends_with(s: str, suffix: str) -> bool` | Check suffix |
| `str.find` | `str.find(s: str, sub: str, from: int) -> int` | First match at/after byte offset `from` (-1 on miss) |
| `str.rfind` | `str.rfind(s: str, sub: str, from: int) -> int` | Last match starting at/before `from` (-1 on miss) |
| `str.starts_with_at` | `str.starts_with_at(s: str, sub: str, pos: int) -> bool` | Match at byte offset (empty never matches) |
| `str.ends_with_at` | `str.ends_with_at(s: str, sub: str, pos: int) -> bool` | Match ending at byte offset `pos` |
| `str.trim_span` | `str.trim_span(s: str, start: int, end: int) -> [int]` | Trimmed `[lo, hi]` byte offsets (Unicode ws, both backends) |
| `str.find_in` | `str.find_in(s: str, sub: str, start: int, end: int) -> int` | First match in `[start, end)` (-1 on miss) |
| `str.rfind_in` | `str.rfind_in(s: str, sub: str, start: int, end: int) -> int` | Last match in `[start, end)` (-1 on miss) |
| `str.count_in` | `str.count_in(s: str, sub: str, start: int, end: int) -> int` | Non-overlapping matches in `[start, end)` |
| `str.bytes` | `str.bytes(s: str) -> [int]` | UTF-8 bytes as plain ints (one copy) |
| `ord` / `str.ord` | `ord(ch: str) -> int` | Codepoint of a single-character string (empty/multi-char is a runtime error) |
| `chr` / `str.chr` | `chr(cp: int) -> str` | 1-char string for a scalar value (surrogates, negatives, >0x10FFFF are runtime errors) |
| `bytes.to_str` | `bytes.to_str(vs: [int]) -> Result<str>` | Strict UTF-8 decode; range/invalid input is `.err` on both backends |
| `bytes.to_ints` | `bytes.to_ints(b: bytes) -> [int]` | Opaque byte buffer as plain ints |
| `str.classify` | `str.classify(text, markers, bstart, bend, nested, whole) -> [int]` | Comment-aware line counts `[lines, code, comments, blanks]`; `markers` line list, `bstart`/`bend` block pair (`""` = none), `nested` Rust-style depth, `whole` whole-line blocks |

Offsets are bytes (O(1) per call, O(n) streaming total; matches Rust
`str::find` semantics). Empty `sub`: `find`/`rfind` return the clamped
`from`, `find_in` the clamped start, `rfind_in` the clamped end,
`count_in` returns 0; `_at` never matches empty. `length`/slicing stay char-oriented — convert
explicitly when mixing. Negative inputs clamp to 0; empty `sub` returns
the clamped position for `find`/`rfind` and never matches for `_at`.
`trim_span` strips Unicode White_Space identically on VM and AOT.

```zz
import std.str

s := "  Hello World  "
s.trim()                // "Hello World"
str.to_upper(s)         // "  HELLO WORLD  "
str.contains(s, "World")  // true
str.split("a,b,c", ",")   // ["a", "b", "c"]
str.replace("foo bar", "bar", "baz")  // "foo baz"
str.starts_with("hello", "he")  // true
str.ends_with("hello", "lo")    // true
str.find("hello world", "o", 0) // 4
str.find("hello world", "o", 5) // 7
str.rfind("hello world", "o", 10) // 7
str.starts_with_at("hello", "ell", 1) // true
str.ends_with_at("hello", "ell", 4)   // true
str.trim_span("  hi  ", 0, 6)          // [2, 4]
str.bytes("AB")                    // [65, 66]
ord("A")                           // 65
chr(233)                           // "é"
chr(ord("Z"))                      // "Z"
bytes.to_str([104, 105])           // .ok("hi")
bytes.to_ints(b)[0]                // first byte as int
```

### Method Call Syntax

String functions also support method syntax on a string value:

```zz
import std.str

"  hello  ".trim()          // "hello"
"hello".to_upper()          // "HELLO"
"hello world".to_lower()    // "hello world"
"hello world".contains("world")  // true
"hello world".split(" ")    // ["hello", "world"]
"hello world".replace("world", "zz")  // "hello zz"
"hello".starts_with("he")  // true
"hello".ends_with("lo")    // true
```

---

## `std.vec` -- Array Operations

```zz
import std.vec
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `vec.len` | `vec.len(v: [T]) -> int` | Array length |
| `vec.push` | `vec.push(v: [T], x: T) -> [T]` | New array with `x` appended |
| `vec.append` | `vec.append(v: [T], x: T) -> [T]` | Alias for `vec.push`: new array with `x` appended |
| `vec.pop` | `vec.pop(v: [T]) -> [T]` | New array with last element removed |
| `vec.reverse` | `vec.reverse(v: [T]) -> [T]` | Reversed copy |
| `vec.join` | `vec.join(v: [T], sep: str) -> str` | Join as string |
| `vec.contains` | `vec.contains(v: [T], x: T) -> bool` | Check if element exists |
| `vec.sort` | `vec.sort(v: [T]) -> [T]` | Sorted copy |
| `vec.insert` | `vec.insert(v: [T], idx: int, x: T) -> [T]` | Insert at index |
| `vec.remove` | `vec.remove(v: [T], idx: int) -> [T]` | Remove at index |

```zz
import std.vec

nums := [3, 1, 2]
vec.push(nums, 4)         // [3, 1, 2, 4]
vec.pop(nums)             // [3, 1]
vec.sort(nums)            // [1, 2, 3]
vec.reverse(nums)         // [2, 1, 3]
vec.join(nums, ", ")      // "3, 1, 2"
vec.contains(nums, 1)     // true
vec.insert(nums, 0, 99)   // [99, 3, 1, 2]
vec.remove(nums, 1)       // [3, 2]
```

### Method Call Syntax

```zz
import std.vec

[3, 1, 2].sort()        // [1, 2, 3]
[3, 1, 2].reverse()     // [2, 1, 3]
[3, 1, 2].push(4)       // [3, 1, 2, 4]
[3, 1, 2].contains(1)   // true
["a", "b"].join(", ")   // "a, b"
```

---

## `std.json` -- JSON Parsing

```zz
import std.json
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `json.parse` | `json.parse(s: str) -> json` | Parse JSON string |
| `json.stringify` | `json.stringify(v: T) -> str` | Serialize to JSON |
| `json.get` | `json.get(j: json, key: str) -> json` | Get object field |
| `json.as_str` | `json.as_str(j: json) -> str` | Extract string |
| `json.as_int` | `json.as_int(j: json) -> int` | Extract integer |
| `json.as_float` | `json.as_float(j: json) -> float` | Extract float |
| `json.as_bool` | `json.as_bool(j: json) -> bool` | Extract boolean |

```zz
import std.json

data := json.parse({"name": "Alice", "age": 30})
name := json.as_str(json.get(data, "name"))   // "Alice"
age := json.as_int(json.get(data, "age"))     // 30

// Serialize
person := {"name": "Bob", "age": 25}
json.stringify(person)  // {"age":25,"name":"Bob"}
```

---

## `std.http` -- HTTP Server + Client

```zz
import std.http
```

### Server

| Function | Signature | Description |
|----------|-----------|-------------|
| `http.server` | `http.server() -> http.server` | Create server handle |
| `http.route_get` | `http.route_get(server, path, handler) -> http.server` | Register GET route |
| `http.route_post` | `http.route_post(server, path, handler) -> http.server` | Register POST route |
| `http.route_put` | `http.route_put(server, path, handler) -> http.server` | Register PUT route |
| `http.route_delete` | `http.route_delete(server, path, handler) -> http.server` | Register DELETE route |
| `http.route` | `http.route(server, method, path, handler) -> http.server` | Single-entry routing (`GET`/`POST`/`PUT`/`DELETE`/`PATCH`/`HEAD`/`OPTIONS`; unknown methods are a loud error) |
| `http.use` | `http.use(server, middleware) -> http.server` | Register middleware (`http.pipe` is a legacy alias) |
| `http.pipe_post` | `http.pipe_post(server, post_fn) -> http.server` | Post-middleware `fn(req, res) -> res` (response headers) |
| `http.with_headers` | `http.with_headers(res, extra) -> http.response` | Merge headers (`extra` wins) |
| `http.log` | `http.log(server, enabled) -> http.server` | Toggle request logging |
| `http.serve_dir` | `http.serve_dir(server, dir) -> http.server` | Serve static files (traversal-proof, ETag, Range) |
| `http.serve_dir_at` | `http.serve_dir_at(server, prefix, dir) -> http.server` | Serve static files under a URL prefix |
| `http.test` | `http.test(server, method, path, body) -> http.response` | Dispatch in-process (no sockets) |
| `http.test_req` | `http.test_req(server, method, path, headers, body) -> http.response` | Like `test`, with request headers |
| `http.body_bytes` | `http.body_bytes(req) -> bytes` | Exact received body bytes (binary-safe; `req.body` is lossy text) |
| `http.handle` | `http.handle(server, method, path, body) -> Result<str, str>` | Legacy dispatch (prefer `test`) |
| `http.listen` | `http.listen(server, port) -> unit` | Start blocking server (keep-alive, graceful SIGINT/SIGTERM drain) |
| `http.listen_cfg` | `http.listen_cfg(server, port, opts) -> unit` | `opts`: `read_ms`, `max_reqs_conn`, `max_body_bytes`, `shutdown_ms` (all optional ints) |
| `http.listen_tls` | `http.listen_tls(server, port, cert_path, key_path) -> unit` | HTTPS listener (rustls, TLS 1.3+1.2, ALPN `http/1.1`); enables HSTS injection |
| `http.listen_tls_cfg` | `http.listen_tls_cfg(server, port, cert_path, key_path, opts) -> unit` | TLS + `listen_cfg` limits |
| `http.fetch_insecure` | `http.fetch_insecure(url, ...) -> Result<http.response, str>` | Like `fetch`, skips TLS verification (self-signed fixtures only) |
| `http.hijack` | `http.hijack(server, path, handler(req, stream)) -> http.server` | WebSocket-upgrade routes; handler takes over the socket after `101` (cleartext only) |
| `http.cors` | `http.cors(server, origins) -> http.server` | CORS: origin gate + preflight + `Allow-Origin` echo |
| `http.secure_headers` | `http.secure_headers(server) -> http.server` | Inject CSP/nosniff/referrer/frame headers |
| `http.secure_header_dict` | `http.secure_header_dict() -> {str: str}` | Raw secure-headers map for manual merges |
| `http.rate_limit` | `http.rate_limit(server, max_requests, window_ms) -> http.server` | Token bucket per client IP; over-limit short-circuits `429 + Retry-After` |
| `http.csrf_token` | `http.csrf_token() -> str` | 32-byte CSPRNG hex token |
| `http.csrf_check` | `http.csrf_check(a, b) -> bool` | Constant-time token compare |
| `http.request_id` | `http.request_id(server) -> http.server` | Per-response `X-Request-Id` (uuid v7) |
| `http.respond` | `http.respond(status, body, headers = {}) -> http.response` | Explicit status/headers |
| `http.ok` | `http.ok(body) -> http.response` | 200 response |
| `http.created` | `http.created(body) -> http.response` | 201 response |
| `http.not_found` | `http.not_found() -> http.response` | 404 response |
| `http.redirect` | `http.redirect(url) -> http.response` | 302 + `Location` header |

Handlers take a typed `Request` and return `str` (200 text), a
`Response` (explicit status/headers), or a dict/array (auto-JSON):

```zz
import std.http

server := http.server()
    |> http.route("GET", "/", |_req: http.request| "Hello, World!")
    |> http.route("GET", "/users/:id", |req| "user-{http.param(req, "id") ?? "?"}")
    |> http.route("POST", "/echo", |req| req.body)

println("Server running on :8080")
http.listen(server, 8080)
```

Method syntax works on server handles (`http.*` methods dispatch on
`http.server` receivers):

```zz
s := http.server()
s = s.route("GET", "/hi", |req| http.ok("hi"))
s = s.use(|req| .ok(req))
s = s.log(true)
```

Routes use one syntax: `:id` captures a segment, `:rest...` captures the
greedy tail (must be last), `*` is a catch-all. Exact routes beat params,
params beat wildcards; a path matched only by other methods returns 405
with `Allow`. The checker validates literal paths, methods, handler arity,
duplicate routes, and `param("typo")` names at compile time.

Static files are traversal-proof (`..`, `%2e%2e`, symlink escapes → 403),
binary-safe, and support `ETag`/`If-None-Match` (304) plus single
`Range` (206, unsatisfiable → 416). Directories need `index.html`.
Static responses flow through post-middleware, so CORS and secure
headers apply to file responses as well; longest matching prefix wins.

```zz
s := http.server()
s = http.serve_dir(s, "./public")            // catch-all fallback
s = http.serve_dir_at(s, "/assets", "./dist") // prefix-scoped
s = http.cors(s, ["https://app.example.com"])
s = http.secure_headers(s)
http.listen_cfg(s, 8080, {"max_body_bytes": 10000000})
```

HTTPS uses `listen_tls` with PEM cert/key files (rustls, no OpenSSL);
responses gain `Strict-Transport-Security` automatically:

```zz
s = http.server()
s = s.route("GET", "/", |_req| "secure")
http.listen_tls(s, 8443, "cert.pem", "key.pem")
```

WebSocket upgrades via `hijack` — the handler receives the raw
`tcp.stream` after the `101` handshake (framing stays userland):

```zz
s = http.hijack(s, "/chat/:room", |req, stream| {
    println("upgraded {req.param("room").unwrap_or("?")}")
})
```

### Request (`http.request`)

| Field | Type | Description |
|-------|------|-------------|
| `method` | `str` | `"GET"`, `"POST"`, … |
| `path` | `str` | Path without query string |
| `body` | `str` | Raw request body |
| `headers` | `{str: str}` | Request headers (case-insensitive lookup via `http.header`) |
| `query` | `{str: str}` | Parsed query string |
| `params` | `{str: str}` | Route path params (`:id` segments) |

| Function | Signature | Description |
|----------|-----------|-------------|
| `http.param` | `http.param(req, name) -> Result<str, str>` | Route param or `.err` |
| `http.query` | `http.query(req) -> {str: str}` | Query dict |
| `http.header` | `http.header(req, name) -> Result<str, str>` | Header (case-insensitive) or `.err` |
| `http.body_json` | `http.body_json(req) -> Result<json, str>` | Parse body as JSON (`?`-able) |
| `http.body_form` | `http.body_form(req) -> {str: str}` | Parse form-encoded body |

Handler type: `func(http.request) -> str | http.response`

### Response (`http.response`)

| Function | Signature | Description |
|----------|-----------|-------------|
| `http.status` | `http.status(res) -> int` | Status code (method syntax: `res.status()`) |
| `http.text` | `http.text(res) -> str` | Body text (`res.text()`) |
| `http.json` | `http.json(res) -> Result<json, str>` | Parse body as JSON (`res.json()?`) |
| `http.headers` | `http.headers(res) -> {str: str}` | Response headers (`res.headers()`) |

### Client (`headers` optional, defaults to `{}`)

| Function | Signature | Description |
|----------|-----------|-------------|
| `http.get` | `http.get(url, headers = {}) -> Result<http.response, str>` | GET request |
| `http.post` | `http.post(url, body, headers = {}) -> Result<http.response, str>` | POST (`body`: `str` or `bytes`) |
| `http.put` | `http.put(url, body, headers = {}) -> Result<http.response, str>` | PUT (`body`: `str` or `bytes`) |
| `http.delete` | `http.delete(url, headers = {}) -> Result<http.response, str>` | DELETE request |
| `http.fetch` | `http.fetch(url, method = "GET", headers = {}, body = "", timeout_ms = 30000) -> Result<http.response, str>` | Unified client, any verb (`GET`/`POST`/`PUT`/`DELETE`/`PATCH`), configurable timeout |
| `http.post_json` | `http.post_json(url, body: T, headers = {}) -> Result<http.response, str>` | POST any value as JSON (sets `Content-Type` unless present) |

```zz
import std.http

// One-liner with defaults.
match http.get("https://api.example.com/users") {
    .ok(res)  => println("users: {res.text()}"),
    .err(e)   => println("request failed: {e}"),
}

// Full control: verb + headers + body + timeout.
match http.fetch("https://api.example.com/users", "POST", {}, "{\"a\": 1}", 5000) {
    .ok(res)  => println("created: {res.status()}"),
    .err(e)   => println("request failed: {e}"),
}

// JSON ergonomics: any value serializes, Content-Type is automatic.
match http.post_json("https://api.example.com/users", {"name": "zz"}) {
    .ok(res)  => println("created: {res.status()}"),
    .err(e)   => println("request failed: {e}"),
}

// `res.json()` is always a `Result` — `?` propagates parse failures:
import std.json

func main() -> Result<int, str> {
    res := http.get("https://api.example.com/users")?
    body := res.json()?
    println(json.stringify(body) ?? "{}")
    .ok(0)
}
```

### Testing Handlers

```zz
import std.http

server := http.server()
    |> http.route("GET", "/", |_req| "Hello!")

// Test without starting a server
response := http.test(server, "GET", "/", "")
println(response.status())   // 200
println(response.text())     // "Hello!"
```

---

## `std.net` -- TCP Networking

```zz
import std.net
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `net.tcp_listen` | `net.tcp_listen(addr) -> Result<tcp.listener, str>` | Bind listener (`"127.0.0.1:8080"`) |
| `net.tcp_connect` | `net.tcp_connect(addr, timeout_ms) -> Result<tcp.stream, str>` | Connect with timeout |
| `net.tcp_accept` | `net.tcp_accept(listener) -> Result<tcp.stream, str>` | Accept (method: `listener.accept()`) |
| `net.tcp_write` | `net.tcp_write(stream, data: str) -> Result<int, str>` | Write UTF-8 text (method: `stream.write()`) |
| `net.tcp_read` | `net.tcp_read(stream, max_bytes) -> Result<str, str>` | Read text — lossy on non-UTF8 (method: `stream.read()`) |
| `net.tcp_readline` | `net.tcp_readline(stream) -> Result<str, str>` | Read a `\n`-terminated line (method: `stream.read_line()`) |
| `net.tcp_read_bytes` | `net.tcp_read_bytes(stream, max_bytes) -> Result<bytes, str>` | Binary-safe read (method: `stream.read_bytes()`) |
| `net.tcp_write_bytes` | `net.tcp_write_bytes(stream, data: bytes) -> Result<int, str>` | Binary-safe write (method: `stream.write_bytes()`) |
| `net.tcp_shutdown` | `net.tcp_shutdown(stream) -> Result<unit, str>` | Real shutdown, both directions (methods: `stream.close()`, `stream.shutdown()`) |
| `net.tcp_close` | `net.tcp_close(stream) -> Result<bool, str>` | Legacy no-op (returns true; prefer `close`) |
| `net.peer_addr` | `net.peer_addr(stream) -> Result<str, str>` | Remote `"ip:port"` |
| `net.local_addr` | `net.local_addr(stream) -> Result<str, str>` | Local `"ip:port"` |
| `net.set_read_timeout` | `net.set_read_timeout(stream, ms) -> Result<bool, str>` | Read deadline |
| `net.set_write_timeout` | `net.set_write_timeout(stream, ms) -> Result<bool, str>` | Write deadline |

Short `net.*` aliases (`accept`, `read`, `write`, `read_line`,
`read_bytes`, `write_bytes`, `close`, `shutdown`, `peer_addr`,
`local_addr`, `set_read_timeout`, `set_write_timeout`) dispatch on
`tcp.stream` / `tcp.listener` receivers, so method syntax works with or
without `import std.net` namespace prefixing. `tcp_read` is
text-oriented (lossy on arbitrary bytes by construction); use
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
    .ok(0)
}
```

---

## `std.fs` -- Filesystem

```zz
import std.fs
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `fs.read_file` | `fs.read_file(path: str)` | Read file contents |
| `fs.write_file` | `fs.write_file(path: str, contents: str)` | Write file |
| `fs.exists` | `fs.exists(path: str) -> bool` | Check existence |
| `fs.read_bytes` | `fs.read_bytes(path: str)` | Raw bytes as `bytes` (contiguous, ~1x RSS) |
| `fs.append` / `fs.copy` / `fs.move` / `fs.rename` | `(…)` | Append, copy, move |
| `fs.is_file` / `fs.is_dir` | `(path) -> bool` | Type predicates |
| `fs.remove_file` / `fs.remove` | `(path)` | Delete a file |
| `fs.mkdir` / `fs.mkdir_all` | `(path)` | Create directories |
| `fs.read_dir` / `fs.readdir` | `(path)` | Child basenames (sorted) |
| `fs.remove_dir_all` / `fs.walk_dir` | `(path)` | Recursive remove / list |
| `fs.stat` | `(path)` | Metadata dict |
| `fs.scan_counts` | `(path, markers, bstart, bend, nested, whole) -> Result<[int]>` | Fused read + NUL-sniff + `str.classify` in one call: `[lines, code, comments, blanks, binary]` (`binary` 1 = NUL present, counts zeroed) |
| `fs.is_generated` | `(path, markers) -> Result<bool>` | Whole-file generated-content heuristic in one call: minified tiny files and case-insensitive header markers |
| `File.open` / `fs.open` | `(path, mode)` | Streaming handle (`r`/`w`/`a`) |
| `fs.read_chunk` | `(f, n)` | Text chunk (`""` at EOF; UTF-8) |
| `fs.read_chunk_bytes` | `(f, n)` | Binary-safe chunk as `bytes` (empty at EOF) |
| `fs.write_chunk` / `fs.seek` / `fs.flush` / `fs.close` | | Handle ops |
| `fs.normalize` | `(path) -> str` | Lexical normalize (OS separators, `.`/`..`, roots/UNC) |
| `fs.join` | `(a, b) -> str` | Join + normalize (`b` wins when absolute, like Python/Rust — use `join(dir, "file.txt")`, not `join(dir, "/file.txt")`) |
| `fs.basename` / `fs.dirname` | `(path) -> str` | Final segment / directory part |
| `fs.is_absolute` | `(path) -> bool` | Rooted (`/x`, `C:\x`, `\\unc\…`) |
| `fs.extension` | `(path) -> str` | Extension without dot |
| `fs.osfs` / `fs.memfs` / `fs.tarfs` / `fs.embedfs` | | FS providers (see below) |
| `fs.read_to_string_at` / `fs.read_bytes_at` | `(fsys, path)` | Provider reads |
| `fs.write_at` / `fs.append_at` | `(fsys, path, data)` | Provider writes (Mem/Os only) |
| `fs.exists_at` / `fs.is_file_at` / `fs.is_dir_at` | `(fsys, path) -> bool` | Provider predicates |
| `fs.read_dir_at` / `fs.mkdir_all_at` / `fs.remove_file_at` | | Provider dir ops |

All fallible ops return `Result<_, str>` with unified
`fs:<op>:<code>: <path>` diagnostics (identical in VM and AOT).
`read_chunk` is text-oriented (lossy on arbitrary bytes by construction);
use `read_chunk_bytes` for binary streaming.

### `bytes` — contiguous byte buffers

`fs.read_bytes`, `fs.read_chunk_bytes`, and `fs.read_bytes_at` return
`bytes`: one contiguous buffer (~1 byte RSS per byte, slices share the
store with zero copies). Prints like an int array (`[104, 105]`).

```zz
match fs.read_bytes("data.bin") {
    .ok(b) => {
        println(len(b))    // or b.len()
        println(b[0])      // u8 as int (negatives wrap)
        println(b[1:4])    // zero-copy slice -> bytes
        println(typeof(b)) // bytes
        for x in b {       // iterate ints
            println(x)
        }
    }
    .err(e) => println(e),
}
```

Buffers are immutable (`b[i] = x` is an error) and serialize to JSON as
int arrays. `==` is deep in the VM; in AOT it matches array behavior.

### FS providers (`fs.FS` handle interface)

System calls accept any provider backing the handle:

```zz
import std.fs

match fs.memfs() {
    .ok(m) => {
        fs.write_at(m, "a/b.txt", "hello")
        match fs.read_to_string_at(m, "a/b.txt") {
            .ok(c)  => println(c),   // hello
            .err(e) => println(e),
        }
    }
    .err(e) => println(e),
}

// Read-only view over a plain .tar archive (no extraction):
match fs.tarfs("assets.tar") {
    .ok(t) => println(fs.exists_at(t, "docs/hi.txt")),
    .err(e) => println(e),
}
```

- `fs.osfs()` — the real OS filesystem.
- `fs.memfs()` — thread-safe in-memory tree (tests, caches, transient data).
- `fs.tarfs(path)` — read-only plain-`.tar` view (regular files + dirs;
  symlinks/devices skipped; writes fail `invalid_input`).
- `fs.embedfs()` — read-only `--embed` assets (empty unless the CLI was
  given `--embed <dir>`).

Paths inside a provider live in one `/`-rooted virtual namespace
(backslash accepted, `.`/`..` resolved, `..` above root clamps).

```zz
import std.fs

// Read
match fs.read_file("data.txt") {
    .ok(content)   => println(content),
    .err(e)        => println("Error: {e}"),
}

// Write
match fs.write_file("out.txt", "Hello World") {
    .ok(_)         => println("Written"),
    .err(e)        => println("Error: {e}"),
}

// Check existence
if fs.exists("config.toml") {
    println("config found")
}
```

---

## `std.env` -- Environment

```zz
import std.env
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `env.get` | `env.get(key: str) -> str?` | Value or `.none` |
| `env.get_var` | `env.get_var(name: str)` | Get env var (legacy alias shape) |
| `env.var` | `env.var(name: str)` | Value or `.err` |
| `env.set` | `env.set(key: str, val: str)` | Set (`.err` on bad key) |
| `env.remove` / `env.unset` | `env.remove(key: str)` | Unset (total no-op) |
| `env.vars` | `env.vars() -> map[str]str` | All variables, sorted by key |
| `env.cwd` | `env.cwd()` | Working directory (`.err` on failure) |
| `env.set_cwd` | `env.set_cwd(path: str)` | Change directory (`.err` on failure) |
| `env.exe_path` | `env.exe_path()` | Absolute path of this binary |
| `env.home_dir` | `env.home_dir() -> str?` | `HOME` / `USERPROFILE` |
| `env.temp_dir` | `env.temp_dir() -> str` | System temp dir (total) |
| `env.user` | `env.user() -> str?` | `USER`/`LOGNAME` / `USERNAME` |
| `env.os` | `env.os() -> str` | `"linux"` / `"macos"` / `"windows"` |
| `env.args` | `env.args() -> [str]` | Script arguments |

```zz
import std.env

// Environment variable
match env.get("HOME") {
    .some(home) => println("Home: {home}"),
    .none       => println("HOME not set"),
}

// Set + remove (process-wide, like POSIX setenv)
match env.set("APP_MODE", "demo") {
    .ok(_)  => println("set"),
    .err(e) => println(e),
}
env.remove("APP_MODE")

// Directories + identity
match env.cwd() {
    .ok(c)  => println(c),
    .err(e) => println(e),
}
println(env.os())
println(env.temp_dir())

// Command line args
for arg in env.args() {
    println(arg)
}
```

---

## `std.math` -- Math Functions

```zz
import std.math
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.abs` | `math.abs(v: T) -> T` | Absolute value |
| `math.floor` | `math.floor(v: float) -> int` | Floor to int |
| `math.ceil` | `math.ceil(v: float) -> int` | Ceil to int |
| `math.sqrt` | `math.sqrt(v: T) -> float` | Square root |
| `math.pow` | `math.pow(base: T, exp: T) -> float` | Power |
| `math.random` | `math.random() -> float` | Random [0, 1) |

```zz
import std.math

math.abs(-5)          // 5
math.floor(3.7)       // 3
math.ceil(3.2)        // 4
math.sqrt(9.0)        // 3.0
math.pow(2, 10)       // 1024.0
x := math.random()    // 0.0 <= x < 1.0
```

---

## `std.time` -- Time

```zz
import std.time
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `time.now_ms` | `time.now_ms() -> int` | Current time (ms since epoch) |
| `time.sleep_ms` | `time.sleep_ms(ms: int) -> unit` | Sleep for N milliseconds |

```zz
import std.time

start := time.now_ms()
time.sleep_ms(1000)
elapsed := time.now_ms() - start
println("Slept for {elapsed}ms")
```

### Calendar dates (dict-based `Date`)

Dates are `{str: int}` dicts (`year`, `month`, `day`, `hour`, `min`, `sec`).
Parse/format are UTC RFC3339 (`...Z`) only; invalid input falls back to
the epoch so callers stay total.

```zz
import std.time

d := time.parse_rfc3339("2024-02-29T12:30:45Z")
println(time.format_rfc3339(d))   // 2024-02-29T12:30:45Z
println(time.add_days(d, 1)["day"])
println(time.diff_days(d, time.make_date(2024, 2, 28, 0, 0, 0)))  // 1
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `time.make_date` | `time.make_date(y, mo, d, h, mi, se) -> {str: int}` | Build a date dict |
| `time.parse_rfc3339` | `time.parse_rfc3339(s: str) -> {str: int}` | Parse UTC RFC3339 (epoch on invalid) |
| `time.format_rfc3339` | `time.format_rfc3339(d) -> str` | Format as UTC RFC3339 |
| `time.add_days` | `time.add_days(d, n: int) -> {str: int}` | Shift by N days (leap-correct) |
| `time.diff_days` | `time.diff_days(a, b) -> int` | `a - b` in days |
| `time.to_epoch_days` | `time.to_epoch_days(d) -> int` | Days since 1970-01-01 |
| `time.from_epoch_days` | `time.from_epoch_days(z: int) -> {str: int}` | Inverse of above |
| `time.is_leap` | `time.is_leap(y: int) -> bool` | Gregorian leap year |
| `time.days_in_month` | `time.days_in_month(y, m: int) -> int` | Month length |
| `time.date_valid` | `time.date_valid(y, m, d: int) -> bool` | Range check |

---

## `std.map` -- Dict Helpers

Pure-ZZ helpers over `{K: V}` dicts: safe lookup, projections, merge.

```zz
import std.map

d := {"a": 1, "b": 2}
println(map.get_or(d, "z", 99))  // 99, no runtime error
println(map.keys(d))             // ["a", "b"]
println(map.merge(d, {"c": 3}))
```

| Function | Description |
|----------|-------------|
| `map.has(d, key)` | Membership test (no error on missing) |
| `map.get_or(d, key, default)` | Value or default (`{str: int}`) |
| `map.get_str(d, key, default)` | Value or default (`{str: str}`) |
| `map.keys / keys_str` | Key arrays |
| `map.values / values_str` | Value arrays |
| `map.len / is_empty` | Size predicates |
| `map.merge / merge_str` | Right-wins union |
| `map.remove(d, key)` | Copy without key |

`Dict` stays insertion-ordered with O(n) lookup; the API is stable so a
future hashed implementation drops in without breaking callers.

---

## `std.set` -- Set Helpers

Sets are `[T]` arrays with uniqueness enforced by helpers.

```zz
import std.set

s := set.insert(["a"], "b")      // ["a", "b"]
println(set.union(["a"], ["b"])) // ["a", "b"]
println(set.intersect(["a", "b"], ["b", "c"]))  // ["b"]
```

| Function | Description |
|----------|-------------|
| `set.has / has_int` | Membership |
| `set.insert / insert_int` | Append if absent (returns same array otherwise) |
| `set.remove / remove_int` | Copy without element |
| `set.union / union_int` | Deduplicated concatenation |
| `set.intersect / intersect_int` | Common elements |
| `set.diff` | Elements of `a` not in `b` |
| `set.len / is_empty` | Size predicates |

---

## `std.dec` -- Exact Decimals

Decimals are `str` (`[-]digits[.digits]`). No float drift:
`dec.add("0.1", "0.2") == "0.3"`. Scale capped at 18 places.

```zz
import std.dec

println(dec.add("0.1", "0.2"))  // 0.3
println(dec.mul("1.5", "2"))    // 3
println(dec.format("3.14159", 2))  // 3.14
```

| Function | Description |
|----------|-------------|
| `dec.is_valid(s)` | Shape check |
| `dec.add / sub / mul(a, b)` | Exact arithmetic as strings |
| `dec.cmp(a, b)` | `-1` / `0` / `1` |
| `dec.eq / lt / gt(a, b)` | Boolean comparisons |
| `dec.format(s, places)` | Round/truncate to N places |
| `dec.scale_of / pow10 / to_scaled / from_scaled / trim_zeros` | Building blocks |

---

## `std.bytes` + String Builders

Strings are immutable; `s = s + part` in a loop is O(n²). Builders
collect parts and join once. Handles are plain arrays (no definitions
needed).

```zz
import std.bytes

b := str.builder()
b = str.push_part(b, "hello ")
b = str.push_part(b, name)
println(str.finish(b))

nums := bytes.builder()
nums = bytes.push_byte(nums, 300)  // wraps to 44
println(bytes.len_of(nums))
```

| Function | Description |
|----------|-------------|
| `str.builder()` | Empty `[str]` handle |
| `str.push_part(b, s)` | Append part |
| `str.finish(b)` | Join to `str` |
| `str.builder_len(b)` | Total chars without joining |
| `bytes.builder()` | Empty `[int]` handle |
| `bytes.push_byte(b, v)` | Append byte (`mod 256`, negatives wrap) |
| `bytes.extend(b, vs)` | Append many |
| `bytes.len_of / from_ints` | Size / normalize array |

---

## `std.csv` -- CSV Parsing

RFC4180 subset: quoted fields, `""` escapes, embedded newlines, CRLF.
Rows are `[[str]]` (first row = header when present). Unterminated
trailing quotes are treated as end-of-field.

```zz
import std.csv

rows := csv.parse("a,b\n1,2\n")
println(csv.header(rows))          // ["a", "b"]
println(csv.get_cell(rows, 1, 0, "?"))  // "1"
println(csv.stringify(rows))       // round-trips
println(csv.to_json(rows))         // JSON array of objects
```

| Function | Description |
|----------|-------------|
| `csv.parse(src)` | Parse with `,` |
| `csv.parse_delim(src, d)` | Parse with one-char delimiter |
| `csv.stringify(rows)` | Serialize with `,` (quotes when needed) |
| `csv.stringify_delim(rows, d)` | Serialize with delimiter |
| `csv.header(rows)` | First row (`[]` when empty) |
| `csv.records(rows)` | All rows after the first |
| `csv.len(rows)` | Row count |
| `csv.get_cell(rows, r, c, default)` | Bounds-checked cell |
| `csv.validate(src)` | Non-empty parse |
| `csv.to_json(rows)` | Header-keyed JSON array |

TOML is intentionally **not** in the stdlib: use the external `toml`
package (`~/Projects/toml`, pure-ZZ, versioned separately).

---

## `std.colors` -- ANSI Styling

```zz
import std.colors
```

Pure-ZZ wrappers: every function takes text first (pipeline-friendly)
and appends a trailing reset so styles never bleed. Nesting stacks
(`bold(reverse(x))`); `strip` recovers the visible text.

| Function | Signature | Description |
|----------|-----------|-------------|
| `colors.black` … `colors.white` | `colors.<name>(s: str) -> str` | Foreground 30–37 |
| `colors.bright_black` … `colors.bright_white` | `colors.<name>(s: str) -> str` | Foreground 90–97 |
| `colors.bg_black` … `colors.bg_white` | `colors.<name>(s: str) -> str` | Background 40–47 |
| `colors.bg_bright_black` … `colors.bg_bright_white` | `colors.<name>(s: str) -> str` | Background 100–107 |
| `colors.bold` | `colors.bold(s: str) -> str` | Bold (SGR 1) |
| `colors.dim` | `colors.dim(s: str) -> str` | Dim (SGR 2) |
| `colors.italic` | `colors.italic(s: str) -> str` | Italic (SGR 3) |
| `colors.underline` | `colors.underline(s: str) -> str` | Underline (SGR 4) |
| `colors.blink` | `colors.blink(s: str) -> str` | Blink (SGR 5) |
| `colors.reverse` | `colors.reverse(s: str) -> str` | Reverse video (SGR 7, selections) |
| `colors.strikethrough` | `colors.strikethrough(s: str) -> str` | Strikethrough (SGR 9) |
| `colors.reset` | `colors.reset(s: str) -> str` | Append reset |
| `colors.strip` | `colors.strip(s: str) -> str` | Remove all `\e[...m` sequences |
| `colors.rgb` | `colors.rgb(s: str, r: int, g: int, b: int) -> str` | Truecolor fg (channels clamped) |
| `colors.bg_rgb` | `colors.bg_rgb(s: str, r: int, g: int, b: int) -> str` | Truecolor bg (channels clamped) |
| `colors.color256` | `colors.color256(s: str, n: int) -> str` | 256-color fg (`38;5;n`, clamped) |
| `colors.bg_256` | `colors.bg_256(s: str, n: int) -> str` | 256-color bg (`48;5;n`, clamped) |
| `colors.hex` | `colors.hex(s: str, code: str) -> str` | `#FF5733` / `FF5733` / `#F53` (malformed input passes through) |
| `colors.hex6` / `colors.hex3` | helpers over digit arrays | Six-/three-digit forms used by `hex` |
| `colors.hex_val` / `colors.hex_byte` | digit parsers | Nibble/byte (`-1` on malformed) |
| `colors.clamp255` | `colors.clamp255(v: int) -> int` | Clamp to `0–255` |

```zz
import std.colors

println("TEST FAIL" |> colors.red() |> colors.bold())
println(colors.strip(colors.reverse("sel")) == "sel")
```

---

## `std.term` -- Terminal Control

```zz
import std.term
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `term.enable_raw` | `term.enable_raw() -> Result<unit, str>` | Save termios, switch stdin to raw (byte-at-a-time, no echo) |
| `term.disable_raw` | `term.disable_raw() -> Result<unit, str>` | Restore saved terminal state (idempotent) |
| `term.read_key` | `term.read_key() -> Result<int, str>` | Block for one stdin byte (`0–255`) |
| `term.poll` | `term.poll(ms: int) -> Result<bool, str>` | True when a byte is ready within `ms` (`<= 0` = no wait; works on pipes too) |
| `term.get_size` | `term.get_size() -> Result<[int, int], str>` | Terminal `[cols, rows]` via `TIOCGWINSZ` |
| `term.is_tty` | `term.is_tty() -> bool` | Total predicate for graceful degradation |
| `term.flush` | `term.flush()` | Flush stdout now (interactive renders without trailing newline) |

Raw mode clears `ICANON`/`ECHO` (plus `ISIG`, so Ctrl+C arrives as
byte `3` and ZZ code can restore the terminal via `defer` instead of
dying with a raw TTY). Non-TTY stdin (pipes, CI) fails soft with
`.err("std.term.<op>: not a tty")`, so always pair with `is_tty`
(`poll` is the exception: it works on pipes — closed stdin reports
ready and the following read returns EOF).

### Key bytes (`read_key` yields raw bytes — decode table)

Measured against xterm-style terminals (see `tests/fixtures/stdlib/term_test.zz`
for the hermetic shape; interactive byte values were probed on a real TTY):

| Key | Bytes |
|-----|-------|
| Up / Down / Right / Left | `27, 91, 65` / `66` / `67` / `68` |
| Enter | `13` |
| Esc (lone) | `27` |
| Space | `32` |
| Backspace | `127` |
| Letters | ASCII (`a` = `97`, `q` = `113`) |
| Ctrl+C | `3` (raw mode passes it through as a byte) |

A lone `27` and an arrow prefix are indistinguishable on the first
byte — after reading `27`, `poll(50)` for the rest: bytes waiting
means an escape sequence, silence means the user pressed Esc:

```zz
import std.term

func main() {
    match term.enable_raw() {
        .ok(_) => println("raw on"),
        .err(e) => println("no tty: {e}"),
    }
    defer term.disable_raw()
    match term.read_key() {
        .ok(k) => println("key: {k}"),
        .err(e) => println("read failed: {e}"),
    }
}
```

---

## Variant Methods

These are available on variant values via method dispatch:

### Option Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `unwrap` | `.unwrap() -> T` | Unwrap or panic |
| `unwrap_or` | `.unwrap_or(default: T) -> T` | Unwrap or use default |
| `expect` | `.expect(msg: str) -> T` | Unwrap or panic with message |

```zz
x := .some(42)
x.unwrap()           // 42
x.unwrap_or(0)       // 42
x.expect("missing")  // 42

y: Option<int> = .none
y.unwrap_or(0)       // 0
```

### Result Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `unwrap` | `.unwrap() -> T` | Unwrap or panic |
| `unwrap_or` | `.unwrap_or(default: T) -> T` | Unwrap or use default |
| `expect` | `.expect(msg: str) -> T` | Unwrap or panic with message |

```zz
ok_val := .ok(42)
ok_val.unwrap()           // 42
ok_val.unwrap_or(0)       // 42

err_val: Result<int, str> = .err("boom")
err_val.unwrap_or(0)      // 0
```
