# Variables and functions

## Variables

```zz
x := 42            // inferred: int
pi: float = 3.14   // checked against annotation
scores: [int] = [1, 2, 3]
```

- `:=` infers from the right side. `: T =` verifies and names the fix direction on mismatch.
- Statements end at newline (depth 0) or `;`. Newlines inside `()`, `[]`, `{}` do not end statements.
- Assignment `=` overwrites. Compound forms evaluate the receiver once:
  `arr[i()] += f()` calls `i()` once, not twice.

```zz
x := 10
x += 1
x *= 3
flags := 0
flags |= 4
```

## Functions

```zz
func add(a: int, b: int) -> int {
    return a + b
}

func greet(name: str) {
    println("Hello, {name}")  // no `return` = unit
}

func hello(name: str, greeting: str = "Hello") {
    println("{greeting}, {name}")
}

hello("Ada")
hello("Ada", greeting: "Hi")
```

Named args: `create_user("Ada", age: 30, active: true)`.
Cannot chain compound assignment (`x += y += z` is an error).

Named functions return a value **only** through `return`: the last
expression of a `func` body is discarded, and a non-`unit` signature that
can fall through is an error. Closures keep their implicit tail, so
`|x| x * 2` still works.

```zz
func abs(x: int) -> int {
    return if x < 0 { -x } else { x }
}
```

## Generics

```zz
func identity<T>(x: T) -> T {
    return x
}

result := identity(42)  // T = int
```

Type args erase at runtime: generic code runs like hand-written copies.

## Closures

```zz
double := |x: int| x * 2
square := |x: int| {
    x * x
}

nums := [1, 2, 3]
doubled := map(nums, |x| x * 2)
```

Closures capture by copy (snapshot). They are values and can be passed to
`map` / `filter` / `task.spawn`. A closure body's last expression is its
return value; closures do not need `return`.

Closure parameters are exempt from `unused variable` warnings: the caller
dictates the closure's arity (`s.route("GET", "/", |req| "ok")` cannot drop
`req`), so an unread parameter is not a mistake. Named-function parameters
still warn.

## Method syntax

`p.dist()` is `dist(p)` — receiver becomes first arg:

```zz
struct Point { x: int, y: int }
func dist(p: Point) -> int { return p.x + p.y }

p := Point{ x: 3, y: 4 }
dist(p)  // 7
p.dist() // 7
```

An `impl` method on the receiver's own type wins over a same-named free
function, so `w.get()` reaches `W.get` even when a free `get(w)` exists:

```zz
struct W { v: int }
func get(w: W) -> str { return "free" }
impl W {
    func get(self) -> str { return "method" }
}

w := W{ v: 1 }
w.get() // "method" — the impl method
get(w)  // "free"   — the free function, still reachable by name
```

Works for stdlib too: `"hi".to_upper()`, `[3, 1, 2].sort()`, `server.route(...)`.

## Pitfalls

- Arrays/structs are values: mutating a parameter never mutates the caller.
- `+` on `str` / `[T]` concatenates; `+` on mixed `int`+`float` yields `float`.
- Integer overflow wraps (`9223372036854775807 + 1` is min-int). Division by zero errors.

Deep reference: [syntax.md](../syntax.md) sections Declarations, Functions, Expressions.

## Explicit Return Rules

Named functions (`func`, `@test`, `impl` methods) return a value **only**
through `return`. The body's trailing expression is discarded — it does
not propagate as a return value.

### When `return` is required

| Function signature | Body ends with | Result |
|---|---|---|
| `func f() -> int { ... }` | `return 1` | ✅ Returns `int` |
| `func f() -> int { 1 }` | `1` (trailing) | ❌ Error: missing `return` |
| `func f() -> int { if c { 1 } }` | `if` without `else` | ❌ Error: missing `return` |
| `func f() -> unit { 1 }` | `1` (trailing) | ✅ Falls through to `unit` |
| `func f() { 1 }` (unannotated) | `1` (trailing) | ✅ Infers `unit` |
| `|x| x * 2` (closure) | `x * 2` (trailing) | ✅ Implicit tail |

### Missing-return diagnostic

When a non-`unit` function can fall through without `return`, the checker
emits an error:

```
error: function returns `int` but its body can fall through; add `return `
  --> file.zz:3:5
   |
 3 |     if true { 1 }
   |     ^^^^^^^^^^^^^
   |
   = help: named functions return a value only through `return`; the last expression is discarded
```

A fixit `"return "` is offered (zero-width insertion at the start of the
tail expression). Accepting it rewrites:

```zz
// Before:
func f() -> int {
    if true { 1 }
}

// After:
func f() -> int {
    return if true { 1 }
}
```

### Fixit withheld (message only, no insertion)

The fixit is **not** offered when inserting `return ` would trade one
error for another:

- **`if` without `else`**: `return if c { 1 }` still yields `unit` on the
  missing-else arm — the fixit would not resolve the error.
- **`match` expressions**: arm bodies are checked individually; a blanket
  `return ` prefix doesn't apply uniformly.
- **`let` bindings whose name is used later**: the binding may be
  intentional shadowing, not a fall-through value.
- **`unit`-typed tails** (e.g. `println(...)`): the value is already
  `unit`, so `return ` adds nothing.

### Closures and block expressions

Closures and block expressions keep their implicit tail — they are not
affected by the explicit-return rule:

```zz
let double = |x: int| x * 2;        // implicit tail: returns int
let result = { let x = 5; x + 3 };   // block: returns 8
```

Only `func` / `@test` / `impl` methods require explicit `return`.
