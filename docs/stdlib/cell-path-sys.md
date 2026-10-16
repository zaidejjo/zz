# System slots — `std.cell`, `std.path`, `std.sys`, `std.args`, `std.log`, TLS/Unix, TOML note

Small modules that glue ZZ to the OS. All fallible ops are `Result<_, str>`.

## `std.cell` — shared-mutable slots

Pool-backed handles for counters and caches. Each op is explicit — no hidden aliasing.

```zz
import std.cell

c := cell.new(0)
cur := cell.get(c).unwrap_or(0)
_ := cell.set(c, cur + 1)
println(cell.get(c).unwrap_or(-1))  // 1
cell.drop(c)          // release back to pool
```

`new(v)`, `get(h)`, `set(h, v)`, `drop(h)`. Prefer channels for communication,
cells for shared counters. Never retain a dropped handle.

## `std.path` — pure lexical paths

Pure-ZZ helpers, no I/O. Same semantics as `std.fs` path fns:

```zz
import std.path

path.join("a", "b.txt")       // a/b.txt (+ normalize)
path.join_all(["a", "b", "c"])
path.normalize("a/./b/../c")  // a/c
path.basename("a/b.txt")       // b.txt
path.dirname("a/b.txt")        // a
path.is_absolute("/x")         // true (also C:\x, \\unc\…)
path.extension("a.tar.gz")     // gz (no dot)
```

`join(a, b)` + normalize: `b` wins when absolute — `join(dir, "f.txt")`, not
`join(dir, "/f.txt")`.

## `std.sys` — infallible machine info

```zz
import std.sys

println(sys.os())         // linux / macos / windows
println(sys.arch())       // x86_64 / aarch64 / …
println(sys.cpu_count())  // logical cores — size your worker pool
println(sys.hostname())
println(sys.total_mem())  // bytes
println(sys.avail_mem())  // bytes
```

All queries infallible (empty string / 0 on unknown, never `.err`). Use
`cpu_count` for `task.spawn` fan-out, mem for cache sizing and diagnostics headers.

## `std.args` — argv + flag parser

Opaque parser handles (`Opaque("args")`):

```zz
import std.args

p := ArgsParser.new()
p.str_flag("out", "out.txt")
p.int_flag("jobs", 4)
p.bool_flag("verbose")
if p.parse(["--jobs", "8"]) {
    println(p.get_str("out"))
    println(p.get_int("jobs"))
    println(p.get_bool("verbose"))
} else {
    println(p.error())
}

println(p.positional(0) ?? "-")  // first leftover or "-"
println(p.subcommand() ?? "none")
println(p.help("myapp"))         // --help text
if p.was_help() { println("help shown") }
```

`get_raw()` is raw argv without parsing. Declare before `parse`; read after.
`help` text derives from flag declarations — keep descriptions short verbs.

## `std.log` — levels, sinks, spans (detail)

```zz
import std.log

log.set_level("debug")  // trace < debug < info < warn < error
println(log.get_level())
log.set_format("json")  // or text
log.to_stderr()
log.to_file("/tmp/app.log")
log.trace("tick")
log.debug("cache hit {k}")
log.info("listening :8080")
log.warn("retry {n}")
log.error("disk full")

s := log.span_begin("request")  // Opaque("span")
s.end()                         // method via std.span.end
```

Keep hot-path logs at `debug` and gate expensive fields behind a level check.
Spans nest; `end` is idempotent. Sinks are process-wide — set once in `main`.

## TLS / Unix sockets

Same handle + `Result` shape as [Networking](net.md):

- `std.tls` / `std.tls_listener` — TLS client/server wrappers (rustls, no OpenSSL).
- `std.unix` / `std.unix_listener` — Unix-domain sockets (`/tmp/app.sock`).
- Method dispatch works (`listener.accept()`, `stream.read()`, `stream.write()`).

Use `std.http.listen_tls` for HTTPS instead of raw TLS unless you own the framing.
Unix sockets skip TCP overhead for same-host IPC — set tight timeouts, unlink stale
socket files on boot.

## TOML note

TOML is intentionally not in stdlib. Use the external pure-ZZ `toml` package
(versioned separately, e.g. `~/Projects/toml`):

```toml
[dependencies]
toml = "1.2.3"
```

```zz
import toml
// toml.parse(src) -> Result<json-ish, str> per package README
```

Exact `i64::MIN`/`MAX` boundary checks in that package are the reference pattern
for guarding wrapping arithmetic at the edges.

## `std.io` legacy note

There is no `std.io` module — console I/O is built-ins (`print`, `println`, `input`).
Old examples showing `import std.io` + `io.println` predate the namespace rule;
replace with bare `println`. `funcs.rs` asserts no `std.io` seeds exist.
