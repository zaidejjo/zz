# Syntax Reference

Complete grammar and syntax guide for the ZZ language.

## Lexical Structure

### Comments

```zz
// Line comment
# Alternative line comment
/* Block comment
   can span multiple lines */
```

### Identifiers

Identifiers start with a letter or underscore, followed by letters, digits, or underscores:

```zz
name := "ZZ"
_private := 42
camelCase := true
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

### Comparison and Logic

```zz
1 < 2           // true
1 == 1          // true
1 != 2          // true
true && false   // false
true || false   // true
!true           // false
```

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

## Modules and Imports

### Import Statement

```zz
import std.io
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
