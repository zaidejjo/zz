# Comprehensive Multi-Scenario Benchmark — ZZ vs Go vs Rust

**Machine:** `x86_64` · **Date:** 2026-09-06T22:13:05Z · **Best-of:** 3 runs

## Benchmarks

| Benchmark | Description |
|-----------|-------------|
| `bench_cpu_intensive` | 10M integer accum + 1M pow/mod + array ops |
| `bench_concurrency_stress` | 1000 workers × 1000 msgs (1M total) |
| `bench_http_throughput` | HTTP server with concurrent connections |
| `bench_memory_alloc` | Mass allocation/destruction (GC/Arena stress) |
| `bench_memory_leak` | 50M short-lived allocs across 2 passes |
| `bench_string_concats` | String building patterns (50×5k + 20×2k) |

## Execution time — Non-HTTP benchmarks (lower is better, ms)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|----------:|----------:|---:|-----:|
| `bench_cpu_intensive   ` |      13 |      14 |  66 |  14 |
| `bench_concurrency_stress` |       0 |       1 | 847 | 185 |
| `bench_memory_alloc    ` |     393 |     397 | 273 | 273 |
| `bench_memory_leak     ` |      66 |      67 |  89 | 227 |
| `bench_string_concats  ` |       2 |       3 | 473 |   1 |

## HTTP Throughput (higher is better, req/sec)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|----------:|----------:|---:|-----:|
| `bench_http_throughput ` |  11,501 |  10,444 | 11,851 | 13,270 |

## Peak RSS — Non-HTTP benchmarks (lower is better; KiB / MB)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|-----------|-----------|---:|-----:|
| `bench_cpu_intensive   ` | 2568 / 2.5 | 924 / 0.9 | 6668 / 6.5 | 4184 / 4.1 |
| `bench_concurrency_stress` | 0 / 0.0 | 0 / 0.0 | 10432 / 10.2 | 25732 / 25.1 |
| `bench_memory_alloc    ` | 131396 / 128.3 | 126584 / 123.6 | 14704 / 14.4 | 2532 / 2.5 |
| `bench_memory_leak     ` | 912 / 0.9 | 916 / 0.9 | 10200 / 10.0 | 2464 / 2.4 |
| `bench_string_concats  ` | 0 / 0.0 | 1480 / 1.4 | 13532 / 13.2 | 0 / 0.0 |

## Binary size (KiB)

| Engine | Binary KiB |
|--------|-----------:|
| ZZ AOT | 883 |
| Go | 8657 |
| Rust | 578 |

## How to reproduce

```bash
cargo build --release
RUNS=5 bash bench/performance_check/run.sh
```

- **ZZ AOT MT**: Multi-threaded execution (all CPU cores)
- **ZZ AOT ST**: Single-threaded execution (1 CPU core, via `taskset -c 0`)
- **Go/Rust**: compiled once into `bench/performance_check/.bin/`
- Peak RSS via `/usr/bin/time -v` (GNU) or `ps` polling fallback
- HTTP benchmark uses `wrk` or `hey` for load testing (if available)

