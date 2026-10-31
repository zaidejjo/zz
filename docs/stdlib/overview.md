# Stdlib overview

Batteries included: HTTP, JSON, fs, math, SQL, crypto, regexp, concurrency.
All fallible ops return `Result<_, str>` with `module:op:code: detail` messages,
identical on VM and AOT.

## Module index

Every module has its own reference page. Combined guide pages (e.g.
`str-vec.md`, `data.md`) remain as cross-module overviews.

| Module | Purpose | Doc |
|---|---|---|
| built-ins | `println`, `len`, `range`, `map`, `int`, `assert_*` | [Built-ins](builtins.md) |
| `std.str` | string search, case, trim, split, join, builder | [`std.str`](str.md) |
| `std.vec` | array pipelines: map/filter/sort/push/pop/fold | [`std.vec`](vec.md) |
| `std.bytes` | mutable byte builder | [`std.bytes`](bytes.md) |
| `std.map` | `{str:int}` / `{str:str}` dict helpers | [`std.map`](map.md) |
| `std.set` | set union/intersect/diff on `[str]`/`[int]` | [`std.set`](set.md) |
| `std.dec` | exact fixed-point decimal arithmetic | [`std.dec`](dec.md) |
| `std.csv` | delimited parse / stringify / JSON | [`std.csv`](csv.md) |
| `std.json` | JSON parse/query/stringify | [`std.json`](json.md) |
| `std.fs` | files, dirs, providers (`zzfs`), file handles | [`std.fs`](fs.md) |
| `std.io` | `file` handle type | [`std.io`](io.md) |
| `std.http` | HTTP server + client | [HTTP](http.md) |
| `std.net` | TCP, TLS, Unix domain sockets | [Networking](net.md) |
| `std.env` | env vars, cwd, exe, platform | [`std.env`](env.md) |
| `std.args` | CLI argument parser | [`std.args`](args.md) |
| `std.process` | spawn/run external processes | [`std.process`](process.md) |
| `std.sys` | OS/arch/hostname/mem/cpu queries | [`std.sys`](sys.md) |
| `std.log` | leveled logging + spans | [`std.log`](log.md) |
| `std.math` | math, trig, aggregates, constants | [`std.math`](math.md) |
| `std.time` | clocks, sleeps, RFC3339 dates | [`std.time`](time.md) |
| `std.term` | terminal raw mode, size, keys | [Terminal](terminal.md) |
| `std.colors` | ANSI colors and styles | [`std.colors`](colors.md) |
| `std.encoding` | base64, hex, URL encode/decode | [`std.encoding`](encoding.md) |
| `std.crypto` | hashes, bcrypt/argon2, sign/verify, JWT | [`std.crypto`](crypto.md) |
| `std.uuid` | UUID v4/v7 generation and validation | [`std.uuid`](uuid.md) |
| `std.regexp` | compiled regular expressions | [Regexp](regexp.md) |
| `std.sqlz` | SQLite open/exec/query/transaction | [`std.sqlz`](sqlz.md) |
| `std.chan` | message-passing channels | [`std.chan`](chan.md) |
| `std.task` | green threads + join handles | [`std.task`](task.md) |
| `std.cell` | interior-mutability cell | [`std.cell`](cell.md) |
| `std.span` | span handle type | [`std.span`](span.md) |
| asserts, `std.test` | assertions, temp dirs, snapshots | [Test helpers](testing-lib.md) |
| Option / Result | `unwrap`, `unwrap_or`, `expect`, `?` | [Built-ins](builtins.md) |

Exhaustive signature tables: [stdlib reference](reference.md) (generated from
`crates/zz_stdlib/src/funcs.rs`).

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
