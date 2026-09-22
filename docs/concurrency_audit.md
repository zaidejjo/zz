# Concurrency Engine Audit — `feat/aot-direct-handoff`

**Scope:** AOT executor + channels + spawn/dup/block-pool (`crates/zz_codegen/src/runtime/core.c`,
`core.h`), green lowerer (`crates/zz_codegen/src/lower/green.rs`, `expr.rs` spawn fuse).
**Method:** read-only code audit + 17-test stress battery (`bench/audit/au*.zz`), release runs,
ASan/TSan harnesses. **No implementation code was modified in this phase.**

**Headline:** the fast paths are sound (handoff airtightness, wake gating, pool exactness all
verified). The battery found **1 Critical deadlock, 2 Major correctness bugs**, plus known
by-design limits. Details + minimal fix plan below. Approval requested before touching code.

> **Status (post-approval): all three runtime fixes landed on this branch — au1/au8/au12
> now pass, full battery + `cargo test --all` green, ASan/TSan silent. Follow-up probing
> found two lowerer holes: MAJOR-6 (resume-skipped C-local initializers — FIXED, au3/au19/
> au20 guards) and MAJOR-5 (nested capture through greencell — open, au18 known-fail
> guard). A follow-up probe refined the helping rule further (see CRITICAL-1 update).
> au17 (indirect-verdict probe) was dropped: the shape it needs is exactly MAJOR-5, so no
> passing test can currently construct it; the help-path save/restore shipped as zero-risk
> hardening under the same proven invariant.

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

> **Landed fix:** (a) — `zz_worker_help_once`: a worker about to park runs one task from
> its **own deque only** and rechecks its predicate; all stealing stays in the hold-nothing
> worker loop. Two hard-won refinements from the au4 regression the first revision caused:
> stealing while nested provably stalls total-order chains (wait-for graph: 17 cycle edges,
> satisfiable predicates rotting mid-stack while tops wait on the unsatisfiable — per-join
> broadcasts only revisit tops). Final rule: own-deque-only helping (own-chain nesting is
> inherently forward: a parent buried under its own child is revisited when the child
> completes and unwinds into it) + `ZZ_HELP_MAX_DEPTH` 64 backstop. au4 (chain 60) + au8
> (tree 255) both pass; MPMC/relay/thrash/flood unaffected.

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

> **Landed (+ hardening):** save/restore in `zz_handoff_run_inline` — au1 now `verdict_ok`.
> Same save/restore added in `zz_worker_help_once`: helping a suspender from inside a
> named-fn blocking wait is the identical hazard (au9-shape). No dedicated test exists:
> the only constructible shape needs a green suspender nested in a green waiter, which is
> exactly MAJOR-5 below — so no passing test can cover it yet. The hardening is zero-risk
> (blocking callers never consume the verdict; only the trampoline does, after later green
> entries re-establish whatever the run needs).

### MAJOR-2: blocking multi-join loses wakeups — `signal` must be `broadcast` (au12)

`zz_task_join_complete` wakes classic sleepers with a single `pthread_cond_signal`, but
handles are explicitly multi-recv ("intentionally not freed here to allow multiple recv").
Two threads blocking in `task.join` on one handle: first completion wakes one, the other
sleeps forever (au12 hangs; ASan proves no corruption involved). Green multi-waiters are
fine (all inlined).

**Fix (one line):** `pthread_cond_broadcast` on the join condvar when `sleepers > 0`.

> **Landed:** au12 now `multijoin_ok`, ASan-clean.

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

### MAJOR-5 (new, lowerer): nested capture through a green cell breaks C codegen

Found by follow-up probing (au18, kept as known-fail guard): a green closure containing a
nested spawn literal that captures an outer variable fails C compilation (`_gcell1`/`zz_fr`
undeclared in the child's static scope). The nested closure routes its capture through the
parent's green frame cell instead of its own env. Discriminant: the identical shape WITHOUT
the capture builds fine, so the hole is precisely capture-through-greencell (spawn fuse
innocent — both shapes use it). Loud failure (compile error, never silent), pre-existing B3
gap, no e2e fixture covers the shape. **Not fixed in this phase (lowerer surgery, out of
the approved scope) — proposed follow-up.** It also blocks the au17 indirect-verdict probe
(a green suspender nested in a green waiter), which was dropped for that reason.

### MAJOR-6 (new, lowerer): resume skips C-local initializers — indeterminate loop bounds

Found chasing au3's ~30% O3-only hang (dev clean, ASan clean, TSan silent): green
range-`for` bounds live in C stack locals (`_s`/`_e`), but a resume `goto`s over their
declarations into the loop body — the trip count reads indeterminate memory (silent
over-count hangs / under-count early exits; O0 usually survives on stack-slot reuse,
which is why only release hung). Same class in match top-level binding arms (plain
`zz_value v = ...` skipped by resume) and in any non-literal fast-path bound. **Fixed:**
range end bounds spill to frame cells (literals stay inline; a `zz_int(...)` wrapper
around a temp is NOT a literal); match top-level bindings use frame cells when green;
raw-scalar unboxing no longer appends `.i` to int64 cell derefs (that also fixes plain
`for i in 0..n` over captured ints, green or not — same line, loud C error before).
Guards: au3 (60/60 release), au20 (variable bound, 15/15), au19 (guarded binding arm).
The array-iteration path already spilled correctly — range was the miss.

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

## 5. Resolution (all steps landed)

- au1 `verdict_ok`, au12 `multijoin_ok`, au8 `tree_ok` (128), au8b `tree_ok` (16),
  au4 `chain_ok` (61) — the full battery (au1–au16, au18 known-fail) passes on release.
- `cargo test --all` exit 0 (44 suites; e2e 141/141, parity 52/52); `cargo fmt --check`
  clean; `cargo clippy --all-targets` zero warnings.
- ASan: zero errors on au1/au2/au3/au4/au5/au8/au12/au15 (constant benign leaks only).
  TSan: silent on au1/au3/au4/au5/au8/au12/au15.
- `bench/handoff` ZZ-vs-Go holds (box-noisy; relative standing kept).
- Open follow-ups: MAJOR-5 lowerer fix (+ au17 probe unblocked by it), handle
  reclamation (MAJOR-4), send backpressure (MAJOR-3), battery wired into CI (step 5).
- Step 5 LANDED: `crates/zz_cli/tests/concurrency_audit_regression.rs` (18 AOT tests,
  hang-safe timeouts, resource bounds, build-failure negatives) + 8 e2e fixtures
  (6 VM success shapes + 2 static-reject errors). VM multi-join deliberately NOT
  covered by e2e: the VM consumes join results by design (second join errors), so
  concurrent multi-join is AOT-only territory (au12).
