# ZZ Threading: tasks and channels

Green threads: `task.spawn` creates a task in ~3µs and the executor
multiplexes thousands of them onto one OS thread per CPU. Tasks run until
they complete or block; blocking suspends the task, never the thread.
Scheduling is work-stealing: each executor thread owns a Chase–Lev deque
(LIFO local pop), spawns from other threads land in a global injector,
and idle workers steal from random victims. Wakeups land on the waking
worker's own deque (direct handoff — the thread that made progress very
likely resumes the waiter next, hot cache).

## Model: snapshot isolation

A spawned closure receives **deep copies** of everything it can see —
captured variables plus every function it (transitively) calls. After the
spawn, the two sides share nothing but channels and join handles. There is
no shared mutable state, so there are no data races by construction.

Consequences:

- Mutating a variable after `spawn` does **not** affect the running task
  (it captured the old value).
- Mutations a task makes to captured variables are invisible to the
  spawner.
- Spawning in a loop is cheap: the function table is snapshotted once and
  extended incrementally; each spawn carries only the functions its own
  code references.

The AOT backend mirrors the model in C: channel sends and spawn captures
are deep copies (`zz_value_dup`), arenas are thread-local, string
refcounts and the intern table are thread-safe.

## Recipes

```zz
import std.task
import std.chan

// Fan-out / fan-in.
c := std.chan()
for i in 0..8 {
    task.spawn(|_| { std.chan.send(c, i * i) })
}
for i in 0..8 {
    v: int = std.chan.recv(c)
}

// Join a single task (panics inside surface as `.err`).
h := task.spawn(|_| { risky() })
r: Result<int, str> = task.join(h)

// Non-blocking poll: `.some` when done (repeatable), `.none` while running.
q := task.try_join(h)
```

## Cooperative safepoints

Tasks yield only at blocking calls — a pure-CPU loop would hog its
executor thread forever. Every `for`/`while` therefore carries a
safepoint at its loop top (VM `Op::Safepoint`, AOT `zz_safepoint()`):
one counter decrement per iteration, clock read once per 1024, voluntary
yield past a 1ms quantum. Cost ~0.02ns/iter (unmeasurable); header (not
back-edge) placement so `continue` cannot skip the check. On the VM the
executor requeues the task; on AOT (pthread-based tasks, no suspendable
frames) it is a `sched_yield` courtesy so siblings get scheduled.

## Pitfalls

- **Blocking before send hangs.** `chan.recv` waits for a `chan.send`
  from somewhere else. If every task is stuck receiving, nothing can send.
  Structure programs as: tasks send, exactly one side receives — or use
  `task.join`, which always resolves (`.err` on worker panic).
- **A worker that panics before sending** leaves its channel empty. Prefer
  `task.join` (yields `.err("...")`) over channel-joins when workers can
  fail. Timeouts are deliberately not part of the language: a missing
  message is a program bug, surface it with joins.
- **`task.join` consumes.** The first main-thread `task.join` takes the
  result; a second join errors loudly (`result already consumed`) instead
  of hanging. `task.try_join` never consumes — poll freely. Green-thread
  joiners each receive their own copy.
- **Interpreted calls inside tasks are rejected.** Tasks run compiled
  chunks only; calling a function with no compiled chunk is a loud error.
  In the unified pipeline every function has a chunk, so this only bites
  hand-built interpreters (REPL internals, tests).
- **Blocking I/O (`sleep`, sockets, files) inside tasks** parks an
  executor thread (the pool tops itself up, so throughput survives, but
  oversubscription costs latency). Keep blocking I/O on the main thread
  when it matters.
- **AOT limitations.** `panic`/`fail` inside AOT task closures lowers to
  `unit` (no error plumbing through C closures yet), so the panic test is
  VM-only. Everything else — spawn, join, try_join, channels — has full
  VM/AOT parity.

## Performance (release `zz`, 4-core Linux/x86_64)

Parking is spin-then-sleep: a worker burns PAUSEs re-checking the
object for ~3µs before registering as a waiter, so rendezvous arriving
inside the quantum cost ~100ns with zero futex ops or registry churn.
Channels add a lock-free MPMC ring fast path (Vyukov, 1024 deep, each
cell cache-line padded) with mutex spillover past capacity, so
green-to-green traffic takes zero locks while depth fits. Executor
threads hot-spin on the ready queue while work flows (adaptive
miss-streak backoff: consecutive misses halve the budget, a hit
restores it — calm machines see zero futex sleeps, loaded ones degrade
to blocking instead of backfiring). Longer quanta backfire (the spinner
steals the peer's core); the remaining floor is thread-hop + VM
dispatch (Phase 2 territory: work-stealing).

Spawning is pool-aware: registry entries recycle through per-shard
pools, the registry itself is 16-way sharded (insert/lookup/remove never
share a lock across shards), empty-capture spawns skip snapshotting
entirely, reachability sets are `Arc`-shared per spawn-site chunk, and
completions with no waiters store by move and skip the condvar notify.
(Full per-chain epoch validation was tried and reverted: loop-var
redefinition churns any global epoch, and per-scope tracking cost more
than the chain walk it replaced — the filter walk stays, at ~2µs.)

| op | ZZ | comparison |
|---|---|---|
| `spawn` dispatch | ~3µs (~88k fan-in/s; 50k burst in 0.46s) | Go 50k burst in 0.66s, 185k fan-in/s |
| `spawn+join` round-trip | ~25µs | pool-era ZZ was ~250ms (10,000× ago) |
| `chan.send+recv` round-trip | ~1.9µs release (was 25µs pre-spin) | Go chan ~1.5µs/rt |
| 64 parallel tasks | linear speedup | real parallelism, not just concurrency |

Measure your own hardware with `ZZ_SPAWN_PROFILE=1 zz run prog.zz`
(per-spawn timing breakdown) — methodology: `time.now_nanos()` around
200-iteration loops, warmed caches, same machine for every column.
Remaining gap vs Go is scheduler latency (two futex round-trips per
join), not cloning: snapshots are reachable-only with structural caches.
