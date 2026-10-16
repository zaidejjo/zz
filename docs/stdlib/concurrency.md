# Concurrency — `std.chan`, `std.task`, `std.cell`

Green threads: `task.spawn` builds a task in microseconds; the executor (one
parked OS thread per CPU, work-stealing Chase–Lev deques) multiplexes thousands
of them. Tasks run until complete or a blocking call; blocking suspends the task,
never the thread. Every `for`/`while` has a safepoint so CPU loops stay fair.

## Snapshot isolation

A spawn gets deep copies of captures plus every function it transitively calls.
After spawn, sides share nothing but channels and join handles — no data races
by construction. Mutating after `spawn` does not affect the task; task mutations
are invisible to the spawner. AOT mirrors this with `zz_value_dup` + thread-local
arenas. See [threading.md](../threading.md).

## Channels

Unbounded (lock-free ring + spill — bursts never block the sender):

```zz
import std.chan

c := chan()
chan.send(c, 42)
v := chan.recv(c)        // blocks
opt := chan.try_recv(c)  // .some(v) or .none, never blocks
```

`chan()` creates, `send` pushes, `recv` blocks, `try_recv` polls.

## Tasks

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
q := task.try_join(h)                // .some when done (repeatable), .none while running
```

`spawn(func)`, `join(handle)`, `try_join(handle)`.

## Fan-out / fan-in recipe

```zz
import std.task
import std.chan

jobs := chan()
out := chan()

for i in 0..4 {
    task.spawn(|_| {
        loop {
            match chan.try_recv(jobs) {
                .some(j) => chan.send(out, j * 2),
                .none    => break,
            }
        }
    })
}
for i in 0..100 { chan.send(jobs, i) }
```

Drain `out` with `recv`/`try_recv`. Full example:
`examples/parallel_worker_pool.zz`.

## Shared slots — `std.cell`

Pool-backed mutable handles for counters and caches:

```zz
import std.cell

c := cell.new(0)
cell.set(c, cell.get(c) + 1)
println(cell.get(c))
cell.drop(c)
```

`new/get/set/drop`. Prefer channels for communication, cells for shared counters.
Each op is explicit — no hidden aliasing.

## Rules

- Do not share mutable state except via `chan`/`task`/`cell`.
- Re-bind immediately after `free_port`-style TOCTOU helpers; retry on collision.
- Mark tests touching global state `@test(tag = "serial")`.
