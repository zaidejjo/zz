# REPL tour

```bash
zz
```

```
ZZ REPL — type :help for commands, Ctrl+D to exit
zz>
```

## Session basics

```
zz> x := 42
42
zz> x + 1
43
zz> func add(a: int, b: int) -> int { a + b }
<func add>
zz> add(3, 4)
7
```

- Bindings persist across snippets.
- Each snippet is type-checked before it runs.
- Continuation prompt is `  | ` for open parens or trailing operators.

## Commands

| Command | Effect |
|---|---|
| `:help` | help |
| `:quit` | exit (Ctrl+D also works) |
| `:type expr` | show inferred type |
| `:clear` | drop all bindings |

## Tips

- Use `:type` when inference surprises you: `:type [1, 2]` is `[int]`.
- Paste multi-line closures and pipelines directly; the REPL waits for the close.
- REPL is VM-backed and ideal for stdlib probing (`str.*`, `vec.*`, `json.*`).
  For file-level behavior (modules, `zz.toml` deps) use `zz run` / `zz test`.

Next: [Language overview](../language/overview.md).
