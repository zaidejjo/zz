# Runtime and codegen — tree-walker, VM, AOT

## Runtimes

| Engine | Command | Notes |
|---|---|---|
| Tree-walker | `zz run` internals | `Env::get` deep-copies; mirrors take-restore behind `run_tree_walker` |
| Bytecode VM | `zz run` | `LoadVar`/`LoadSlot` copy; `TakeVar`/`TakeSlot` move self-reassignments; loop safepoints folded into `ForNext`/`WhileCond` |
| AOT | `zz build`, `zz run --native` | HIR→C (`lower/`) + chunk→C (`chunk.rs`), Clang driver, PGO, cross |

`zz run` leaves no artifacts. `zz run --native` publishes to the same destination
as `zz build` and execs a private staged copy.

## Memory: CoW values

Cloning bumps a refcount O(1); mutation goes through a CoW gate (unique mutates
in place, shared copies once). Programs cannot observe sharing: equality stays
deep, mutation never leaks. `clone(v)` pins an independent deep copy.

- Self-reassignment threading: `doc = f(doc, ...)` moves the single-mentioned
  slot local in (`Unit` sits in the slot during eval, unobservable).
  Only plain uncaptured slot locals outside top-level `main`.
- Take-restore field windows: `s.f = pure`, `s.f[i] = pure`, `s.f = s.f OP pure`,
  `s.f[i]` reads (one element), `len(s.f)` (zero clones). Gated on purity
  (no closures/spawns/early-exits, no home refs; calls eval pre-take).
- AOT readonly borrows (`param_escape.rs` fixpoint): proven read-only params pass
  borrowed; anything unproven clones.

Differential runs over fixtures must read identically with all opts on/off.

## Codegen

- HIR→C + chunk→C, `-O3 -flto=thin` release, stripped, DCE via callgraph.
- `-fwrapv`, `-fno-strict-aliasing`, no `-ffast-math`, no `-march=native`;
  cross uses safe baseline CPU + `-fuse-ld=lld`.
- Conditional syslibs: no-fetch skips libcurl, no-query skips libsqlite3.
- Precompiled runtime archives + per-module objects cached in `~/.zz/cache`.
- Asset edits invalidate by content hash; `app.c` manual builds include same tables.
- C twins must be byte-identical to VM natives; `zz_regexp_*`, `zz_crypto_*`,
  `zz_process_*` share impls via `zz_native_rt`.

### Progress seam

The pipeline reports what it is doing instead of letting callers infer it:
`zz_codegen::CodegenSink` (generate → per-unit compile → link) and
`zz_loader::LoadSink` (one call per checked module) feed
`zz_build::BuildObserver`, which the CLI renders as five steps. The
user-facing labels — `Checking`, `Optimizing`, `Compiling`, `Linking`,
`Publishing` — are deliberately backend-neutral: the HIR and chunk backends
map onto the same five, so internals can change without touching the UI.
Everything is optional; callers that leave the default no-op observer
(`zz run --native`, `zz test`, benches) keep byte-identical output.

## Concurrency mapping

Green tasks snapshot captures + transitive fns; channels unbounded
(lock-free ring + spill + mutex-paired waiter service). AOT uses `zz_value_dup`,
thread-local arenas, thread-safe refcounts/intern table. Pure-CPU loops yield at
safepoints (folded into `ForNext`/`WhileCond` on the VM, `zz_safepoint()` in AOT).

Specs: [IR draft](../ir-spec-draft.md), [threading](../threading.md),
[concurrency audit](../concurrency_audit.md), [VM CoW](../design/vm-cow.md).
