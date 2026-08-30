# Type System

ZZ uses a unification-based type checker with full type inference.

## Primitive Types

| Type | Description | Example |
|------|-------------|---------|
| `int` | 64-bit signed integer | `42`, `-1` |
| `float` | 64-bit IEEE 754 float | `3.14`, `-0.5` |
| `bool` | Boolean | `true`, `false` |
| `str` | UTF-8 string | `"hello"`, `""` |
| `unit` | Empty tuple / void | `()` |

## Compound Types

### Arrays

Homogeneous, dynamically-sized:

```zz
nums: [int] = [1, 2, 3]
names: [str] = ["Alice", "Bob"]
empty: [int] = []
```

### Dicts

Key-value pairs, insertion-ordered:

```zz
ages: {str: int} = {"Alice": 30, "Bob": 25}
```

### Structs

Named record types with typed fields:

```zz
struct Point { x: int, y: int }
struct Rect { origin: Point, w: int, h: int }

p := Point{ x: 1, y: 2 }
r := Rect{ origin: p, w: 10, h: 20 }
```

## Variant Types

ZZ has two built-in variant types: `.some`/`.none` and `.ok`/`.err`. These are constructed with dot-prefixed literals:

```zz
x := .some(42)
y := .none
a := .ok("success")
b := .err("failed")
```

## Type Inference Rules

### Short Declaration (`:=`)

The type is inferred from the right-hand side:

```zz
x := 42           // inferred: int
pi := 3.14        // inferred: float
name := "ZZ"      // inferred: str
alive := true     // inferred: bool
arr := [1, 2, 3]  // inferred: [int]
```

### Explicit Declaration (`: type =`)

The type is checked against the right-hand side:

```zz
x: int = 42           // OK
x: int = 3.14         // ERROR: expected int, found float
scores: [int] = [1, 2, 3]  // OK
```

### Function Return Type Inference

Return type is inferred from the body when not specified:

```zz
func add(a: int, b: int) { a + b }
// Inferred return type: int
```

### Generic Type Inference

```zz
func identity<T>(x: T) -> T { x }
result := identity(42)    // T inferred as int
```

## Type Checking Rules

### Expression Type Checking

| Expression | Result Type |
|------------|-------------|
| `42` | `int` |
| `3.14` | `float` |
| `"hello"` | `str` |
| `true` / `false` | `bool` |
| `a + b` | Common type of `a` and `b` |
| `a == b` | `bool` |
| `!a` | `bool` |
| `a \|> f` | Return type of `f` |
| `a ?? b` | Type of `b` |
| `f(args)` | Return type of `f` |
| `.some(x)` | Variant wrapping `x` |
| `.ok(x)` | Variant wrapping `x` |

### Operator Type Rules

| Operator | Left Type | Right Type | Result |
|----------|-----------|------------|--------|
| `+`, `-`, `*`, `/`, `%` | `int` | `int` | `int` |
| `+`, `-`, `*`, `/` | `float` | `float` | `float` |
| `+` | `int` | `float` | `float` |
| `+` | `str` | `str` | `str` |
| `+` | `[T]` | `[T]` | `[T]` (concat) |
| `**` | `int` | `int` | `int` |
| `==`, `!=` | any | same | `bool` |
| `<`, `>`, `<=`, `>=` | `int`/`float` | same | `bool` |
| `&&`, `\|\|` | `bool` | `bool` | `bool` |
| `??` | any | `T` | `T` |
| `..` | `int` | `int` | Range |

### Unification

The type checker uses unification to resolve types:

```zz
x := 42         // x: T where T unifies with int
y := x + 1      // x must be int, result is int
```

### Error Suppression

When a type error is detected, a sentinel `Error` type is used to prevent cascading errors:

```zz
x := undefined_var   // ERROR: undefined variable `undefined_var`
y := x + 1           // No additional error; x is already Error type
```

## Special Types (Internal)

| Type | Description |
|------|-------------|
| `json` | Opaque JSON value from `std.json.parse` |
| `http.server` | HTTP server handle from `std.http.server` |
| `int..` | Integer range from `range()` or `..` operator |

## Struct Type Rules

### Field Access

```zz
struct Point { x: int, y: int }
p := Point{ x: 1, y: 2 }
p.x    // int
p.y    // int
p.z    // ERROR: Point has no field `z`
```

### Field Mutation

Fields are mutable if the struct variable is mutable:

```zz
p := Point{ x: 1, y: 2 }
p.x = 10    // OK
```

### Cross-Module Structs

Structs can be defined and used across modules:

```zz
// shapes.zz
struct Point { x: int, y: int }

// main.zz
import shapes
p := shapes.Point{ x: 1, y: 2 }
```

### Dotted Struct Names

```zz
struct shapes.Point { x: int, y: int }
// Accessible as shapes.Point
```
