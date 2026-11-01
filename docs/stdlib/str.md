# String — `std.str`

String manipulation with byte-offset semantics matching Rust `str::find` exactly. Byte-identical results on VM and AOT backends. All functions take the string first, so they compose cleanly with `|>` pipelines.

```zz
import std.str
```

## Purpose

`std.str` is the canonical home for string operations. Bare `ord`, `chr`, and `len` are globals available without import; the `std.str.*` qualified forms enable method-call syntax (`"hi".trim()`) and selective imports (`import std.str(trim)`). Every function is pure (no side effects) and takes the string as its first parameter.

Strings are immutable UTF-8. Character-oriented operations (`length`, slicing via `[]`) count Unicode scalars; byte-oriented operations (`find`, `rfind`, `trim_span`, `bytes`) operate on UTF-8 byte offsets. This distinction is intentional — mixing them without conversion is a common bug source.

## Conceptual model

| Layer | What it operates on | Unit |
|-------|--------------------|------|
| Char layer | `length`, indexing `s[i]`, slicing `s[i:j]` | Unicode scalar values |
| Byte layer | `find`, `rfind`, `find_in`, `rfind_in`, `count_in`, `trim_span`, `bytes`, `ord`, `chr` | UTF-8 bytes |

Byte offsets are O(1) per call because ZZ strings store UTF-8 and cache byte length. Streaming over a string is O(n) total.

## Quickstart

```zz
import std.str

s := "  Hello World  "
str.trim(s)                  // "Hello World"
str.to_upper(s)              // "  HELLO WORLD  "
str.to_lower(s)              // "  hello world  "
str.contains(s, "World")     // true
str.split("a,b,c", ",")      // ["a", "b", "c"]
str.replace("foo bar", "bar", "baz")  // "foo baz"
str.starts_with("hello", "he")  // true
str.ends_with("hello", "lo")    // true
str.find("hello world", "o", 0)   // 4
str.rfind("hello world", "o", 10) // 7
str.bytes("AB")              // [65, 66]
ord("A")                     // 65
chr(233)                     // "é"
```

## Method syntax

Every `std.str` function works as a method on a string value:

```zz
import std.str

"  hello  ".trim()                    // "hello"
"hello".to_upper()                    // "HELLO"
"hello world".to_lower()              // "hello world"
"hello world".contains("world")       // true
"hello world".split(" ")              // ["hello", "world"]
"hello world".replace("world", "zz")  // "hello zz"
"hello".starts_with("he")             // true
"hello".ends_with("lo")               // true
"abc123".find("123", 0)                 // 3
```

Method calls resolve through `canonical_std_name` fallback — `s.trim()` is sugar for `std.str.trim(s)`.

## Error model

| Situation | Behavior |
|-----------|----------|
| `find` / `rfind` / `count_in` miss | Returns `-1` |
| `find_in` / `rfind_in` with negative start | Clamped to `0` |
| `starts_with_at` / `ends_with_at` empty `sub` | Never matches (returns `false`) |
| `ord("")` or `ord("ab")` (not one char) | Runtime error |
| `chr(negative)`, `chr > 0x10FFFF`, or surrogate | Runtime error |
| `bytes.to_str(invalid_utf8)` | `.err(message)` — never panics |

All lookups are total (no `.Result` wrapper) except `bytes.to_str`, which is fallible because UTF-8 validation can fail.

## Exhaustive reference

### Core operations

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `str.length` | `str.length(s: str) -> int` | Number of Unicode scalars (char-oriented) | `str.length("héllo")` → 5 |
| `str.byte_len` | `str.byte_len(s: str) -> int` | Number of UTF-8 bytes | `str.byte_len("é")` → 2 |
| `str.split` | `str.split(s: str, sep: str) -> [str]` | Split on separator (no regex) | `str.split("a,b,c", ",")` → `["a","b","c"]` |
| `str.contains` | `str.contains(s: str, sub: str) -> bool` | Substring test | `str.contains("hello", "ell")` → true |
| `str.count` | `str.count(s: str, sub: str) -> int` | Non-overlapping occurrences; empty `sub` counts `len+1` | `str.count("aaa", "a")` → 3 |
| `str.trim` | `str.trim(s: str) -> str` | Strip leading/trailing Unicode whitespace | `str.trim("  hi  ")` → `"hi"` |
| `str.to_upper` | `str.to_upper(s: str) -> str` | Unicode uppercase | `str.to_upper("hi")` → `"HI"` |
| `str.to_lower` | `str.to_lower(s: str) -> str` | Unicode lowercase | `str.to_lower("HI")` → `"hi"` |
| `str.replace` | `str.replace(s: str, old: str, new: str) -> str` | Replace all non-overlapping occurrences | `str.replace("foo bar", "bar", "baz")` → `"foo baz"` |
| `str.starts_with` | `str.starts_with(s: str, prefix: str) -> bool` | Prefix test | `str.starts_with("hello", "he")` → true |
| `str.ends_with` | `str.ends_with(s: str, suffix: str) -> bool` | Suffix test | `str.ends_with("hello", "lo")` → true |

