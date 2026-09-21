# Concurrency Engine Audit — `feat/aot-direct-handoff`

**Scope:** AOT executor + channels + spawn/dup/block-pool (`crates/zz_codegen/src/runtime/core.c`,
`core.h`), green lowerer (`crates/zz_codegen/src/lower/green.rs`, `expr.rs` spawn fuse).
**Method:** read-only code audit + 17-test stress battery (`bench/audit/au*.zz`), release runs,
ASan/TSan harnesses. **No implementation code was modified in this phase.**

**Headline:** the fast paths are sound (handoff airtightness, wake gating, pool exactness all
verified). The battery found **1 Critical deadlock, 2 Major correctness bugs**, plus known
by-design limits. Details + minimal fix plan below. Approval requested before touching code.

## 1. Battery results

| Test | Shape | Release | ASan | TSan | Notes |
|---|---|---|---|---|---|
| au1_verdict | trailing handoff-send, waiter re-suspends | **FAIL** (`verdict_stale`) | leaks only | silent | **MAJOR-1**, deterministic repro |
| au2_doublejoin | same-thread double join, array result | PASS | leaks only (const) | — | aliasing memory-safe |
| au3_mpmc | 8 senders × 1000, 2 receiver tasks | PASS 8000 | leaks only | **silent** | MPMC paths clean |
| au4_joinchain | 60 nested spawn+join links | PASS 61 | leaks only | silent | sequential inline OK |
| au5_relay30 | 30-task channel cascade (depth cap 8) | PASS 30 | leaks only | silent | cap fallback works |
| au6_thrash | 300× chan alloc/ping/drop | PASS, 3.5MB peak | — | — | pool steady-state |
| au7_flood | 50k sends, no receiver, drain | PASS, 3.5MB peak | — | — | spill efficient, unbounded (see MAJOR-3) |
| au8_tree / au8b_tree4 | blocking-join tree d7/d4 | **HANG** | n/a (logic) | n/a | **CRITICAL-1** |
| au9_indirect | named-fn blocking recv on worker | PASS 123 | — | — | fallback + top-up OK (single) |
| au10_bigcap | str/array/dict/int capture | PASS 54 | — | — | fused dup OK |
| au11_deepnest | 3-level closure literals | PASS 6 | — | — | lowerer nesting OK |
| au12_multijoin | 2 blockers, 1 array handle | **HANG** | no mem error pre-hang | n/a | **MAJOR-2** (lost wakeup, not corruption) |
| au13_negspawn | `task.spawn(42)` | clean type error | — | — | proper diagnostic |
| au14_negsend | `send(1, 2)` | statically rejected | — | — | no runtime path |
| au15_bigpool | 8KB capture (pool overflow bucket) | PASS 999 | const leaks | — | overflow free path exact |
| au16_deeprec | 100k-deep recursion in task | **SIGSEGV** | n/a | n/a | LIMIT-1, pre-existing |

ASan "leaks only" = constant 3×64KB arenas + handles (known-benign, identical to pre-change
baseline). No heap-use-after-free / overflow / double-free in any passing shape. au12 under ASan
shows **zero memory errors before the hang** — its hang is pure lost-wakeup.

Repro: `zz build -p bench/audit/auN_*.zz && ./bin/auN_*` (au1/au8/au12 fail/hang as noted).

## 2. Findings

### CRITICAL-1: top-up private-deque stranding → permanent deadlock (au8/au8b)

**Trigger:** any worker-side blocking-join tree deeper than ~2 levels where an intermediate task
spawns before joining. `au8b_tree4` (31 tasks) reproduces in <4s: 11 threads, **all** in
`__futex_wait`, top-up cap (32) nowhere near exhausted.

**Mechanism** (`core.c` `zz_executor_top_up` + `zz_worker_loop` + `zz_enqueue_task`):
1. A top-up worker runs with a **private deque** that is *never stolen from by design*
   ("its work is always its own" — only founding `nworkers` deques are steal victims).
2. It spawns child B → B lands on its private deque → it blocks in `task.join(B)`.
3. Nobody can run B (only the blocked owner can). A parked founder *would* steal it, but
   top-up deques are excluded from `zz_steal_siblings`.
4. Replacement top-up is refused by the parked-gate (`zz_executor.sleepers > 0` — a founder
   *is* parked, but it can only steal founding deques, so it spins helplessly).

Single-blocker shapes (au4, au9) never strand; the hole needs spawn-then-block **on a top-up
thread**. Recursive divide-and-conquer through blocking join is the natural trigger.

**Fix options (pick one pre-merge):** (a) blocking join/recv on a worker pumps its own deque
while waiting (helping — bounded: run local tasks until predicate true); (b) make top-up
deques stealable; (c) route top-up spawns to the injector instead of the private deque.
(a) is the most robust (also covers founder stranding under cap exhaustion).

### MAJOR-1: inline re-suspend clobbers the sender's verdict → completion dropped (au1)

**Trigger:** green task S sends on a channel with a registered task waiter W; W's inline run
re-suspends (recvs/joins on empty); S returns a value without another green entry. The
trampoline reads stale `verdict=1` and treats S as suspended — but S registered **no waiter**,
so its join never completes (hang for joiners, frame+rep leak). Deterministic repro in au1
(`verdict_stale`, exit 0 — silent, no crash).

