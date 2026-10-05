# M0 parity report — differential harness + edge corpus (current pipeline)

Branch: `feat/m0-parity-harness`. Date: 2026-10-05. Toolchain:
`zz 0.1.6` (debug build → `debug_assertions` ON), `clang` dev/native legs.

## What M0 delivered

1. **Feature tags on all 277 fixtures** (`// features:` header, closed
   vocabulary in `tests/fixtures/FEATURES.md`, enforced by
   `zz_cli::fixture_meta` unit tests). Tagging is heuristic
   (`scripts/tag_fixtures.py`) + sampled review; milestone gating only
   needs union-level accuracy, and gate failures are loud (easy fix).
2. **Milestone subsets are now data**: `M2_FEATURES` in `fixture_meta.rs`
   yields **8 M2 candidates** (`console`, `const`, `declarations`,
   `elif_chain`, `main_entrypoint`, `string_blocks`, `generic_bounds`,
   `branch_call_tails`). Pure-M2 is 0 without string literals; the spec
   decision recorded in FEATURES.md is that `print("marker")` needs only
   a const-pool entry, so `string-literal` is in M2 while `string-ops` /
   `fstrings` / `string-blocks` lower in M6.
3. **Edge corpus**: 13 new fixtures (`regression/edge_*`,
   `errors/edge_*`), registered in `e2e.rs` and `dual_engine_parity.rs`.
4. **Sweep extended**: `parity_discover_all_fixtures` now covers
   `syntax + types + stdlib + regression` (was missing `regression`).
   `modules/` stays manual (import-layout fixtures, one broken — §4);
   `test/` stays on the `zz test` runner.

## Divergences found (all reproduced on the current pipeline)

Strict-agreement probes (registered as strict parity):

| Probe | VM | Native | Notes |
|---|---|---|---|
| `edge_shift_mask` (`<<`/`>>` ≥ width) | masks `& 63` | masks `& 63` | agree |
| `edge_cast_float_int` (INF sat, trunc) | MAX/MIN/1/-1 | same | agree at -O0 **and** -O3 |
| `edge_cast_str_int` (overflow/invalid/ws) | none/none/-42/0 | same | agree |
| `edge_neg_shift_err`, `edge_rem_zero_err` | exit 1 | exit 1 | messages differ (ok for error parity) |
| slice clamp `a[1:10]` | `[2, 3]` | `[2, 3]` | agree (probed, no fixture needed) |

New known divergences (registered in `known_native_failure` + fixtures):

| Probe | VM (debug) | Native | Class |
|---|---|---|---|
| `edge_int_overflow_add/mul` | trap, exit 1 | wraps, exit 0 | overflow semantics |
| `edge_int_neg_min` | trap, exit 1 | wraps MIN, exit 0 | overflow semantics |
| `edge_int_min_div_neg1` | trap, exit 1 | prints **0**, exit 0 | C signed-overflow UB |
| `edge_int_pow_neg` (`2 ** -1`) | error, exit 1 | prints **0**, exit 0 | missing native error path |
| `edge_index_oob` (`a[10]`) | error, exit 1 | prints empty, exit 0 | missing bounds check |
| `edge_chained_store` (`m[0][0] = 99`) | reads back `1` | reads back `99` | value-model (write dropped vs write-through) |
| `edge_float_nan_display` | `NaN` | `nan` | float formatting |
| `int(NaN)` under **-O3** | `0` | `MIN` (-O0 native: `0`) | `-ffast-math`/UB folds the NaN guard — release-only divergence, no fixture (covered by nan-display entry) |

Pre-existing divergences re-confirmed as still present (no change):
`struct_impl`, `struct_embedding`, `local_wildcard`,
`scalar_global_copy`, `move_append_struct_copy`, `concurrency_panic_test`,
`encoding_test`, `math_extended_test`, `decorators`,
`extension_methods`, selective/wildcard/alias imports,
`main_result_err`, `pg_connect_refused`, P3 HTTP `vm-only` set.

## Profile caveat (M1 must resolve)

The VM itself disagrees with itself: debug builds trap on overflow,
release builds wrap (`cfg(debug_assertions)` in `runtime/ops.rs` and
`vm/runtime.rs`). The parity harness runs the debug VM, so every
overflow probe above is simultaneously a VM-debug-vs-VM-release
divergence. The M1 spec must pick ONE semantics for all profiles;
until then the overflow fixtures pin the debug behavior as errors.

## Manual sweeps (not in automation yet)

- `modules/`: 10/11 match byte-for-byte VM vs native (exit 0 + stdout).
  `import_alias.zz` fails on **both** (missing `math/utils.zz` — broken
  fixture or needs project context; triage in M6 modules milestone).
- `test/`: `zz test` runner, out of scope for run-parity (has
  `e2e_test_success!` coverage).

## Gate status

- `fixture_meta` unit tests (incl. full-tree tag walk): pass.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test -p zz_cli --test e2e`: 286 passed (incl. 13 new edge tests).
- `cargo test -p zz_cli --test dual_engine_parity` (full, both legs):
  **154 passed, 0 failed** — incl. the extended `regression/` sweep and
  all 13 new edge fixtures (5 strict agree, 8 diverge as documented).
- **Stop for review here** (per plan): M1 spec draft next — value model
  first (copy/move, chained stores, aliasing), then per-op semantics
  using this report as the decision list.
