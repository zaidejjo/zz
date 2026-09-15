# `zz test` — Built-in Test Framework

VM-only in v1. AOT (`zz build`) harness, doctests, and `--isolate` subprocess mode are deferred (stubs below).

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
zz test [filter] [FLAGS]
zz test --list
```

| Flag | Meaning |
|------|---------|
| `[filter]` | Substring filter on `file :: test_name` |
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
| `--json` | Structured per-test JSON to stdout |
| `--junit <path>` | JUnit XML for CI |
| `--slow-threshold <ms>` | Mark passing tests slower than threshold with ⚠️ |
| `--changed` | Only tests affected by files changed since last success (content hash, conservative) |
| `-h/--help` | Help |

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
❌ [FAIL] tests/unit/math_test.zz :: test_addition (attempt 1/1, 2ms)
   Assertion Failed: Expected equality
   ------------------------------------------
   - Left:  15        (red)
   + Right: 18        (green)
```

## Terminal UI & Output Modes

- Live spinner/counter on TTY only; non-TTY/non-interactive falls back to plain, color-free, spinner-free output automatically.
- Per-test timing; `--slow-threshold` flags slow passes with ⚠️ even on pass.
- Summary: total, passed (green), failed (red), ignored (yellow), retried/flaky (magenta), wall-clock, seed.
- `--json`: one JSON object per test (name, file, status, attempts, duration_ms, stdout, stderr, diff).
- `--junit <path>`: JUnit XML (Jenkins/GitLab/GH Actions).

Non-TTY detection via `IsTerminal`; colors via `std.colors` with plain fallback.

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | All matched tests passed (or zero tests matched — see below) |
| `1` | Any test failed |
| `2` | Compilation/discovery error before any test ran |

**Zero tests matched:** `zz test <substring>` matching 0 tests exits `0` with message `0 tests matched (filter: "<s>")`. Reasoning: filter is a local developer convenience, not a failure signal; CI gates that need a non-empty suite should assert on `passed == 0` via `--json`/`--junit` or a wrapper script. A future `--fail-on-empty` flag is reserved if CI users want `exit 1` on empty match without parsing output. This keeps `zz test` consistent with `cargo test -- --skip` semantics where empty filter is not an error.

## `zz.toml` `[test]` Config

```toml
[test]
threads = 4
timeout = 1000
seed = 42
tags = ["unit"]
slow_threshold_ms = 200
retry = 0
```

CLI flags always override file config. File is searched upward from `cwd`.

## Deferred

- **Doctests:** `/// ```zz ... ``` ` extraction → `doctests` category. Design stub: reuse doc-comment trivia, register as synthetic `@test` with `file:line` of fence.
- **AOT harness:** `zz build` test binary + same discovery.
- **`--isolate` subprocess mode:** per-test child `zz test --run-one` for true segfault isolation.
- **Precise `--changed`:** cross-file fine-grained dependency hashing.

## Examples

```bash
zz test                          # all tests, parallel
zz test parser --tag unit        # filtered
zz test --seed 123 --serial      # reproducible serial run
zz test --nocapture              # live output (implies --serial)
zz test --list
zz test --json > results.json
zz test --junit junit.xml
zz test --changed
```
