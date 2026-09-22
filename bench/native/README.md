# ZZ Performance Suite

Comparative benchmarks for the ZZ dual-engine: bytecode VM vs native AOT,
vs Go and Bun.

## Categories

| File           | Category       | Workload                      |
|----------------|----------------|-------------------------------|
| `loop.zz`      | Arithmetic     | 10M integer accumulation      |
| `fib.zz`       | Recursion      | `fib(30)` self-recursive      |
| `string.zz`    | Strings        | 20k concatenations            |
| `math.zz`      | Math           | 1M pow iterations             |

## Engines compared

| Column         | Command                       |
|----------------|-------------------------------|
| `zz VM`        | `zz run <file>`               |
| `zz --native`  | `zz run --native <file>`      |
| `zz build`     | `zz build` dev `-O1` binary   |
| `zz build -p`  | `zz build -p` release `-O3`   |
| `Go`           | compiled Go helper            |
| `Bun`          | Bun TS helper                 |

## Run

```bash
cargo build --release
bash bench/native/run_all.sh
```

Set `RUNS=5 bash bench/native/run_all.sh` for more stable averages.

## Output

- Times in ms (best-of-N runs, lower is better). `▀` marks per-benchmark
  fastest.
- `%` column: each engine's time as a percentage of the fastest
  (100% = fastest; 400% = 4x slower).
- Bar row: visual scale capped at 400% (shorter = faster).
- Summary: per-benchmark speedup vs `zz VM` + geometric mean across all
  categories.
