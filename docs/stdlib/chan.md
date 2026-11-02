# Chan — `std.chan`

Unbounded lock-free channels for task communication. Green-thread safe by construction: `send` pushes, `recv` blocks, `try_recv` polls. Channels are the primary way tasks exchange data without shared mutable state.

```zz
import std.chan
```

## Purpose

`std.chan` creates unbounded (lock-free ring + spill — bursts never block the sender) channels. `send` pushes, `recv` blocks, `try_recv` polls. Use channels to pass data between tasks — no shared mutable state, no data races by construction.

## Quickstart

```zz
import std.chan

c := chan()
chan.send(c, 42)
v: int = chan.recv(c)              // blocks
opt: Option<int> = chan.try_recv(c)  // .some(v) or .none, never blocks
```

## Fan-out / fan-in recipe

```zz
import std.task
import std.chan

jobs := chan()
out := chan()

for i in 0..4 {
    task.spawn(|_| {
        while true {
            match chan.try_recv(jobs) {
                .some(j) => chan.send(out, j * 2),
                .none    => break,
            }
        }
    })
}
for i in 0..100 { chan.send(jobs, i) }
// Drain `out` with recv/try_recv
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `chan` | `chan<T>() -> chan<T>` | Create a channel | `c := chan()` |
| `chan.send` | `chan.send<T>(ch: chan<T>, v: T) -> unit` | Push a value (never blocks) | `chan.send(c, 42)` |
| `chan.recv` | `chan.recv<T>(ch: chan<T>) -> T` | Block for a value | `v := chan.recv(c)` |
| `chan.try_recv` | `chan.try_recv<T>(ch: chan<T>) -> Option<T>` | Poll, never blocks | `opt := chan.try_recv(c)` |

## Edge cases & gotchas

- **Unbounded** — a fast producer can outpace a slow consumer; memory grows. Apply backpressure externally if needed.
- **`recv` blocks** the task (not the OS thread) — other tasks keep running.
- **`try_recv` never blocks** — returns `.none` when empty.
- **Channels are not cloneable** — pass the handle by value/move.

## Production patterns

```zz
import std.chan
import std.task

// Simple producer/consumer
func main() -> Result<int, str> {
    c := chan()
    task.spawn(|_| -> Result<int, str> {
        chan.send(c, 42)
        return .ok(0)
    })
    v: int = chan.recv(c)
    println(v)   // 42
    return .ok(0)
}
```

## Rules

- Do not share mutable state except via `chan`/`task`/`cell`.
- Re-bind immediately after `free_port`-style TOCTOU helpers; retry on collision.
- Mark tests touching global state `@test(tag = "serial")`.

