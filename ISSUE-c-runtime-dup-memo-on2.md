# Issue: C runtime `zz_dup_memo` linear-scan memo approaches O(n²)

## Location
`crates/zz_codegen_runtime/src/runtime/core.c` — `zz_dup_memo_get` / `zz_dup_memo_put` (core.c:1325–1355)

## Problem
The deep-copy memo used by `zz_value_dup` (snapshot isolation for `spawn`, `chan.send`, `task`) is a flat `zz_dup_entry` array searched linearly on every `get`/`put`:
```c
static int zz_dup_memo_get(zz_dup_memo *m, const void *src, zz_value *out) {
    for (size_t i = 0; i < m->len; i++) {
        if (m->items[i].src == src) { ... }
    }
    return 0;
}
```
Values that are heavily shared across many cells (e.g. a closure capturing one large array referenced from many slots) cause each `dup_inner` recursion to pay O(memo_len), and the memo can approach O(n²) for deep/shared graphs. The fast path (`ZZ_DUP_INLINE_CAP=8`) covers the common 1–2 entry case, but large captures fall back to the linear array.

## Impact
`spawn`/`chan.send` on values with large shared subgraphs degrade toward quadratic in the memo size. Snapshot isolation stays correct (memo is best-effort dedup; copy stays correct on OOM), but it is slow.

## Suggested fix
Replace the linear array with a small hash table keyed by pointer, keeping the inline fast path for ≤8 entries. Cap growth and fall back gracefully (memo is best-effort: a missed dedup only causes extra copying, not incorrectness).

## References
Found during C-runtime audit (fix/c-runtime-audit). Related: `zz_closure_dup_build`, `zz_value_dup`, `zz_spawn_ex`.
