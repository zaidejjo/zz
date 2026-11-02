# Operators — precedence, associativity, types

One page for every operator. When in doubt, parenthesize.

## Precedence (tightest first)

| Level | Operators | Assoc | Notes |
|---|---|---|---|
| 1 | `~` `!` `-x` (unary) | right | `~` int-only, `!` bool-only |
| 2 | `**` | right | power; `2 ** 3 ** 2` is `2 ** 9` |
| 3 | `*` `/` `%` | left | `/` int-div on ints; `%` remainder; divide-by-zero errors |
| 4 | `+` `-` | left | `+` also concat on `str` / `[T]`; mixed `int+float` → `float`; overflow wraps |
| 5 | `<<` `>>` | left | int-only; count masks `& 63`; negative count errors; `>>` arithmetic |
| 6 | `&` | left | int-only AND |
| 7 | `^` | left | int-only XOR |
| 8 | `\|` | left | int-only OR (single pipe; `\|\|` is logic) |
| 9 | `<` `<=` `>` `>=` `==` `!=` | left | `==`/`!=` any same-type; ordering on `int`/`float` (+ `str` compare) |
| 10 | `&&` | left | bool-only, short-circuit |
| 11 | `\|\|` | left | bool-only, short-circuit |
| 12 | `..` | — | range `0..5`; binds looser than `+` (`0+1..5` is `1..5`) |
| 13 | `??` | right | elvis: `.some(v) ?? d` is `v`, `.none ?? d` is `d`, non-variant passes through |
| 14 | `\|>` | left | pipeline: `x \|> f` is `f(x)`; chains left to right |
| 15 | `=` `+=` `-=` `*=` `/=` `%=` `**=` `<<=` `>>=` `&=` `\|=` `^=` | right | assignment; receiver evaluated once; cannot chain (`a += b += c` errors) |
| 16 | `?` | postfix | try: unwraps or early-returns `.none`/`.err`; joins with next line |

Bitwise chain reads: `flags & mask == expected` is `(flags & mask) == expected`;
`a + b << c` is `(a + b) << c`.

## Type rules

| Op | Left | Right | Result |
|---|---|---|---|
| `+ - * / %` | `int` | `int` | `int` (wrap on `+ - *`, error on `/ %` by zero) |
| `+ - * /` | `float` | `float` | `float` |
| `+` | `int` | `float` | `float` (and symmetrically) |
| `+` | `str` | `str` | `str` (concat) |
| `+` | `[T]` | `[T]` | `[T]` (concat) |
| `**` | `int` | `int` | `int` |
| `== !=` | any | same | `bool` (deep on values) |
| `< > <= >=` | `int`/`float` | same | `bool` |
| `&& \|\|` | `bool` | `bool` | `bool` |
| `??` | any | `T` | `T` |
| `..` | `int` | `int` | range |
| `& \| ^ << >> ~` | `int` | `int` | `int` (`~` unary) |

## Pitfalls with fixes

```zz
// WRONG: precedence surprise — `==` binds tighter than `&`
// flags & 7 == 7   // reads as (flags & 7) == 7 — parenthesize intent
flags := 7
mask := 7
ok := (flags & mask) == 7   // fixed: parenthesize intent

// WRONG: float equality — prefer epsilon comparison
// if x == 0.1 + 0.2 { }   // unreliable on floats
x := 1.0
if x > 0.9 && x < 1.1 { }   // epsilon-style comparison

// WRONG: chaining assignment — not allowed
// x += y += z   // error: cannot chain compound assignment

// WRONG: `?` across lines — joins with next line
// val := int(s)
// ?           // error: `?` binds to the next line
s := "42"
func f() -> Option<int> {
    val := int(s)?   // fine: `?` on the same line, inside a function
    return .some(val)
}
```

Integer guarantees (both engines): wrapping `+ - *`, `INT64_MIN / -1` errors,
literal `1 / 0` fails at check time, `>>` sign-extends, shifts mask `& 63`.

See [Value semantics](value-semantics.md), [Parity](../guides/parity.md).
