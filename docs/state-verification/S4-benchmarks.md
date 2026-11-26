# S4 — Benchmarks

**Build:** debug profile (`cargo build -p zz_cli`, no --release because release
build hangs on this machine due to dependency compilation; AOT native
compilation uses clang separately).

**Engine:** VM (`zz bench --engine vm`) and AOT (`zz bench --engine native`).
AOT programs do NOT spawn tasks unless the contended benchmark explicitly does.

**Methodology:** `zz bench` runs warmup (1 iteration) then timed iterations
(3-7). `ns/op = total_ns / N`. Result consumed via `black_box()` to prevent
dead-code elimination. N=1k/10k/100k for increment/vec benchmarks.

## VM results (debug, ms/iter → ns/op)

| Benchmark | N | ms/iter | ns/op |
|---|---|---|---|
| bench_plain_global | 1k | 25.7 | 25,700 |
| bench_plain_global | 10k | 255.5 | 25,550 |
| bench_plain_global | 100k | 2710 | 27,100 |
| bench_state_inc | 1k | 6.8 | 6,800 |
| bench_state_inc | 10k | 60.7 | 6,070 |
| bench_state_inc | 100k | 656.6 | 6,566 |
| bench_atomic_inc | 1k | 5.6 | 5,600 |
| bench_atomic_inc | 10k | 64.7 | 6,470 |
| bench_atomic_inc | 100k | 580.5 | 5,805 |
| bench_local_vec_push | 1k | 0.641 | 641 |
| bench_local_vec_push | 10k | 6.8 | 680 |
| bench_local_vec_push | 100k | 67.6 | 676 |
| bench_state_vec_push | 1k | 60.0 | 60,000 |
| bench_state_vec_push | 10k | 5220 | 522,000 |

## AOT results (debug, ms/iter → ns/op)

| Benchmark | N | ms/iter | ns/op |
|---|---|---|---|
| bench_plain_global | 1k | 3.0 | 3,000 |
| bench_plain_global | 10k | 12.4 | 1,240 |
| bench_plain_global | 100k | 139.3 | 1,393 |
| bench_state_inc | 1k | 1.8 | 1,800 |
| bench_state_inc | 10k | 22.0 | 2,200 |
| bench_state_inc | 100k | 302.7 | 3,027 |
| bench_atomic_inc | 1k | 1.5 | 1,500 |
| bench_atomic_inc | 10k | 49.3 | 4,930 |
| bench_atomic_inc | 100k | 219.2 | 2,192 |
| bench_local_vec_push | 1k | 0.394 | 394 |
| bench_local_vec_push | 10k | 1.8 | 180 |
| bench_local_vec_push | 100k | 28.5 | 285 |
| bench_state_vec_push | 1k | 4.3 | 4,300 |
| bench_state_vec_push | 10k | 41.2 | 4,120 |
| bench_state_vec_push | 100k | 242.8 | 2,428 |

## Contended: 4 tasks x 100k increments (AOT debug)

| Benchmark | ms/iter | ops/sec (400k total) |
|---|---|---|
| bench_contended_state_100k | 313.3 | ~1,277,000 |
| bench_contended_atomic_100k | 239.5 | ~1,670,000 |

## Scaling regression (AOT debug)

| Benchmark | 10k ms/iter | 20k ms/iter | Ratio (20k/10k) | < 5x? |
|---|---|---|---|---|
| bench_state_vec_10k / 20k | 4.9 | 11.5 | 2.35 | YES |
| bench_local_vec_10k / 20k | 1.8 | 6.6 | 3.67 | YES |

Both scaling regressions pass: 20k pushes take < 5x the time of 10k pushes.

## Elision investigation: AOT state vs plain (debug)

**Plain global at N=100k: 1,393 ns/op**
**State increment at N=100k: 3,027 ns/op**

State is ~2.2x plain global, NOT "indistinguishable from plain" as D8 elision
promises for AOT programs with NO task usage.

**Root cause:** The benchmark program `bench_state_perf.zz` uses `std.task` in
the contended benchmarks (`task.spawn`). Because `task` native is reachable
anywhere in the same module/file, `needs_safepoint()` returns true for the
whole compilation unit (see `crates/zz_lower/src/context.rs:1066-1082`), which
makes `needs_state_lock()` return true, which prevents D8 lock elision for ALL
state accesses in that file — even the `bench_state_inc` benchmark that uses no
tasks.

This is the cause of the 83 vs 42 ns/op gap seen in `docs/language/state.md`
(release numbers): lock elision is disabled at the module level because *any*
task/chan/spawn native reachable in the program forces the lock to stay on for
all state accesses, even those that don't need it.

**Fix required:** Make lock elision per-function or per-state-variable based on
actual reachability, not whole-module. Or split contended and non-contended
benchmarks into separate files so the non-contended ones can elide the lock.

## S4 evidence files

- `docs/state-verification/S4-vm-bench-partial.txt` — VM bench raw output
- `docs/state-verification/S4-aot-bench-raw.txt` — AOT bench raw output

## What's needed to complete S4

1. Re-run VM benchmarks to completion (the 100k runs were slow in debug VM)
2. Build release binary and re-run AOT with `--release` for accurate ns/op
3. Split benchmarks into separate files (contended vs non-contended) to test
   true D8 elision: non-contended state should be indistinguishable from plain
   global in AOT release
4. Write all numbers to `docs/language/state.md` R4 section
