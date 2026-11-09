# Checker Hardening — Decisions

## Decision: closure capture semantics = capture by reference (live variable)

**Decision:** Closures capture variables **by reference** — the closure shares the captured variable with its enclosing scope and reads/writes its current value at call time. Mutations to a captured variable are visible both inside the closure and in the enclosing scope (and vice versa).

**Status:** Decided & implemented in the VM (tree-walker) engine. Remaining: confirm the AOT/native engine matches; document; add dual-engine parity fixtures.

## Evidence (VM behavior, verified by direct execution)

| Scenario | VM result | Interpretation |
|----------|-----------|----------------|
| `x := 1; f := \|y\| y + x; x = 99; println(f(1))` → `100` | closure reads mutated `x` | live reference read |
| `x := 1; f := \|y\| { x = x + y; x }; f(10); println(x)` → `11`, `11` | closure writes outer `x`, visible outside | live reference write, bidirectional |

Source tests confirming this model: `vm_module_scope_loop_capture` ("Per-iteration rebinding: the captured value tracks the current iteration, not a stale first binding") and the block-local capture test in `crates/zz_runtime/src/vm/tests.rs` ("settling the slot-homed closure must still leave the outer binding intact").

`snapshot_env` (`crates/zz_runtime/src/value.rs`) deep-clones the captured env, but only for cross-thread `spawn`/worker paths; same-thread closure calls share the `Rc<RefCell<Env>>` environment, i.e. reference semantics.

## Alternatives considered

1. **Capture by copy (snapshot)** — deep-copy captured values at closure-creation time; closure sees a frozen snapshot, immune to later mutation. Simple to reason about and trivial to make native match (native can copy values into the closure struct). **Rejected** — contradicts the observed VM behavior (prints 100/11, not 2) and would be a silent behavior change for existing programs. Would also break the "per-iteration rebinding tracks current iteration" loop-capture pattern that existing tests and programs rely on.

2. **Capture by reference (chosen)** — closure shares the variable. Matches VM. Sound because the checker already tracks captured variables via `capture_set` / `param_escape` and excludes them from slot fast paths / move optimizations (see `crates/zz_lower/src/param_escape.rs`, `green.rs`). Native backend must materialize captured variables in a shared, addressable cell (env slot) so both engines see identical reads/writes.

## What must agree

All four must implement capture-by-reference identically:
- **Checker** (`zz_checker` / `zz_type_check`): infer closure param/return types, type captured-variable uses, report errors; mark captured bindings so lowering keeps them addressable.
- **ZVM** (`zz_runtime` tree-walker): already reference — closure reads/writes the shared env cell.
- **Native/AOT** (`zz_lower` → C): captured vars must live in a shared heap/stack cell reachable by both the outer frame and the closure frame, not copied by value.
- **Docs** (`docs/language/variables-functions.md`): state capture-by-reference, with read/write-visibility example.

## Divergence policy

Any VM-vs-native divergence on capture (read or write visibility) is a **P0 bug**. Add dual-engine parity fixtures (Phase 1) to guard it:
- closure mutating captured var
- closure outliving its scope
- closures in loops
- nested closures
- closures capturing struct/array values

## Related decision (deferred)

"Eliminate the double check in `zz run`" (loader already checks each module once via `per_module_results`; `main.rs` then merges + calls `build_program`→`check_program_typed` again). Identified as a Phase 1 performance candidate but **not implemented** in this session: replacing the merge+recheck requires threading `per_module_results` through codegen while preserving decorator expansion (`expand_program` runs in `build_program`, not in the loader's per-module check). Risk of regressing the run path. Tracked in `docs/checker/decisions.md` as a known candidate, not a completed fix.
