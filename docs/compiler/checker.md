# Checker — unification, inference, errors

`zz_checker` is a unification type checker with full inference and spans everywhere.

## Inference

```zz
x := 42         // T unifies with int
y := x + 1      // x must be int, result int
result := identity(42)  // T inferred as int
```

- `:=` infers; `: T =` checks and names the fix direction
  (``parameter `greeting` expects `str` ``, suggest corrected annotation).
- Return type inferred from body when omitted.
- Generic args inferred at construction/calls (`Box{ v: 1 }` is `Box<int>`);
  use sites must name args (`Box<int>`, never bare `Box`).
- Aliases erase at check time; enums tracked statically with exhaustiveness.

## Unification and sentinels

Mismatches produce an `Error` sentinel to stop cascades:

```zz
x := undefined_var  // ERROR: undefined variable
y := x + 1          // no second error; x is already Error
```

Typo suggestions use Levenshtein + fix-it hints with safety levels
(`zz fix` applies safe ones, prompts or `--hard` for ambiguous).

## What the checker proves (so backends stay identical)

- Literal tuple indices in range; dynamic `t[i]` rejected (destructure instead).
- Match exhaustiveness (missing variant named, or add `_`); dead arms after
  catch-all warn; `.ok`/`.err`/`.some` patterns require payload args.
- Route literals/methods/handler arity/duplicate routes/`param("typo")` for HTTP.
- Guard purity (bindings + literals + operators only; calls/strings/field/index
  error naming the subexpression).
- `Result`/`Option` bare-statement must-use warnings (block tails exempt).
- Recursive types behind indirection only for inhabitation; direct `struct V { v: V }`
  checks clean but is uninhabited and stays boxed.

## Performance

Per-module seed key-set restore (not O(seed) clones) + S1 content-addressed
check cache. Cold single file ~40ms, warm `0 checked`. See
[Check, fix, format](../tooling/check-fmt.md).

Types reference: [types.md](../types.md).
