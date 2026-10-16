# HTTP — `std.http`

```zz
import std.http
```

Handlers take `http.request` and return `str` (200 text), `http.response`
(explicit status/headers), or dict/array (auto-JSON).

```zz
server := http.server()
    |> http.route("GET", "/", |_req: http.request| "Hello, World!")
    |> http.route("GET", "/users/:id", |req| "user-{http.param(req, "id") ?? "?"}")
    |> http.route("POST", "/echo", |req| req.body)

http.listen(server, 8080)
```

Method syntax works: `s.route("GET", "/hi", |req| http.ok("hi"))`,
`s.use(...)`, `s.log(true)`.

## Routing

One syntax: `:id` captures a segment, `:rest...` greedy tail (must be last),
`*` catch-all. Exact > params > wildcards. Path matched only by other methods
returns 405 + `Allow`. Use `http.route` single entry (`GET`/`POST`/`PUT`/`DELETE`/
`PATCH`/`HEAD`/`OPTIONS`); unknown methods are a loud error. `route_get/post/put/delete`
are shorthands. Checker validates literal paths, methods, handler arity,
duplicate routes, and `param("typo")` names at compile time.

## Requests

Fields: `method`, `path` (no query), `body` (lossy text — use `body_bytes` for binary),
`headers` (case-insensitive via `http.header`), `query`, `params`.

| Helper | Notes |
|---|---|
| `param(req, name)` | route param or `.err` |
| `query(req)` | query dict |
| `header(req, name)` | case-insensitive or `.err` |
| `body_json(req)` | `Result<json, str>`, `?`-able |
| `body_form(req)` | form-encoded dict |
| `body_bytes(req)` | exact `bytes` |

## Responses

`respond(status, body, headers = {})`, `ok`, `created`, `not_found`,
`redirect(url)` (302 + Location). Inspect with `status()`, `text()`,
`json()?` (always `Result`), `headers()`. `with_headers(res, extra)` merges (extra wins).

## Middleware and hardening

```zz
s := http.server()
s = http.cors(s, ["https://app.example.com"])
s = http.secure_headers(s)
s = http.rate_limit(s, 100, 60000)  // token bucket per IP, 429 + Retry-After
s = http.request_id(s)              // X-Request-Id (uuid v7)
s = http.log(s, true)
http.listen_cfg(s, 8080, {"max_body_bytes": 10000000})
```

`use(server, middleware)` (`pipe` is legacy alias), `pipe_post` for
`fn(req, res) -> res` post-hooks, `secure_header_dict()` for manual merges,
`csrf_token()` (32-byte hex) + `csrf_check(a, b)` constant-time.

## Static files

Traversal-proof (`..`, `%2e%2e`, symlink escapes → 403), ETag/304, single Range/206
(unsatisfiable → 416), `index.html` for dirs. Flows through post-middleware;
longest prefix wins.

```zz
s = http.serve_dir(s, "./public")
s = http.serve_dir_at(s, "/assets", "./dist")
```

## TLS and WebSockets

```bash
# rustls, TLS 1.3+1.2, ALPN http/1.1, auto-HSTS
```

```zz
http.listen_tls(s, 8443, "cert.pem", "key.pem")
http.listen_tls_cfg(s, 8443, "cert.pem", "key.pem", {"max_body_bytes": 10000000})
```

`hijack(server, path, handler(req, stream))` upgrades to raw `tcp.stream`
after `101` (cleartext only) — framing stays userland.

## Client

`headers` optional, defaults `{}`. `body` may be `str` or `bytes`.

| Call | Notes |
|---|---|
| `get(url, headers={})` | GET |
| `post/put(url, body, headers={})` | body str/bytes |
| `delete(url, headers={})` | DELETE |
| `fetch(url, method="GET", headers={}, body="", timeout_ms=30000)` | any verb, timeout |
| `post_json(url, body: T, headers={})` | serializes any value, sets Content-Type |
| `fetch_insecure(...)` | skips TLS verify — self-signed fixtures only |

```zz
match http.get("https://api.example.com/users") {
    .ok(res) => println("users: {res.text()}"),
    .err(e)  => println("failed: {e}"),
}

func main() -> Result<int, str> {
    res := http.get("https://api.example.com/users")?
    body := res.json()?
    println(json.stringify(body) ?? "{}")
    .ok(0)
}
```

`listen` blocks with keep-alive + graceful SIGINT/SIGTERM drain.
`listen_cfg` adds `read_ms`, `max_reqs_conn`, `max_body_bytes`, `shutdown_ms`.

## Testing without sockets

```zz
server := http.server()
    |> http.route("GET", "/", |_req| "Hello!")

res := http.test(server, "GET", "/", "")
println(res.status())  // 200
println(res.text())    // Hello!
```

`test_req` adds request headers. Legacy `handle` returns `Result<str, str>` — prefer `test`.
