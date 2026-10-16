# Test helpers — asserts, `std.test`, snapshots

`@test` needs no import. All asserts work bare and as `std.test.*`.

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

| Assert | Notes |
|---|---|
| `assert(cond)` | bool |
| `assert_eq/ne(left, right)` | structural diff on failure |
| `assert_approx_eq(a, b, eps)` | floats |
| `assert_contains(hay, needle)` | str; empty needle passes |
| `assert_starts_with/ends_with` | str |
| `assert_len(val, n)` | array/str/dict/bytes (chars for str) |
| `assert_gt/lt(a, b)` | numeric, mixed int+float ok |
| `assert_is_some/none/opt` | Option |
| `assert_is_ok/err(res)` | Result |
| `fail(msg)` | abort test |
| `assert_snapshot(actual, name)` | golden files, see below |

## `std.test` helpers

Bare or `test.*` via `import std.test`. Return `Result` so `?`/`match` surface setup failures.

```zz
import std.test
import std.fs

@test
func test_export() {
    d := ""
    match test.temp_dir("export") {
        .ok(v) => d = v
        .err(_) => fail("temp_dir failed"),
    }
    f := "{d}/out.txt"
    assert_is_ok(fs.write(f, "hi"))
    assert(fs.exists(f))
    assert_is_ok(fs.remove_dir_all(d))
}
```

- `temp_dir(prefix)` — unique 0700 subdir under OS temp, sanitized to one component,
  per-call unique (pid+counter+nanos). Always clean up, usually in `@teardown`.
- `free_port()` — bind `127.0.0.1:0`, close, return port. TOCTOU: re-bind immediately,
  retry on collision, tag `serial`.
- `black_box(v)` — optimization barrier for benches (AOT cannot see through it).
- No env/mock framework — use `std.env.set/remove` + `tag = "serial"`, inject func
  params as fakes, pass clock/seed params for determinism.

## Snapshots

```zz
@test
func test_render() {
    assert_snapshot(render_page("hi"), "homepage")
}
```

Compares against `<file-parent>/snapshots/<name>.snap` (exact bytes).
First run fails `Snapshot missing … run with --update-snapshots`; review, run
`zz test --update-snapshots`, commit the `.snap`. Never pass `--update-snapshots` in CI.
Names are single components (`/`, `\`, `..` rejected).

## Doc-tests

`/// ```zz` fences run as tests on both engines. `,ignore`, `,should_panic`,
`,compile_fail` (VM-only), `,no_run` (check only), `,prelude` (shared setup).
`#` lines compile but hide from rendered docs. Report as `doctest <file>#<n>`.
`--no-doctests` skips running them.

Full runner flags: [Test and bench](../tooling/test-bench.md), [testing.md](../testing.md).
