# Pre-M1 matrix report — quad legs, normalizers, VM fix, modules, benches

Branch: `feat/pre-m1-matrix` (M0 merged to `dev` as `77ebe71`; this
branch = dev + `fix/vm-chained-index-store` + the work below).
Separate PR branch: `fix/vm-chained-index-store` (`d16b1bb`).

## 1. Quad matrix (item 1)

`quad_regression_and_edge_matrix` in `dual_engine_parity.rs` runs every
`regression/` fixture + every `errors/edge_*` probe through four legs:
VM-debug (test binary), VM-release (`target/release/zz`, auto-built
once via a lock-serialized `cargo build --release -p zz_cli`,
overridable with `ZZ_VM_RELEASE_BIN`), native-O0 (`ZZ_NATIVE_DEV=1`),
native-O3 (default flags). `ZZ_PARITY_VM_ONLY=1` / `ZZ_SKIP_NATIVE=1`
degrade to VM legs. Default expectation is all-agree / all-fail;
`assert_quad_split` documents the decided splits (each arm names the M1
decision and panics FIXED on unexpected agreement).

New fixtures: `edge_cast_float_nan` (int(NaN)), `edge_slice_clamp`
(`a[1:10]` → `[2, 3]`, strict everywhere).

Quad findings beyond M0:

- `2 ** -1`: **both** VM legs trap (explicit check, not profile-gated).
  Only natives diverge. M1 decision (trap) is therefore already
  profile-independent on the VM side.
- `int(NaN)` under -O3 is **TU-unstable**: the same probe printed MIN
  in one TU and 0 in another (`-ffast-math` folds `f != f`, leaving
  `(int64_t)f` UB). Quad pins exits only; dual scope skips the fixture
  as nondeterministic (with reason); e2e keeps the VM leg.
- `scalar_global_copy` natives fail at **C compile time** (exit 1), not
  just output-diff — the split arm mirrors the known-failure macro
  semantics (diverged = exit≠0 OR stdout differs).

## 2. Normalizers, negatives, benches (item 2)

- Audit tests: `norm_keeps_value_differences` (m00=1/99, NaN/nan,
  exit codes, stderr), `norm_still_normalizes` (scratch paths, ports,
  numeric lines), `signal_gate_catches_numeric_only`,
  `known_lists_match_fixture_tags` (harness lists ↔ fixture tags,
  uses `zz_cli::fixture_meta`).
- Parity-signal gate: `assert_parity_strict` and the sweep reject
  numeric-only stdout (`PARITY BLIND`) instead of passing vacuously.
  This immediately caught 8 `modules/` fixtures printing bare numbers;
  they now carry trailing `_ok` markers (inside `main()`).
- Bench baselines for the M5 5% rule: `bench/ir_gate/` (fib35, tak,
  sieve, arraysum, strconcat) + `bench/ir_baseline.md` with method,
  table (64/10/31/37/4ms on i3-4005U), and caveats (foldable loops
  excluded on purpose; re-baseline per machine).

## 3. Chained stores = VM bug (item 3)

`compile_write_back` (bytecode compiler) and `write_back`
(tree-walker) dropped nested `Index` targets, so `m[0][0] = v` mutated
a temp clone. Both now store back and recurse to the root (same
double-evaluation contract as the `Field` arm). Native write-through
confirmed correct. `edge_chained_store` is strict.
Bonus find: field-of-index stores (`s[0].v = x`) now write through on
the VM too, while native still drops them — recorded as a native-side
bug of the same class (M1 §11).
Probes that the fix also closed: compound chained `m[0][1] += 10`
(VM printed the stale value).

## 4. Modules automation (item 4)

Parity sweep covers `modules/` (10 strict macros + sweep); broken
`import_alias.zz` (missing sibling, fails on VM too) skips with reason
for M6 triage. e2e discover sweeps intentionally unchanged (`zz run`
only covers syntax/types/stdlib there; modules have explicit macros).

## 5. Into M1

`docs/ir-spec-draft.md` is the starting point with the locked
decisions: wrap `+ - *`; trap `MIN/-1`, `MIN%-1`, neg `**`, OOB;
write-through stores; canonical `inf`/`-inf`/`NaN` with Rust-core-owned
float formatting; canonical `{kind, message, span, exit 1}` traps;
bool-only jumps; value-model-first ordering; HIR analyses as
droppable annotations.
