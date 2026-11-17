# Shared Mutable State — `state` / `atomic { }`

Status: design (Phase 0). Implemented across VM + AOT in phases 1–4.

## Problem

ZZ has no safe, convenient shared mutable state across tasks. Users reach for
plain mutable globals (`n := 0; n = n + 1` inside `task.spawn`), which is a
data race in the AOT engine and diverges between VM (snapshot-frozen) and AOT
(live shared). Closing that gap requires a dedicated construct.

## Goals (decisions already made — do not redesign)

- **Easy:** no lifetimes, no ceremony in the common case. `state hits := 0`;
  `hits += 1` is a single read-modify-write under protection.
- **Fast:** zero cost when no concurrency is reachable (AOT lock elision →
  plain static, indistinguishable from a plain global).
- **Safe:** no data races, no torn reads, no lost updates in one operation,
  no deadlocks. Identical observable behavior in VM and AOT.

## D1 — Declarations (`state`, contextual keyword, module-level only in v1)

```
state NAME := expr
state NAME: T = expr
pub state NAME := expr
```

- `state` is **contextual**: only a keyword when it begins a top-level
  statement and is immediately followed by an identifier. `state` remains a
  valid identifier everywhere else (`state := 1`, `foo.state`, `let state = 5`)
  — no breakage of existing programs.
- **Eager initialization**, same dependency order as normal globals
  (imports first, entry file last, cycle detection unchanged).
- Reading a `state` before its initializer ran is a compile error when
  detectable; otherwise a runtime abort with a span.
- `pub state` is exported like `pub` globals; visibility rules identical to
  normal globals.

### AST

`Stmt::Decl` gains an `is_state: bool` field (mirrors `is_const: bool`), so
`state` reuses the declaration machinery rather than a new variant. Parser
entry point: `parse_stmt` — after the `Pub` arm, add a `state` arm before the
fallback `try_parse_explicit_decl`; `pub state NAME ...` handled inside the
`Pub` arm. Parser tests must prove `state` and `atomic` still parse as plain
identifiers.

## D2 — Auto-atomic statements

Every statement that reads/writes a `state` variable executes as **one atomic
unit**: a consistent snapshot of all states it reads; its write is indivisible.

- `hits += 1`, `hits = hits + 1`, `log = vec.push(log, m)`, `cfg.x = 1`,
  `arr[i] = v` on a state are real read-modify-write under protection.
- **Deadlock constraint:** the protection is NEVER held across user code or
  blocking calls. Therefore inside an auto-atomic statement the expression may
  only contain:
  - literals, operators, comparisons
  - field/index access
  - string interpolation
  - natives from an explicit **WHITELIST** of pure, non-blocking natives

Calls to user functions/closures or non-whitelisted natives inside a statement
that touches `state` → **compile error**, fix hint: "wrap in `atomic { }`".

### Whitelist derivation

Derived from the native metadata in `crates/zz_stdlib_meta/src/table.rs`
(`FuncSig.readonly`, set by the `#[readonly]`/`#[pure]` decorator) — every
native that is pure, non-blocking, and does not touch concurrency state.
Whitelisted natives (with justification):

| Native | Justification |
|--------|---------------|
| `std.str.length`, `std.str.contains`, `std.str.find`, `std.str.split` | Pure string queries, no side effects, non-blocking |
| `std.math.*` (readonly ones: abs, signum, clamp, floor, ceil, round, etc.) | Pure arithmetic |
| `len`, `range`, `ord`, `chr` | Pure builtins |
| `std.vec.*` query natives that are readonly | Read-only vector ops |
| `std.dict.*` query natives that are readonly | Read-only dict ops |

Exact list emitted in Phase 2 by scanning `FuncSig.readonly == true` and
manually auditing for blocking/concurrency contact. Natives that are NOT
whitelisted (blocking or effectful): `chan.recv`, `chan.send`, `task.join`,
`task.spawn`, `time.sleep_ms`, `time.sleep_micros`, `std.fs.*` (blocking I/O),
`std.net.*`, `std.http.*`, `std.process.*`, `std.env.set_var`, `sqlz.*`,
`io.*`, `print`/`println` (side effect on stdout).

## D3 — `atomic { ... }` block

Contextual keyword: `atomic` at statement start followed by `{`.

- Executes the body while holding the state lock.
- v1: a **statement**, not an expression.
- `return`, `break`, `continue`, errors/`?`, and `defer` inside release the
  lock correctly (scoped cleanup on every unwind path).
- Nested `atomic` is allowed — reentrant via a depth counter.
- User function calls ARE allowed inside `atomic`.
- Blocking inside is forbidden:
  - Checker rejects known-blocking natives (chan.recv, task.join,
    task.spawn+join, blocking I/O, sleep…).
  - **Runtime guard** aborts with a clear diagnostic if a task yields/blocks
    while atomic depth > 0.
- Because a task can never yield while holding the lock, an **OS-thread-local**
  depth counter is sufficient even though tasks are green threads — verified
  against the executors:
  - VM: `crates/zz_stdlib/src/natives/concurrency/executor.rs` (green tasks
    yield only at blocking natives; a held atomic depth blocks the yield).
  - AOT: C executor in `core.c` (green suspend verdict is TLS; `zz_yield()`
    checks depth).

## D4 — Checker rule R1 (check-then-act)

Within ONE function/closure body, outside any `atomic` block: if a `state` S is
read in one statement and written in a DIFFERENT statement, and the read can
reach the write (control-flow or loop), it is an **ERROR** with fix hint
"wrap both in `atomic { }`".

