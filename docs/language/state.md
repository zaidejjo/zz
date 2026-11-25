# Shared Mutable State — `state` / `atomic { }`

Status: implemented (VM + AOT). All decisions D1–D10 are implemented.
Remaining: docs/tooling (LSP hover, benchmarks, test-isolation row).

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

Derived from `crates/zz_checker_core/src/lib.rs` `state_safe_natives()` — the
audited set of natives that are pure, non-blocking, never call back into user
code, and do no I/O. These are exactly the natives allowed inside D2
auto-atomic statements (outside `atomic { }`).

Whitelisted natives:

| Native | Justification |
|--------|---------------|
| `range`, `len`, `ord`, `chr` | Pure builtins |
| `std.str.length`, `std.str.contains`, `std.str.find`, `std.str.rfind`, `std.str.byte_len`, `std.str.ord`, `std.str.starts_with`, `std.str.ends_with`, `std.str.starts_with_at`, `std.str.ends_with_at`, `std.str.is_empty`, `std.str.count`, `std.str.count_in`, `std.str.find_in`, `std.str.rfind_in` | Pure string queries, no side effects, non-blocking |
| `std.str.builder`, `std.str.push_part`, `std.str.finish`, `std.str.builder_len`, `std.str.join_parts`, `std.str.push_byte`, `std.str.extend` | Linear string builders; mutate a builder buffer directly, no user callback, no I/O, non-blocking |
| `std.math.abs`, `std.math.signum`, `std.math.clamp`, `std.math.floor`, `std.math.ceil`, `std.math.round`, `std.math.min`, `std.math.max`, `std.math.pow`, `std.math.sqrt` | Pure arithmetic |
| `std.vec.len`, `std.vec.contains`, `std.vec.push`, `std.vec.insert`, `std.vec.remove`, `std.vec.pop`, `std.vec.sort` | Vector ops; non-closure forms only — closure-taking `sort_by`/`sort_by_desc`/`sort_key` stay OUT |
| `std.dict.len`, `std.dict.contains`, `std.dict.get`, `std.dict.keys`, `std.dict.values` | Dict query natives. Dicts use index syntax (`d["k"] = v`, `d["k"]`) which lowers to a store op allowed under D2; there are no `dict.set`/`dict.insert`/`dict.remove` natives |
| `std.json.len`, `std.json.has`, `std.json.type`, `std.json.as_str`, `std.json.as_int`, `std.json.as_float`, `std.json.as_bool`, `std.json.is_null`, `std.json.is_bool`, `std.json.is_number`, `std.json.is_string`, `std.json.is_array`, `std.json.is_object`, `std.json.is_empty`, `std.json.deep_get`, `std.json.path_exists`, `std.json.path_get_or` | Pure JSON queries (NOT parse/stringify/merge) |
| `std.sys.os`, `std.sys.arch`, `std.sys.cpu_count`, `std.sys.hostname`, `std.sys.total_mem`, `std.sys.avail_mem` | Pure system-info queries |
| `std.log.get_level` | Pure query only (NOT set_level/set_format/to_file) |
| `std.crypto.sha256`, `std.crypto.sha256_bytes`, `std.crypto.sha512`, `std.crypto.hmac_sha256`, `std.crypto.random_bytes`, `std.crypto.ct_eq` | Pure hash/query (NOT keygen/sign/verify/Argon2/bcrypt) |
| `std.encoding.base64_encode`, `std.encoding.base64_decode`, `std.encoding.base64_decode_bytes`, `std.encoding.hex_encode`, `std.encoding.hex_decode`, `std.encoding.url_encode`, `std.encoding.url_decode` | Pure encoding/decoding |
| `std.regexp.is_match`, `std.regexp.find`, `std.regexp.captures`, `std.regexp.is_email` | Pure regexp query only (NOT compile) |
| `std.uuid.is_valid` | Pure validation only (NOT v4/v7/parse) |
| `std.args.get_str`, `std.args.get_int`, `std.args.get_bool`, `std.args.positional`, `std.args.was_help` | Pure getters only (NOT parser/flag setters) |

Still EXCLUDED (blocking, effectful, I/O, closure-taking, spawn/chan/task):
`chan.recv`, `chan.send`, `task.join`, `task.spawn`, `time.sleep_ms`,
`time.sleep_micros`, `std.fs.*`, `std.net.*`, `std.http.*`, `std.process.*`,
`std.env.set_var`, `sqlz.*`, `io.*`, `print`/`println`, and closure-taking
natives (`vec.sort_by`, `vec.sort_by_desc`, `vec.sort_key`, `map`, `filter`,
…). Those raise **Z0014** inside an auto-atomic statement: "wrap the
statement in `atomic { }`".

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

## R1–R5 — Multi-state read batching (fix for torn multi-state reads)

A statement's header expression (the expression in a `decl`, the target and
value of an assignment/compound-assignment, the condition of `if`/`while`/`match`-scrutinee/`range()`, the parts of string interpolation, the fields of composite literals, and the arguments of a call) is split into **maximal PURE regions**: subexpressions with no call of any kind (no user function, no closure, no native — whitelisted or not — no I/O, no blocking call). The `&&`/`||` short-circuit expression is one region; binary operands, array/struct literals, string interpolation, and comparisons are each one region when pure.

