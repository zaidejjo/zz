# `std.map` — string-keyed dict helpers

Convenience operations on `{str: int}` and `{str: str}` dictionaries.
(Dicts are a built-in type; `std.map` provides common read/write helpers.)

```zz
import std.map

d := {"a": 1, "b": 2}
println(map.get_or(d, "a", 0))   // 1
println(map.get_or(d, "z", 9))   // 9 (default)
println(map.has(d, "a"))         // true
println(map.keys(d))             // ["a", "b"]
println(map.values(d))           // [1, 2]
println(map.len(d))              // 2
println(map.is_empty(d))         // false

merged := map.merge_str({"x": "1"}, {"y": "2"})
println(map.keys_str(merged))    // ["x", "y"]
```

## Types

None (operates on built-in `{str: int}` / `{str: str}` dicts).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `get_or(d, key, default)` | `get_or(d: {str: int}, key: str, default: int) -> int` | Value or `default` when absent. |
| `get_str(d, key, default)` | `get_str(d: {str: str}, key: str, default: str) -> str` | String value or `default`. |
| `has(d, key)` | `has(d: {str: int}, key: str) -> bool` | Whether `key` is present. |
| `is_empty(d)` | `is_empty(d: {str: int}) -> bool` | Whether the dict has no entries. |
| `keys(d)` | `keys(d: {str: int}) -> [str]` | All keys. |
| `keys_str(d)` | `keys_str(d: {str: str}) -> [str]` | All keys (string dict). |
| `len(d)` | `len(d: {str: int}) -> int` | Entry count. |
| `merge(a, b)` | `merge(a: {str: int}, b: {str: int}) -> {str: int}` | Merge two int dicts (`b` wins on conflict). |
| `merge_str(a, b)` | `merge_str(a: {str: str}, b: {str: str}) -> {str: str}` | Merge two string dicts. |
| `remove(d, key)` | `remove(d: {str: int}, key: str) -> {str: int}` | New dict without `key`. |
| `values(d)` | `values(d: {str: int}) -> [int]` | All values. |
| `values_str(d)` | `values_str(d: {str: str}) -> [str]` | All values (string dict). |
