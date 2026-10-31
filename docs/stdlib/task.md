# `std.task` — green threads and tasks

Lightweight concurrent tasks (`task.spawn`) with join handles (`task.join`)
to collect results or poll with `task.try_join`.

```zz
import std.task

h := task.spawn(|| {
    1 + 1
})
match task.join(h) {
    .ok(v) => println(v),   // 2
    .err(e) => println(e),
}

// Poll without blocking
match task.try_join(h) {
    .some(.ok(v)) => println("done: {v}"),
    .some(.err(e)) => println("failed: {e}"),
    .none => println("still running"),
}
```

## Types

| Type | Notes |
|---|---|
| `task.join` | Opaque join handle from `task.spawn`; await with `task.join` or poll with `task.try_join`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `spawn(f)` | `spawn(f: func(T) -> T) -> task.join` | Spawn `f` on a green thread; returns a handle. Generic over `T`. |
| `join(handle)` | `join(handle: task.join) -> T` | Block until `f` completes; returns its value or `.err`. |
| `try_join(handle)` | `try_join(handle: task.join) -> Option<Result<T, str>>` | `.some` when done, `.none` while still running. |