**Mechanism:** `zz_run_task` resets the TLS verdict at entry; W's suspend sets it to 1;
`zz_handoff_run_inline` returns without restoring; `zz_chan_send` never touches the verdict.
The existing comment ("the sender's next blocking call re-establishes it") only holds when a
next blocking call exists. Same hazard via `zz_task_join_complete`'s inline loop, and via
indirect sends (green task → named fn → send).

**Fix (one spot):** save/restore `zz_suspend_verdict` around `zz_run_task` in
`zz_handoff_run_inline`. Re-run au1 → expect `verdict_ok`.

### MAJOR-2: blocking multi-join loses wakeups — `signal` must be `broadcast` (au12)

`zz_task_join_complete` wakes classic sleepers with a single `pthread_cond_signal`, but
handles are explicitly multi-recv ("intentionally not freed here to allow multiple recv").
Two threads blocking in `task.join` on one handle: first completion wakes one, the other
sleeps forever (au12 hangs; ASan proves no corruption involved). Green multi-waiters are
fine (all inlined).

**Fix (one line):** `pthread_cond_broadcast` on the join condvar when `sleepers > 0`.

### MAJOR-3 (by design): sends never apply backpressure

`zz_chan_send` always succeeds; the spill queue grows without bound. au7 (50k) is cheap
(3.5MB), but an adversarial/unpaced producer OOMs the process — Go would park the sender.
At minimum document the boundlessness; consider a `send_blocking` variant or a spill cap
with park. Not a merge-blocker if documented.

### MAJOR-4 (by design): handles + channels live forever

Every spawn leaks its join handle (mutex+cond+result); channels have no teardown either.
At ~500k spawns/s this is a genuine steady-state growth rate for long-lived services.
Recommend handle reclamation (free on completed+consumed, or a handle pool) as fast follow;
not a merge-blocker for benchmarks, but it is for production use.

### MINOR-1: send queue-grow OOM leaks the dup'd value

`zz_chan_send`: `owned = zz_value_dup(val)` happens before lock; the `ch->cap` grow-failure
path returns `err=1` without freeing `owned`. OOM-only; free it on that path.

### MINOR-2: dup-build OOM result gets memoized as a copy

`zz_closure_dup_build` OOM returns null-payload; the dup-inner memo check
(`out.payload != v.payload`) treats it as a real copy and memoizes it, so later dups of the
same source silently yield dead closures. OOM-only; skip memoizing null-payload results.

### MINOR-3: annotated `let` rejected inside closure bodies

`v: int = task.join(prev)` parses in `fn` bodies but not in closure bodies
(`expected '}' to close dict literal`). Forces named-helper workarounds (au4/au12 use them).
UX papercut; also limits stress-test expressiveness.

### MINOR-4: `task.join` needs explicit annotation

Join results don't infer (`a := task.join(h)` fails); `[int]` annotation required. Same class
as MINOR-3; fine once documented.

### LIMIT-1: deep ZZ recursion segfaults (au16, pre-existing)

100k-deep calls in a task overflow the 1MB worker C-stack (SIGSEGV, core dump). Arenas live
on the heap so inline-handoff depth (cap 8) is unaffected — this is plain call recursion, a
pre-existing engine limit, not a handoff regression. Recommend documenting a max-depth
guideline and measuring the exact threshold as follow-up.

### LIMIT-2: 32-bit literal arithmetic wraps

`10000 * 1000000000` overflows (found while writing `bench/handoff`: printed `67`);
divide-first ordering required. Type-system note, not concurrency — recording here since the
benchmark tripped over it.

## 3. Verified sound (do not "fix")

- Handoff airtightness: waiter registration vs serve-waiter both under `ch->lock`; no
  lock-free send pre-check; announce-then-verify on `sleepers`/`gparked` (chan + join +
  injector) — MPMC/relay TSan-silent.
- Chase–Lev `bottom` atomic stores (prior TSan fix) hold under steal-heavy shapes.
- Pool exactness: bucket-size checkout/checkin, overflow exact-malloc freed (au15 ASan),
  16B RAW alignment consistent across size/layout/pre-scan/fill.
- Depth-cap fallback: 30-cascade over cap-8 completes (au5), TSan-silent.
- `zz_run_task` TLS save/restore; non-green path never consults the verdict (immune to
  MAJOR-1 — only green senders affected).
- Channel/join leak-by-design means **no UAF on waiter abandon** (nothing is ever freed
  under a queued waiter) — only growth (MAJOR-4).

## 4. Action plan (minimal, pre-merge)

1. `zz_handoff_run_inline`: save/restore `zz_suspend_verdict` (fixes MAJOR-1). Re-run au1.
2. Join completion: `broadcast` when `sleepers > 0` (fixes MAJOR-2). Re-run au12.
3. Blocking wait on workers pumps the local deque while the predicate is false (fixes
   CRITICAL-1). Re-run au8/au8b + full battery + e2e/parity + ASan/TSan.
4. Document MAJOR-3/4 + LIMIT-1/2 in `docs/threading.md`.
5. Promote battery: wire `bench/audit/` markers into a runner or e2e (au8/au12 become
   regression guards for the three fixes).

Steps 1–2 are one-liners; step 3 is the only substantive change. Requesting approval to
proceed.
