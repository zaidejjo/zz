# `std.bytes` — mutable byte builder

A growable, zero-copy-friendly builder for constructing `bytes` from
integer byte values. Use when you need to assemble binary content byte by
byte (e.g. protocol frames, file headers) before converting to `str` or
`bytes`.

```zz
import std.bytes

b := bytes.builder()
bytes.push_byte(b, 72)   // 'H'
bytes.push_byte(b, 101)  // 'e'
bytes.push_byte(b, 108)  // 'l'
bytes.push_byte(b, 108)  // 'l'
bytes.push_byte(b, 111)  // 'o'
println(bytes.to_str(b)) // "Hello"

out := bytes.finish(b)   // Result<bytes, str> — consumes the builder
```

## Types

| Type | Notes |
|---|---|
| `bytes` | Immutable byte sequence produced by `bytes.finish`. Indexable, sliceable, convertible with `str.bytes` / `bytes.to_str`. |
| `[int]` builder | The builder itself is a mutable `[int]` of byte values (0–255). Pass it by reference to `push_byte`/`extend`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `builder()` | `builder() -> [int]` | Create a new empty byte builder. |
| `push_byte(b, v)` | `push_byte(b: [int], v: int) -> [int]` | Append byte `v` (0–255). Returns the builder for chaining. |
| `extend(b, vs)` | `extend(b: [int], vs: [int]) -> [int]` | Append many byte values. Returns the builder. |
| `len_of(vs)` | `len_of(vs: [int]) -> int` | Number of bytes in a builder/int array. |
| `to_ints(b)` | `to_ints(b: bytes) -> [int]` | Convert `bytes` back to a `[int]` of byte values. |
| `from_ints(vs)` | `from_ints(vs: [int]) -> [int]` | Identity-ish helper: normalize a builder/int array. |
| `finish(vs)` | `finish(vs: [int]) -> Result<bytes, str>` | Seal the builder into an immutable `bytes`. Fails if any value is out of range. |
| `len(v)` | `len(v: bytes) -> int` | Byte length of a `bytes` value. |
| `to_str(vs)` | `to_str(vs: [int]) -> Result<str, str>` | Interpret builder bytes as UTF-8 `str`. Fails on invalid UTF-8. |
