# Stdlib overview

Batteries included: HTTP, JSON, fs, math, SQL, crypto, regexp, concurrency.
All fallible ops return `Result<_, str>` with `module:op:code: detail` messages,
identical on VM and AOT.

## Module index

| Module | Purpose | Doc |
|---|---|---|
| built-ins | `println`, `len`, `range`, `map`, `int`, `clone`, `dbg` | [Built-ins](builtins.md) |
| `std.str` | string manipulation, byte offsets | [String](str.md) |
| `std.vec` | generic array operations | [Vector](vec.md) |
| `std.bytes` | contiguous byte buffers | [Bytes](bytes.md) |
| `std.map` | dict helpers: safe lookup, merge | [Map](map.md) |
| `std.set` | set helpers over arrays | [Set](set.md) |
| `std.dec` | exact decimal math on strings | [Decimal](dec.md) |
| `std.json` | parse/stringify/get/as_* | [JSON](json.md) |
| `std.csv` | RFC4180 subset | [CSV](csv.md) |
| `std.property` | seeded generators + shrinking | [Property](property.md) |
| `std.colors` | ANSI styling, truecolor, 256-color | [Colors](colors.md) |
| `std.crypto` | digests, HMAC, CSPRNG, passwords, signatures, JWT | [Crypto](crypto.md) |
| `std.encoding` | base64, hex, URL codecs | [Encoding](encoding.md) |
| `std.uuid` | v4/v7 generation, parse, validate | [UUID](uuid.md) |
| `std.log` | levels, sinks, spans | [Log](log.md) |
| `std.sys` | infallible machine info | [Sys](sys.md) |
| `std.term` | raw mode, single-key reads, size | [Term](term.md) |
| `std.args` | argv + flag parser | [Args](args.md) |
| `std.env` | environment, cwd, args | [Env](env.md) |
| `std.process` | run/spawn external processes | [Process](process.md) |
| `std.fs` | filesystem, providers, streaming | [FS](fs.md) |
| `std.path` | pure lexical path helpers | [Path](path.md) |
| `std.http` | server + client | [HTTP](http.md) |
| `std.net` (+ tls/unix) | TCP networking | [Net](net.md) |
| `std.cell` | shared-mutable slots | [Cell](cell.md) |
| `std.regexp` | compiled patterns | [Regexp](regexp.md) |
| `std.sqlz` (+ postgres/mysql) | SQLite + wire drivers | [SQL](sqlz.md) |
| `std.chan` | unbounded channels | [Chan](chan.md) |
| `std.task` | spawn and join green threads | [Task](task.md) |
| `std.test` | asserts, temp dirs, snapshots | [Test](test.md) |
| `std.math` | numerics, constants | [Math](math.md) |
| `std.time` | clocks, sleeps, calendar dates | [Time](time.md) |
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
    return .ok(c)
}
```

Bare `Result` as a statement warns. Handle it (`match`), propagate (`?`),
or ignore explicitly (`_ := ...`).
