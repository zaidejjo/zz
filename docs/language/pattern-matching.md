# Pattern matching

```zz
x := .some(5)
match x {
    .some(n) => println("got {n}"),
    .none    => println("nothing"),
}
```

Separate arms with comma or newline. Missing separator is a named error and
`zz fix` inserts the comma.

## Payload rules

`.ok` / `.err` / `.some` always carry a payload — patterns need an arg.
Use `_` to ignore: `.ok(_)`. Only `.none` matches bare.

```zz
match .ok(5) {
    .ok(n)  => n * 2,
    .err(e) => 0,
}
```

## Guards

```zz
match x {
    .some(n) if n > 0 => println("pos"),
    .some(n) => println("non-pos"),
    .none    => println("nothing"),
}
```

Guards accept the arm's bindings + literals + operators only. Calls, strings,
field/index access are check-time errors naming the subexpression. Move outer
vars and helper calls into the arm body.

For user enums the same shape holds:

```zz
match t {
    .Eof if verbose => println("eof!"),
    .Eof => println("eof"),
    _    => println("other"),
}
```

A guarded catch-all (`_ if cond`) does not count as exhaustive — the guard
may fail and fall through.

## Literals, nesting, wildcard

```zz
match 42 {
    0 => "zero",
    1 => "one",
    _ => "other",
}

match .some(.ok(2)) {
    .some(.ok(n)) => n,
    _             => 0,
}
```

## Statement arms

Arms accept statements (`=`, `:=`, `return`, `defer`) as if braced:

```zz
y := 0
match x {
    .some(v) => y = v,
    .none    => y = 0 - 1,
}
```

`break` / `continue` keep expression form so divergence checking is unchanged.

## Must-use results

`Result` / `Option` as a bare statement warns (value discarded):

```zz
fs.write(path, data)   // warning: unused Result
match fs.write(path, data) {
    .ok(_)  => println("ok"),
    .err(e) => println(e),
}
// or: _ := fs.write(path, data)  // explicit ignore
// or: fs.write(path, data)?      // propagate
```

Block tails are values, never discards — `-> Result` bodies never warn.
Dead arms after `_` / bare binding warn as unreachable.

Next: [Control flow](control-flow.md), [Error catalog](../guides/errors.md).
