# Test — `std.test`

Asserts, temp dirs, free ports, snapshots, black box. `@test` needs no import. All asserts work bare and as `std.test.*`. Doc-tests run from `/// ```zz` fences on both engines.

```zz
import std.test
```

## Purpose

`std.test` provides assertions, temporary directories, free port discovery, optimization barriers for benchmarks, and snapshot (golden-file) testing. All asserts work both bare (`assert_eq(1+1, 2)`) and qualified (`std.test.assert_eq(...)`).

## Attributes

```zz
@test
func test_add() { assert_eq(1 + 1, 2) }

@test(should_panic = true)
func test_boom() { fail("boom") }

@test(ignore = true, reason = "flaky env")
func test_skip() { }

@test(timeout = 500)
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

## Asserts

```zz
@test
func test_all() {
    assert(1 < 2)
    assert_eq(1 + 1, 2)
    assert_ne(1, 2)
    assert_approx_eq(0.1 + 0.2, 0.3, 0.001)
    assert_contains("hello", "ell")
    assert_starts_with("hello", "he")
    assert_ends_with("hello", "lo")
    assert_len([1,2,3], 3)
    assert_gt(2, 1)
    assert_lt(1, 2)
    assert_is_some(.some(1))
    assert_is_none(.none)
    assert_is_ok(.ok(1))
    assert_is_err(.err("x"))
    assert_snapshot("hello", "home")
    fail("unreachable")
}
```

| Assert | Notes |
|--------|-------|
| `assert(cond)` | bool condition |
| `assert_eq/ne(left, right)` | structural diff on failure |
| `assert_approx_eq(a, b, eps)` | floats |
| `assert_contains(hay, needle)` | str; empty needle passes |
| `assert_starts_with/ends_with` | str |
| `assert_len(val, n)` | array/str/dict/bytes (chars for str) |
| `assert_gt/lt(a, b)` | numeric, mixed int+float ok |
| `assert_is_some/none/opt` | Option |
| `assert_is_ok/err(res)` | Result |
| `fail(msg)` / `panic(msg)` | abort test |
| `assert_snapshot(actual, name)` | golden files |

## `std.test` helpers

Return `Result` so `?`/`match` surface setup failures.

```zz
import std.test
import std.fs

@test
func test_export() {
    d := ""
    match test.temp_dir("export") {
        .ok(v) => d = v,
        .err(_) => fail("temp_dir failed"),
    }
    f := "{d}/out.txt"
    assert_is_ok(fs.write(f, "hi"))
    assert(fs.exists(f))
    assert_is_ok(fs.remove_dir_all(d))
}
```

- `temp_dir(prefix)` — unique 0700 subdir under OS temp, per-call unique (pid+counter+nanos). Clean up in `@teardown`.
- `free_port()` — bind `127.0.0.1:0`, close, return port. TOCTOU: re-bind immediately, retry on collision, tag `serial`.
- `black_box(v)` — optimization barrier for benches (AOT cannot see through it).
- No env/mock framework — use `std.env.set/remove` + `tag = "serial"`, inject func params as fakes.

## Snapshots

```zz
@test
func test_render() {
    assert_snapshot("hi", "homepage")
}
```

Compares against `<file-parent>/snapshots/<name>.snap` (exact bytes). First run fails `Snapshot missing … run with --update-snapshots`; review, run `zz test --update-snapshots`, commit the `.snap`. Never pass `--update-snapshots` in CI. Names are single components (`/`, `\`, `..` rejected).

## Doc-tests

`/// ```zz` fences run as tests on both engines. Modifiers: `,ignore`, `,should_panic`, `,compile_fail` (VM-only), `,no_run` (check only), `,prelude` (shared setup). `#` lines compile but hide from rendered docs. Reported as `doctest <file>#<n>`. `--no-doctests` skips them.

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `assert` | `assert(cond: bool) -> unit` | Bool condition | `assert(x > 0)` |
| `assert_eq` | `assert_eq<T>(left: T, right: T) -> unit` | Structural equality | `assert_eq(1+1, 2)` |
| `assert_ne` | `assert_ne<T>(left: T, right: T) -> unit` | Structural inequality | |
| `assert_approx_eq` | `assert_approx_eq(a: float, b: float, epsilon: float) -> unit` | Float approx | |
| `assert_contains` | `assert_contains(hay: str, needle: str) -> unit` | Substring | |
| `assert_starts_with` | `assert_starts_with(s: str, prefix: str) -> unit` | Prefix | |
| `assert_ends_with` | `assert_ends_with(s: str, suffix: str) -> unit` | Suffix | |
| `assert_len` | `assert_len<T>(val: T, n: int) -> unit` | Length check | `assert_len([1,2,3], 3)` |
| `assert_gt` | `assert_gt<T,U>(left: T, right: U) -> unit` | Numeric greater-than | |
| `assert_lt` | `assert_lt<T,U>(left: T, right: U) -> unit` | Numeric less-than | |
| `assert_is_some` | `assert_is_some<T>(val: Option<T>) -> unit` | Option is some | |
| `assert_is_none` | `assert_is_none<T>(val: Option<T>) -> unit` | Option is none | |
| `assert_is_ok` | `assert_is_ok<T,E>(val: Result<T,E>) -> unit` | Result is ok | |
| `assert_is_err` | `assert_is_err<T,E>(val: Result<T,E>) -> unit` | Result is err | |
| `fail` / `panic` | `fail(msg: str) -> unit` | Abort test | `fail("boom")` |
| `assert_snapshot` | `assert_snapshot(actual: str, name: str) -> unit` | Golden file | |
| `temp_dir` | `temp_dir(prefix: str) -> Result<str, str>` | Temp dir | |
| `free_port` | `free_port() -> Result<int, str>` | Free port | |
| `black_box` | `black_box<T>(val: T) -> T` | Optimization barrier | |

All asserts also available bare (no import) and as `std.test.*`.

## Gotchas

- **`temp_dir`** — always clean up, usually in `@teardown`.
- **`free_port`** is TOCTOU — re-bind immediately, retry on collision.
- **`black_box`** prevents AOT from optimizing away benchmark values.
- **Snapshot names** are single components only.