- **R1 (one region = one lock):** all `state` reads inside one pure region run inside a single `StateBatchBegin`/`StateBatchEnd` (VM) / `zz_state_lock`/`zz_state_unlock` (AOT) — one lock acquisition for the whole region. Nothing else runs inside the region.
- **R2 (call cell):** for a call, argument evaluation (receiver + arguments) is one region: emit `BatchBegin`, evaluate all args, `BatchEnd`, THEN the `Call`. The call itself (user code / I/O) never runs inside the batch.
- **R3 (split reads → Z0019):** if one statement reads `state` in two or more different pure regions (reads separated by a call, e.g. `foo(a, f(), b)` or `g(a) + h(b)`), it can observe two different snapshots → compile error **Z0019**: "state reads in one statement are separated by a call and may observe different snapshots; wrap the statement in `atomic { }` or read them in one expression". Reads of a single `state` in several regions are allowed; only two or more *different* states split across regions are rejected.
- **R4 (atomic / loops):** inside `atomic { }` no batch is emitted (the block already holds the lock). A `while` condition opens and closes its batch on every evaluation. Each `if`/`else-if` condition is its own region; `match` guards likewise.
- **R5 (boundaries):** every path out of a batch region closes it (short-circuit jumps included; `?`/error cannot occur inside a pure region — asserted). A debug assertion checks batch depth is 0 at every statement boundary.

The VM compile path (`crates/zz_runtime/src/vm/compiler.rs`) and the AOT lowering
(`crates/zz_lower/src/context.rs`, `expr.rs`, `stmt.rs`) share the pure-region
decomposition in `crates/zz_span_ast/src/ast/regions.rs` so all three layers
agree. `Op::StateBatchBegin`/`StateBatchEnd` already cross every layer
(IR, run cache, `zz_chunk`, AOT), so the fix widens existing regions — no new
ops, no temporaries.

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
| Z0019 | R3 state reads in one statement separated by a call (torn-snapshot risk) |

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

None found during implementation. All decisions D1–D10 are implemented as specified.

## R4 — Benchmarks

Methodology: release build (`cargo build --release -p zz_cli`), warm-up run,
median of 7 timed runs, measured with `date +%s%N` nanosecond timestamps.
All benchmarks run N iterations of a read-modify-write (RMW) on a single
`state` variable inside a loop; `ns/op = total_ns / N`.

| Benchmark | N | Engine | Total ns | ns/op |
|---|---|---|---|---|
| `state_read_write` (`counter = counter + 1`) | 1,000 | AOT | 3,688,227 | 3,688 |
| `state_read_write` | 10,000 | AOT | 4,745,131 | 474 |
| `state_read_write` | 100,000 | AOT | 8,340,701 | 83 |
| `state_vec_push` (`log = vec.push(log, "m")`) | 1,000 | AOT | 3,898,394 | 3,898 |
| `state_vec_push` | 10,000 | AOT | 4,778,441 | 477 |
| `state_vec_push` | 100,000 | AOT | 9,145,442 | 91 |
| `local_read_write` (`s = s + 1`, plain local, baseline) | 1,000 | AOT | 4,948,429 | 4,948 |
| `local_read_write` | 10,000 | AOT | 4,017,430 | 401 |
| `local_read_write` | 100,000 | AOT | 4,226,872 | 42 |
| `state_read_write` | 1,000 | VM | 16,973,878 | 16,973 |
| `state_read_write` | 10,000 | VM | 38,287,212 | 3,828 |
| `state_read_write` | 100,000 | VM | 248,795,075 | 2,487 |
| `state_vec_push` | 1,000 | VM | 45,553,576 | 45,553 |
| `state_vec_push` | 10,000 | VM | 2,521,179,721 | 252,117 |
| `local_read_write` | 1,000 | VM | 14,950,239 | 14,950 |
| `local_read_write` | 10,000 | VM | 15,442,687 | 1,544 |

Key observations:

- **AOT `state` RMW at N=100,000 is 83 ns/op** — well within the same order of
  magnitude as a plain local RMW (42 ns/op). The lock acquisition cost at AOT
  scale is modest (~2x a plain local).
- **AOT `state_vec_push` at N=100,000 is 91 ns/op** — the vec allocation
  dominates, so the state-lock overhead is nearly invisible.
- **VM is ~10-30x slower than AOT** — expected for an interpreter; the state
  lock adds no extra asymptotic cost beyond the per-op interpreter dispatch.
- At small N (1,000) the `zz` binary startup cost (~15ms) dominates, so
  ns/op figures there are inflated relative to large-N.

## Choosing between `state`, `cell`, and `chan`

| Construct | Best for | Cost | Concurrency model |
|-----------|----------|------|-------------------|
| `state` | Module-level shared mutable state accessed from multiple tasks | One process-global lock (elided in AOT when no concurrency reachable) | Lock-protected, value semantics, no borrows out |
| `cell` | Single-threaded mutable interior (e.g. accumulator within one task) | Zero (plain field access) | No cross-task safety — task-local only |
| `chan` | Message passing between tasks | Queue + condvar per channel | Send/recv with blocking, no shared state |

- Use `state` when multiple tasks need to read/write the same module-level variable (counters, configs, registries).
- Use `cell` when you need mutation within a single task and no cross-task access.
- Use `chan` when you want to pass values between tasks without shared memory (producer/consumer patterns).
- Never store closures/function values in `state` (Z0012) — they capture by reference through shared heap cells and would smuggle unsynchronized state across tasks.
