# Checker Hardening — Baseline (Phase 0)

Branch: `checker-hardening` (off `dev`).  
Date: 2026-11-06.  
Working tree: clean at creation (`git status --porcelain` empty).

## Build

`cargo build --workspace` → **pass** (dev profile, ~112s). All 39 crates compile, zero errors.  
`cargo build --release -p zz_cli` → **pass** (~8m13s). Release binary at `target/release/zz`.

## Test suite

The full `cargo test --workspace` was **not executed to completion** (runs >2 min with native legs + plugins). Baseline counts below reflect the suites actually run; mark each green as confirmed.

| Suite | Command | Pass | Fail | Skip | Status |
|-------|---------|------|------|------|--------|
| workspace unit | `cargo test --workspace` | — | — | — | pending |
| e2e discovery | `cargo test -p zz_cli --test e2e -- e2e_discover` | 2 | 0 | 0 | green |
| e2e full | `cargo test -p zz_cli --test e2e` | 390 | 0 | 0 | green |
| dual-engine parity (closure subset) | `cargo test -p zz_cli --test dual_engine_parity -- closure` | 7 | 0 | 0 | green |
| dual-engine parity (full) | `cargo test -p zz_cli --test dual_engine_parity` | — | — | — | pending |
| batched parity | `cargo test -p zz_cli --test batched_parity` | 2 | 0 | 0 | green |
| check regression | `cargo test -p zz_cli --test performance_check_regression` | 4 | 0 | 0 | green |

## Benchmark results (Phase 0, scripts/bench_check.sh)

Cold `zz check` on generated programs (many funcs/structs/long match chains/big arrays/imports). Full table: `docs/checker/bench_results.md`.

**Release profile** (`target/release/zz`):

| Lines | Approx funcs | `zz check` time (s) | Peak RSS (KB) |
|-------|--------------|---------------------|---------------|
| 1250 | ~69 | 0.0003 | 17144 |
| 3076 | ~170 | 0.0006 | 22016 |
| 6134 | ~340 | 0.0011 | 30536 |
| 12250 | ~680 | 0.0020 | 45652 |
| 24482 | ~1360 | 0.0041 | 78320 |

**Dev profile** (`target/debug/zz`):

| Lines | Approx funcs | `zz check` time (s) | Peak RSS (KB) |
|-------|--------------|---------------------|---------------|
| 1250 | ~69 | 0.0137 | 33860 |
| 3076 | ~170 | 0.0070 | 39356 |
| 6134 | ~340 | 0.0141 | 47800 |
| 12250 | ~680 | 0.0271 | 63824 |
| 24482 | ~1360 | 0.0410 | 95784 |

Linear scaling confirmed (~0.17 µs/line release, ~1.7 µs/line dev).  
Target: 20k lines < 5s in CI — **already met** (~0.004s release on 24.5k lines).

Note: the task's "20,005 lines / 10,005 funcs = 260s" figure refers to a much larger real program; this synthetic generator yields ~1.4k funcs at 24k lines (~18 lines/func). The script is committed for CI regression tracking.

## Known pre-existing context (no changes made yet)

- `zz_stdlib` has ~460 natives (count assertion in `natives/mod.rs`).
- Checker entry points: `zz_checker::check_program`, `check_program_typed`, `check_program_with_module_ns`, plus `zz_hir::build_program` (which calls `check_program_typed` again).
- The loader (`zz_loader`) already checks each module once and caches `per_module_results` (AST + `CheckResult` + span types); `zz_cli/src/main.rs` then merges all modules and calls `build_program` → `check_program_typed` **again** on the merged program (double-check candidate in `docs/checker/decisions.md`).
- `zz check` already sets `use_cache=true` (S1 cache: cold=miss+store, warm=hit).
- Closure capture: verified by execution — closures share captured variables by reference (read/write visible across scope); see `docs/checker/decisions.md` and `docs/language/variables-functions.md`.

## Next steps

1. Run full `cargo test --workspace` + full `dual_engine_parity`, record counts here (Phase 0 completion).
2. Phase 1: add perf regression test (20k lines < 5s) to `performance_check_regression.rs`; address double-check in `zz run` if a clean, low-risk refactor emerges.
