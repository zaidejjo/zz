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

### User Enums

`enum` declares a tagged union with payload variants. Values erase to
qualified objects; the type system tracks the enum statically:

```zz
enum Token {
    Eof,
    IntLit(int),
    Name(str),
    Pair((int, int)),
}

b := Token.IntLit(42)
```

Construction is call-shaped (`Token.IntLit(42)`); matching uses
dot-prefixed patterns with payload bindings, including nesting:

```zz
func tag(t: Token) -> str {
    match t {
        .Eof => "eof"
        .IntLit(_) => "int"
        .Name(_) => "name"
        .Pair(_) => "pair"
    }
}
```

Matches over user enums are exhaustiveness-checked: a missing variant
is a check-time error naming it (`non-exhaustive match: missing
`.IntLit` (or add a `_` arm)`). Enums support methods (`impl Token`),
generic parameters (`enum Box<T> { V(T), E }`, inferred from the
payload), structural equality, `if let` patterns, and nesting inside
structs, arrays, and other enums. A call to a plain function that
returns an enum is always a call, never construction — even when the
enum is declared first in the file.

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

### Embedded Fields

`struct User { Base, age: int }` embeds `Base`. Field and method lookup
promote transitively (`u.id` → `u.Base.id`); direct members shadow promoted
ones. Literals accept flattened leaves (`User{ id: 1, name: "Z", age: 19 }`),
explicit values (`User{ Base: Base{...}, age: 19 }`), or the shorthand
(`User{ Base{...}, age: 19 }`). Printing a struct — `println(u)`, f-strings,
`str(u)` — renders `Type{field: value, ...}` identically on the VM and AOT
backends.

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

### Recursive Types

Structs and enums may reference themselves behind an indirection —
arrays, options, or dicts — plus mutually recursive pairs:

```zz
struct Node { n: int, kids: [Node] }
struct A { b: B, n: int }
struct B { a: Option<A>, n: int }
enum R { Null, Int(int), List([R]) }
```

The checker pre-registers type names, so declaration order never
matters: enums and helpers may be declared before or after the types
they use. Both engines share identical semantics; the AOT unbox
analysis treats any cycle as boxed.

Value semantics apply unchanged: every read or bind of a recursive
value deep-copies the whole tree, so copying costs O(size) and
building depth-n trees by wrapping costs O(n²) until move semantics
(#267) land. Shallow-to-moderate depth is routine (50-deep
constructs and walks in milliseconds on both engines; 300-deep in
about a second on VM debug builds). Very deep values (tens of
thousands) abort the process with a stack-overflow message — never
silently corrupt, but not a catchable error either; keep recursive
depth modest and prefer iterative accumulation for large inputs.
Direct (unindirected) cycles such as `struct V { v: V }` check clean
but are uninhabited — no finite value exists — and stay boxed.

## Value Semantics: Copy by Default, Moves Are Invisible

ZZ has value semantics. Every variable binding holds an independent
value: after `y := x`, `y := f(x)`, or `return x`, mutating one side
never observes through the other. There is no aliasing of mutable
state through ordinary bindings — `y := id(doc)` stays independent of
`doc` no matter what the callee does with its parameter.

Concretely, what the programmer can rely on:

- Reads copy. Evaluating a variable (`LoadVar`/`LoadSlot` in the
  bytecode VM, `Env::get` in the tree-walker) yields a deep,
  independent copy of the whole value.
- Binds copy. `p := expr` stores a copy in `p` and yields the
  evaluated value (both engines `define` a clone and push the
  original); the source (if it names a variable) is untouched and
  stays live.
- Calls conceptually copy in, move internally. Argument expressions
  are evaluated (cloning any variables they read) and the resulting
  values are moved into the callee frame — no second copy at bind
  time. Returns move to the caller.
- Plain stores move. `x = v` overwrites the home without cloning.
- No use-after-move exists. The checker performs no move tracking
  (`crates/zz_checker/src` has no moved-from state) because no move
  is observable: every statement leaves its target holding a complete,
  valid value, and every live binding keeps an independent copy.

What stays copying (the engine must clone):

- Any read of a binding that stays live afterwards. The canonical
  case is a by-value read argument: `p := find_pos(doc, ...)` clones
  `doc` on evaluation because `doc` is used again later in the loop —
  last-use threading cannot fire, and restoring after the call is
  impossible without callee cooperation. See below.
- Closure captures (cloned at capture; captures outlive the call),
  multi-use right-hand sides (`x = x + x`), globals, and field/index
  reads outside the fused windows.

Where the engine moves internally (unobservable optimization, not a
semantic promise):

- Self-reassignment threading: `doc = push_node(doc, ...)` moves the
  single-mentioned local into the call (`TakeVar`/`TakeSlot`; `Unit`
  sits in the slot during evaluation, unobservable since the
  statement completes before any user read). Fires only for plain
  uncaptured slot locals outside top-level `main`; globals, captured
  names, and path targets keep the cloning path.
- Take-restore field windows (#394): `s.f = pure`, `s.f[i] = pure`,
  `s.f = s.f OP pure`, `s.f[i]` reads (clones one element only), and
  `len(s.f)` (zero clones) take the home, operate owned, and restore —
  including on error paths. Gated on window purity (no
  closures/spawns/early-exits, no home references; calls evaluate
  pre-take). Tree-walker mirrors sit behind `run_tree_walker` with a
  callee-visibility guard.
- AOT readonly-parameter borrows (#383): `zz_codegen/src/lower/`
  `param_escape.rs` proves per-parameter read-only (no stores,
  returns, forwards to unknown code, captures, or slot rebinds, to
  fixpoint) and passes those arguments borrowed instead of retained
  shares. Anything unproven keeps clone behavior.

None of the above changes program meaning: differential runs over the
fixture corpus must read identically with every optimization on or
off. If a future analysis ever made a move observable, that would be
a language change requiring a checker error (use-after-move), not a
silent optimization.

Backing stores are shared, values are not: on the VM, cloning an
array, dict, tuple, struct, or string bumps a reference count (O(1))
instead of duplicating the contents. Any mutation goes through a
copy-on-write gate — uniquely-owned buffers mutate in place, shared
ones copy once at the write. Programs cannot observe the sharing
(equality stays deep, mutation never leaks across bindings).

`clone(v)` pins a fully independent deep copy regardless of sharing:
use it when a value must survive later mutation of its source, or to
force a snapshot before handing data to tasks and plugins.
