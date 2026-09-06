# Comprehensive Multi-Scenario Benchmark — ZZ vs Go vs Rust

**Machine:** `x86_64` · **Date:** 2026-09-06T21:01:11Z · **Best-of:** 1 runs

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
| `bench_cpu_intensive   ` |      26 |      36 | 156 |  19 |
| `bench_concurrency_stress` |       4 |       3 | 1009 | 213 |
| `bench_memory_alloc    ` |     417 |     438 | 275 | 275 |
| `bench_memory_leak     ` |      67 |      68 |  99 | 246 |
| `bench_string_concats  ` |       8 |       6 | 554 |   3 |

## HTTP Throughput (higher is better, req/sec)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|----------:|----------:|---:|-----:|
| `bench_http_throughput ` |   7,509 |   5,492 | 26,402 | 10,343 |

## Peak RSS — Non-HTTP benchmarks (lower is better; KiB / MB)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|-----------|-----------|---:|-----:|
| `bench_cpu_intensive   ` | 924 / 0.9 | 924 / 0.9 | 9548 / 9.3 | 3308 / 3.2 |
| `bench_concurrency_stress` | 0 / 0.0 | 0 / 0.0 | 9552 / 9.3 | 27244 / 26.6 |
| `bench_memory_alloc    ` | 136768 / 133.6 | 132824 / 129.7 | 12352 / 12.1 | 2484 / 2.4 |
| `bench_memory_leak     ` | 916 / 0.9 | 908 / 0.9 | 6664 / 6.5 | 2456 / 2.4 |
| `bench_string_concats  ` | 0 / 0.0 | 0 / 0.0 | 16316 / 15.9 | 0 / 0.0 |

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