- Reads only, or a single self-contained RMW statement: fine.
- Intra-procedural only (no effect system). Implemented on the AST/HIR with
  existing infrastructure; documented conservative approximation (reuse the
  definite-assignment walk in `crates/zz_type_check/src/definite_assignment.rs`
  — no CFG exists).
- **Known, documented limit:** logic races across separate functions
  (`if get() > 0 { dec() }`) are not detected — same guarantee as Rust's
  `Mutex`. Stated in docs.
- False positives measured by running the checker over ALL fixtures, examples
  and stdlib; each hit reported.

## D5 — Checker rule R2 (plain globals vs tasks)

A normal mutable top-level global (neither `const` nor `state`) that is
accessed (read or written) by any code reachable from a `task.spawn` closure
is a **compile ERROR**: "global `x` is reachable from a task; declare it
`state`".

- Reachability: direct references inside the spawn closure, plus the call
  graph (`crates/zz_hir/src/callgraph.rs`), conservative for function values
  (any function whose value is taken is reachable from any indirect call).
- **Exception:** a plain global NEVER assigned anywhere after its initializer
  (whole-program, all modules) is effectively immutable and may be read from
  tasks.
- Closes the VM/AOT divergence (`direct_vis.zz`, `race.zz`, `race_fn.zz` in
  `global-state-audit.md`) by making those programs invalid.
- Verify the call graph is conservative enough; if not, report exactly where
  it is unsound.

## D6 — State value types

Any value type EXCEPT closures/function values (and any type containing them).
Reason: closures capture by reference through shared heap cells
(`docs/checker/decisions.md:3-32`), so storing one in a `state` would smuggle
unsynchronized shared cells across tasks. Compile error with explanation.
Channel/join/opaque handles are allowed (already Arc-shared).

- Reads copy (value semantics, COW/refcount dup under the lock).
- No way to borrow a reference out of a `state`.

## D7 — Lock design

ONE process-global reentrant lock in v1. No lock ordering → no deadlock.
Per-state locks / sharding explicitly OUT OF SCOPE.

## D8 — Lock elision (performance)

In AOT, if no `task.spawn`/channel-based concurrency is reachable in the whole
program (and no native plugin can call back / spawn), `state` compiles to
a plain static with no lock and no atomics.

- Reuse how `ZZ_ARC_RELAXED` / the whole-program "nothing reachable uses
  spawn" analysis is determined (`crates/zz_codegen_runtime/src/runtime/core.h:152-178`
  and its codegen-side switch `Lowerer::needs_safepoint` at
  `crates/zz_lower/src/context.rs:1027`, which keys on reachable natives
  containing `task`/`chan`/`spawn`).
- VM keeps the (uncontended, cheap) lock; no VM elision in v1.

## D9 — Name resolution to ONE shared storage

Both engines resolve `state` names to ONE shared storage in every context:
top level, functions, methods, nested closures, spawn closures, workers.
Never through the VM's frozen/snapshotted env (`crates/zz_runtime/src/env.rs:810-860`).

- **VM:** a state registry owned by the program run (`Arc` shared with worker
  interpreters), reset per `Interp`/test per `docs/test-isolation.md` (add a
  row to its audit table).
- **AOT:** `static zz_value` + the global lock + `__thread` depth counter.
- **IR (.zzc):** dedicated ops `LoadState` / `StoreState` / `RmwState` /
  `AtomicEnter` / `AtomicExit`; update the IR codec; keep the stack-based VM
  design.

## D10 — `unsafe impl Send/Sync for Value` invariant

Do NOT weaken it (`crates/zz_runtime/src/value.rs:400-415`). Document in a
comment how `state` access preserves it (all cross-thread state goes through
the single lock; snapshots are frozen before crossing thread boundaries).

## Out of scope

Lazy init-once globals, task-local state, effect/purity system, per-state
locks, memory orders/atomic types, `state` inside functions/blocks, `atomic`
as an expression, changing channel/cell semantics.

## Error codes (new, consistent with Z0001 style)

| Code | Title |
|------|-------|
| Z0011 | `state` used outside module level |
| Z0012 | `state` holds closure/function type (D6) |
| Z0013 | read of `state` before init |
| Z0014 | non-whitelisted native / user call in auto-atomic statement |
| Z0015 | blocking call inside `atomic { }` |
| Z0016 | R1 check-then-act (state read then write across statements) |
| Z0017 | R2 plain global reachable from a task |
| Z0018 | runtime: yield/block while atomic depth > 0 |

Each gets an `ERROR_CATALOG` entry in `crates/zz_diag_core/src/lib.rs` plus
`.with_code(ErrorCode(N))` at emission sites, and `zz explain ZNNNN`
coverage (test `error_codes_have_explanations` enforces this).

## Known limits / open questions

1. R1 is intra-procedural only — cross-function logic races not detected
   (documented, same as Rust Mutex).
2. VM state registry is reset per `Interp`/test — cross-test state bleed
   possible if a test stores a `state` and another reads it; documented in
   test-isolation audit table.
3. `ZZ_ARC_RELAXED` is env-gated (`getenv`), not compile-time — lock elision
   decision must be made at codegen time from reachability, not the env var.
4. Native plugin callback/spawn reachability for elision — verify plugins
   can't spawn; report if unsound.

## Deviations from D1–D10

None yet. Any deviation found during implementation will be listed here with
evidence before proceeding past the gate that discovers it.
