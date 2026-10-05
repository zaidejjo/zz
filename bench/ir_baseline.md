# IR gate benchmarks (pre-M1 baselines for the M5 5% rule)

Gate: AOT-from-IR must be within 5% of current-AOT wall time on this set.
Measure with `zz build -p` (release flags — the M5 comparison mode),
best-of-5 runs, binaries outside the repo (`/tmp`).

Set (`bench/ir_gate/*.zz` — committed so M5 reruns the same sources):

| bench | what it stresses | expected out |
|---|---|---|
| `fib35.zz` | recursion + call frames | 9227465 |
| `tak.zz` | nested calls, branches | 9 |
| `sieve.zz` | arrays, indexing, while loops | 17984 |
| `arraysum.zz` | vec build + iteration | 47999055 |
| `strconcat.zz` | string allocation/concat | 100000 |

Baselines (2026-10-05, Intel i3-4005U 1.70GHz x86_64, Linux, clang
release `-O3 -flto=thin`, current HIR-direct AOT):

| bench | best-of-5 |
|---|---|
| fib35 | 64ms |
| tak | 10ms |
| sieve | 31ms |
| arraysum | 37ms |
| strconcat | 4ms |

Caveats:

- Re-run baselines on the M5 machine before judging: these numbers are
  machine- and clang-version-specific.
- `strconcat` is small and noisy; it is an allocation smoke test, not a
  decision-maker. `tak` is small but call-shaped; keep both, weight
  fib/sieve/arraysum.
- Simple accumulation loops (`sum 0..N`) are NOT in the set on purpose:
  LLVM folds them to closed form, so they measure idiom recognition
  instead of ZZ codegen (verified: 10M vs 100M both ~3ms).
- Always diff against a same-machine, same-day current-AOT rebuild —
  never against this table across machines.
