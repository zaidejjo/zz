# Syntax Reference

Complete grammar and syntax guide for the ZZ language.

## Lexical Structure

### Comments

```zz
// Line comment
/* Block comment
   can span multiple lines */
```

### Identifiers

Identifiers start with a letter or underscore, followed by letters, digits, or underscores:

```zz
name := "ZZ"
_private := 42
pascal_case := true
```

### Keywords

| Keyword | Purpose |
|---------|---------|
| `import` | Module import |
| `as` | Import alias |
| `func` | Function declaration |
| `return` | Return from function |
| `if` | Conditional |
| `else` | Alternative branch |
| `while` | Loop |
| `match` | Pattern matching |
| `for` | Iteration |
| `in` | Range/array iteration |
| `struct` | Record type |
| `break` | Exit loop |
| `continue` | Skip iteration |
| `defer` | Defer execution to scope exit |
| `true` / `false` | Boolean literals |

### Statement Terminator

Statements end with a newline at bracket depth 0, or a semicolon:

```zz
x := 1
y := 2

// Equivalent:
x := 1; y := 2
```

Newlines inside parentheses, brackets, or braces are **not** statement terminators:

```zz
result := add(
    1,
    2
)
```

## Declarations

### Short Declaration (Inferred Type)

```zz
x := 42            // int
pi := 3.14         // float
name := "ZZ"       // str
alive := true      // bool
```

### Explicit Declaration (Typed)

```zz
x: int = 42
pi: float = 3.14
scores: [int] = [1, 2, 3]
```

### Type Aliases

```zz
type Tokens = [Token]
type Pair<T> = (T, T)

toks: Tokens = ["a"]
p: Pair<int> = (1, 2)
```

Aliases erase at check time: uses resolve to the target type, so there
is no runtime cost and both engines behave identically. Generic aliases
take plain parameters and name their arguments at use sites
(`Pair<int>`), exactly like generic structs. `type` is contextual —
`json.type(x)` and variables named `type` keep working. Aliases export
across modules with `pub type` and import qualified
(`shapes.Tokens`) or selective (`import shapes(Tokens)`).

### User Enums

```zz
enum Token {
    Eof,
    IntLit(int),
    Name(str),
}

t := Token.IntLit(42)   // construction is qualified
match t {
    .Eof => "eof",      // patterns name the variant short
    .IntLit(v) => "int",
    .Name(s) => s,
}
```

Enums erase to qualified objects at runtime, so construction, matching,
equality, and `impl` methods behave identically on both engines. Matches
are exhaustiveness-checked (missing variants report, or add a `_` arm).
Variants hold at most one payload — use a tuple for more
(`Pair((int, int))`). `enum` is contextual, like `type`. Enums export
across modules with `pub enum` and chain methods inline, including off
payload construction (`Token.IntLit(1).add(2)` fills the payload with
`1`, calls `add` with `2`).

Generic enums take plain parameters and name their arguments at use
sites (`Box<int>`), exactly like generic structs:

```zz
enum Box<T> {
    V(T),
    E,
}

b: Box<int> = Box.E
```

Match guards (`n if n > 0 =>`) accept the arm's pattern bindings,
literals, and operator combinations — on both engines. Anything else
(calls, strings, field/index access) is a check-time error naming the
exact subexpression, so VM and native builds never diverge silently.
Outer variables and helper calls belong in the arm body:

```zz
match x {
    .some(n) => if n > limit && is_valid(n) { println("ok") } else { println("bad") },
    .none => println("nothing"),
}
```

## Functions

### Basic Function

```zz
func add(a: int, b: int) -> int {
    a + b
}
```

### No Return Value

```zz
func greet(name: str) {
    println("Hello, {name}")
}
```

### Default Parameters

```zz
func greet(name: str, greeting: str = "Hello") {
    println("{greeting}, {name}")
}

greet("Alice")                    // "Hello, Alice"
greet("Alice", greeting: "Hi")    // "Hi, Alice"
```

### Named Arguments

```zz
func create_user(name: str, age: int, active: bool) {
    // ...
}

create_user("Alice", age: 30, active: true)
```

### Generics

```zz
func identity<T>(x: T) -> T {
    x
}
```

