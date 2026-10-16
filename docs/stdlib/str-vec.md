# Strings and arrays — `std.str`, `std.vec`, `std.bytes`

```zz
import std.str
import std.vec
import std.bytes
```

## `std.str`

| Function | Description |
|---|---|
| `length`, `is_empty`, `reverse` | basics |
| `split(s, sep)`, `join(parts, sep)` | split/join |
| `contains`, `starts_with`, `ends_with` | predicates |
| `trim`, `trim_start`, `trim_end` | whitespace |
| `to_upper`, `to_lower` | case |
| `replace(s, old, new)`, `repeat(s, n)` | build |
| `count(s, sub)` | non-overlapping; empty `sub` counts chars+1 |
| `pad_left`, `pad_right` | padding |
| `find(s, sub, from)`, `rfind(s, sub, from)` | byte offset or -1; empty `sub` returns clamped `from` |
| `starts_with_at`, `ends_with_at` | match at byte offset (empty never matches) |
| `find_in`, `rfind_in`, `count_in` | bounded `[start, end)` search |
| `trim_span(s, start, end)` | trimmed `[lo, hi]` byte offsets, Unicode ws |
| `bytes(s)` | UTF-8 bytes as `[int]` |
| `byte_len(s)` | byte length (vs char `length`) |
| `ord(ch)` / `str.ord` | codepoint; empty/multi-char errors |
| `chr(cp)` / `str.chr` | 1-char string; surrogates/negatives/>0x10FFFF error |
| `classify(text, markers, bstart, bend, nested, whole)` | `[lines, code, comments, blanks]` |

Method syntax works: `"  hi  ".trim()`, `"hello".to_upper()`.

Offsets are bytes O(1) per call and match Rust `str::find` edge semantics.
`length`/slicing stay char-oriented — convert explicitly when mixing.

## `std.vec`

| Function | Description |
|---|---|
| `len`, `contains`, `index_of`, `last_index_of` | search |
| `push(v, x)`, `pop(v)` | append / drop last (return new arrays) |
| `insert(v, idx, x)`, `remove(v, idx)` | splice |
| `reverse`, `sort` | copies |
| `join(v, sep)` | string join |
| `enumerate`, `fold`, `concat`, `flatten` | iteration |
| `sum`, `product`, `min_val`, `max_val` | ints |
| `sum_f`, `product_f` | floats |

```zz
import std.vec

nums := [3, 1, 2]
vec.push(nums, 4)    // [3, 1, 2, 4]
vec.sort(nums)       // [1, 2, 3]
[3, 1, 2].reverse()  // [2, 1, 3]
```

## Builders (`std.str` + `std.bytes`)

Strings are immutable; `s = s + part` in a loop is O(n²). Collect and join once:

```zz
import std.bytes

b := str.builder()          // [str] handle
b = str.push_part(b, "hi ")
b = str.push_part(b, name)
println(str.finish(b))
println(str.builder_len(b)) // total chars without joining

nums := bytes.builder()     // [int] handle
nums = bytes.push_byte(nums, 300)  // mod 256, negatives wrap
nums = bytes.extend(nums, [1, 2])
println(bytes.len_of(nums))
bytes.from_ints(nums)       // normalize array
bytes.to_str([104, 105])    // .ok("hi"), strict UTF-8
bytes.to_ints(b)            // opaque bytes -> [int]
```

Full tables: [stdlib.md](../stdlib.md).
