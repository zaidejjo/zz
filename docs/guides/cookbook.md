# Cookbook — copy-paste recipes

## JSON API with tests

```zz
import std.http
import std.json

func handler(req: http.request) -> http.response {
    match http.body_json(req) {
        .ok(body) => http.ok(json.stringify(body) ?? "{}"),
        .err(e)   => http.respond(400, "bad json: {e}"),
    }
}

func routes() -> http.server {
    s := http.server()
    s = http.route(s, "POST", "/echo", handler)
    s
}

func main() {
    http.listen(routes(), 8080)
}
```

```zz
import std.http

@test
func test_echo() {
    res := http.test(routes(), "POST", "/echo", `{"a":1}`)
    assert_eq(res.status(), 200)
}
```

## Static site with hardening

```zz
import std.http

s := http.server()
s = http.serve_dir_at(s, "/assets", "./dist")
s = http.cors(s, ["https://app.example.com"])
s = http.secure_headers(s)
s = http.request_id(s)
http.listen_cfg(s, 8080, {"max_body_bytes": 10000000})
```

## CLI with flags

```zz
import std.args

p := args.parser()
p = args.str_flag(p, "out", "o", "output file", "out.txt")
p = args.bool_flag(p, "verbose", "v", "verbose")
p = args.parse(p)

out := args.get_str(p, "out") ?? "out.txt"
if args.get_bool(p, "verbose") ?? false {
    println("writing {out}")
}
```

## CSV → JSON

```zz
import std.csv
import std.json
import std.fs

func main() -> Result<int, str> {
    src := fs.read("data.csv")?
    rows := csv.parse(src)
    println(json.stringify(csv.to_json(rows)) ?? "[]")
    .ok(0)
}
```

## Worker pool

```zz
import std.task
import std.chan

jobs := chan()
out := chan()
for i in 0..4 {
    task.spawn(|_| {
        loop {
            match chan.try_recv(jobs) {
                .some(j) => chan.send(out, j * 2),
                .none    => break,
            }
        }
    })
}
for i in 0..100 { chan.send(jobs, i) }
for i in 0..100 {
    v: int = chan.recv(out)
}
```

Full: `examples/parallel_worker_pool.zz`.

## Hash a file (binary-safe)

```zz
import std.fs
import std.crypto

func main() -> Result<int, str> {
    b := fs.read_bytes("data.bin")?
    println(crypto.sha256_bytes(b))
    .ok(0)
}
```

## Regex extract

```zz
import std.regexp

match regexp.compile(`[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}`) {
    .ok(re) => println(re.find("mail ada@example.com now") ?? "none"),
    .err(e) => println(e),
}
```

Demo: `examples/regexp_email_demo/extract_emails.zz`.

## Env-gated server with graceful config

```zz
import std.env
import std.http

func port() -> int {
    match env.get("PORT") {
        .some(p) => int(p) ?? 8080,
        .none    => 8080,
    }
}

func main() {
    s := http.server()
    s = http.route(s, "GET", "/health", |_req| "ok")
    s = http.log(s, true)
    http.listen(s, port())
}
```

## SQLite users table with transaction

```zz
import std.sqlz

func main() -> Result<int, str> {
    db := sqlz.open("users.db")?
    sqlz.exec(db, "CREATE TABLE IF NOT EXISTS users (id INTEGER, name TEXT)")?
    sqlz.transaction(db, |tx| {
        sqlz.exec(tx, "INSERT INTO users VALUES (1, 'Ada')")?
        sqlz.exec(tx, "INSERT INTO users VALUES (2, 'Bo')")?
        .ok(0)
    })?
    rows := sqlz.query(db, "SELECT * FROM users")?
    println(rows)
    sqlz.close(db)?
    .ok(0)
}
```

## Terminal picker with raw mode + colors

```zz
import std.term
import std.colors

func main() {
    if !term.is_tty() {
        println("needs a tty")
        return
    }
    match term.enable_raw() {
        .ok(_)  => println("raw on"),
        .err(e) => println("no tty: {e}"),
    }
    defer term.disable_raw()
    match term.read_key() {
        .ok(k) => println("picked {k}" |> colors.green() |> colors.bold()),
        .err(e) => println(e),
    }
}
```

## JWT issue + verify round-trip

```zz
import std.crypto

tok := crypto.jwt_encode("secret", `{"sub":"1"}`)
match crypto.jwt_decode("secret", tok) {
    .ok(payload) => println("ok"),
    .err(e)      => println("bad: {e}"),
}
```

## Property test for a pure function

```zz
import std.property

func double(n: int) -> int { n * 2 }

func is_even_twice(x: int) -> bool {
    double(x) % 2 == 0
}

property.for_all_int(-1000, 1000, is_even_twice, 200, 7)
```
