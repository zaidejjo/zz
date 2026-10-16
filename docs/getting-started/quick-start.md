# Quick start

Create `hello.zz`:

```zz
name := input("What is your name? ")
println("Hello, {name}!")
```

Run it (VM, no build artifacts):

```bash
zz run hello.zz
```

Build a real binary (needs clang or `zig cc`):

```bash
zz build hello.zz
./hello
```

## The 60-second tour

```zz
// Variables: := infers, : T = checks.
x := 42
pi: float = 3.14

// Functions return their last expression.
func add(a: int, b: int) -> int {
    a + b
}

// Pipelines read left to right.
result := "  Hello World  "
    |> str.trim()
    |> str.to_upper()
// "HELLO WORLD"

// Safe parsing with Option + fallback.
age := int("42") ?? 0

// Match must handle every case.
match int("nope") {
    .some(v) => println("got {v}"),
    .none    => println("not a number"),
}
```

```bash
zz check hello.zz   # type-check, rich errors + suggestions
zz fmt hello.zz     # format in place
zz eval "1 + 2"     # quick eval, prints 3
```

Next: [Project layout](project-layout.md) and [REPL tour](repl-tour.md).
Deep grammar: [syntax.md](../syntax.md).
