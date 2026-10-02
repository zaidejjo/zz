# Append-elision baseline (pre-optimization)

Date: 2026-10-01T18:28:32Z · Machine: `x86_64` · zz: `zz 0.1.6`
Commit: `a3b3105` · Corpus manifest sha: `645532fc56366eaa`
ZZ binary: `target/debug/zz` · Timeout: 590s

Method: each case runs once per engine; wall time via
monotonic clock, peak RSS via per-child `wait4` `ru_maxrss`
(same kernel counter as GNU `time -v`; GNU time is not
installed on this machine). VM cases over 32 KB are skipped
above 32 KB (superlinear — infeasible); native 1 MB may hit
the timeout, which is itself the baseline signal.

## Microbenchmarks (wall ms / peak RSS KiB)

| case | VM | VM RSS | native | native RSS |
|------|----|--------|--------|------------|
| `push_int N=10000` | 8406 | 29188 | 4070 | 12476 |
| `push_str N=5000` | 9676 | 30008 | 1867 | 12476 |
| `field_push N=10000` | 22433 | 29832 | 4274 | 12168 |
| `thread_doc N=2000` | 11516 | 28984 | 959 | 12268 |

## Corpus proxy (wall ms / peak RSS KiB)

| case | VM | VM RSS | native | native RSS |
|------|----|--------|--------|------------|
| `proxy/flat_1k` | 302 | 29188 | 51 | 12268 |
| `proxy/mixed_1k` | 251 | 29068 | 50 | 12260 |
| `proxy/flat_8k` | 4197 | 29776 | 101 | 12260 |
| `proxy/mixed_8k` | 1459 | 29620 | 52 | 12268 |
| `proxy/flat_32k` | 56867 | 30708 | 1056 | 6912 |
| `proxy/mixed_32k` | 10894 | 30628 | 253 | 6588 |
| `proxy/flat_128k` | skipped-infeasible | skipped-infeasible | 15508 | 9512 |
| `proxy/mixed_128k` | skipped-infeasible | skipped-infeasible | 2661 | 8280 |
| `proxy/flat_1m` | skipped-infeasible | skipped-infeasible | timeout:590016ms | timeout:590016ms |
| `proxy/mixed_1m` | skipped-infeasible | skipped-infeasible | 347422 | 12316 |

## After move-append elision (VM, debug)

Date: 2026-10-02 · Machine: `x86_64` (same) · zz: `zz 0.1.6`
Branch: `perf/move-append-elision` · Binary: `target/debug/zz`
Method: `time zz run bench/move_append/<case>.zz -- N`, best of 2,
uncontended CPU. Fixed startup overhead (~160 ms) dominates small N;
the scaling column is the signal, not the absolute ms.

| case | before (wall ms) | after (wall ms) | scaling after |
|------|------------------|-----------------|---------------|
| `push_int N=10000` | 8406 | ~165 | flat 5k→40k: 171→195 ms (linear) |
| `push_str N=5000` | 9676 | ~193 | linear (same fused path) |
| `field_push N=10000` | 22433 | ~6900 | ~3.2x (partial: stdlib arg-reuse only) |
| `thread_doc N=2000` | 11516 | ~2300 | ~5x (stdlib arg-reuse + slot takes) |

What fired: `x = vec.push(x, e)` / `x = x.push(e)` / `x.push(e)`
compile to a fused take→push→store op (zero clones); every
`vec.push` call site also reuses the owned `Vec` out of the arg slot.
`x = f(x, ...)` takes the single `x` load for uncaptured stack locals.

Known follow-up: the struct-field fused op (`s.f = vec.push(s.f, e)`)
is emitted but the 10k micro still shows ~1 clone/push — the direct
field fast path is not hitting at runtime (value correct, all suites
green). Suspect: promoted/embedded field layout bypasses the inline
scan and falls back to clone-read + push + write. Profile
`VecPushField` vs generic before calling the field shape done.

Native after-curves: re-run `bench/move_append/run.py` (the harness).
Pre-optimization native baselines above stand; native `zz_vec_push_take`
/ `zz_object_push_field_take` landed on this branch with the same
shapes plus `x = f(x, ...)` takes, and all e2e + parity suites pass on
both engines.
