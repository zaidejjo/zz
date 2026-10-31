# `std.set` — set helpers

Set operations on arrays of strings (`[str]`) and ints (`[int]`). Values
are deduplicated by membership checks.

```zz
import std.set

a := ["a", "b", "c"]
b := ["b", "c", "d"]

println(set.union(a, b))     // ["a", "b", "c", "d"]
println(set.intersect(a, b)) // ["b", "c"]
println(set.diff(a, b))      // ["a"]
println(set.has(a, "b"))     // true
println(set.len(a))          // 3
```

## Types

None (operates on `[str]` / `[int]` arrays).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `has(s, x)` | `has(s: [str], x: str) -> bool` | Whether `x` is in `s`. |
| `has_int(s, x)` | `has_int(s: [int], x: int) -> bool` | Whether `x` is in an int set. |
| `insert(s, x)` | `insert(s: [str], x: str) -> [str]` | Add `x`, deduped. |
| `insert_int(s, x)` | `insert_int(s: [int], x: int) -> [int]` | Add `x` to int set. |
| `remove(s, x)` | `remove(s: [str], x: str) -> [str]` | Remove `x`. |
| `remove_int(s, x)` | `remove_int(s: [int], x: int) -> [int]` | Remove `x` from int set. |
| `is_empty(s)` | `is_empty(s: [str]) -> bool` | Whether empty. |
| `len(s)` | `len(s: [str]) -> int` | Member count. |
| `union(a, b)` | `union(a: [str], b: [str]) -> [str]` | All members of either. |
| `union_int(a, b)` | `union_int(a: [int], b: [int]) -> [int]` | Int union. |
| `intersect(a, b)` | `intersect(a: [str], b: [str]) -> [str]` | Members of both. |
| `intersect_int(a, b)` | `intersect_int(a: [int], b: [int]) -> [int]` | Int intersect. |
| `diff(a, b)` | `diff(a: [str], b: [str]) -> [str]` | In `a` but not `b`. |
