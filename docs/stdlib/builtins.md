# Built-ins (no import)

| Function | Signature | Notes |
|---|---|---|
| `print` / `println` | `(...values: T) -> unit` | Variadic: prints every argument space-separated (`println()` prints only a newline). Unwrap one layer: `.ok(v)`/`.some(v)` print `v`, bare `.none` prints `none`. Bare `.err` aborts — `match` it. Bare fn value is a compile error. |
| `input` | `(prompt: str) -> str` | Line from stdin. Prompt arg optional. |
| `typeof` | `(v: T) -> str` | Name string: `"int"`, `"str"`, `"bytes"`, ... |
| `str` | `(v: T) -> str` | Keeps wrappers (`.ok(1)` stays `".ok(1)"`, unlike `println`). |
| `int` | `(v: T) -> Option<int>` | Parse/convert; `.none` on failure. Use `??` fallback. |
| `float` | `(v: T) -> Option<float>` | Same as `int`. |
| `len` | `(v: T) -> int` | Arrays, tuples, bytes, strings (chars), dicts, ranges. |
| `range` | `(stop)` / `(start, stop)` / `(start, stop, step)` | `range(5)` is `0..5`. |
| `map` | `(arr, f) -> [U]` | Also `std.vec`. |
| `filter` | `(arr, pred) -> [T]` | Also `std.vec`. |
| `enumerate` | `(arr)` | Index+value pairs. Also `vec.enumerate`. |
| `zip` | `(a, b)` | Pair elements. |
| `clone` | `(v: T) -> T` | Pin an independent deep copy. |
| asserts | `assert`, `assert_eq`, `assert_ne`, ... | Bare and `std.test.*`. See [Test](test.md). |

```zz
println("hello")
println("value:", 42, true)   // value: 42 true
name := input("name: ")
println("hi {name}, len {len(name)}")

n := int("42") ?? 0
f := float("3.14") ?? 0.0

evens := [x for x in range(10) if x % 2 == 0]
doubled := map([1, 2, 3], |x| x * 2)
```

## Option / Result methods

Available via method dispatch on variant values:

```zz
x := .some(42)
x.unwrap()          // 42
x.unwrap_or(0)      // 42
x.expect("missing") // 42

y: Option<int> = .none
y.unwrap_or(0)      // 0

ok := .ok(42)
ok.unwrap_or(0)     // 42
e: Result<int, str> = .err("boom")
e.unwrap_or(0)      // 0
```

`unwrap`/`expect` panic on the wrong variant. Prefer `match`, `?`, or `unwrap_or`.
