# Quick start

Create `hello.zz`:

```zz
name := "World"
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

## From zero to a project

```bash
# Create a new CLI project (templates: cli | lib | web | plugin)
zz new my-app --template cli
cd my-app

# Run the generated src/main.zz (VM, fast)
zz run

# Build a native binary
zz build
./my-app
```

## The 60-second tour

```zz
// Variables: := infers, : T = checks.
x := 42
pi: float = 3.14

// Named functions return through `return`.
func add(a: int, b: int) -> int {
    return a + b
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

ZZ's sweet spot — an HTTP server in a few lines:

```zz
import std.http

func main() {
    s := http.server()
    s = s.route("GET", "/", |req| "ok")
    s = s.route("GET", "/users/:id",
        |req| "user-{http.param(req, "id") ?? "?"}")
    http.listen(s, 8080)
}
```

Run it:

```bash
zz run main.zz
```

Then `curl http://localhost:8080/` and `curl http://localhost:8080/users/42`.

## Verify

```bash
zz check hello.zz   # type-check, rich errors + suggestions
zz fmt hello.zz     # format in place
zz eval "1 + 2"     # quick eval, prints 3
zz test             # run @test functions (uses ./tests/ if present, else .)
```

Next: [Project layout](project-layout.md) and [REPL tour](repl-tour.md).
Deep grammar: [syntax.md](../syntax.md).
