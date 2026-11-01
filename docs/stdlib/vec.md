# Vector — `std.vec`

Generic array operations over `[T]`. Arrays are values (copy-on-write under the hood): mutating a parameter mutates a copy, so you re-bind the result. All functions return a new array. `vec.*` short form works after `import std.vec`. Fully generic over element type `T`. Byte-identical on VM and AOT.

```zz
import std.vec
```

## Purpose

`std.vec` is the canonical home for array pipelines. It provides immutable-style operations (each returns a new array) plus method-call syntax (`[1,2].push(3)`). Because arrays are values, `vec.push` does not mutate in place — use `xs = vec.push(xs, x)` or `xs = xs.push(x)`.

## Conceptual model

Arrays in ZZ are copy-on-write values. A `vec.*` call shares storage with the input until a write occurs, then copies (CoW). This makes chained operations cheap until the final result is materialized.

Method-call form (`xs.push(x)`) is sugar for `std.vec.push(xs, x)` and returns the new array — you must re-bind:

```zz
xs := [1, 2]
xs = xs.push(3)        // [1, 2, 3] — re-bind required!
```

## Quickstart

```zz
import std.vec

nums := [3, 1, 2]
vec.push(nums, 4)         // [3, 1, 2, 4]
vec.pop(nums)             // [3, 1]
vec.sort(nums)            // [1, 2, 3]
vec.reverse(nums)         // [2, 1, 3]
vec.join(nums, ", ")      // "3, 1, 2"
vec.contains(nums, 1)     // true
vec.insert(nums, 0, 99)   // [99, 3, 1, 2]
vec.remove(nums, 1)       // [3, 2]

// Method syntax (re-bind!)
[3, 1, 2].sort()          // [1, 2, 3]
[3, 1, 2].push(4)         // [3, 1, 2, 4]
[3, 1, 2].contains(1)     // true
["a", "b"].join(", ")     // "a, b"
```

## Aggregates & folds (pure-ZZ)

```zz
import std.vec

vec.sum([1,2,3])                  // 6
vec.product([1,2,3])              // 6
_ := vec.min_val([3,1,2])         // .some(1)
_ := vec.max_val([3,1,2])         // .some(3)
vec.sum_f([1.5, 2.5])             // 4.0   (Kahan-compensated)
vec.product_f([2.0, 3.0])         // 6.0
vec.concat([1,2], [3,4])          // [1,2,3,4]
vec.flatten([[1,2],[3]])          // [1,2,3]
vec.index_of([1,2,3], 2)          // 1
vec.last_index_of([1,2,2], 2)     // 2
vec.enumerate(["a","b"])          // [(0,"a"), (1,"b")]
```

## Exhaustive reference

### Core operations

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `vec.len` | `vec.len(v: [T]) -> int` | Array length | `vec.len([1,2])` → 2 |
| `vec.push` | `vec.push(v: [T], x: T) -> [T]` | New array with `x` appended | `vec.push([1,2], 3)` → `[1,2,3]` |
| `vec.pop` | `vec.pop(v: [T]) -> [T]` | New array without last element | `vec.pop([1,2,3])` → `[1,2]` |
| `vec.reverse` | `vec.reverse(v: [T]) -> [T]` | Reversed copy | `vec.reverse([1,2,3])` → `[3,2,1]` |
| `vec.join` | `vec.join(v: [T], sep: str) -> str` | Join elements as string | `vec.join([1,2], ",")` → `"1,2"` |
| `vec.contains` | `vec.contains(v: [T], x: T) -> bool` | Element exists (equality) | `vec.contains([1,2], 1)` → true |
| `vec.sort` | `vec.sort(v: [T]) -> [T]` | Sorted copy (natural order) | `vec.sort([3,1,2])` → `[1,2,3]` |
| `vec.insert` | `vec.insert(v: [T], idx: int, x: T) -> [T]` | Insert at index | `vec.insert([1,2], 0, 99)` → `[99,1,2]` |
| `vec.remove` | `vec.remove(v: [T], idx: int) -> [T]` | Remove at index | `vec.remove([1,2,3], 1)` → `[1,3]` |

### Comparator / key sorting

| Function | Signature | Description |
|----------|-----------|-------------|
| `vec.sort_by` | `vec.sort_by(v: [T], cmp: func(T,T)->int) -> [T]` | Comparator ordering (-1/0/1) |
| `vec.sort_by_desc` | `vec.sort_by_desc(v: [T], cmp: func(T,T)->int) -> [T]` | Descending comparator |
| `vec.sort_key` | `vec.sort_key(v: [T], key: func(T)->K) -> [T]` | Key-based ordering (K: int\|str\|float) |

