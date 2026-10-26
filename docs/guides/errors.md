# Error catalog — most common errors, exact fixes

## Type mismatch

```
x: int = "hello"  // expected int, found str
```

Fix: annotate correctly (`x: str = "hello"`) or convert (`x := int("hello") ?? 0`).
Arg errors name the parameter: ``parameter `greeting` expects `str` ``.

## Undefined variable / typo

```
undefined variable `pritnln`  // did you mean `println`?
```

Fix: `zz fix` applies the safe rename; check selective-import names
(`import shapes(Tokens)` vs qualified `shapes.Tokens`).

## Missing match arm

```
non-exhaustive match: missing `.IntLit` (or add a `_` arm)
```

Fix: add the variant or `_ => ...`. Guarded `_ if cond` does not count.

## Bare payload pattern

```
.ok  // type error: .ok always carries a payload
```

Fix: `.ok(_)` to ignore, `.ok(n)` to bind. `zz fix` rewrites bare patterns.

## Unused Result

```
fs.write(path, data)  // warning: unused Result
```

Fix: `match`, `?`, or `_ := fs.write(...)`. Block tails are values (no warning);
loop tails and `defer` expressions do warn.

## Dead arm

Arms after `_` / bare binding warn unreachable. Move the catch-all last or guard it.

## Tuple dynamic index

```
t[i]  // compile error
```

Fix: destructure (`(a, b) := t`) or literal index (`t[0]`). Out-of-range literals
are also compile-time.

## Import errors

- `circular import: a imports b, b imports a` → extract shared `c.zz`.
- Two files under one namespace → alias one side (`import a as x`).
- Stale lock pin → `zz install` rewrites from manifest (note names the source).
- `param("typo")` → checker names valid params; fix the string.

## Integer traps

- Overflow wraps (never traps) — guard counters/money at the boundary.
- `1 / 0` literal fails at check time; runtime divide-by-zero and `INT64_MIN / -1` error.
- Negative shift count errors; shifts mask to `& 63` (`1 << 64` is `1`).
- Bitwise needs `int` both sides; `~` needs `int`.

## HTTP / IO

- Bare `.err` to `println` aborts — `match` it first.
- `req.body` is lossy text; binary needs `http.body_bytes(req)` / `read_bytes`.
- `join(dir, "/file.txt")` loses `dir` — use `join(dir, "file.txt")`.
- Non-TTY `term.*` fails soft `.err("... not a tty")` — gate with `is_tty` (except `poll`).
- `free_port()` is TOCTOU — re-bind immediately, retry, tag tests `serial`.

Run `zz fix` for safe rewrites, `zz check --format json` for machine output.
