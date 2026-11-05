# Checker Hardening — Baseline (Phase 0)

Branch: `checker-hardening` (off `dev`).  
Date: 2026-11-06.  
Working tree: clean at creation (`git status --porcelain` empty).

## Build

`cargo build --workspace` → **pass** (dev profile, ~112s). All 39 crates compile, zero errors.

## Test suite

The full `cargo test --workspace` was **not executed to completion** in this session (it runs >2 min with native legs + plugins and was scoped out to keep this session focused). Baseline test counts are therefore **not yet recorded** — see the table below (to be filled once suites are run).

| Suite | Command | Pass | Fail | Skip | Status |
|-------|---------|------|------|------|--------|
| workspace unit | `cargo test --workspace` | — | — | — | not-run |
| e2e discovery | `cargo test -p zz_cli --test e2e -- e2e_discover` | 2 | 0 | 0 | green |
| dual-engine parity (closure subset) | `cargo test -p zz_cli --test dual_engine_parity -- closure` | 7 | 0 | 0 | green |
| dual-engine parity (full) | `cargo test -p zz_cli --test dual_engine_parity` | — | — | — | not-run (interrupted; closure subset green) |
| batched parity | `cargo test -p zz_cli --test batched_parity` | 2 | 0 | 0 | green |
| check regression | `cargo test -p zz_cli --test performance_check_regression` | 4 | 0 | 0 | green |

> Update this table when the full dual_engine_parity + workspace suites complete. Do not mark Phase 0 complete until every suite is green (or every failure is explained).

## Benchmark results (Phase 0, scripts/bench_check.sh)

Dev-profile cold `zz check` on generated programs (many funcs/structs/long match chains/big arrays/imports). Full table: `docs/checker/bench_results.md`.

| Lines | Approx funcs | `zz check` time (s) | Peak RSS (KB) |
|-------|--------------|---------------------|---------------|
| 1250 | ~69 | 0.0078 | 34652 |
| 3076 | ~170 | 0.0091 | 39440 |
| 6134 | ~340 | 0.0176 | 49084 |
| 12250 | ~680 | 0.0411 | 63644 |
| 24482 | ~1360 | 0.0931 | 94936 |

Note: the task's "20,005 lines / 10,005 funcs = 260s" figure refers to a much larger real program; this synthetic generator yields ~1.4k funcs at 24k lines. The script is committed for CI regression tracking (target: 20k lines < 5s).

## Known pre-existing context (no changes made yet)

- `zz_stdlib` has ~460 natives (count assertion in `natives/mod.rs`).
- Checker entry points: `zz_checker::check_program`, `check_program_typed`, `check_program_with_module_ns`, plus `zz_hir::build_program` (which calls `check_program_typed` again).
- The loader (`zz_loader`) already checks each module once and caches `per_module_results` (AST + `CheckResult` + span types); `zz_cli/src/main.rs` then merges all modules and calls `build_program` → `check_program_typed` **again** on the merged program (see `docs/checker/decisions.md` double-check finding).
- Closure capture: verified by execution — closures share captured variables by reference (read/write visible across scope); see `docs/checker/decisions.md` and `docs/language/variables-functions.md`.

## Next steps (Phase 0 → Phase 1)

1. Record real suite counts in this file.
2. Commit `scripts/bench_check.sh` + a first results table.
3. Record closure-capture decision in `docs/checker/decisions.md` and document it in `docs/language/variables-functions.md`.
4. Add dual-engine parity fixtures for closure capture cases (mutating captured var, closure outliving scope, loops, nested, struct/array captures) and confirm VM == native.
