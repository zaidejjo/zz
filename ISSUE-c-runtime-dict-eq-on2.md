# Issue: C runtime `zz_values_equal` dict comparison is O(n²)

## Location
`crates/zz_codegen_runtime/src/runtime/core.c` — `zz_values_equal`, `ZZ_DICT` arm (core.c:121–143)

## Problem
Dictionary equality compares every entry of `a` against every entry of `b` with a nested linear scan:
```c
for (size_t i = 0; i < da->len; i++) {
    int found = 0;
    for (size_t j = 0; j < db->len; j++) { ... }
    if (!found) return 0;
}
```
This is O(da_len × db_len) per `==` comparison and O(n²) overall for equal-sized dicts. It feeds both `ZZOP_EQ`/`ZZOP_NE` in `zz_binop` and `zz_key_eq` (dict key matching), so `dict == dict` and dict construction/lookup degrade quadratically with size.

## Impact
Any program comparing large dicts natively pays quadratic time. The VM uses a hash-backed `DictData`, so this is also a VM/native parity gap for large dicts.

## Suggested fix
Build a hash index (or sort-based comparison) for the dict-equality path so matching keys are found in expected O(1) instead of O(n), matching the VM's `DictData` semantics. Guard small dicts to keep the linear scan (it's faster below the indexing threshold, as with `ZZ_DICT_INDEX_THRESHOLD`).

## References
Found during C-runtime audit (fix/c-runtime-audit). Related: `zz_key_eq` in `collections.h`.
