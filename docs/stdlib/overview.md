# Stdlib overview

Batteries included: HTTP, JSON, fs, math, SQL, crypto, regexp, concurrency.
All fallible ops return `Result<_, str>` with `module:op:code: detail` messages,
identical on VM and AOT.

## Module index

| Module | Purpose | Doc |
|---|---|---|
| built-ins | `println`, `len`, `range`, `map`, `int` | [Built-ins](builtins.md) |
| `std.str`, `std.vec`, `std.bytes` | strings, arrays, builders | [Strings and arrays](str-vec.md) |
| `std.map`, `std.set`, `std.dec`, `std.csv`, `std.property` | data helpers | [Data](data.md) |
| `std.fs` + `bytes` | files, dirs, providers, buffers | [Filesystem](fs-bytes.md) |
| `std.json` | parse/stringify/get/as_* | [Data](data.md) + [Filesystem](fs-bytes.md) |
| `std.http` | server + client | [HTTP](http.md) |
| `std.net` (+ tls/unix) | TCP | [Networking](net.md) |
| `std.env`, `std.args`, `std.process`, `std.sys`, `std.log` | OS integration | [Env and processes](env-process.md) |
| `std.math`, `std.time` | numerics, clocks, dates | [Math and time](math-time.md) |
| `std.term`, `std.colors` | terminal + ANSI | [Terminal](terminal.md) |
| `std.encoding`, `std.crypto`, `std.uuid` | codecs, hashes, IDs | [Encoding and crypto](encoding-crypto.md) |
| `std.regexp` | compiled patterns | [Regexp](regexp.md) |
| `std.sqlz` (+ postgres/mysql) | SQLite + wire drivers | [Databases](sql-db.md) |
| `std.chan`, `std.task`, `std.cell` | concurrency | [Concurrency](concurrency.md) |
| asserts, `std.test` | tests, temp dirs, snapshots | [Test helpers](testing-lib.md) |
| Option/Result methods | `unwrap`, `unwrap_or`, `expect` | [Built-ins](builtins.md) |

Exhaustive tables: [stdlib.md](../stdlib.md).
Signatures source of truth: `crates/zz_stdlib/src/funcs.rs`.

## Namespace rule

Globals are convenience aliases. Canonical homes:

- `std.vec.*` for array pipelines (`vec.*` short form works after `import std.vec`).
- `str.*` for `ord`/`chr`. Bare `ord`/`chr` are aliases.
- `int`/`float` return `Option` — reason discarded, fall back at the call site: `int(s) ?? 0`.
- `typeof` returns a name string (`"int"`, `"bytes"`) — compare with string equality only.

Prefer canonical homes in new code.

## Error handling pattern

```zz
import std.fs

match fs.read("data.txt") {
    .ok(c)  => println(c),
    .err(e) => println("read failed: {e}"),
}

// Propagate with ? inside Result/Option functions:
func load(p: str) -> Result<str, str> {
    c := fs.read(p)?
    .ok(c)
}
```

Bare `Result` as a statement warns. Handle it (`match`), propagate (`?`),
or ignore explicitly (`_ := ...`).
