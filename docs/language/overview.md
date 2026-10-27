# Language overview

ZZ is a small systems language for services, CLIs, and data pipelines.

## Mental model

1. **Values, not references.** Variables hold independent values. Functions receive
   copies. There is no `&`, no borrow checker, no aliasing to reason about.
   See [Value semantics](value-semantics.md).
2. **Inferred, then checked.** `:=` infers, `: T =` pins. The checker unifies types
   and reports every error in one pass with a fix hint. See [Checker](../compiler/checker.md).
3. **Pipelines over nesting.** `|>` passes left as first arg. `match` + `?` + `??`
   keep happy-path code flat.
4. **Same on VM and native.** `zz run` (VM) and `zz build` (AOT) agree bit-for-bit
   on ints/floats. If they could diverge, the checker errors instead.
   See [Parity](../guides/parity.md).

## A complete program in 30 lines

```zz
import std.http
import std.json

struct User { name: str, age: int }

func adult(u: User) -> bool {
    return u.age >= 18
}

func main() {
    users := [User{ name: "Ada", age: 36 }, User{ name: "Bo", age: 12 }]
    names := [u.name for u in users if adult(u)]
    println(names) // ["Ada"]

    s := http.server()
    s = http.route(s, "GET", "/", |req| "ok")
    println("routes ready")
}
```

## Where to go next

- Syntax details: [Variables and functions](variables-functions.md),
  [Control flow](control-flow.md), [Strings](strings.md),
  [Collections](collections.md), [Structs and enums](structs-enums.md),
  [Pattern matching](pattern-matching.md), [Modules](modules.md).
- Complete grammar: [syntax.md](../syntax.md). Type rules: [types.md](../types.md).
