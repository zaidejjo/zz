# REPL tour

```bash
zz
```

```
ZZ REPL — type :help for commands, Ctrl+D to exit
zz>
```

## Session basics

```zz
zz> x := 42
42
zz> x + 1
43
zz> func add(a: int, b: int) -> int { return a + b }
<func add>
zz> add(3, 4)
7
```

- Bindings persist across snippets.
- Each snippet is type-checked before it runs.
- Continuation prompt is `  | ` for open parens or trailing operators — paste multi-line closures and pipelines directly:

```zz
zz> result := "  Hello World  "
  |> str.trim()
  |> str.to_upper()
"HELLO WORLD"
```

## Commands

| Command | Effect |
|---------|--------|
| `:help` | Show REPL help |
| `:quit` / `Ctrl+D` | Exit the REPL |
| `:type expr` | Show the inferred type of an expression |
| `:clear` | Drop all bindings |

## Tips

- Use `:type` when inference surprises you: `:type [1, 2]` is `[int]`.
- The REPL is VM-backed — ideal for probing the stdlib interactively (`str.*`, `vec.*`, `json.*`).
- For file-level behavior (modules, `zz.toml` dependencies, AOT builds) use `zz run` / `zz test` / `zz build` instead.
- Every snippet runs in the same session, so a function defined earlier is available later.

Next: [Language overview](../language/overview.md).