`sort_by` comparator must return a negative/zero/positive integer. `sort_key` key type `K` must be naturally comparable.

### Aggregates

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `vec.fold` | `vec.fold(arr: [int], init: int, f: func(int,int)->int) -> int` | Left fold | `vec.fold([1,2,3], 0, |a,b| a+b)` → 6 |
| `vec.sum` | `vec.sum(arr: [int]) -> int` | Sum of ints | `vec.sum([1,2,3])` → 6 |
| `vec.product` | `vec.product(arr: [int]) -> int` | Product of ints | `vec.product([1,2,3])` → 6 |
| `vec.min_val` | `vec.min_val<T>(arr: [T]) -> Option<T>` | Minimum or `.none` if empty | `vec.min_val([3,1,2])` → `.some(1)` |
| `vec.max_val` | `vec.max_val<T>(arr: [T]) -> Option<T>` | Maximum or `.none` if empty | `vec.max_val([3,1,2])` → `.some(3)` |
| `vec.sum_f` | `vec.sum_f(arr: [float]) -> float` | Kahan-compensated float sum | `vec.sum_f([1.5,2.5])` → 4.0 |
| `vec.product_f` | `vec.product_f(arr: [float]) -> float` | Float product | `vec.product_f([2.0,3.0])` → 6.0 |
| `vec.concat` | `vec.concat(a: [int], b: [int]) -> [int]` | Concatenate two int arrays | `vec.concat([1,2],[3,4])` → `[1,2,3,4]` |
| `vec.flatten` | `vec.flatten(arr: [[int]]) -> [int]` | Flatten one level | `vec.flatten([[1,2],[3]])` → `[1,2,3]` |
| `vec.index_of` | `vec.index_of(arr: [int], target: int) -> int` | First index; `-1` on miss | `vec.index_of([1,2,3], 2)` → 1 |
| `vec.last_index_of` | `vec.last_index_of(arr: [int], target: int) -> int` | Last index; `-1` on miss | `vec.last_index_of([1,2,2], 2)` → 2 |
| `vec.enumerate` | `vec.enumerate<T>(v: [T]) -> [(int, T)]` | Index + value pairs | `vec.enumerate(["a","b"])` → `[(0,"a"),(1,"b")]` |

## Method syntax

```zz
import std.vec

[3, 1, 2].sort()          // [1, 2, 3]
[3, 1, 2].reverse()       // [2, 1, 3]
[3, 1, 2].push(4)         // [3, 1, 2, 4]  — re-bind: xs = xs.push(4)
[3, 1, 2].contains(1)     // true
["a", "b"].join(", ")     // "a, b"
["a","b"].enumerate()     // [(0,"a"),(1,"b")]
```

> **Important:** `push`/`pop`/`insert`/`remove`/`sort`/`reverse` return new arrays. Without re-binding, the original is unchanged.

## Edge cases & gotchas

- **Arrays are values.** `vec.push(nums, 4)` does not mutate `nums`. Re-bind: `nums = vec.push(nums, 4)` or `nums = nums.push(4)`.
- **`sort` requires comparable `T`.** For custom types, use `sort_by`/`sort_key`.
- **`sort_by` comparator contract:** return `-1`/`0`/`1` (any negative/zero/positive works). Inconsistent comparators yield unspecified order.
- **`sort_key` key `K` must be `int`\|`str`\|`float`.**
- **`min_val`/`max_val` return `Option`** — always handle `.none` for possibly-empty arrays.
- **`vec.append` was removed.** Use `vec.push` + method form `xs.push(x)`.
- **Index bounds:** `insert`/`remove` with out-of-range index are runtime errors.

## Performance notes

- CoW: chained `vec.*` calls share storage until a write; the final result is a single copy.
- `sort`/`sort_by`/`sort_key` allocate a new array (immutable style).
- `join` allocates the result string once.
- `fold`/`sum`/`product` are O(n) single-pass.
- All functions are readonly on input — safe across tasks.

## Production patterns

```zz
import std.vec

// Pipeline: filter even, double, sum
nums := [1,2,3,4,5,6]
evens := filter(nums, |n| n % 2 == 0)   // [2,4,6]
doubled := map(evens, |n| n * 2)        // [4,8,12]
result := vec.sum(doubled)              // 24

// Safe min with default
scores := [3, 1, 2]
best := vec.min_val(scores).unwrap_or(0)
```

