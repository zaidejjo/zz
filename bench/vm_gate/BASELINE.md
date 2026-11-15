# Math perf-gate baseline (PR 2)

Captured **before** deleting any `std.math` Rust/C body, as required by the
Phase 2 plan (§5). The comparison ran two copies of the same release binary,
so every delta below is measurement noise, not a change — it is the floor the
real PR 2 delta is read against.

| field | value |
|---|---|
| repo commit | `43877d1a` |
| captured | 2026-11-15 18:22 +03 |
| compiler binary | `cargo build --release -p zz_cli` |
| host | 4 cores, Intel i3-4005U @ 1.70 GHz |
| load at capture | 2.28 (VM) / 3.29 (AOT) 1-min on 4 cores |
| repetitions | best-of-7 per fixture, interleaved |

## Workloads

| fixture | what it exercises |
|---|---|
| `math_agg` | `sum_f` + `mean_f` over a 10^6-element f64 array (2e6 element-passes, plan §5 size) |
| `math_scalar` | `abs` / `signum` / `clamp` / `gcd` / `lcm` / `factorial` over 5e5 iterations |

Both sizes are large enough that the in-VM millisecond timer and host-load
jitter stay well under the budget; the 5e5 scalar loop exists because the AOT
form of the workload is otherwise only tens of milliseconds, where scheduler
noise alone can exceed 20%.

## VM (bytecode)

```bash
ZZ_FIXTURES="math_agg math_scalar" bench/vm_perf.sh 7
```

| fixture | baseline best | result payload | budget |
|---|---:|---|---|
| `math_agg` | 1110 ms | `1500000:15` | VM <= 2x |
| `math_scalar` | 1300 ms | `20499809471:-50.0` | VM <= 2x |

## AOT (release `zz build -p`)

```bash
ZZ_MODE=native ZZ_FIXTURES="math_agg math_scalar" bench/vm_perf.sh 7
```

| fixture | baseline best | result payload | budget |
|---|---:|---|---|
| `math_agg` | 44 ms | `1500000:15` | AOT <= 1.2x |
| `math_scalar` | 87 ms | `20499809471:-50.0` | AOT <= 1.2x |

`ZZ_MODE=native` compiles each fixture with `zz build -p` (release: `-O3
-flto=thin`) and times the produced binary. The harness forces
`ZZ_BUILD_CACHE=0` because the build-cache key does not include the compiler
binary's identity: without it, an artifact cached by the base compiler would
be reused by the new compiler and mask a codegen change.

## Re-running for PR 2

```bash
cargo build --release -p zz_cli
cp target/release/zz ~/.bench/zz_base      # keep the baseline binary
# ... after the migration ...
cp target/release/zz ~/.bench/zz_new
ZZ_FIXTURES="math_agg math_scalar" bench/vm_perf.sh 7
ZZ_MODE=native ZZ_FIXTURES="math_agg math_scalar" bench/vm_perf.sh 7
```

Any `MISMATCH` fails the behavior gate; the delta column enforces the budget
(negative delta = the migration is slower). On this host, runs at only 1-3
repetitions have shown 15-40% same-binary noise, so do not read a budget
verdict from fewer than 7.
