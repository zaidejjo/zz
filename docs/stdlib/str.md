# `std.str` — string operations

Text processing: search, case conversion, padding, trimming, splitting,
joining, and a string builder.

```zz
import std.str

s := "  Hello World  "
println(str.trim(s))        // "Hello World"
println(str.to_upper(s))    // "  HELLO WORLD  "
println(str.split(s, " "))  // ["", "Hello", "World", ""]
println(str.join(["a","b"], ",")) // "a,b"
println(str.contains(s, "World")) // true
println(str.find(s, "World"))     // 9

// String builder
b := str.builder()
str.push_part(b, "foo")
str.push_part(b, "bar")
println(str.finish(b))      // "foobar"
```

## Types

None (operates on `str` / `[str]`).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `length(s)` | `length(s: str) -> int` | Character length. |
| `byte_len(s)` | `byte_len(s: str) -> int` | Byte length. |
| `bytes(s)` | `bytes(s: str) -> [int]` | UTF-8 byte values. |
| `contains(s, sub)` | `contains(s: str, sub: str) -> bool` | Substring check. |
| `count(s, sub)` | `count(s: str, sub: str) -> int` | Occurrences of `sub`. |
| `count_in(s, sub, start, end)` | `(s: str, sub: str, start: int, end: int) -> int` | Count in range `[start, end)`. |
| `find(s, sub, from)` | `find(s: str, sub: str, from: int) -> int` | First index of `sub` from `from`; `-1` if absent. |
| `find_in(s, sub, start, end)` | `(s: str, sub: str, start: int, end: int) -> int` | Find in range. |
| `rfind(s, sub, from)` | `rfind(s: str, sub: str, from: int) -> int` | Last index of `sub`. |
| `rfind_in(s, sub, start, end)` | `(s: str, sub: str, start: int, end: int) -> int` | Last index in range. |
| `starts_with(s, prefix)` | `starts_with(s: str, prefix: str) -> bool` | Prefix check. |
| `starts_with_at(s, sub, pos)` | `(s: str, sub: str, pos: int) -> bool` | Whether `sub` starts at `pos`. |
| `ends_with(s, suffix)` | `ends_with(s: str, suffix: str) -> bool` | Suffix check. |
| `ends_with_at(s, sub, pos)` | `(s: str, sub: str, pos: int) -> bool` | Whether `sub` ends at `pos`. |
| `split(s, sep)` | `split(s: str, sep: str) -> [str]` | Split on `sep`. |
| `join(items, sep)` | `join(items: [str], sep: str) -> str` | Join with separator. |
| `join_parts(parts, sep)` | `join_parts(parts: [str], sep: str) -> str` | Join builder parts. |
| `replace(s, old, new)` | `replace(s: str, old: str, new: str) -> str` | Replace all occurrences. |
| `to_lower(s)` / `to_upper(s)` | `(s: str) -> str` | Case conversion. |
| `trim(s)` / `trim_start(s)` / `trim_end(s)` | `(s: str) -> str` | Trim whitespace (start/both/end). |
| `trim_span(s, start, end)` | `(s: str, start: int, end: int) -> [int]` | Trimmed span as `[start, end]`. |
| `pad_left(s, len, pad)` / `pad_right(s, len, pad)` | `(s: str, len: int, pad: str) -> str` | Pad to width `len`. |
| `repeat(s, n)` | `repeat(s: str, n: int) -> str` | Repeat `s` `n` times. |
| `reverse(s)` | `reverse(s: str) -> str` | Reverse characters. |
| `chr(cp)` | `chr(cp: int) -> str` | One-char string from codepoint. |
| `ord(ch)` | `ord(ch: str) -> int` | Codepoint of a single char. |
| `classify(text, markers, bstart, bend, nested, whole)` | `(...) -> [int]` | Classify text by marker spans. |
| `builder()` | `builder() -> [str]` | Create a string builder. |
| `push_part(b, s)` | `push_part(b: [str], s: str) -> [str]` | Append a part. |
| `builder_len(b)` | `builder_len(b: [str]) -> int` | Number of parts. |
| `finish(b)` | `finish(b: [str]) -> str` | Concatenate all parts. |
