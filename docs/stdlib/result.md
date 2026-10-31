# `std.result` — `Result<T, E>` type

`Result<T, E>` is a built-in enum (`ok` / `err`) for fallible operations.
Most operations are builtins (`unwrap`, `unwrap_or`, `expect`) or the `?`
propagation operator inside functions returning `Result`.

```zz
import std.result

match fs.read("data.txt") {
    .ok(c)  => println(c),
    .err(e) => println(e),
}

func load(p: str) -> Result<str, str> {
    c := fs.read(p)?   // propagate on error
    return .ok(c)
}
```

## Types

| Type | Notes |
|---|---|
| `Result<T, E>` | Built-in enum: `.ok(T)` or `.err(E)`. Match with `.ok(v) => ...` / `.err(e) => ...`. Propagate with `?`. |

## Variables

None.

## Functions

None (use builtins `unwrap`, `unwrap_or`, `expect`, and match patterns).
