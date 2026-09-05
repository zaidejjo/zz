# Performance / Stress / Memory Benchmark — ZZ vs Go vs Rust

**Machine:** `x86_64` · **Date:** 2026-09-05T17:03:10Z · **Best-of:** 3 runs

## Workloads

| Benchmark              | Workload                                                                  |
|------------------------|---------------------------------------------------------------------------|
| `bench_memory_leak`   | 50M short-lived array/dict allocs in deeply nested loops, two passes        |
| `bench_cpu_intensive` | 10M integer accum + 1M pow/mod + 100k array fill/sum                       |
| `bench_string_concats`| 50×5000 single-char concat + 20×2000 multi-char concat + 10k final concat   |

## Execution time (lower is better)

| Benchmark              |    ZZ     | Rust    | Go      |
|------------------------|----------:|--------:|--------:|
| `bench_memory_leak     ` | 55 ms | 160 ms | 89 ms |
| `bench_cpu_intensive   ` | 163 ms | 13 ms | 57 ms |
| `bench_string_concats  ` | 5 ms | 3 ms | 367 ms |

## Peak RSS (lower is better; KiB / MB)

| Benchmark              |    ZZ     | Rust    | Go      |
|------------------------|----------:|--------:|--------:|
| `bench_memory_leak     ` | 780 / 0.76MB | 2444 / 2.39MB | 5828 / 5.69MB |
| `bench_cpu_intensive   ` | 804 / 0.79MB | 2800 / 2.73MB | 4012 / 3.92MB |
| `bench_string_concats  ` | 0 / 0.00MB | 0 / 0.00MB | 9708 / 9.48MB |

## Binary size (KiB)

| Engine | Binary KiB |
|--------|-----------:|
| ZZ     | 759 |
| Go     | 2393 |
| Rust   | 468 |

## How to reproduce

```bash
cargo build --release
RUNS=5 bash bench/performance_check/run.sh
```

- ZZ column uses `zz build -p <file>` (AOT native, `-O3`).
- Go/Rust compiled once into `bench/performance_check/.bin/`.
- Peak RSS via `/usr/bin/time -v` (GNU) or `ps` polling fallback.
- Machine-readable: `bench/performance_check/results.json`.

