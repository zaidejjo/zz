# VM value-copy performance (COW + last-use move)

Status: design only. No implementation.

## Problem

Every VM read of a non-scalar clones deeply:

- tree-walker `Env::get` → `Some(v.clone())` (`crates/zz_runtime/src/env.rs`);
- `Stmt::Decl` → `define(name, v.clone())` **and** returns the original;
- bytecode VM `LoadVar` (`env.get`), `LoadSlot` (stack `.clone()`),
  `DefineVar` (clone-then-push);
- `StoreVar`/`StoreSlot`, call args, and returns move (no clone there).

`Value::Object`/`Array`/`Dict`/`Str` are `Box`-owned trees, so one
`doc` read copies all 16 arena arrays. The toml package appends one key
per `push_node` call and pays 3–5 full deep copies per key → the
~quadratic curve (2 KB ~1 s, 10 KB ~43 s on the VM).

The native engine does not have this problem: boxed structs travel as
`zz_value` handles (`zz_clone` = refcount bump, O(1)); per-step cost is
only the `vec_push` element-array dup.

## (1) COW for arrays / objects / strings

Wrap the backing stores, not `Value`:

- `Array(Box<Vec<Value>>)` → `Array(CowVec)` where
  `CowVec = Arc<Vec<Value>>` (same for `Dict` entries, `Object` fields,
  `Str` bytes).
- Clone becomes an `Arc` bump (O(1)). Every *mutation* path goes through
  `make_mut`: `set_object_field`, `set_index`/`zz_array_set`,
  `vec.push`/`append`/`insert`, dict `set`, string append/concat-into.
- `make_mut` on `Arc::get_mut` failure copies once (O(n)); the copy is
  exactly the copy the VM does today, so worst case equals status quo.

**Rc vs Arc.** The VM is single-threaded while interpreting, BUT `Value`
is `unsafe impl Send + Sync` and spawned tasks receive snapshotted/duped
values across threads (`snapshot_env_cow`, `zz_value_dup` at crossings).
Keeping `Send + Sync` is load-bearing for that story. The atomic bump
costs ~1 ns against deep clones costing ms: **use `Arc`**. (`Rc` would
force removing the `Send`/`Sync` impls and re-auditing every crossing —
rejected.)

Strings: `Str(Box<String>)` → `Arc<String>` (or `Arc<str>`); slicing
already shares via `Bytes` (`Arc` + window) and is the model to copy.
Concat produces a fresh buffer (no sharing to preserve).

## (2) Minimal last-use move for self-reassignment

Syntactic rule, no dataflow analysis. In the tree-walker and the
bytecode compiler, when lowering `x = <rhs>` / `x := <rhs>`:

> If the RHS mentions `x` exactly once, that occurrence is a *move*
> (no clone), AND `x` is a plain local (not a global, not captured by a
> closure in scope, not a path/index target), AND the single mention is
> in argument position of the outermost call or a direct operand —
> then evaluate the RHS by moving `x`'s slot instead of cloning it.

Covered shapes: `x = f(x, ...)`, `x = x + ...`, `x = vec.push(x, e)`,
`x = m(x)` (method call). The callee/operator receives owned `x`; the
final store overwrites the (moved-from) slot. Unit (`()`) left in the
slot during evaluation is unobservable: the statement completes before
any user code can read `x`.

MUST NOT apply when:

- `x` occurs 2+ times in the RHS (`x = x + x`, `x = f(x, x)`) — the
  second read needs the value;
- `x` is read inside a nested closure in the RHS (`x = apply(|v| x)`):
  the closure must capture a clone (capture outlives the call);
- `x` appears in a loop condition or is captured by a closure
  *elsewhere in scope* (conservative: captured names never move);
- target is `obj.field`, `arr[i]`, or a global (write-back paths re-read
  the container — move only the *value*, never the container);
- RHS can fail partway (`?`, bounds-checked index, div-by-zero): on
  error the function aborts with an error value, so a moved-from slot is
  fine — BUT the error *message* path must not print the slot. Rule:
  moves are allowed only if the RHS is total or the error path never
  formats locals (audit `EvalError` construction);
- `x` is `mut`-borrowed anywhere (no borrows exist today; revisit if
  `&mut` lands).

## (3) Trace: `doc = push_node(doc, x)`

`push_node` appends to ~5 of the 16 arrays and returns `doc`. Caller
shape (toml): `doc = push_node(doc, kind, ...)`.

Today (refcounts are deep clones, shown as copies):

