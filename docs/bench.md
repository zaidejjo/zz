# `zz bench` — Built-in Benchmark Runner

Separate command from `zz test` by design: benches run serially in file
order (never shuffled or parallelized — both corrupt timings), each bench
runs `warmup` untimed iterations then `iters` timed ones, and the output is
a timing table, never pass/fail verdicts.

Zero-config layout: with no path, `zz bench` uses `./bench/` when it holds
`.zz` files, else the current directory. No `zz.toml` needed.

## Decorators

```zz
@bench
func bench_sort() { }

@bench(iters = 5000)            // timed iterations (default 1000)
func bench_parse() { }

@bench(warmup = 200)            // untimed warmup (default 100)
func bench_warm() { }

@bench(tag = "math")
func bench_math() { }

@setup
func setup_module() { }         // once per bench, around iterations

@teardown
func teardown_module() { }      // once per bench, always runs
```

Rules (diagnosed at compile time):

- Bench functions take no parameters and must not be generic.
- No mixing `@bench` with `@test`/`@setup`/`@teardown` or ordinary
  decorators on the same function.
- Keep benches self-contained: iterations share one interpreter (VM) or
  process (AOT). Reset mutable state inside the function instead of
  relying on fresh state per iteration.

## CLI

```
zz bench [path] [FLAGS]          # VM by default
zz bench --engine native [path]  # AOT: dev build per file, one process per bench
zz bench --list
```

| Flag | Meaning |
|------|---------|
| `[path]` | `.zz` file or directory (default: `./bench/` if it holds `.zz` files, else `.`) |
| `--filter/-f`, `--exact` | Substring (or exact) filter on bench name |
| `--tag/-t <t>` | Only benches with `tag = t` |
| `--skip/-s <s>` | Exclude benches matching substring `s` |
| `--iters/-n N` | Timed iterations per bench (default 1000; `@bench(iters)` wins) |
| `--warmup/-w N` | Untimed warmup iterations (default 100; `@bench(warmup)` wins) |
| `--timeout <ms>` | Total budget per bench (default 60000; overruns abort the bench) |
| `--list` | List discovered benches without running |
| `--json` | Structured per-bench JSON to stdout |
| `--engine=vm\|native` | Select the bench engine (default `vm`) |
| `-p`, `--release` | With `--engine native`: optimized build (default: dev) |
| `-h/--help` | Help |

### Engines

- **VM** (default): one interpreter setup per bench; iterations call the
  bench function in-process, timed with a monotonic clock. Reports
  median/mean/min/max per iteration.
- **AOT** (`--engine native`): each bench file is compiled once, then every
  bench runs in its own process, timing itself with
  `time.monotonic_nanos` over all iterations and printing one summary
  line. Reports the mean as median/min/max (one sample cannot produce a
  spread). Absolute numbers differ between engines — compare VM-to-VM or
  AOT-to-AOT across runs, never VM-to-AOT.

### Output

```
engine: vm
Running bench/sort_bench.zz
running 2 benches
bench sort_bench.bench_sort ... 12.3µs/iter (1000 iters, mean 12.8µs, min 11.9µs, max 25.1µs)
bench sort_bench.bench_parse ... 3.1ms/iter (5000 iters, mean 3.2ms, min 3.0ms, max 5.4ms)
```

Durations print adaptive (`12ns`, `3.4µs`, `2.1ms`, `1.20s`). `--json`
emits one object per bench (`name`, `file`, `iters`, `warmup`,
`median_ns`, `mean_ns`, `min_ns`, `max_ns`, `over_budget`, `error`).

Exit codes: `0` all benches ran (timings never fail), `1` a bench
function errored or overran its budget, `2` load/compile error.

## `zz.toml` `[bench]` Config

```toml
[bench]
iters = 1000
warmup = 100
timeout = 60000   # ms, total budget per bench
engine = "vm"     # or "native"
```

CLI flags always override file config. No config file is required.

## Writing stable benches

- Size the work so one iteration takes microseconds or more; sub-100ns
  bodies drown in timer granularity — loop inside the function instead.
- Avoid printing inside bench functions (output is captured and
  discarded on the VM; on AOT it pollutes the child pipe).
- Avoid global state across iterations; `@setup` runs once, not per iter.
- For allocation-heavy benches, prefer larger `iters` over conclusions
  from a single run; compare medians across runs on the same machine.
