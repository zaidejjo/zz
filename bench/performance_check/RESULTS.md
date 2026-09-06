# Comprehensive Multi-Scenario Benchmark — ZZ vs Go vs Rust

**Machine:** `x86_64` · **Date:** 2026-09-06T23:01:14Z · **Best-of:** 3 runs

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
| `bench_cpu_intensive   ` |      12 |      16 |  74 |  12 |
| `bench_concurrency_stress` |       0 |       2 | 808 | 183 |
| `bench_memory_alloc    ` |     403 |     377 | 279 | 267 |
| `bench_memory_leak     ` |      65 |      59 |  90 | 227 |
| `bench_string_concats  ` |       2 |       4 | 408 |   1 |

## HTTP Throughput (higher is better, req/sec)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|----------:|----------:|---:|-----:|
| `bench_http_throughput ` |  50,446 |  48,066 | 41,779 | 45,834 |

## Peak RSS — Non-HTTP benchmarks (lower is better; KiB / MB)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|-----------|-----------|---:|-----:|
| `bench_cpu_intensive   ` | 924 / 0.9 | 924 / 0.9 | 6752 / 6.6 | 2404 / 2.3 |
| `bench_concurrency_stress` | 0 / 0.0 | 0 / 0.0 | 9708 / 9.5 | 26540 / 25.9 |
| `bench_memory_alloc    ` | 131004 / 127.9 | 145960 / 142.5 | 12036 / 11.8 | 2480 / 2.4 |
| `bench_memory_leak     ` | 912 / 0.9 | 912 / 0.9 | 8132 / 7.9 | 2440 / 2.4 |
| `bench_string_concats  ` | 0 / 0.0 | 0 / 0.0 | 13960 / 13.6 | 0 / 0.0 |

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