1. eval arg `doc` — copy #1 (whole `Doc`, 16 arrays).
2. param bind — move.
3. each `doc.<arr> = vec.push(doc.<arr>, v)` — field read copies whole
   `Doc` again (copies #2..#6), `vec.push` dups that one array (O(n)),
   field write-back moves.
4. `return doc` — move. Caller `Decl`/assign — `Decl` clones (copy #7).

Per key: ~7 whole-`Doc` copies + 5 array dups → O(keys × nodes).

With (2) only (no COW):

1. eval arg `doc` — MOVE (single use in `doc = push_node(doc, ...)`).
2. param bind — move.
3. field reads still clone whole `Doc` (no rule fires inside — the rule
   is statement-level; `doc.t_tbl` read inside `vec.push(doc.t_tbl, …)`
   mentions `doc` once… the inner statement is an *expression*, not an
   assignment to `doc`, so no move; each field read still deep-copies).
4. return/assign — moves.

Per key: ~5 whole-`Doc` copies (field reads) + 5 array dups. **Better
constant, still O(keys × nodes).** `push` does NOT become in-place:
`vec.push` receives a *shared-or-not unknown* array and must dup.

With (1) COW + (2):

1. arg — `Arc` bump. 2. field reads — `Arc` bumps; element arrays shared.
3. `vec.push(shared_arr, v)` — `make_mut` copies that ONE array once
   (O(n)), all other 15 arrays stay shared. 4. return/assign — bumps.

Per key: one O(n) array copy + O(1) bumps → O(keys × avg_len) total with
a tiny constant — the same complexity as the native engine. `push`
becomes in-place exactly when the array is uniquely owned
(`Arc::get_mut` succeeds), which is the common accumulator case.

## (4) Semantic risks

- **Equality** (`==`, `!=`): deep comparison, sharing-invisible. Safe.
- **Aliasing through closures**: captured envs clone at capture today;
  with COW they would *share* until a write. Every write path already
  routes through `make_mut` (section 1 list) — audit must prove no
  direct `Vec`/`String` mutation exists outside it (`grep mut` on
  `Value` internals). Highest-risk site: `set_object_field` write-back
  in the tree-walker, which re-reads and rewrites whole objects.
- **Dicts of arrays / nested sharing**: `make_mut` is per-buffer, so a
  write to a nested array copies only that array. Diagrams that print
  shared structure (`{:?}`) must not expose addresses — they don't.
- **`str` interning**: interned literals are immortal singletons; they
  never participate in COW (excluded like the native `interned` flag).
- **Rollback hazard**: `Arc::get_mut` + interior retry logic must never
  be reachable from two threads for the same buffer — thread crossings
  keep today's deep `dup`, so no buffer is ever shared across threads
  while mutable. State this as an invariant with a `debug_assert!`.

## (5) Expected effect

- toml append curve (VM): from ~quadratic (43 s @ 10 KB) to ~linear
  with native-like constant (one array copy per key). Native path
  unchanged (already handle-passing); `vec_push` still copies the one
  array per push — that copy is *required* by value semantics unless the
  caller is proven unique owner (not knowable without (2)-style
  analysis in codegen; out of scope here).
- String-heavy loops: `out = out + ...` moves the accumulator (rule)
  and shares RHS temps (COW); one copy per stored value, same as the
  native heal.

## (6) Incremental plan (gates + rollback per step)

1. **Instrument** (0.5 d): counters for deep-clone sites
   (`env.get`, `DefineVar`, `LoadSlot`) behind `#[cfg(feature)]` or an
   env flag; gate: toml-10KB run prints the copy histogram. Rollback:
   delete the counters.
2. **`Arc<String>` for `Str`** (1 d): smallest blast radius, no nesting.
   Gate: `cargo test` + fuzz 200 + clone-counter delta on string bench.
   Rollback: revert the type alias.
3. **`Arc<Vec<Value>>` for arrays + `make_mut` on all write paths**
   (2–3 d): enumerate write paths by test (`grep -n "arr:"` mutations);
   gate: full e2e + parity + fuzz 1000, zero new DIFFs; clone counters
   show O(1) per read. Rollback: revert (single-commit).
4. **Dicts + objects** (2 d): same gate. Rollback: revert.
5. **Last-use move rule** (2 d): tree-walker first, then bytecode
   compiler; gate: targeted tests for every MUST-NOT case in section 2
   (each asserts old value preserved / error identical); fuzz 1000.
   Rollback: flag-gate the rule (`ZZ_NO_MOVE=1` env) before removing.
6. **toml curve re-measure** (0.5 d): accept if 10 KB < 2 s on VM.

Total ≈ 8–10 d. Each step is independently revertible; steps 2–4 are
near-mechanical, step 5 carries the semantic risk.

## `&mut` comparison (deferred)

`&mut` params would give true zero-copy hot loops with caller-visible
mutation, beating COW (which still copies once per `make_mut`).
Deferred because it is a language feature, not an optimization: new
syntax, borrow rules in the checker, codegen for both engines, aliasing
semantics to specify, and a migration story for existing by-value code.
Do COW + move first; revisit `&mut` only if measured hot loops still
miss targets after step 6.
