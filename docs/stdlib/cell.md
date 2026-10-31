# `std.cell` — interior mutability cell

A generic `cell<T>` holds a single value of type `T` that can be read and
replaced after creation. Use it when you need mutable state that survives
across scopes without threading a value through every function.

```zz
import std.cell

c := cell.new(41)          // cell<int>
cell.set(c, 42)?           // replace the value
match cell.get(c) {
    .ok(v) => println(v)   // 42
    .err(e) => println(e),
}
cell.drop(c)               // release the cell
```

## Types

| Type | Notes |
|---|---|
| `cell<T>` | Opaque mutable cell holding one `T`. Created with `cell.new`, read with `cell.get`, replaced with `cell.set`, freed with `cell.drop`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `new(v)` | `new(v: T) -> cell` | Create a cell holding `v`. Generic over `T`. |
| `get(c)` | `get(c: cell) -> Result<T, str>` | Read the current value. `.err` if the cell was already dropped. |
| `set(c, v)` | `set(c: cell, v: T) -> Result<bool, str>` | Replace the stored value. Returns `.ok(true)` on success. |
| `drop(c)` | `drop(c: cell) -> bool` | Free the cell. Returns `true` when released. |
