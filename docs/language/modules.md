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