### Closures

```zz
double := |x: int| x * 2
add := |a: int, b: int| a + b
greet := |name: str| println("Hello, {name}")

// Multi-line closure
square := |x: int| {
    x * x
}
```

### Dotted Names (Cross-Module)

```zz
func shapes.distance(p1: Point, p2: Point) -> float {
    // ...
}
```

## Control Flow

### If / Else

```zz
if age < 18 {
    println("minor")
} else {
    println("adult")
}

// Single-expression (no braces needed for short if)
if x > 0 { x } else { -x }
```

### If Let (Pattern Binding)

```zz
x := .some(5)
if let .some(n) = x {
    println("got {n}")
} else {
    println("nothing")
}
```

### While Loop

```zz
i := 0
while i < 10 {
    println("{i}")
    i = i + 1
}
```

### For Loop (Range)

```zz
for i in 0..5 {
    println("{i}")
}

// With step
for i in range(0, 10, 2) {
    println("{i}")
}
```

### For Loop (Array)

```zz
for name in ["Alice", "Bob", "Charlie"] {
    println("Hello, {name}")
}
```

### Break and Continue

```zz
for i in 0..100 {
    if i == 5 { break }
    if i % 2 == 0 { continue }
    println("{i}")
}
```

### Defer

Executes when the enclosing scope exits:

```zz
func process() {
    println("start")
    defer println("cleanup")
    println("done")
    // Prints: start, done, cleanup
}
```

## Expressions

### Arithmetic

```zz
1 + 2       // 3
10 - 3      // 7
3 * 4       // 12
10 / 3      // 3 (integer division)
10 % 3      // 1 (remainder)
2 ** 10     // 1024 (power, right-associative)
```

Signed integer overflow on `+`, `-`, `*` wraps two's-complement
(`9223372036854775807 + 1` is `-9223372036854775808`) on both engines —
it never traps. Guard manually at the boundary when wrapping would
corrupt the result (counters, timestamps, money math); the `toml`
package's exact `i64::MIN`/`MAX` checks are the reference pattern.
Integer division by zero (including literal `1 / 0`, which fails at
check time) and `INT64_MIN / -1` are runtime errors, not wraps.

### Comparison and Logic

```zz
1 < 2           // true
1 == 1          // true
1 != 2          // true
true && false   // false
true || false   // true
!true           // false
```

### Bitwise (int-only)

```zz
6 & 3           // 2 (AND)
6 | 3           // 7 (OR)
6 ^ 3           // 5 (XOR)
~6              // -7 (NOT)
1 << 10         // 1024 (shift left)
1024 >> 3        // 128 (shift right, arithmetic)
```

Precedence (tightest first): `~` > `+ -` > `<< >>` > `&` >
`^` > `|` > comparison (`<`, `==`, …) > `&&` > `||`. So
`flags & mask == expected` parses as `(flags & mask) == expected`,
and `a + b << c` as `(a + b) << c`.

