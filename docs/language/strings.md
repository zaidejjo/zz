# Strings

## Interpolation

`{...}` runs inside `"..."` and `"""..."""` when `{` is followed by
identifier, digit, or `(`:

```zz
name := "Ada"
println("Hello, {name}!")
println("{1 + 2}")   // 3

pi := 3.14159
println("pi = {pi:.2f}")  // 3.14
n := 255
println("hex = {n:x} HEX = {n:X} oct = {n:o} bin = {n:b} dec = {n:d}")
```

A bare `{` that cannot expand warns at check time. Keep one interpolated
string per line for readable diffs.

## Literal braces: double them

`{{` renders `{`. A `}}` that closes a `{{` renders `}`. Unmatched `}}`
stays literal, so JSON needs no escaping:

```zz
println("{{name}}")      // {name}
name := "Ada"
println("{{{name}}}")    // {Ada}
println("{\"a\": {}}")  // {"a": {}}
```

Prefer doubled braces over `\{` / `\}` (kept for compatibility).

## Raw strings: backticks

Fully literal — no interpolation, no `\` escapes, no brace collapsing:

```zz
j := json.parse(`{"a": 1}`)
pat := `^[0-9a-f]{8}$`
win := `C:\new\test`
println(`a``b`)  // a`b — doubled backtick is the only escape
```

Triple-backtick spans lines and dedents like `"""..."""`. A fence cannot
contain itself; split the string there.

Rule of thumb: brace-heavy payloads with no interpolation go in backticks;
literal braces inside interpolated strings use `{{` / `}}`.

## Multiline

```zz
css := """
    .a{{color:red}}
    """
```

Dedents to the least-indented line. Interpolation and doubling rules match `"..."`.

## Common ops (`std.str`)

```zz
import std.str

println("  hi  ".trim())                 // "hi"
println("hello".to_upper())              // "HELLO"
println(str.contains("hello", "ell"))    // true
println(str.split("a,b,c", ","))         // ["a", "b", "c"]
println(str.replace("foo bar", "bar", "baz"))
```

Offsets (`find`, `rfind`, `*_at`, `*_in`, `trim_span`) are byte offsets, O(1) each.
`length`/slicing stay char-oriented — convert explicitly when mixing.

See [String](../stdlib/str.md) for the full table.
