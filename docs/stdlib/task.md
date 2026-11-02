# Task — `std.task`

Spawn and join green threads. Tasks run in microseconds; the executor (one parked OS thread per CPU, work-stealing Chase–Lev deques) multiplexes thousands of them. Snapshot isolation: a spawn gets deep copies of captures.

```zz
import std.task
```

## Purpose

`std.task.spawn` builds a task; `join` waits for its result; `try_join` polls. A spawn gets deep copies of captures plus every function it transitively calls — after spawn, sides share nothing but channels and join handles, so no data races by construction. Blocking calls suspend the task, never the thread. Every `for`/`while` has a safepoint so CPU loops stay fair.

## Quickstart

```zz
import std.task
import std.chan

c := chan()
for i in 0..8 {
    task.spawn(|_| { chan.send(c, i * i) })
}
for i in 0..8 {
    v: int = chan.recv(c)
    println(v)
}

h := task.spawn(|_| { 21 * 2 })
r: Result<int, str> = task.join(h)   // panics inside surface as .err
q := task.try_join(h)                // .some when done, .none while running
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `task.spawn` | `task.spawn<T>(f: func(T) -> T) -> task_join<T>` | Spawn a green thread | `h := task.spawn(|_| 21 * 2)` |
| `task.join` | `task.join<T>(handle: task_join<T>) -> T` | Wait for result (panics → `.err`) | `r := task.join(h)` |
| `task.try_join` | `task.try_join<T>(handle: task_join<T>) -> Option<Result<T, str>>` | Poll result (repeatable) | `q := task.try_join(h)` |

## Method syntax

Join handles dispatch method calls: `h.join()`, `h.try_join()`.

```zz
import std.task

h := task.spawn(|_| { 40 + 2 })
r: int = h.join()      // 42
```

## Edge cases & gotchas

- **Snapshot isolation** — mutating after `spawn` does not affect the task; task mutations are invisible to the spawner.
- **AOT mirrors this** with `zz_value_dup` + thread-local arenas.
- **Blocking calls suspend the task** — never the OS thread.
- **`try_join` is repeatable** — `.some` after done, `.none` while running.

## Production patterns

```zz
import std.task

// Fan-out / fan-in
func main() -> Result<int, str> {
    h1 := task.spawn(|_| -> int { 10 })
    h2 := task.spawn(|_| -> int { 20 })
    a: int = task.join(h1)
    b: int = task.join(h2)
    println(a + b)   // 30
    return .ok(0)
}
```

## Rules

- Tasks share nothing but channels/join handles after spawn.
- Mark tests touching global state `@test(tag = "serial")`.
- See [threading.md](../threading.md) for details.

