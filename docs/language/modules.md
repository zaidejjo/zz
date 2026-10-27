# Modules

```zz
import std.math
import std.str

println(math.abs(-5))
println(str.to_upper("hi"))
```

## Selective and aliased imports

```zz
import app.math(add as f)   // bare f
import app.utils.helpers as h
import shapes(Tokens)       // bring a type into scope
```

## Package imports

Inside a project (nearest `zz.toml`), the package name maps to `src/` from any file:

```zz
import app.math          // <root>/src/math.zz as math.*
import app               // <root>/src/main.zz as app.*
```

Rules: `std` > package > registry dep > relative file.
`-` and `_` spellings agree (`my-app` is `my_app`).
One file may live under several namespaces at once; the first binding wins as
canonical and aliases rewrite through it so types keep one identity.
Two different files under one namespace stay an error — alias one side.

## Relative files

The CLI resolves imports relative to the importing file:

```
project/
  main.zz
  utils/helper.zz   // import utils.helper from main.zz
```

## Circular imports are rejected

```
a.zz imports b.zz, which imports a.zz  ->  error
```

Fix: extract shared types into `c.zz`; both `a` and `b` import `c`.

## Importing a std module

```zz
import std.str
import std.math
```

A full std import (`import std.str`) binds the head name `str` in your module,
so you can call its functions directly: `str.trim("  x  ")`, `str.length("hi")`.
That's just how the language works — `str` refers to the std module here, no
alias or type annotation required.

Selective imports (`import std.str(split)`) bind only the named items and never
the head, so they can't collide with a `str` binding you define yourself.

## Name collisions between packages

If two packages (or a package and a std module) import under the same head name
— e.g. both `import utils` and `import other.utils` bind `utils` — `zz check`
warns and tells you to alias one of them with `as`. You choose the alias:

```zz
import utils as u          // or: import other.utils as u
```

Aliasing is only required when two names actually collide; a single import of a
std module needs no alias.

## Cross-module types

```zz
// shapes.zz
struct Point { x: int, y: int }

// main.zz
import shapes
p := shapes.Point{ x: 1, y: 2 }
```

`pub type` / `pub enum` export aliases and enums. Dotted declarations
(`func shapes.distance(...)`, `struct shapes.Point`) define into a namespace.

Next: [Project layout](../getting-started/project-layout.md),
[Packages](../tooling/packages.md).
