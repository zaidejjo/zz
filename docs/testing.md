# `zz test` — Built-in Test Framework

Two engines: VM (default, in-process interpreter) and AOT (`--native`,
per-file dev build with one process per test). Discovery, filters, and
output are identical on both.

Zero-config layout: with no path, `zz test` uses `./tests/` when it holds
`.zz` files, else the current directory. No `zz.toml` needed — just drop
test files in `tests/` (e.g. `tests/unit/math_test.zz`).

## Decorators

```zz
@test
func test_addition() { assert_eq(1 + 1, 2) }

@test(should_panic = true)
func test_panics() { fail("boom") }

@test(ignore = true, reason = "flaky env")
func test_skipped() { }

@test(timeout = 500)          // ms, cooperative ZZ-code budget
func test_slow() { }

@test(retry = 3)
func test_flaky() { }

@test(tag = "integration")
func test_net() { }

@test(cases = [[1, 2], [2, 3]])
func test_param(a: int, b: int) { assert(a < b) }

@setup
func setup_module() { }

@teardown
func teardown_module() { }
```

Metadata flags (`@test(...)` named args only):

| Flag | Type | Meaning |
|------|------|---------|
| `should_panic` / `should_fail` | `bool` | Expect failure (`EvalError`/`fail`); aliases |
| `ignore` / `skip` | `bool` | Skip test |
| `reason` | `str` | Skip reason (with `ignore`/`skip`) |
| `timeout` | `int` | Per-test cooperative budget in ms |
| `retry` | `int` | Attempts for flaky mitigation; report which attempt passed |
| `tag` | `str` | Category for `--tag` filtering |
| `cases` | `[array]` | Table-driven: one sub-test `name#i` per element |

`@setup` / `@teardown` run with guaranteed cleanup even on failure/panic (defer-style). Module-level and per-test setup/teardown are supported. Validate at compile time; `should_panic + retry` together errors.

`cases` requires array literal with literal elements; non-literal errors at compile time. Expansion happens at HIR build — each entry reports individually.

## CLI

```
zz test [path] [FLAGS]          # VM by default; ./tests/ when present, else .
zz test --native [path] [FLAGS] # AOT: dev build per file, one process per test
zz test --list
```

| Flag | Meaning |
|------|---------|
| `[path]` | `.zz` file or directory (default: `./tests/` if it holds `.zz` files, else `.`) |
| `--exact` | Filter is exact match, not substring |
| `--tag <t>` | Only tests with `tag = t` |
| `--skip <s>` | Exclude tests matching substring `s` |
| `--jobs N` | Thread-pool size (default = logical cores) |
| `--serial` | Sequential execution |
| `--nocapture` | Show stdout/stderr live (forces `--serial`, see below) |
| `--seed <n>` | Seed for shuffle; printed on any failure; `zz test --seed <n>` replays order |
| `--list` | List discovered tests without running |
| `--repeat N` | Run suite N times |
| `--fail-fast` | Stop after first failure |
| `--fail-on-empty` | Exit 1 when no tests matched (default: exit 0) |
| `--native` | AOT engine alias for `--engine native` (deprecated, prints a note) |
| `--engine=vm\|native` | Select the test engine (default `vm`) |
| `-p`, `--release` | With `--engine native`: optimized build (default: dev) |
| `--timeout <ms>` | Hard time budget per test (default 60000): overruns fail the test |
| `--soft-timeout` | Budget overruns print a notice only, never fail (legacy mode) |
| `--json` | Structured per-test JSON to stdout |
| `--junit <path>` | JUnit XML for CI |
| `--slow-threshold <ms>` | Mark passing tests slower than threshold as slow |
| `--changed` | Only tests affected by files changed since last success (content hash, conservative) |
| `-h/--help` | Help |

### Engines

- **VM** (default): fastest. Each test runs in a fresh `Interp`;
  `@teardown` always runs, even on failure.
