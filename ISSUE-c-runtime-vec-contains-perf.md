# Issue: C runtime `zz_vec_contains` is O(n) with per-element `zz_binop` + 2 clones

## Location
`crates/zz_codegen_runtime/src/runtime/collections.c` — `zz_vec_contains` (collections.c:1458–1466)

## Problem
`vec.contains(arr, item)` scans the whole array and, per element, clones both the element and the item, then runs the full `zz_binop(ZZOP_EQ, ...)` equality:
```c
for (size_t i = 0; i < arr.arr->len; i++) {
    if (zz_truthy(zz_binop(ZZOP_EQ, zz_clone(arr.arr->items[i]), zz_clone(item))))
        return true;
}
```
This is O(n) with two refcount bumps + a full structural equality call per element. For arrays of strings/arrays/dicts that is two heap bumps per element plus potentially recursive deep equality.

## Impact
`contains` on large arrays of heap values is slower than necessary: O(n) scans with per-element allocation traffic (clone bumps) instead of a cheap hash/pointer pre-check.

## Suggested fix
Add a cheap pointer/interned-string fast path: if `item` is an interned string singleton (or any interned/immortal value), compare by pointer first (`a.s == b.s`) before falling through to `zz_binop`. For other tags, keep `zz_binop` but avoid cloning when tags differ (unequal tags can never be equal per `zz_values_equal`). Consider a hash-backed `contains` for large dict-keyed lookups if `contains` is proven hot.

## References
Found during C-runtime audit (fix/c-runtime-audit). Related: `zz_values_equal`, `zz_key_eq`.
