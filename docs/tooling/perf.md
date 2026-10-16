# Performance — budgets, PGO, profiling

## Budgets

- `zz check` cold p50 ≤0.5s on 10k lines, warm ≤50ms/file (`--stats` to verify).
- `zz test` default 60s hard budget per test (`--timeout`, `--soft-timeout` legacy).
- `zz bench` serial, `warmup` untimed + `iters` timed; never shuffle/parallelize timings.

## PGO and optimization

```bash
zz build -p --full main.zz -- fast representative workload  # LTO + training run
zz build --pgo main.zz        # native host only
zz profile main.zz -- args    # profile flow (llvm-profdata merge under the hood)
```

All builds: `-O3 -flto=thin` (release), `-fwrapv`, `-fno-strict-aliasing`,
no `-ffast-math`, no `-march=native`. Cross uses safe baseline CPU.
`--verbose` prints the exact clang line; manual `app.c` + `build.sh`/`build.bat`
ship next to every binary.

## Measuring

```bash
zz bench
zz bench --engine native --json > bench.json
zz test --slow-threshold 200
```

- `test.black_box(v)` returns its arg and blocks AOT dead-code elimination —
  wrap measured inputs/outputs.
- Callgraph + DCE run before codegen; `perf/` + `bench/` + `benchmarks/` hold
  history (`perf/baseline.md`, `perf/append-elision-report.md`).
- Profile CPU with `workers-profiling` skill flow (deployed Workers/DOs) or
  local `perf`/` Instruments` on the built binary; memory via arena benches
  (`performace_check/arena/bench_memory_arena.zz`).

## Fast paths to know

- CoW keeps clones O(1) until write; `bytes` slices share zero-copy.
- `take-restore` windows make `s.f = pure`, `s.f[i] = pure`, `len(s.f)` clone-free.
- AOT readonly-param borrows skip retains for proven read-only params.
- Builders (`str.builder`/`bytes.builder`) turn O(n²) loops into one join.
- `fs.scan_counts` fuses read+NUL-sniff+classify; `str.find*` are O(1) byte ops.

History: [perf/baseline.md](../perf/baseline.md),
[perf/append-elision-report.md](../perf/append-elision-report.md).
