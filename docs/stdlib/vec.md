# `std.vec` — array pipelines

Array operations: map/filter/enumerate/fold/sort/push/pop and more.
`vec.*` is the canonical home; `map`/`filter`/`enumerate`/`zip` also work
as bare builtins.

```zz
import std.vec

nums := [3, 1, 4, 1, 5]
println(vec.sum(nums))           // 14
println(vec.contains(nums, 4))   // true
println(vec.sort(nums))          // [1, 1, 3, 4, 5]
println(vec.map(nums, |n| n * 2)) // [6, 2, 8, 2, 10]
println(vec.filter(nums, |n| n > 2)) // [3, 4, 5]

v := [1, 2, 3]
vec.push(v, 4)
println(vec.pop(v))              // 4
println(vec.len(v))              // 3
```

## Types

None (operates on `[T]` arrays).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `len(v)` | `len(v: [T]) -> int` | Element count. |
| `push(v, x)` | `push(v: [T], x: T) -> [T]` | Append `x`. |
| `pop(v)` | `pop(v: [T]) -> [T]` | Remove last element. |
| `insert(v, idx, x)` | `insert(v: [T], idx: int, x: T) -> [T]` | Insert at `idx`. |
| `remove(v, idx)` | `remove(v: [T], idx: int) -> [T]` | Remove at `idx`. |
| `reverse(v)` | `reverse(v: [T]) -> [T]` | Reversed copy. |
| `sort(v)` | `sort(v: [T]) -> [T]` | Sorted ascending. |
| `sort_by(v, cmp)` | `sort_by(v: [T], cmp: func(T,T)->int) -> [T]` | Sort with comparator. |
| `sort_by_desc(v, cmp)` | `(...) -> [T]` | Sort descending with comparator. |
| `sort_key(v, key)` | `sort_key(v: [T], key: func(T)->U) -> [T]` | Sort by key projection. |
| `contains(v, x)` | `contains(v: [T], x: T) -> bool` | Membership. |
| `index_of(v, target)` | `index_of(v: [int], target: int) -> int` | First index; `-1` if absent. |
| `last_index_of(v, target)` | `(v: [int], target: int) -> int` | Last index; `-1` if absent. |
| `map(v, f)` | `map(v: [T], f: func(T)->U) -> [U]` | Transform elements. |
| `filter(v, f)` | `filter(v: [T], f: func(T)->bool) -> [T]` | Keep matching elements. |
| `enumerate(v)` | `enumerate(v: [T]) -> [(int, T)]` | Index-value pairs. |
| `fold(arr, init)` | `fold(arr: [int], init: int) -> int` | Left fold over ints. |
| `concat(a, b)` | `concat(a: [int], b: [int]) -> [int]` | Concatenate int arrays. |
| `flatten(arr)` | `flatten(arr: [[int]]) -> [int]` | Flatten one level. |
| `join(v, sep)` | `join(v: [T], sep: str) -> str` | Join elements as text. |
| `max_val(v)` / `min_val(v)` | `(v: [T]) -> Option<T>` | Max / min element. |
| `sum(arr)` | `sum(arr: [int]) -> int` | Sum. |
| `sum_f(arr)` | `sum_f(arr: [float]) -> float` | Float sum. |
| `product(arr)` | `product(arr: [int]) -> int` | Product. |
| `product_f(arr)` | `product_f(arr: [float]) -> float` | Float product. |
