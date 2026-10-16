# Lexicon — keywords, literals, comments, terminators

Source: `zz_frontend/src/token.rs` (lossless lexer) + `fmt.rs`. If this page and
the lexer disagree, the lexer wins and this page gets a fix same day.

## Keywords

Reserved: `import as func return if else while match for in struct break continue
defer true false pub impl const extern mut`.

Contextual (work as identifiers except in their position): `type`, `enum`.
`json.type(x)` and a variable named `type` keep working; `Token.IntLit(42)` vs
plain calls never confuse construction with invocation.

## Literals

| Form | Example | Type |
|---|---|---|
| int | `42`, `-1` | `int` (i64, wrap on overflow) |
| float | `3.14`, `-0.5` | `float` (f64, no fast-math) |
| bool | `true` / `false` | `bool` |
| str | `"hi"`, `"""multi"""` | `str` (UTF-8, immutable) |
| raw | `` `{"a":1}` ``, triple-backtick | `str`, fully literal |
| array | `[1, 2]`, `[]` | `[T]` |
| dict | `{"a": 1}`, `{}` | `{K: V}`, insertion-ordered |
| tuple | `(7, "seven")` | tuple, literal index only |
| range | `0..5`, `range(0, 10, 2)` | range |
| variant | `.some(1)`, `.none`, `.ok(1)`, `.err("x")` | Option / Result |
| unit | `()` (implicit when no `-> T`) | `unit` |

`{}` is an empty dict; `()` is unit. `[]` needs annotation when empty (`[]: [int]` context
or `empty: [int] = []`).

## Identifiers

Start with letter or `_`, then letters/digits/`_`: `name`, `_private`, `x1`.
`pub` prefix exports (`pub type`, `pub enum`); selective imports bring names bare.

## Comments

```zz
// line comment
/* block comment
   spans lines */
```

Comments are trivia (preserved by the lossless lexer for `zz fmt`); they never
nest and never affect semantics. Doc fences `/// ```zz` run as doctests —
see [Test helpers](../stdlib/testing-lib.md).

## Terminators

Newline at bracket depth 0, or `;`:

```zz
x := 1
y := 2
x := 1; y := 2  // same
result := add(
    1,
    2
)  // inner newlines do not terminate
```

## Formatting (`zz fmt`)

Lossless: trivia preserved, only spacing normalized. Check without writing:
`zz fmt -c src/`. Format in place: `zz fmt src/`. CI gates `cargo fmt` (Rust) and
`zz fmt` (ZZ) separately — one never substitutes for the other.

Next: [Operators](operators.md), [Strings](strings.md).
