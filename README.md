# ZZ

A statically-typed language with inferred declarations, pipeline operators, and rich diagnostics.

## Quick Start

```bash
# Install
cargo install --path crates/zz_cli

# Run a file
zz run hello.zz

# Start REPL
zz

# Check for errors
zz check src/
```

```zz
// hello.zz
name := input("What is your name? ")
println("Hello, {name}!")

func factorial(n: int) -> int {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}

println("5! = {factorial(5)}")
```

## Features

- **Inferred declarations** -- `x := 1` infers `int`; `x: float = 1.0` declares explicitly
- **Pipeline operator** -- `val |> func()` chains function calls, including across lines
- **Pattern matching** -- `match` with exhaustive variant patterns, `if let` for binding
- **Elvis operator** -- `val ?? fallback` unwraps or provides default
- **String interpolation** -- `"Hello {name}"` with format specs `{pi:.2f}`
- **Closures** -- `|x: int| x * 2` with captured environment
- **Generics** -- `func map<T, U>(...)` with type inference
- **Standard library** -- HTTP, JSON, filesystem, math, string operations
- **LSP support** -- autocompletion, go-to-definition, hover, diagnostics, formatting
- **Smart diagnostics** -- Levenshtein suggestions, fix-it hints, source-mapped errors

## Architecture

```
zz_frontend    Lexer, parser, AST, lossless formatter, diagnostics
zz_checker     Unification-based type checker with inference
zz_runtime     Tree-walker interpreter + bytecode VM
zz_stdlib      Standard library (70+ native functions)
zz_cli         CLI binary: REPL, run, check, fix, fmt
zz_lsp         Language Server Protocol implementation
```

```
zz_frontend ──> zz_checker ──> zz_stdlib ──> zz_cli
                    |                          |
                    v                          v
              zz_runtime ──────────────> zz_lsp
```

## Documentation

- [Syntax Reference](docs/syntax.md) -- complete grammar, operators, statements, expressions
- [Type System](docs/types.md) -- primitives, structs, generics, type inference rules
- [Standard Library](docs/stdlib.md) -- all 70+ built-in functions and modules
- [CLI Reference](docs/cli.md) -- commands, flags, REPL, diagnostics

## License

MIT