Both operands (and `~`'s operand) must be `int` — floats, bools,
and strings are type errors. Shifts mask the count to `& 63`
(`1 << 64` is `1`); a negative shift count is a runtime error.
`>>` on negative values shifts arithmetically (sign-extending).

### String Interpolation

```zz
name := "World"
println("Hello, {name}!")

// Format specs
pi := 3.14159
println("pi = {pi:.2f}")    // 3.14

n := 255
println("hex = {n:x}")      // ff
println("HEX = {n:X}")      // FF
println("oct = {n:o}")      // 377
println("bin = {n:b}")      // 11111111
println("dec = {n:d}")      // 255
```

Literal braces use doubled escapes — `{{` renders `{`, `}}`
renders `}` — in both `"..."` and `"""..."""` strings. This is the
preferred way to emit JSON, CSS, or template syntax:

```zz
println("{{name}}")        // {name} (no interpolation)
println("{{{name}}}")     // {World} (literal braces + value)
println(".a{{color:red}}") // .a{color:red}

// Multiline works the same way:
css := """
    .a{{color:red}}
    """
```

`\{` / `\}` remain accepted for backwards compatibility and mean
the same as `{{` / `}}`, but prefer the doubled form.

#### What `{...}` expands

Both `"..."` and `"""..."""` strings open an interpolation when `{` is
followed by an identifier, a digit, or `(`:

```zz
println("{1 + 2}")   // 3
println("{(a)}")     // value of (a)
```

`{{`, `{}`, and JSON-like `{"key"` stay literal text in both forms —
double the braces for a literal `{`:

```zz
println("{{1 + 2}}") // {1 + 2} (no interpolation)
```

The same rule covers regular-expression quantifiers, which must be
doubled like in Python f-strings or Rust `format!`:

```zz
pat := "^[0-9a-f]{{8}}$"   // matches 8 hex digits
```

When a string DOES interpolate elsewhere, a bare `{` that cannot expand
(e.g. `"{x} of {!done}"`) warns at check time naming the exact rule.

### Array Literals

```zz
nums := [1, 2, 3]
mixed := [1, "two", true]
empty := []
```

### Dict Literals

```zz
ages := {"Alice": 30, "Bob": 25}
empty := {}
```

### Tuple Literals and Destructuring

```zz
t := (7, "seven")
t[0]            // 7 (integer-literal index, checked at compile time)
t[1]            // "seven"
t[-1]           // "seven" (negative counts from the end, like arrays)
len(t)          // 2

// Destructuring — parens or bare form (identical meaning):
(a, b) := t     // a = 7, b = "seven"
c, d := t       // same; `_` skips: `_, e := t`
```

Dynamic indices (`t[i]`) are a compile error — destructure instead.
Out-of-range literal indices are also caught at compile time.

Tuples share the array representation: `t[0] = 99` writes in
place, and behavior is identical on the VM and native backends.

### Indexing and Slicing

```zz
arr := [10, 20, 30, 40]
arr[0]          // 10
arr[-1]         // 40 (negative index from end)
arr[1:3]        // [20, 30]
arr[:2]         // [10, 20]
arr[2:]         // [30, 40]
arr[:]          // [10, 20, 30, 40]

s := "hello"
s[1]            // "e"
s[1:3]          // "el"
```

### Index Assignment

```zz
arr := [1, 2, 3]
arr[0] = 99     // [99, 2, 3]

dict := {"a": 1}
dict["b"] = 2   // {"a": 1, "b": 2}

struct Box { items: [int] }
b := Box{ items: [1, 2, 3] }
b.items[1] = 99  // b.items == [1, 99, 3]
```

### Compound Assignment

`x OP= y` is equivalent to `x = x OP y` with the receiver evaluated
exactly once (so `arr[i()] += f()` calls `i()` then `f()`, once each —
unlike textual expansion, which would evaluate `i()` twice):

```zz
x := 10
x += 1    // 11, like x = x + 1
x -= 2    // 9
x *= 3    // 27
x /= 4    // 6 (integer division)
x %= 4    // 2
n := 2
n **= 10  // 1024

p.x += 5        // struct fields (same targets as `=`)
arr[0] *= 2     // indices

// Bitwise forms work too:
flags := 0
flags |= 4
flags &= 7
flags ^= 1
flags <<= 2
flags >>= 1
```

Type rules are exactly the binary operator's: `x += 1.5` is accepted
precisely when `x = x + 1.5` is. Cannot be chained (`x += y += z`
is an error — split it into two statements).

### Value Semantics

Function parameters are values (copies) from the programmer's
perspective — a function can never mutate its caller's variables:

```zz
struct Counter { n: int }

func bump(c: Counter) -> int {
    c.n += 100   // mutates only the local copy
    c.n
}

c := Counter{ n: 10 }
bump(c)     // 110
c.n         // still 10
```

There are no reference parameters, no borrows, and no borrow checker.
`value semantics != mandatory physical memcpy`: the compiler and
runtime may eliminate physical copies and reuse storage internally
(copy-on-write, in-place slot operations) whenever provably safe, but
such optimizations are never observable — no alias can witness an
intermediate mutation. Correctness always wins over optimization.

### Ranges

```zz
0..5             // 0, 1, 2, 3, 4
range(0, 10, 2)  // 0, 2, 4, 6, 8
```

### List Comprehensions

```zz
squares := [x ** 2 for x in range(0, 6)]
// [0, 1, 4, 9, 16, 25]

evens := [x for x in range(21) if x % 2 == 0]

doubled := [x * 2 for x in [1, 2, 3, 4, 5] if x < 4]
// [4, 6]
```

### Elvis Operator (`??`)

Provides a fallback when a value is `.none`:

```zz
x := int("not_a_number") ?? 0       // 0
name := .some("Alice")
greeting := name ?? "stranger"       // "Alice"

// Works with non-variant values (pass-through)
val := 42 ?? 0                       // 42
```

### Pipeline Operator (`|>`)

Passes the left value as the first argument to the right function:

```zz
func inc(n: int) -> int { n + 1 }
func dbl(n: int) -> int { n * 2 }

5 |> inc |> dbl    // dbl(inc(5)) = dbl(6) = 12

// Multi-line pipelines
result := "  Hello World  "
    |> str.trim()
    |> str.to_upper()
// "HELLO WORLD"
```

### Field Access

```zz
struct Point { x: int, y: int }
p := Point{ x: 1, y: 2 }
p.x    // 1
p.y    // 2
```

### Struct Initialization

```zz
struct Point { x: int, y: int }

p1 := Point{ x: 1, y: 2 }       // standard
p2 := Point { x: 1, y: 2 }      // spaces around braces OK
```

### Struct Mutation

```zz
p := Point{ x: 1, y: 2 }
p.x = 10
println(p.x)    // 10
```

### Nested Field Access and Mutation

```zz
struct Point { x: int, y: int }
struct Rect { p: Point, w: int }

r := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }
r.p.x = 9
println(r.p.x)  // 9
```

### Struct Embedding (Anonymous Fields)

A struct can embed another struct by naming the type without a field name.
The embedded struct's fields and methods are promoted: they resolve on the
outer struct as if declared there (transitively; direct members win).

```zz
struct Base { id: int, name: str }
struct User { Base, age: int }   // embeds Base

impl Base {
    func area(self) -> int { self.id * 2 }
}

zaid := User{ id: 1, name: "Zaid Ajo", age: 19 }  // flat init nests into Base
println(zaid.id)      // 1 — promoted, same as zaid.Base.id
println(zaid.area())  // 2 — promoted method, receiver is the embedded Base
zaid.id = 99          // writes through to zaid.Base.id
```

Explicit (`User{ Base: Base{ id: 1, name: "Z" }, age: 19 }`) and shorthand
(`User{ Base{ id: 1, name: "Z" }, age: 19 }`) inits mean the same thing.
Mixing an explicit embedded value with flattened leaves of the same subtree
is rejected as ambiguous.

### Generic Structs

Structs take type parameters (`struct Box<T> { v: T }`). Construction
infers the arguments (`Box{ v: 1 }` is `Box<int>`), exactly like generic
function calls — no turbofish needed. Annotate when you want to pin it
(`x: Box<int> = Box{ v: 1 }`):

```zz
struct Box<T> { v: T }

impl Box<T> {
    func get(self) -> T {
        self.v
    }
}

b := Box{ v: 42 }      // Box<int>
println(b.get())       // 42
s := Box{ v: "hi" }    // Box<str>

struct Pair<A, B> { a: A, b: B }
p := Pair{ a: 1, b: "s" }   // Pair<int, str>
```

Rules:
- Use sites name their arguments (`Box<int>`); a bare `Box` for a generic
  struct is an error, as is the wrong count (`Pair<int>`).
- `Box<int>` and `Box<str>` are distinct types — assigning one to the
  other is a type mismatch.
- `impl Box<T>` scopes `T` over every method; the receiver unifies the
  arguments at each call. A plain `impl Box` for a generic struct is an
  error, and a method parameter may not shadow an impl parameter.
- Type arguments erase at runtime: values store field data only, so
  generic code runs identically to hand-monomorphized code (same
  opcodes, same generated C — verified by test, not just claimed).
  Value semantics hold unchanged: functions receive copies regardless
  of type arguments.

### Method Call Syntax

Method calls desugar to function calls with the receiver as the first argument:

```zz
struct Point { x: int, y: int }
func dist(p: Point) -> int { p.x + p.y }

p := Point{ x: 3, y: 4 }
dist(p)       // function call
p.dist()      // method call equivalent
```

### Try Operator (`?`)

Unwraps a variant, propagating `.none`/`.err` upward on failure:

```zz
func process(input: str) -> Option<int> { val := int(input)?; .some(val + 1) }
```

Note: `?` joins with the next line. Use `match` for branching across lines:

```zz
func parse_age(input: str) {
    match int(input) {
        .some(val) => println("age: {val}"),
        .none      => println("not a number"),
    }
}
```

### Variant Constructors

```zz
.ok(42)                     // Result variant
.err("boom")                // Result variant
.some("hello")              // Option variant
.none                       // Option variant
.Point{ x: 1, y: 2 }       // Struct variant
```

## Pattern Matching

### Basic Match

```zz
x := .some(5)
match x {
    .some(n) => println("got {n}"),
    .none    => println("nothing"),
}
```

Separate arms with a comma or a newline — a missing separator names
itself (`expected `,` or newline between match arms`) and `zz fix`
inserts the comma.

### Literal Patterns

```zz
match 42 {
    0   => "zero",
    1   => "one",
    _   => "other",
}
```

### Binding Patterns

```zz
match .ok(5) {
    .ok(n)  => n * 2,
    .err(e) => 0,
}
```

`.ok`, `.err`, and `.some` always carry a payload, so their patterns
require an argument — a bare `.ok` / `.err` is a type error. Use `_`
to ignore the payload (`.ok(_)`, `.err(e)`). Only payloadless
variants (`.none`) match bare. `zz fix` rewrites bare patterns
automatically.

### Must-use Results

A `Result` or `Option` used as a bare statement is a warning, not an
error — the value (and any error) is silently discarded:

```zz
fs.write(path, data)   // warning: unused `Result`
```

Handle it (`match`), propagate it (`?`), or ignore it explicitly
(`_ := fs.write(path, data)`). Block tails are values, not discards,
so `-> Result` function bodies never warn. Loop-body tails and
`defer` expressions do warn — their values are discarded too.

Type mismatches name the fix direction: argument errors name the
expected parameter (``parameter `greeting` expects `str` ``), and
annotation errors suggest the corrected annotation.

### Dead Arms

Arms after a catch-all arm (`_` or a bare binding) can never run and
warn as unreachable. A guarded catch-all (`_ if cond =>`) does not
count — the guard may fail and fall through.

### Nested Patterns

```zz
match .some(.ok(2)) {
    .some(.ok(n)) => n,
    _             => 0,
}
```

### Wildcard Pattern

```zz
match x {
    .some(_) => println("has value"),
    _        => println("nothing"),
}
```

### Statement Arms

Arms accept statements as well as expressions — assignment, `:=`
declarations, `return`, `defer` — wrapped as if braced. `break` and
`continue` keep their expression form so divergence checking is
unchanged.

```zz
y := 0
match x {
    .some(v) => y = v,
    .none    => y = 0 - 1,
}
```

## Modules and Imports

### Import Statement

```zz
import std.math
import std.str
```

### Using Imports

```zz
import std.math
println(math.abs(-5))    // 5

import std.str
println(str.to_upper("hello"))    // "HELLO"
```

### Package Imports

Inside a project (a directory tree with `zz.toml`), the package name
maps to `src/` from any file — so `tests/` can import `src/` modules
without fragile `../src/...` paths:

```zz
import app.math               // <project-root>/src/math.zz, used as math.*
import app.math(add as f)     // selective: bare `f`
import app.utils.string_helpers as sh
import app                     // <project-root>/src/main.zz, used as app.*
```

Rules: precedence is `std` > package > registry dependency >
relative file; `-` and `_` spellings agree (`my-app` ⇔ `my_app`).
One file keeps one namespace: importing the same file as both
`app.utils` and a relative `utils` in one program is an error —
migrate all importers to the `app.*` form.

## Blocks as Expressions

Blocks evaluate to their last expression:

```zz
result := {
    x := 10
    y := 20
    x + y    // result = 30
}
```

## Unit Type

Functions without a return type return `unit`:

```zz
func do_nothing() {
    // returns unit implicitly
}
```
