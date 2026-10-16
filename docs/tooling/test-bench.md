# Test and bench

## `zz test` — VM default, AOT opt-in

Zero-config: no path uses `./tests/` when it holds `.zz`, else `.`. No `zz.toml` needed.

```bash
zz test
zz test parser --tag unit
zz test --seed 123 --serial
zz test --nocapture              # live output, forces --serial
zz test --native                 # AOT dev build, one process per test
zz test --native -p              # AOT release
zz test --list
zz test --json > results.json
zz test --junit junit.xml
zz test --changed                # content-hash + import graph, conservative
```

| Flag | Meaning |
|---|---|
| `--exact` | exact match, not substring |
| `--tag <t>` / `--skip <s>` | include / exclude |
| `--jobs N` / `--serial` | pool size (default cores) / sequential |
| `--seed <n>` | shuffle order, printed on failure for replay |
| `--repeat N`, `--fail-fast` | rerun / stop early |
| `--fail-on-empty` | exit 1 when filter matches 0 (default exit 0) |
| `--engine vm\|native`, `--native` | engine; `--native` is deprecated alias |
| `-p/--release` | optimized AOT (with `--engine native`) |
| `--timeout <ms>` (default 60000) | hard budget, overruns fail |
| `--soft-timeout` | notice-only (legacy) |
| `--json`, `--junit <path>` | machine / CI reports |
| `--slow-threshold <ms>` | flag slow passes |
| `--update-snapshots` | write goldens (never in CI) |
| `--no-doctests` | skip `/// ```zz` fences |

Engines: VM runs each test in a fresh `Interp` (fastest, teardown always runs).
AOT compiles each file once, runs each test in its own process (assert aborts
only its process; segfaults report per test). `@test(timeout)` uses a worker
subprocess (VM) / killed process (AOT) — true wall-clock even for blocking natives.
`serial`-tagged tests always run in file order. Captured stdout stays quiet on
pass, prints `| `-prefixed on fail; `--json`/`--junit` always carry it. TTY gets
spinner/colors; pipes are plain. Exit `0` all pass (or 0 matched), `1` any fail,
`2` discovery/compile error.

`zz.toml [test]`: `jobs`, `serial`, `fail-fast`, `slow-threshold`, `timeout`,
`soft-timeout`, `seed`, `repeat`, `engine`, `update-snapshots`. CLI wins.

## `zz bench` — separate by design

Serial in file order (never shuffled/parallel — corrupts timings).
Each bench: `warmup` untimed + `iters` timed; output is a timing table, never pass/fail.

```zz
@bench
func bench_sort() { }

@bench(iters = 5000, warmup = 200, tag = "math")
func bench_parse() { }
```

```bash
zz bench
zz bench --engine native [path]
zz bench --list
```

Flags: `--filter/-f`, `--exact`, `--tag/-t`, `--skip/-s`, `--iters/-n` (1000),
`--warmup/-w` (100), `--timeout`, `--list`, `--json`. Benches take no params,
not generic, no `@test` mixing. Iterations share one interp/process — reset
mutable state inside. `test.black_box(v)` is the AOT dead-code barrier.

## Coverage

```bash
zz cover [path]   # VM + AOT binaries; see coverage.md
```

Design: [coverage.md](../coverage.md). Isolation: [test-isolation.md](../test-isolation.md).
Parity gate (`zz check` vs `zz-lsp`): `cargo test -p zz_lsp --test check_parity`.
Full asserts: [Test helpers](../stdlib/testing-lib.md). Spec: [testing.md](../testing.md),
[bench.md](../bench.md).
