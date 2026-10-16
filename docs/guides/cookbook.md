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
