# Cell — `std.cell`

Shared-mutable slots. Pool-backed handles for counters and caches. Each op is explicit — no hidden aliasing. Generic over the stored value type `T`.

```zz
import std.cell
```

## Purpose

`std.cell` gives you a mutable slot holding a value of any type `T`. Handles are pool-backed; each op takes the pool lock once. Prefer channels for communication; use cells for shared counters, caches, or config that multiple tasks read/write.

## Conceptual model

A cell is a mutable reference cell. `cell.new(v)` allocates a slot; `get`/`set` read/replace; `drop` releases the handle back to the pool. All ops are fallible (`Result`) because the pool can reject a dropped/closed handle. Cells share nothing with tasks except through explicit `get`/`set` — no data races by construction.

## Quickstart

```zz
import std.cell

c := cell.new(0)
cur := cell.get(c).unwrap_or(0)
_ := cell.set(c, cur + 1)
println(cell.get(c).unwrap_or(-1))  // 1
cell.drop(c)          // release back to pool
```

## Method syntax

Cell handles dispatch method calls: `c.get()`, `c.set(v)`, `c.drop()`.

```zz
import std.cell

func main() -> Result<int, str> {
    c := cell.new(0)
    c.set(5)?
    println(c.get().unwrap_or(-1))   // 5
    c.drop()
    return .ok(0)
}
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `cell.new` | `cell.new<T>(v: T) -> cell<T>` | Create a cell holding `v` | `cell.new(0)` |
| `cell.get` | `cell.get<T>(c: cell<T>) -> Result<T, str>` | Read current value | `cell.get(c)` |
| `cell.set` | `cell.set<T>(c: cell<T>, v: T) -> Result<bool, str>` | Replace value | `cell.set(c, 5)` |
| `cell.drop` | `cell.drop<T>(c: cell<T>) -> bool` | Release handle back to pool | `cell.drop(c)` |

`T` is inferred from the value passed to `cell.new`.

## Error model

All ops return `Result<_, str>` — pool rejection on dropped/closed handles yields `.err`. `get` returns the stored `T`; `set` returns `bool` (success).

## Gotchas

- **Never retain a dropped handle** — ops on it return `.err`.
- **Each op locks the pool once** — don't hold a cell across a blocking call expecting stability.
- **Cells share nothing with tasks** except through explicit `get`/`set` — no data races by construction.
- **Generic `T`** — inferred at `new`; all ops on a handle use the same `T`.

## Production patterns

```zz
import std.cell
import std.task

// Shared counter across tasks
func main() -> Result<int, str> {
    counter := cell.new(0)
    for i in 0..8 {
        task.spawn(|_| {
            cur := cell.get(counter).unwrap_or(0)
            match cell.set(counter, cur + 1) {
                .ok(_) => (),
                .err(e) => println(e),
            }
        })
    }
    // read final value
    println(cell.get(counter).unwrap_or(0))
    cell.drop(counter)
    return .ok(0)
}
```

