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
