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

Baselines (Intel i3-4005U 1.70GHz x86_64, Linux, clang release
`-O3 -flto=thin`, current HIR-direct AOT). Best-of-5 for the M0
snapshot; median-of-5 with spread from the flags PR onward:

| bench | M0 best-of-5 | flags-PR median-of-5 (spread) |
|---|---|---|
| fib35 | 64ms | 70ms (66–75) |
| tak | 10ms | 11ms (10–11) |
| sieve | 31ms | 31ms (29–35) |
| arraysum | 37ms | 40ms (36–43) |
| strconcat | 4ms | 4ms (4–5) |

Raw runs around the flags PR (`-ffast-math`/`-march=native` removed,
`-fwrapv -fno-strict-aliasing` added), same box:

- before (5 runs): fib35 74 76 80 102 106 · tak 15 16 17 17 21 ·
  sieve 47 51 59 60 72 · arraysum 56 57 57 64 69 · strconcat 5 11 12 12 16
- after (5 runs): fib35 66 67 70 73 75 · tak 10 10 11 11 11 ·
  sieve 29 29 31 33 35 · arraysum 36 38 40 40 43 · strconcat 4 4 4 4 5

No attributable change in either direction: the before-run overlapped
a background release build (note its 2–3x wider spreads), and the
medians overlap within noise. Verdict for the flags PR: perf-neutral
within measurement noise. The median column above becomes the M5
comparison baseline (re-run same-machine/same-day before judging).

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
