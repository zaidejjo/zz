# Structs and enums

## Structs

```zz
struct Point { x: int, y: int }

p := Point{ x: 1, y: 2 }
p.x = 10
```

Nested mutation works: `rect.origin.x = 9`.

## Embedding

Name a type without a field to embed it. Fields and methods promote transitively;
direct members win:

```zz
struct Base { id: int, name: str }
struct User { Base, age: int }

impl Base {
    func area(self) -> int { self.id * 2 }
}

u := User{ id: 1, name: "Ada", age: 36 }
u.id      // 1, same as u.Base.id
u.area()  // 2
u.id = 99 // writes through to u.Base.id
```

`User{ id: 1, ... }`, `User{ Base: Base{...}, ... }`, and
`User{ Base{...}, ... }` mean the same. Mixing explicit + flattened leaves
of one subtree is ambiguous and rejected.

## Generic structs

```zz
struct Box<T> { v: T }

impl Box<T> {
    func get(self) -> T { self.v }
}

b := Box{ v: 42 }  // Box<int>, inferred
```

Rules: use sites name args (`Box<int>`, never bare `Box`).
`Box<int>` vs `Box<str>` are distinct types. `impl Box<T>` scopes `T` over
methods. Args erase at runtime.

## Type aliases

```zz
enum Token { Eof, IntLit(int), Name(str) }

type Tokens = [Token]
type Pair<T> = (T, T)

toks: Tokens = [Token.Eof]
p: Pair<int> = (1, 2)
```

Erase at check time, zero runtime cost. `pub type` exports across modules.

## Enums

```zz
enum Token {
    Eof,
    IntLit(int),
    Name(str),
}

t := Token.IntLit(42)
match t {
    .Eof     => "eof",
    .IntLit(v) => "int",
    .Name(s)   => s,
}
```

- Construction is qualified (`Token.IntLit(42)`); patterns are short (`.IntLit(v)`).
- Matches are exhaustiveness-checked; add `_` or cover every variant.
- One payload max per variant — use a tuple for more.
- Generic (`enum Box<T> { V(T), E }`), methods (`impl Token`), `if let`, nesting,
  and structural equality all work. `pub enum` exports.

## Method dispatch

`value.name(...)` resolves in this order:

1. **the receiver's own `impl` method** — `impl W { func get(self) }` registers
   `W.get`, and `w.get()` reaches it even when a free `get(w)` exists;
2. **a free function `name`** with the receiver as its first argument
   (`p.dist()` is `dist(p)`), when the receiver's type has no such method;
3. **the receiver-typed stdlib namespace** (`str.`, `vec.`, `dict.`, `option.`).

Both forms stay reachable, so a same-named pair can coexist:

```zz
struct W { v: int }
func get(w: W) -> str { "free" }
impl W {
    func get(self) -> str { "method" }
}

w := W{ v: 1 }
w.get() // "method"
get(w)  // "free"
```

Keep the names distinct when the semantics differ — a method call always
prefers the method, never the free function.

## Recursive types

Behind an indirection (array, Option, dict) plus mutual pairs:

```zz
struct Node { n: int, kids: [Node] }
enum R { Null, Int(int), List([R]) }
```

Order never matters. Every bind deep-copies, so depth-n builds by wrapping
cost O(n²) until move semantics land. Keep depth modest; prefer iteration
for large inputs. Very deep values abort with stack-overflow, never silent corruption.

Printing renders `Type{field: value, ...}` identically on VM and AOT.