- **AOT** (`--native`, dev build; `-p` upgrades to release): each test
  file is compiled once, then every test runs in its own process.
  A failing `assert` aborts only its own process (exit-code isolation),
  and segfaults/signal deaths are reported per test instead of killing
  the suite. `@setup` runs in the test's process; `@teardown` always
  runs too, in a fresh process afterwards (best-effort, failures
  discarded — same as the VM's teardown handling). Note: setup/teardown
  share only external state (files, ports) across the two processes, not
  in-memory globals. `should_panic`, `retry`, `cases`, `timeout` behave
  the same on both. Extra AOT flags: `--allow-source-builds`,
  `--allow-hooks` (same meaning as `zz build`).

### `--nocapture` + parallel execution

`--nocapture` **forces `--serial` automatically** (recommended). Reasoning: parallel live output interleaves lines nondeterministically and breaks diff/assert rendering. When `--nocapture --jobs N` is passed, the runner emits a note `note: --nocapture forces --serial (ignoring --jobs N)` and runs serially. Alternative considered — prefixing every line with `[test_name]` — was rejected for `zz test` v1 because it still scrambles assertion diffs and confuses CI parsers expecting contiguous blocks per test. A prefixed parallel live mode may return under `--isolate` (subprocess, per-test fd) in v2.

### `--seed` determinism

Shuffle seed controls test order deterministically. The seed is printed on any failure and in the summary block. `zz test --seed <n>` replays order exactly.

### `--changed` (incremental)

Content-hash manifest `.zz_test_cache` keyed by file mtime+hash plus import graph from the loader. Only tests in changed files + direct dependents run. Conservative: cross-file precise tracking deferred.

## Assertions & Diffing

```zz
assert(cond, "msg")
assert_eq(left, right)
assert_ne(left, right)
assert_approx_eq(a, b, epsilon)
fail("msg")
```

Failure output includes `file:line` + source snippet. Structural diffing:

- Multi-line strings → Myers line diff, word-level highlight within changed lines.
- Structs/arrays/maps → recursive field/index diff, only differing fields highlighted, nesting via indentation.

```
test test_addition ... ok (2ms)
test test_flaky ... ok (5ms) (retried 2x)
test test_slow ... ok (1.20s) (slow)
test test_over ... FAILED (65.20s) (took 65.20s; exceeded 60s budget)
  timeout: exceeded 60s budget (took 65.20s)
test test_broken ... FAILED (3ms)
  error: Assertion Failed: Expected equality
  - Left:  15
  + Right: 18
```

With `--soft-timeout`, the over-budget test stays `ok` with a
`(took 65.20s; over 60s soft budget, not interrupted)` suffix instead.

## Terminal UI & Output Modes

- Cargo-style blocks per file: `Running <file>`, `running N tests`,
  `test <name> ... ok|FAILED|ignored (<dur>)`, then
  `test result: ok|FAILED. X passed; Y failed; Z ignored; finished in <dur>`.
  `ok` is green, `FAILED` red, `ignored` yellow; piped output is plain.
  No symbols — `grep FAILED` / `grep "^test .* ok"` work in CI.
- Durations print as `898ms` below a second, `1.20s` at/above it.
- The 60s budget (`--timeout <ms>`) fails overrunning tests; pass
  `--soft-timeout` for notice-only mode. Hard timeouts come only from
  `@test(timeout = ms)`: VM runs the test in a worker subprocess and
  kills it on overrun (true wall-clock, even for blocking natives);
  AOT kills the test process the same way.
- Tests tagged `serial` (`@test(tag = "serial")`) always run
  sequentially in file order, even in parallel mode — use for tests
  touching process-global state (log level, fixed ports, cwd).
- Test stdout is captured per test: passing suites stay quiet (use
  `--nocapture` for live output); a failing test prints its captured
  output with `| `, and `--json`/`--junit` always carry it
  (`stdout` field / `<system-out>`). This also keeps `--json` clean
  for machine parsing.
- Live spinner/counter on TTY only; non-TTY/non-interactive falls back to plain, color-free, spinner-free output automatically.
- Per-test timing; `--slow-threshold` flags slow passes even on pass.
- Global footer across files: `test result: ... N total across M files; ...`.
- `--json`: one JSON object per test (name, file, status, attempts, duration_ms, stdout, stderr, diff).
- `--junit <path>`: JUnit XML (Jenkins/GitLab/GH Actions).

Non-TTY detection via `IsTerminal`; colors via `std.colors` with plain fallback.

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | All matched tests passed (or zero tests matched — see below) |
| `1` | Any test failed |
| `2` | Compilation/discovery error before any test ran |

**Zero tests matched:** `zz test <substring>` matching 0 tests prints
`warning:` + a hint and exits `0` (use `--fail-on-empty` for exit 1). Reasoning: filter is a local developer convenience, not a failure signal; CI gates that need a non-empty suite should assert on `passed == 0` via `--json`/`--junit` or a wrapper script. A future `--fail-on-empty` flag is reserved if CI users want `exit 1` on empty match without parsing output. This keeps `zz test` consistent with `cargo test -- --skip` semantics where empty filter is not an error.

## `zz.toml` `[test]` Config

```toml
[test]
jobs = 4
serial = false
fail-fast = false
slow-threshold = 200   # ms
timeout = 60000        # ms, hard budget (overruns fail)
soft-timeout = false   # true = notice-only budget (legacy mode)
seed = 42
repeat = 1
engine = "vm"          # or "native"
```

CLI flags always override file config. No config file is required.

## Deferred

- **Doctests:** `/// ```zz ... ``` ` extraction → `doctests` category. Design stub: reuse doc-comment trivia, register as synthetic `@test` with `file:line` of fence.
- **`--isolate` subprocess mode:** per-test child `zz test --run-one` for true segfault isolation *in the VM engine* (AOT already isolates per test).
- **Precise `--changed`:** cross-file fine-grained dependency hashing.

## Examples

```bash
zz test                          # ./tests/ when present, else ., VM, parallel
zz test parser --tag unit        # filtered
zz test --seed 123 --serial      # reproducible serial run
zz test --nocapture              # live output (implies --serial)
zz test --native                 # AOT (dev build), one process per test
zz test --native -p              # AOT, optimized release build
zz test --list
zz test --json > results.json
zz test --junit junit.xml
zz test --changed
```
