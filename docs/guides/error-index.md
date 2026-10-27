# Error index — real messages, exact fixes

Every message below is a verbatim `error_at` / `warning_at` string from
`zz_checker` + `zz_frontend`. Copy the message, apply the fix, re-run `zz check`.

## Types

| Message | Fix |
|---|---|
| `type mismatch` / `type mismatch: expected \`X\`, found \`Y\`` | Annotate correctly or convert (`int(s) ?? 0`). Arg errors name the param. |
| `expected \`bool\`, found \`X\`` | Condition must be bool; compare explicitly (`x != 0`). |
| `struct \`S\` has no field \`f\`` | Check embedding promotion (`u.Base.f`) or add the field. |
| `unknown struct \`S\`` | Import the module or fix namespace (`shapes.Point`). |
| `unknown variant \`.X\`` | Fix spelling; user-enum patterns are short (`.IntLit(v)`). |
| `non-exhaustive match: missing \`…\` (or add a \`_\` arm)` | Add the variant or `_`. Guarded `_ if` does not count. |
| `expected tuple with N elements, found M` | Destructure arity must match (`(a, b) := t`). |
| `cannot use \`?\` on a value of type \`X\`` | `?` needs Option/Result; `match` or `??` otherwise. |
| `expected a dictionary or an array of tuples for \`for k, v in …\`` | Two-var `for` needs dict or `[(k, v)]`. |
| `expected {N} argument(s), found {M}` | Fix arity; named/default args still count. |
| `unknown function \`f\`` | Check import + selective list; typo suggestion applies. |
| `unknown parameter \`p\`` | Named-arg spelling; checker names valid params. |

## Scope

| Message | Fix |
|---|---|
| ``undefined variable `x` `` (+ `did you mean \`y\`?`) | Typo or missing import; `zz fix` renames safe cases. |
| ``requires `import MOD` `` | Add the import (`std.*` helpers gate on import). |
| ``x := …` shadows the outer `x`` (warning) | `:=` binds a fresh block-scoped name: use `=` to update the outer binding (fix-it), or rename the inner one. Destructuring warns without a fix — keep the result and assign separately. |
| ``import binds `head`, shadowing the std module `head`` (warning) | A full `import std.<head>` binds `head`, hiding `std.<head>` at runtime. Alias it: `import std.<head> as <name>_mod`. Selective imports (`import std.<head>(item)`) do not warn. |
| `unused Result` (warning) | `match`, `?`, or `_ := …`. Block tails exempt. |
| `unreachable` / dead arm after `_` | Move catch-all last or guard it. |

## Parser (recovery messages name the token)

| Message | Fix |
|---|---|
| `expected expression, found X` | Complete the RHS; check for stray operator/newline. |
| ``expected `,` or newline between match arms`` | Insert comma; `zz fix` does it. |
| ``expected `:` in struct init, found `=` `` | Use `:` (`Point{ x: 1 }`). |
| ``expected `)` / `]` / `}` to close …`` | Close the bracket on its line; inner newlines are fine. |
| `expected digit after decimal point` | `3.` is invalid; write `3.0`. |
| `expected \`{\` or \`(\` after \`sqlz!\`` | Macro shape; see `sqlz_universal` example. |

## HTTP lint (compile-time)

Duplicate routes, unknown methods, bad literal paths, handler arity, and
`param("typo")` names all error at check time naming the route + fix.
See [HTTP](../stdlib/http.md).

## Workflow

```bash
zz check --format json   # machine feed, same path as CLI
zz fix src/              # safe rewrites only
zz fix --hard src/       # incl. ambiguous
```

Pair with [Error catalog](errors.md) for narrative fixes and
[Parity](parity.md) for VM/AOT guarantees.
