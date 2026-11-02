# Standard Library

> Reference (complete). Learning path: [Stdlib overview](stdlib/overview.md).
> Per-module guides: one file per stdlib module (see Module Index below).
> Generated name index: [reference.md](stdlib/reference.md).
> Signatures source of truth: `crates/zz_stdlib/src/funcs.rs`.

## Built-in Functions

Available without imports:

| Function | Signature | Description |
|----------|-----------|-------------|
| `print` | `print(v: T) -> unit` | Print value without newline |
| `println` | `println(v: T) -> unit` | Print value with newline |
| `input` | `input(prompt: str) -> str` | Read line from stdin (optional prompt) |
| `typeof` | `typeof(v: T) -> str` | Runtime type name as a string (e.g. `"int"`, `"str"`, `"bytes"`) |
| `str` | `str(v: T) -> str` | Convert to string (keeps `.ok`/`.some` wrappers) |
| `int` | `int(v: T) -> Option<int>` | Parse/convert to int; `.none` on failure (use `??` fallback) |
| `float` | `float(v: T) -> Option<float>` | Parse/convert to float; `.none` on failure (use `??` fallback) |
| `len` | `len(v: T) -> int` | Length of array, tuple, bytes, string, dict, or range |
| `range` | `range(stop)` / `range(start, stop)` / `range(start, stop, step)` | Create integer range |
| `map` | `map(arr: [T]\|T.., f: func(T)->U) -> [U]` | Apply function to each element (also `std.vec`) |
| `filter` | `filter(arr: [T]\|T.., f: func(T)->bool) -> [T]` | Keep elements where predicate is true (also `std.vec`) |
| `enumerate` | `enumerate(arr: [T]\|T..)` | Index + value pairs (also `vec.enumerate` / `std.vec.enumerate`) |
| `zip` | `zip(a: [T]\|T.., b: [U]\|U..)` | Pair elements from two iterables |
| `clone` | `clone(v: T) -> T` | Deep copy, fully independent value |
| `weak` | `weak(v: T) -> T` | Weak reference for cycle breaking |
| `upgrade` | `upgrade(w: T) -> T` | Upgrade a weak reference to strong |
| `dbg` | `dbg(v: T) -> T` | Debug print preserving wrappers, returns `v` |

> Namespace rule: globals are convenience aliases. Canonical homes are
> `std.vec.*` for array pipelines, `str.*` for `ord`/`chr`. Prefer the
> canonical home in new code; the bare forms stay until 0.3. `int`/`float`
> return `Option` — the parse-failure reason is discarded, so fall back
> explicitly at the call site: `int(s) ?? 0`. `typeof` returns a type *name
> string*, not a type value — compare with string equality only.

Option/Result methods (method dispatch on variant values) are documented in
[Built-ins](stdlib/builtins.md).

## Module Index

One doc file per stdlib module. Each has a purpose paragraph, quickstart
example, method-syntax forms, error model, and an exhaustive table of every
function/constant/type with signature + description.

| Module | Purpose | Doc |
|--------|---------|-----|
| `std.str` | String manipulation, byte offsets | [String](stdlib/str.md) |
| `std.vec` | Generic array operations | [Vector](stdlib/vec.md) |
| `std.bytes` | Contiguous byte buffers | [Bytes](stdlib/bytes.md) |
| `std.map` | Dict helpers: safe lookup, merge | [Map](stdlib/map.md) |
| `std.set` | Set helpers over arrays | [Set](stdlib/set.md) |
| `std.dec` | Exact decimal math on strings | [Decimal](stdlib/dec.md) |
| `std.json` | Parse/stringify/get/as_* | [JSON](stdlib/json.md) |
| `std.csv` | RFC4180 subset | [CSV](stdlib/csv.md) |
| `std.property` | Seeded generators + shrinking | [Property](stdlib/property.md) |
| `std.colors` | ANSI styling, truecolor, 256-color | [Colors](stdlib/colors.md) |
| `std.crypto` | Digests, HMAC, CSPRNG, passwords, signatures, JWT | [Crypto](stdlib/crypto.md) |
| `std.encoding` | Base64, hex, URL codecs | [Encoding](stdlib/encoding.md) |
| `std.uuid` | v4/v7 generation, parse, validate | [UUID](stdlib/uuid.md) |
| `std.log` | Levels, sinks, spans | [Log](stdlib/log.md) |
| `std.sys` | Infallible machine info | [Sys](stdlib/sys.md) |
| `std.term` | Raw mode, single-key reads, size | [Term](stdlib/term.md) |
| `std.args` | Argv + flag parser | [Args](stdlib/args.md) |
| `std.env` | Environment, cwd, args | [Env](stdlib/env.md) |
| `std.process` | Run/spawn external processes | [Process](stdlib/process.md) |
| `std.fs` | Filesystem, providers, streaming | [FS](stdlib/fs.md) |
| `std.path` | Pure lexical path helpers | [Path](stdlib/path.md) |
| `std.http` | Server + client | [HTTP](stdlib/http.md) |
| `std.net` (+ tls/unix) | TCP networking | [Net](stdlib/net.md) |
| `std.cell` | Shared-mutable slots | [Cell](stdlib/cell.md) |
| `std.regexp` | Compiled patterns | [Regexp](stdlib/regexp.md) |
| `std.sqlz` (+ postgres/mysql) | SQLite + wire drivers | [SQL](stdlib/sqlz.md) |
| `std.chan` | Unbounded channels | [Chan](stdlib/chan.md) |
| `std.task` | Spawn and join green threads | [Task](stdlib/task.md) |
| `std.test` | Asserts, temp dirs, snapshots | [Test](stdlib/test.md) |
| `std.math` | Numerics, constants | [Math](stdlib/math.md) |
| `std.time` | Clocks, sleeps, calendar dates | [Time](stdlib/time.md) |

TOML is intentionally **not** in stdlib — use the external pure-ZZ `toml`
package (versioned separately).

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

## Error handling pattern

All fallible ops return `Result<_, str>` with `module:op:code: detail`
messages, identical on VM and AOT.

```zz
import std.fs

match fs.read("data.txt") {
    .ok(c)  => println(c),
    .err(e) => println("read failed: {e}"),
}

// Propagate with `?` inside Result/Option functions:
func load(p: str) -> Result<str, str> {
    c := fs.read(p)?
    return .ok(c)
}
```

Bare `Result` as a statement warns. Handle it (`match`), propagate (`?`),
or ignore explicitly (`_ := ...`).