### Byte-offset search (O(1) per call)

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `str.find` | `str.find(s: str, sub: str, from: int) -> int` | First match at/after byte `from`; `-1` on miss | `str.find("hello world", "o", 0)` → 4 |
| `str.rfind` | `str.rfind(s: str, sub: str, from: int) -> int` | Last match at/before byte `from`; `-1` on miss | `str.rfind("hello world", "o", 10)` → 7 |
| `str.starts_with_at` | `str.starts_with_at(s: str, sub: str, pos: int) -> bool` | Does `sub` match starting at byte `pos`? | `str.starts_with_at("hello", "ell", 1)` → true |
| `str.ends_with_at` | `str.ends_with_at(s: str, sub: str, pos: int) -> bool` | Does `sub` match ending at byte `pos`? | `str.ends_with_at("hello", "ell", 4)` → true |
| `str.trim_span` | `str.trim_span(s: str, start: int, end: int) -> [int]` | Trimmed `[lo, hi]` byte offsets within `[start,end)` | `str.trim_span("  hi  ", 0, 6)` → `[2, 4]` |
| `str.find_in` | `str.find_in(s: str, sub: str, start: int, end: int) -> int` | First match in `[start, end)`; `-1` on miss | |
| `str.rfind_in` | `str.rfind_in(s: str, sub: str, start: int, end: int) -> int` | Last match in `[start, end)`; `-1` on miss | |
| `str.count_in` | `str.count_in(s: str, sub: str, start: int, end: int) -> int` | Non-overlapping matches in `[start, end)` | |

### Unicode scalar conversions

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `ord` / `str.ord` | `ord(ch: str) -> int` | Unicode scalar of a single-char string | `ord("A")` → 65 |
| `chr` / `str.chr` | `chr(cp: int) -> str` | One-char string for scalar (validates range) | `chr(233)` → `"é"` |

### Byte conversion

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `str.bytes` | `str.bytes(s: str) -> [int]` | UTF-8 bytes as plain ints (one copy) | `str.bytes("AB")` → `[65, 66]` |
| `bytes.to_str` | `bytes.to_str(vs: [int]) -> Result<str, str>` | Strict UTF-8 decode; `.err` on invalid | `bytes.to_str([104, 105])` → `.ok("hi")` |

### Classification

| Function | Signature | Description |
|----------|-----------|-------------|
| `str.classify` | `str.classify(text, markers, bstart, bend, nested, whole) -> [int]` | Comment-aware line counts `[lines, code, comments, blanks]` |

`classify` parameters: `markers` = line-comment list (e.g. `["//"]`), `bstart`/`bend` = block-comment pair (e.g. `"/*"`, `"*/"`; `""` = none), `nested` = allow nested blocks, `whole` = block markers must occupy whole lines.

### Additional helpers

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `str.repeat` | `str.repeat(s: str, n: int) -> str` | Repeat `s` `n` times | `str.repeat("ab", 3)` → `"ababab"` |
| `str.is_empty` | `str.is_empty(s: str) -> bool` | True when zero scalars | `str.is_empty("")` → true |
| `str.reverse` | `str.reverse(s: str) -> str` | Reverse scalar order | `str.reverse("abc")` → `"cba"` |
| `str.pad_left` | `str.pad_left(s: str, len: int, pad: str) -> str` | Left-pad to `len` scalars | `str.pad_left("7", 3, "0")` → `"007"` |
| `str.pad_right` | `str.pad_right(s: str, len: int, pad: str) -> str` | Right-pad to `len` scalars | `str.pad_right("7", 3, "0")` → `"700"` |

### String builders

`str.builder` / `str.push_part` / `str.finish` / `str.builder_len` accumulate parts and join once — O(n) total vs O(n²) for repeated `+` concatenation in a loop.

```zz
import std.str

b := str.builder()
b = str.push_part(b, "hello ")
b = str.push_part(b, "Ada")
println(str.finish(b))       // "hello Ada"
println(str.builder_len(b))  // 10 (total scalars, without joining)
```

Use builders when constructing strings inside loops; never use `s = s + part` in a hot loop.

## Edge cases & gotchas

- **Offsets are bytes; `length` is chars.** `str.byte_len` and `str.length` can differ for non-ASCII. Always use byte offsets with `find`/`rfind`/`*_at`/`*_in`.
- **Empty `sub` semantics:** `find`/`rfind` return the clamped `from`; `find_in` returns clamped `start`; `rfind_in` returns clamped `end`; `count_in` returns `0`; `starts_with_at`/`ends_with_at` never match empty.
- **Negative inputs** to `find_in`/`rfind_in`/`trim_span` clamp to `0`.
- **`ord` requires exactly one scalar.** `ord("")` and `ord("ab")` are runtime errors. `chr` rejects surrogates, negatives, and values > `0x10FFFF`.
- **VM/AOT parity:** Byte offsets and empty-substring semantics are byte-identical on both backends (matching Rust `str::find`).

## Performance notes

- `find`/`rfind`/`contains` are O(1) setup + O(n) worst-case scan (Rust `str::find`).
- `str.builder` + `str.finish` is O(n); `s = s + part` in a loop is O(n²).
- `str.bytes` makes one copy; prefer `bytes.to_ints` if you already have a `bytes` buffer.
- All functions are readonly/pure — safe to call concurrently from multiple tasks.

