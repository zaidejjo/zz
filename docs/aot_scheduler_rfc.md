# RFC: AOT Native M:N Scheduler Migration

Status: design (no C code yet — approved scope is VM-first through Phase A).
Target: `< 1.0µs` channel round-trip, `> 250k/s` fan-in in AOT, beating Go
by removing the VM interpreter from the hot path (not by out-tuning it).

## 1. Why AOT must change

AOT `zz_spawn` today creates one pthread per task (~40µs) and channels
ride pthread mutexes + the generic value model. The VM's 226k/s comes
from green multiplexing; AOT has none of it. There is no incremental
path — pthreads cannot do M:N. This RFC ports the proven VM executor
design (work-stealing, direct handoff, ring channels, pools) to C.

## 2. Primitives (`core.c` / `core.h`)

### B1. Lock-free channel ring (C11 atomics)
Port `zz_runtime::lf_chan` (Vyukov MPMC,供需 sequence protocol) 1:1:
`_Atomic size_t head/tail`, per-cell `_Atomic size_t seq`, 64-byte cell
padding, `memory_order_acquire/release` on the exact edges the Rust
version documents. Unbounded ZZ channels keep the two-tier design: ring
fast path + mutex spillover (reuse the existing spill mutex, already
correct). Values crossing threads go through `zz_value_dup` (already
thread-safe post-W4: atomic string refs, intern lock, typed cells).

### B2. M:N executor in C
Fixed worker pool (one per CPU, `pthread`, 1MB stacks — already shipped
in Phase 1). Chase–Lev deques: port `fetch`/`steal`/park-token logic
from `executor.rs` (~200 lines, mechanically portable — the algorithm
is fully documented in code comments). Global injector for main-thread
spawns, single-sleeper round-robin wakeups, direct handoff (completions
land on the completing worker's deque). Park tokens via
`pthread_cond_t` + generation counter (same announce-then-verify shape).

### B3. Suspendable AOT frames (the hard part)
AOT closures are plain C functions — they cannot suspend mid-frame like
VM frames. Two options:

- **(i) Trampoline + explicit state machine (recommended).** The
  lowerer splits closures at blocking calls (`chan.recv`, `task.join`,
  `sleep`): each segment becomes a `switch` case indexed by a resume
  counter stored in the task struct; blocking calls return to the
  scheduler instead of blocking. Locals that live across a yield point
  spill to the task struct (liveness analysis at lowering). Gated behind
  `--green-tasks`, pthread fallback by default until parity is green.
- **(ii) `ucontext`/stackful coroutines (rejected for now).** Fast to
  integrate, platform-fragile (macOS ARM deprecations, signal-stack
  interactions), and reintroduces 8MB-stack memory costs we already
  eliminated. Revisit only if (i) proves intractable.

### B4. Slab task allocation
Fixed thread-local pools in `core.c`: task structs (64–128 slots per
worker) + 64KB arenas for closure env cells. Mirrors the VM shell pools
(`VmShell`, env shells, join pool, registry pools) — same checkout/
checkin discipline, same caps. Expected to remove the ~0.5µs of
`malloc` traffic per AOT task.

## 3. Expected deltas

| Metric | AOT today | AOT after B1–B4 (projected) | Go 1.27 |
|---|---|---|---|
| Chan round-trip | ~15µs | < 1.0µs (no VM dispatch, direct handoff) | 1.5µs |
| Fan-in | ~24k/s (pthread/spawn) | > 250k/s (M:N + slabs) | 185k/s |
| 50k spawn RSS | 32MB (1MB stacks) | ~10MB (slabs + shared rings) | 133MB |

Projections scale from measured VM numbers minus VM dispatch (~0.5µs):
the C executor does the same queue dance with cheaper frames.

## 4. Risks & gates
- C11 atomics on ARM need exact orderings (x86 TSO forgives; the macOS
  ARM CI leg + TSan harness from W4 cover this).
- (i) state-machine lowering must preserve exact panic/`.err` semantics
  — parity suite extended with green-task AOT fixtures before the flag
  flips on.
- No B-code lands without the VM suite green (e2e 141, parity 52) —
  AOT shares fixtures, so regressions surface immediately.
- Estimated scope: B1 (1 session) → B2 (1–2) → B4 (1) → B3 (2–3).

## 5. Decision requested
Approve B1+B2+B4 first (mechanical ports, low risk), B3 (compiler
surgery) as a separate review once B1–B2 are green.
