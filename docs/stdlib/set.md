# Set — `std.set`

Set helpers over arrays — uniqueness enforced by helpers. Sets are `[T]` arrays with `*_int` variants for `[int]`. Immutable style: helpers return new arrays.

```zz
import std.set
```

## Purpose

`std.set` treats arrays as mathematical sets: insert-if-absent, union, intersection, difference, membership. Helpers are pure and return new arrays — the input is never mutated. Use for deduplication, membership testing, and set algebra on small-to-medium collections.

## Quickstart

```zz
import std.set

set.insert(["a"], "b")                  // ["a", "b"]
set.insert(["a"], "a")                  // ["a"]  (no-op, already present)
set.union(["a"], ["b"])                 // ["a", "b"]
set.intersect(["a", "b"], ["b", "c"])   // ["b"]
set.diff(["a", "b"], ["b"])             // ["a"]
set.has(["a", "b"], "a")                // true
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `set.has` | `set.has(s: [str], x: str) -> bool` | Membership | `set.has(["a"],"a")` → true |
| `set.has_int` | `set.has_int(s: [int], x: int) -> bool` | `[int]` membership | |
| `set.insert` | `set.insert(s: [str], x: str) -> [str]` | Append if absent | `set.insert(["a"],"b")` → `["a","b"]` |
| `set.insert_int` | `set.insert_int(s: [int], x: int) -> [int]` | `[int]` insert-if-absent | |
| `set.remove` | `set.remove(s: [str], x: str) -> [str]` | Copy without element | `set.remove(["a","b"],"a")` → `["b"]` |
| `set.remove_int` | `set.remove_int(s: [int], x: int) -> [int]` | `[int]` remove | |
| `set.union` | `set.union(a: [str], b: [str]) -> [str]` | Deduplicated concat | `set.union(["a"],["b"])` → `["a","b"]` |
| `set.union_int` | `set.union_int(a: [int], b: [int]) -> [int]` | `[int]` union | |
| `set.intersect` | `set.intersect(a: [str], b: [str]) -> [str]` | Common elements | `set.intersect(["a","b"],["b","c"])` → `["b"]` |
| `set.intersect_int` | `set.intersect_int(a: [int], b: [int]) -> [int]` | `[int]` intersect | |
| `set.diff` | `set.diff(a: [str], b: [str]) -> [str]` | Elements of `a` not in `b` | `set.diff(["a","b"],["b"])` → `["a"]` |
| `set.len` | `set.len(s: [str]) -> int` | Size | `set.len(["a","b"])` → 2 |
| `set.is_empty` | `set.is_empty(s: [str]) -> bool` | Empty check | `set.is_empty([])` → true |

## Edge cases & gotchas

- **O(n) per check.** Sets are arrays, not hash sets — fine for small collections.
- **`insert` is idempotent.** Returns the same array when the element is already present (no duplicates).
- **Union/intersect/diff preserve first-array order** with deduplication.
- **No generic `T` set.** Only `[str]` and `[int]` variants exist; build custom set logic over `vec` for other types.

## Performance notes

- All ops: O(n·m) worst case (linear scans). Keep sets small (< hundreds) for interactive use.
- Immutable — safe across tasks.

