# `std.option` — `Option<T>` type

`Option<T>` is a built-in enum (`some` / `none`) for values that may be
absent. `std.option` is the type module; most operations are builtins
(`int`, `float`, `clone`, `dbg`) or methods on `Option`.

```zz
import std.option

match env.get("HOME") {
    .some(home) => println(home),
    .none       => println("no home"),
}
v := int("42") ?? 0   // fallback on none
```

## Types

| Type | Notes |
|---|---|
| `Option<T>` | Built-in enum: `.some(T)` or `.none`. Match with `.some(v) => ...` / `.none => ...`. |

## Variables

None.

## Functions

None (use builtins `unwrap`, `unwrap_or`, `expect`, and match patterns).
