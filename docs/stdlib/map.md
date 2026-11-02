# Map — `std.map`

Pure-ZZ dict helpers over `{K: V}`: safe lookup, projections, merge. Dicts are insertion-ordered with O(n) lookup; the API is stable so a hashed implementation can drop in later without breaking callers.

```zz
import std.map
```

## Purpose

`std.map` provides safe, fallible-free lookups and projections over dicts. No runtime errors on missing keys — use `get_or` for defaults. Both `{str: int}` and `{str: str}` variants exist (`get_str`, `keys_str`, `values_str`, `merge_str`). Ideal for configuration, lookup tables, and small-to-medium keyed data.

## Conceptual model

Dicts are immutable values: `map.merge`, `map.remove` return a new dict; the original is unchanged. Lookup is O(n) (linear scan) — intentional for simplicity and stable iteration order. A future hashed implementation can replace the backing store without changing the API.

## Quickstart

```zz
import std.map

d := {"a": 1, "b": 2}
map.get_or(d, "z", 99)     // 99, no error
map.has(d, "a")            // true
map.keys(d)                // ["a", "b"]
map.values(d)              // [1, 2]
map.merge(d, {"c": 3})     // {"a":1,"b":2,"c":3}
map.remove(d, "a")         // {"b":2}
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `map.has` | `map.has(d: {str:int}, key: str) -> bool` | Membership (no error on missing) | `map.has(d,"a")` → true |
| `map.get_or` | `map.get_or(d: {str:int}, key: str, default: int) -> int` | Value or default | `map.get_or(d,"z",99)` → 99 |
| `map.get_str` | `map.get_str(d: {str:str}, key: str, default: str) -> str` | `{str:str}` get-or-default | `map.get_str({"k":"v"},"k","-")` → `"v"` |
| `map.keys` | `map.keys(d: {str:int}) -> [str]` | Key array (insertion order) | `map.keys(d)` → `["a","b"]` |
| `map.keys_str` | `map.keys_str(d: {str:str}) -> [str]` | `{str:str}` keys | |
| `map.values` | `map.values(d: {str:int}) -> [int]` | Value array | `map.values(d)` → `[1,2]` |
| `map.values_str` | `map.values_str(d: {str:str}) -> [str]` | `{str:str}` values | |
| `map.len` | `map.len(d: {str:int}) -> int` | Size | `map.len(d)` → 2 |
| `map.is_empty` | `map.is_empty(d: {str:int}) -> bool` | Empty check | `map.is_empty({})` → true |
| `map.merge` | `map.merge(a: {str:int}, b: {str:int}) -> {str:int}` | Right-wins union | `map.merge(d,{"c":3})` |
| `map.merge_str` | `map.merge_str(a: {str:str}, b: {str:str}) -> {str:str}` | `{str:str}` right-wins union | |
| `map.remove` | `map.remove(d: {str:int}, key: str) -> {str:int}` | Copy without key | `map.remove(d,"a")` → `{"b":2}` |

## Edge cases & gotchas

- **O(n) lookup.** Fine for configs and small tables; not for large lookup-heavy workloads.
- **Immutable:** `merge`/`remove` return new dicts; originals unchanged.
- **`get_or`/`get_str` never fail** — return the default on missing keys.
- **Right-wins merge:** `merge(a, b)` — `b`'s values overwrite `a`'s on key collision.
- **Insertion order preserved** across `merge`/`remove`.

## Performance notes

- Lookup: O(n). Insertion-ordered iteration is free.
- `merge`/`remove` allocate a new dict (copy of entries).
- Safe to share across tasks (immutable values).

