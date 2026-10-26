# Performance deep dive — budgets, PGO, profiling, memory

Companion to [Performance](../tooling/perf.md). This page gives commands and numbers;
that page gives the map.

## Budgets with proof

```bash
zz check --stats src/main.zz
# stats: src/main.zz: 1 modules (0 cached, 1 checked), seed 878 funcs, 38.1ms
# run twice: second shows cached instead of checked

zz test --slow-threshold 200
zz bench --json > bench.json
```

- Check: cold p50 ≤0.5s on 10k lines, warm ≤50ms/file.
- Test: 60s hard budget per test (`--timeout`, `--soft-timeout` legacy).
- Bench: serial, `warmup` (default 100) untimed + `iters` (default 1000) timed.

## PGO flows

```bash
zz build -p --full main.zz -- fast representative workload
zz build --pgo main.zz
zz profile main.zz -- args
zz build -p --verbose main.zz   # exact clang line
```

`llvm-profdata` merge runs under the hood. `--pgo --target <foreign>` rejected
(native host only). `--static` rejected on macOS. Cross uses safe baseline CPU.

## Profiling recipes

```bash
# CPU on the built binary
perf record -g ./main && perf report
# or Instruments (macOS), or Workers flow via workers-profiling skill

# Keep the measured code alive in AOT
# test.black_box(v) — optimizer cannot see through it
```

Bench harness shares one interp/process across iterations — reset mutable state
inside the bench fn. Benches take no params, not generic, no `@test` mixing.

## Memory: what actually allocates

| Path | Cost | Notes |
|---|---|---|
| Clone live var | O(1) refcount bump | CoW gate on write |
| Unique write | in place | no copy |
| Shared write | one copy | then in place |
| `bytes` slice | zero-copy share | immutable, `b[i] = x` errors |
| `str.builder` / `bytes.builder` | amortized push + one join | use for loops, not `s = s + p` |
| Spawn | ~µs + deep copy of captures | snapshot isolation |
| Channel send (uncontended) | ~25ns + mutex pair | unbounded via ring + spill |
| `s.f = pure`, `len(s.f)` | zero clones | take-restore window |
| AOT readonly param | borrowed, no retain | proven by `param_escape.rs` fixpoint |
| Recursive wrap build | O(n²) depth-n | prefer iteration for large inputs |

History: `perf/baseline.md`, `perf/append-elision-report.md`,
`bench/`, `benchmarks/`, `performace_check/arena/bench_memory_arena.zz`.
Design: `design/vm-cow.md`.

## Checklist before claiming fast

1. `zz bench` before/after with `--json` diff, not wall-clock eyeballing.
2. `black_box` on inputs/outputs so AOT does not delete the bench.
3. Same flags both sides (`-p` vs dev matters more than the code).
4. `check --stats` for checker regressions (seed size + wall time).
5. Commit `*.snap`/`bench.json` deltas with the code, not after.
