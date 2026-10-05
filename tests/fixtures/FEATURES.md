# Fixture feature tags (M0)

Every `.zz` fixture under `tests/fixtures/` carries a machine-readable
header on its first line(s):

```zz
// features: int-arith, func-call, print
```

A fixture may span the header over several `// features:` lines inside the
leading comment block; the parser unions them. Tags come from the closed
vocabulary below — enforced by `zz_cli::fixture_meta` unit tests, so a
typo fails `cargo test -p zz_cli --lib`.

Purpose (per M0 change 1): each IR milestone gates on a **defined fixture
subset** computed from these tags, not on an abstract "language subset"
prose. `fixture_meta::M2_FEATURES` (and later `M4_`, `M5_`, `M6_*`) are the
executable form of the milestone scopes.

## Vocabulary

Values and operators:
`int-arith`, `float-arith`, `bool-logic`, `bitwise`, `comparison`, `casts`

Bindings:
`locals`, `const`, `destructuring`

Control flow and functions:
`if-else`, `while-loop`, `for-loop`, `break-continue`, `func-call`,
`recursion`, `closures`, `hof`, `match`, `if-let`, `try-question`, `defer`,
`short-circuit`, `elvis`, `pipe`, `return`, `assert`, `decorators`

Data:
`string-literal` (any `"..."` literal — M2 includes these as const-pool
entries for `print`), `string-ops` (concat / interpolation machinery),
`fstrings`, `string-blocks`, `arrays`, `vec-push`, `dicts`,
`tuples`, `ranges`, `indexing`, `slicing`, `bytes`

Types:
`structs`, `struct-methods`, `methods`, `generics`, `aliases`, `enums`,
`option-result`, `type-inference`

Modules:
`imports`, `selective-import`, `wildcard-import`, `import-alias`,
`pub-visibility`

Standard library (one per `std.*` module; `import std.math` implies both
`imports` and `std-math`):
`std-args`, `std-bytes`, `std-chan`, `std-colors`, `std-crypto`, `std-csv`,
`std-db`, `std-dec`, `std-encoding`, `std-env`, `std-fs`, `std-http`,
`std-json`, `std-log`, `std-map`, `std-math`, `std-net`, `std-path`,
`std-process`, `std-regexp`, `std-sqlz`, `std-str`, `std-sys`, `std-task`,
`std-term`, `std-time`, `std-uuid`, `std-vec`

Runner and verdict:
`print`, `stdin`, `cli-args`, `embed`, `vfs`, `zz-test`,
`error-case`, `nondeterministic`, `vm-only`, `known-divergence`

Tagging rules:

- `error-case` on every fixture under `errors/` (they must fail on the VM).
- `zz-test` on every fixture under `test/` (run via `zz test`, not `zz run`).
- `nondeterministic` when output varies run to run (timestamps, ports);
  the parity harness skips these via `native_skip_reason`.
- `known-divergence` when VM and native verifiably disagree (tracked in
  `known_native_failure` in `dual_engine_parity.rs` and the M0 report).
- `vm-only` when the fixture exercises natives with no AOT lowering
  (same tracking as `known-divergence`, kept separate so M5 can claim
  them by implementing the lowering instead of fixing a bug).
- Prefer content tags over intent: tag what the file *uses* (`vec-push`
  when it calls `vec.push`, `closures` when a `|x|` literal appears),
  not what it is "about". Milestone gating depends on it.

## Milestone subsets

- **M2** (front-end lowering: ints, floats, bools, locals, if/while,
  calls, print): exactly
  `M2_FEATURES = [int-arith, float-arith, bool-logic, comparison,
  locals, const, if-else, while-loop, func-call, print, string-literal]`.
  `string-literal` is in because marker printing needs only const-pool
  entries; `string-ops`/`fstrings`/`string-blocks` lower in M6.
  A fixture is an M2 candidate when all its tags are in this set.
- **M4 / M5** (VM / AOT consume IR): same set as M2; parity is
  VM-vs-old-VM (M4) then VM-vs-AOT (M5) on the M2 candidate fixtures.
- **M6** expands feature by feature; each step names its tags, e.g.
  `structs` + `struct-methods`, then `string-literal` + `string-ops` +
  `fstrings` + `std-str`,
  then `arrays` + `vec-push` + `indexing` + `slicing`, then `closures` +
  `hof`, then `dicts` + `tuples` + `ranges` + `for-loop`, then `match` +
  `option-result` + `try-question`, then `imports` + std modules one by
  one (M3 migrates the registries module by module alongside).
- **M7** removes the old paths once every non-`nondeterministic` fixture
  passes parity.

## Edge corpus (M0)

`regression/edge_*` probes pin semantics the IR spec (M1) must decide.
Probes where both engines agree today are strict parity; probes exposing
a divergence carry `known-divergence` (or land in `errors/` with a
`known_native_failure` entry when the VM fails and native exits 0):

- `edge_shift_mask`: `<<` / `>>` with counts >= width (both mask `& 63`).
- `edge_chained_store`: chained index stores write through (fixed VM bug).
- `edge_cast_float_nan`: int(NaN) — VM and -O0 print 0; -O3 output is
  clang-UB (0 or MIN by TU shape). Skipped in dual scope as
  nondeterministic; the quad matrix pins exits.
- `edge_slice_clamp`: out-of-range slice ends clamp (strict everywhere).
- `edge_cast_float_int`: float->int truncation and INF saturation.
- `edge_cast_str_int`: str->int overflow/invalid/whitespace (`none`).
- `edge_neg_shift_err`, `edge_rem_zero_err` (`errors/`): both engines
  fail (messages differ; error parity only requires failure).
- `edge_int_overflow_add`, `edge_int_overflow_mul`, `edge_int_neg_min`,
  `edge_int_min_div_neg1`, `edge_int_pow_neg` (`errors/` +
  known-failure): VM traps (debug) / native wraps or miscomputes.
- `edge_index_oob` (`errors/` + known-failure): VM errors exit 1,
  native prints empty and exits 0.
- `edge_float_nan_display` (`regression/` + known-failure): both exit 0
  with different stdout (`NaN` vs `nan`).
